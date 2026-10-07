//! Off-tick persistence of player saves.
//!
//! The game loop is a single-threaded tokio runtime, so any `.await` on
//! Postgres inside a tick freezes the whole world. Saving every online
//! player serially from the tick (the old autosave) therefore stalled
//! every connected player for the duration of the slowest write — and
//! under pool starvation, for seconds (GitHub issue #29, "extreme lag").
//!
//! Design
//! ------
//! * **Snapshot on the tick, write off it.** [`SaveCoordinator::request_background`]
//!   builds the owned payload synchronously (pure in-memory ECS reads) and
//!   hands only the DB write to a `tokio::spawn`ed task. The tick never
//!   awaits it.
//! * **Staggered, not bursty.** [`SaveCoordinator::autosave_due`] picks, at
//!   most `AUTOSAVE_PER_SCAN` characters whose own last save is older than
//!   the autosave interval, oldest first. Each character therefore saves
//!   once per interval counted from *their* last save (login, explicit
//!   `save`, or previous autosave), so saves spread out naturally instead
//!   of every player hitting the pool in the same tick.
//! * **Bounded concurrency.** Background writers share a small semaphore
//!   (default 2) so the 8..16-connection pool always has headroom for
//!   logins and foreground saves.
//! * **Per-character ordering.** Every character has a [`Slot`] holding an
//!   async mutex (the "order lock") plus a monotonically increasing
//!   generation counter. A snapshot is stamped with the next generation
//!   when it is taken; whoever writes holds the order lock for the whole
//!   write and records the highest generation committed. A write whose
//!   generation is lower than one already committed is skipped as stale,
//!   so an old autosave can never overwrite a newer quit-save. Foreground
//!   saves (`save_player`) take the same lock *before* snapshotting, so
//!   they also queue behind an in-flight background write of the same
//!   character and observe its item-id stamps.
//! * **One in-flight write per character.** `in_flight` stops a second
//!   background snapshot of a character until the first has been folded
//!   back into the ECS ([`SaveCoordinator::apply_completions`]); this keeps
//!   `PersistedItemId` stamping consistent (no duplicate INSERTs).
//! * **Shutdown.** [`SaveCoordinator::flush`] waits for every spawned write.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use tokio::sync::{Notify, OwnedMutexGuard, Semaphore};
use tracing::warn;

use crate::login::{PlayerSaveSnapshot, apply_commit};

/// Max characters snapshotted per autosave scan (the scan runs once per
/// second). 2/s is ~600 players per default 5-minute interval — far above
/// the server's `max_connections` — while keeping the burst tiny.
pub(crate) const AUTOSAVE_PER_SCAN: usize = 2;
/// Concurrent background DB writers. Leaves most of the pool free for
/// login / command traffic.
pub(crate) const BACKGROUND_WRITERS: usize = 2;

/// Result of one background write, folded into the ECS on the next tick.
#[derive(Debug)]
enum Outcome {
    Committed(HashMap<usize, i32>),
    /// A newer generation was already committed; nothing written.
    Stale,
    Failed(String),
}

struct Completion {
    snapshot: Arc<PlayerSaveSnapshot>,
    slot: Arc<Slot>,
    outcome: Outcome,
}

/// Per-character save state.
struct Slot {
    /// Highest generation committed. Holding this lock *is* holding the
    /// character's write turn.
    order: Arc<tokio::sync::Mutex<u64>>,
    next_generation: AtomicU64,
    /// A background snapshot exists whose completion has not yet been
    /// applied to the ECS.
    in_flight: AtomicBool,
    last_save: Mutex<Instant>,
}

impl Slot {
    fn new() -> Self {
        Self {
            order: Arc::new(tokio::sync::Mutex::new(0)),
            next_generation: AtomicU64::new(1),
            in_flight: AtomicBool::new(false),
            last_save: Mutex::new(Instant::now()),
        }
    }

    fn touch(&self) {
        *self.last_save.lock().expect("last_save lock") = Instant::now();
    }

    fn since_last_save(&self) -> Duration {
        self.last_save.lock().expect("last_save lock").elapsed()
    }
}

struct Shared {
    slots: Mutex<HashMap<String, Arc<Slot>>>,
    writers: Arc<Semaphore>,
    done: Mutex<Vec<Completion>>,
    pending: AtomicUsize,
    idle: Notify,
}

/// Cloneable handle (stored as an ECS resource) to the save machinery.
#[derive(Resource, Clone)]
pub(crate) struct SaveCoordinator(Arc<Shared>);

impl Default for SaveCoordinator {
    fn default() -> Self {
        Self::new(BACKGROUND_WRITERS)
    }
}

/// A character's write turn, held across a foreground save.
pub(crate) struct OrderedSave {
    slot: Arc<Slot>,
    last_committed: OwnedMutexGuard<u64>,
}

impl OrderedSave {
    /// Generation for the snapshot about to be taken. Must be called while
    /// holding the turn so it is greater than every earlier snapshot.
    pub(crate) fn next_generation(&self) -> u64 {
        self.slot.next_generation.fetch_add(1, Ordering::SeqCst)
    }

    /// Record a successful commit so older in-flight snapshots go stale.
    pub(crate) fn record_commit(&mut self, generation: u64) {
        *self.last_committed = (*self.last_committed).max(generation);
        self.slot.touch();
    }
}

/// Completes a background task's bookkeeping even if the writer panics.
struct TaskGuard {
    shared: Arc<Shared>,
    snapshot: Arc<PlayerSaveSnapshot>,
    slot: Arc<Slot>,
    finished: bool,
}

impl TaskGuard {
    /// Publish the outcome. Called while the order lock is still held so a
    /// foreground save that acquires the lock next always finds it.
    fn finish(&mut self, outcome: Outcome) {
        self.finished = true;
        self.shared
            .done
            .lock()
            .expect("done lock")
            .push(Completion {
                snapshot: Arc::clone(&self.snapshot),
                slot: Arc::clone(&self.slot),
                outcome,
            });
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        if !self.finished {
            // Writer panicked or the runtime is shutting down.
            self.finish(Outcome::Failed("save task aborted".to_string()));
        }
        if self.shared.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.shared.idle.notify_waiters();
        }
    }
}

impl SaveCoordinator {
    pub(crate) fn new(max_background_writers: usize) -> Self {
        Self(Arc::new(Shared {
            slots: Mutex::new(HashMap::new()),
            writers: Arc::new(Semaphore::new(max_background_writers.max(1))),
            done: Mutex::new(Vec::new()),
            pending: AtomicUsize::new(0),
            idle: Notify::new(),
        }))
    }

    fn slot(&self, character_id: &str) -> Arc<Slot> {
        Arc::clone(
            self.0
                .slots
                .lock()
                .expect("slots lock")
                .entry(character_id.to_string())
                .or_insert_with(|| Arc::new(Slot::new())),
        )
    }

    /// Wait for this character's write turn (queues behind any in-flight
    /// background write). Foreground saves call this *before* snapshotting.
    pub(crate) async fn begin_ordered(&self, character_id: &str) -> OrderedSave {
        let slot = self.slot(character_id);
        let last_committed = Arc::clone(&slot.order).lock_owned().await;
        OrderedSave {
            slot,
            last_committed,
        }
    }

    /// Fold finished background writes back into the ECS (item-id stamps,
    /// time-played anchor) and release their `in_flight` flags. Cheap; call
    /// every tick and before any foreground snapshot.
    pub(crate) fn apply_completions(&self, world: &mut World) {
        let done = std::mem::take(&mut *self.0.done.lock().expect("done lock"));
        for c in done {
            match c.outcome {
                Outcome::Committed(assigned) => apply_commit(world, &c.snapshot, assigned),
                Outcome::Stale => {}
                Outcome::Failed(ref e) => {
                    warn!(error = %e, character_id = %c.snapshot.character_id,
                        "background save failed; will retry next interval");
                }
            }
            c.slot.in_flight.store(false, Ordering::SeqCst);
        }
    }

    /// Snapshot `character_id` now and write it from a spawned task.
    ///
    /// `snapshot` receives the generation to stamp and builds the payload
    /// (synchronously, from the ECS); `writer` performs the I/O. Returns
    /// `false` when a background save is already in flight for this
    /// character or `snapshot` yields `None` (not a player). Must be called
    /// inside a tokio runtime.
    pub(crate) fn request_background<S, W, Fut>(
        &self,
        character_id: &str,
        snapshot: S,
        writer: W,
    ) -> bool
    where
        S: FnOnce(u64) -> Option<PlayerSaveSnapshot>,
        W: FnOnce(Arc<PlayerSaveSnapshot>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<HashMap<usize, i32>, String>> + Send + 'static,
    {
        let slot = self.slot(character_id);
        if slot.in_flight.swap(true, Ordering::SeqCst) {
            return false;
        }
        let generation = slot.next_generation.fetch_add(1, Ordering::SeqCst);
        let Some(snap) = snapshot(generation) else {
            slot.in_flight.store(false, Ordering::SeqCst);
            return false;
        };
        slot.touch();
        let snap = Arc::new(snap);
        let shared = Arc::clone(&self.0);
        shared.pending.fetch_add(1, Ordering::SeqCst);
        let mut guard = TaskGuard {
            shared: Arc::clone(&shared),
            snapshot: Arc::clone(&snap),
            slot: Arc::clone(&slot),
            finished: false,
        };
        tokio::spawn(async move {
            let mut last_committed = Arc::clone(&slot.order).lock_owned().await;
            let outcome = if snap.generation <= *last_committed {
                Outcome::Stale
            } else {
                // Permit taken after the order lock so a task parked on the
                // semaphore never blocks a foreground save of *another*
                // character, only waits its own turn.
                let _permit = shared.writers.acquire().await;
                match writer(Arc::clone(&snap)).await {
                    Ok(assigned) => {
                        *last_committed = snap.generation;
                        Outcome::Committed(assigned)
                    }
                    Err(e) => Outcome::Failed(e),
                }
            };
            guard.finish(outcome);
            drop(last_committed);
        });
        true
    }

    /// Characters due for an autosave: not in flight, last saved at least
    /// `interval` ago, oldest first, at most `limit`. Also prunes slots of
    /// characters no longer in `online` that nothing references.
    pub(crate) fn autosave_due(
        &self,
        online: &[String],
        interval: Duration,
        limit: usize,
    ) -> Vec<String> {
        let mut slots = self.0.slots.lock().expect("slots lock");
        slots.retain(|k, s| {
            Arc::strong_count(s) > 1
                || s.in_flight.load(Ordering::SeqCst)
                || online.iter().any(|o| o == k)
        });
        // A character with no slot yet is freshly logged in: create it now
        // so its autosave clock starts at login (staggering saves by login
        // time) instead of saving it instantly or never.
        let mut due: Vec<(Duration, &String)> = Vec::new();
        for cid in online {
            let slot = slots
                .entry(cid.clone())
                .or_insert_with(|| Arc::new(Slot::new()));
            if slot.in_flight.load(Ordering::SeqCst) {
                continue;
            }
            let age = slot.since_last_save();
            if age >= interval {
                due.push((age, cid));
            }
        }
        due.sort_by(|a, b| b.0.cmp(&a.0));
        due.into_iter()
            .take(limit)
            .map(|(_, c)| c.clone())
            .collect()
    }

    /// Number of spawned writes not yet finished.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.0.pending.load(Ordering::SeqCst)
    }

    /// Wait until every spawned write has finished (or `timeout`), then
    /// fold the results into `world`. Shutdown calls this so SIGTERM never
    /// drops an in-flight autosave. Returns `false` on timeout.
    pub(crate) async fn flush(&self, world: &mut World, timeout: Duration) -> bool {
        let wait = async {
            loop {
                let notified = self.0.idle.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.0.pending.load(Ordering::SeqCst) == 0 {
                    return;
                }
                notified.await;
            }
        };
        let ok = tokio::time::timeout(timeout, wait).await.is_ok();
        self.apply_completions(world);
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::login::snapshot_player;
    use mud_world::{Account, Health, Item, Located, PersistedItemId, WorldKey};
    use std::sync::atomic::AtomicBool;
    use tokio::sync::oneshot;

    fn player(world: &mut World, cid: &str) -> Entity {
        let room = world.spawn_empty().id();
        world
            .spawn((
                Account {
                    user_id: String::new(),
                    character_id: cid.to_string(),
                    role: mud_db::enums::UserRole::Player,
                    account_role: mud_db::enums::UserRole::Player,
                    perms: vec![],
                },
                Health { hp: 10, max: 10 },
                Located(room),
            ))
            .id()
    }

    fn request<W, Fut>(
        c: &SaveCoordinator,
        world: &mut World,
        e: Entity,
        cid: &str,
        writer: W,
    ) -> bool
    where
        W: FnOnce(Arc<PlayerSaveSnapshot>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<HashMap<usize, i32>, String>> + Send + 'static,
    {
        c.request_background(cid, |g| snapshot_player(world, e, g), writer)
    }

    /// The tick-side call is a plain `fn`: it snapshots and returns even
    /// though the writer takes a long time, and the world only learns of
    /// the result once the tick folds it in.
    #[tokio::test(flavor = "current_thread")]
    async fn background_save_does_not_block_on_a_slow_writer() {
        let c = SaveCoordinator::new(2);
        let mut world = World::new();
        let e = player(&mut world, "char-a");
        let item = world
            .spawn((Item, WorldKey { zone: 1, id: 1 }, Located(e)))
            .id();
        let started = Instant::now();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_w = Arc::clone(&ran);
        assert!(request(
            &c,
            &mut world,
            e,
            "char-a",
            move |_snap| async move {
                tokio::time::sleep(Duration::from_millis(400)).await;
                ran_w.store(true, Ordering::SeqCst);
                Ok(HashMap::from([(0, 42)]))
            }
        ));
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "request must not wait for the writer"
        );
        assert_eq!(c.pending(), 1);
        assert!(!ran.load(Ordering::SeqCst));
        // A second snapshot of the same character is refused while the
        // first is in flight (id stamping would race otherwise).
        assert!(!request(&c, &mut world, e, "char-a", |_| async {
            Ok(HashMap::new())
        }));
        // Nothing is stamped until the tick applies the completion.
        assert!(world.get::<PersistedItemId>(item).is_none());
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        assert!(ran.load(Ordering::SeqCst));
        assert_eq!(world.get::<PersistedItemId>(item).unwrap().0, 42);
        // Applied, so the character can be snapshotted again.
        assert!(request(&c, &mut world, e, "char-a", |_| async {
            Ok(HashMap::new())
        }));
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
    }

    /// A quit-save that arrives while an autosave write is running queues
    /// behind it and gets a strictly newer generation.
    #[tokio::test(flavor = "current_thread")]
    async fn quit_save_waits_for_inflight_autosave_and_wins() {
        let c = SaveCoordinator::new(2);
        let mut world = World::new();
        let e = player(&mut world, "char-b");
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let (gate_tx, gate_rx) = oneshot::channel::<()>();
        let log_w = Arc::clone(&log);
        assert!(request(
            &c,
            &mut world,
            e,
            "char-b",
            move |snap| async move {
                let _ = gate_rx.await;
                log_w
                    .lock()
                    .unwrap()
                    .push(format!("auto g{}", snap.generation));
                Ok(HashMap::new())
            }
        ));
        // Let the task take the order lock and park inside the writer.
        tokio::task::yield_now().await;
        let c2 = c.clone();
        let log_q = Arc::clone(&log);
        let quit = tokio::spawn(async move {
            let mut ordered = c2.begin_ordered("char-b").await;
            let g = ordered.next_generation();
            log_q.lock().unwrap().push(format!("quit g{g}"));
            ordered.record_commit(g);
            g
        });
        tokio::task::yield_now().await;
        assert!(!quit.is_finished(), "quit-save must wait for the autosave");
        gate_tx.send(()).unwrap();
        let quit_gen = quit.await.unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec!["auto g1".to_string(), format!("quit g{quit_gen}")]
        );
        assert!(quit_gen > 1);
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
    }

    /// An autosave snapshot older than an already-committed quit-save is
    /// dropped without ever calling the writer, so it cannot overwrite it.
    #[tokio::test(flavor = "current_thread")]
    async fn stale_autosave_never_overwrites_a_newer_quit_save() {
        let c = SaveCoordinator::new(2);
        let mut world = World::new();
        let e = player(&mut world, "char-c");
        let wrote = Arc::new(AtomicBool::new(false));
        let wrote_w = Arc::clone(&wrote);
        // Autosave snapshot taken first (generation 1), task not yet run.
        assert!(request(&c, &mut world, e, "char-c", move |_| async move {
            wrote_w.store(true, Ordering::SeqCst);
            Ok(HashMap::new())
        }));
        // Quit-save grabs the turn before the autosave task gets to run,
        // snapshots later (generation 2) and commits.
        {
            let mut ordered = c.begin_ordered("char-c").await;
            let g = ordered.next_generation();
            assert_eq!(g, 2);
            ordered.record_commit(g);
        }
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        assert!(
            !wrote.load(Ordering::SeqCst),
            "stale autosave must not reach the database"
        );
        // And the in-flight flag was released.
        assert!(request(&c, &mut world, e, "char-c", |_| async {
            Ok(HashMap::new())
        }));
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
    }

    /// A panicking writer must not wedge the character or shutdown.
    #[tokio::test(flavor = "current_thread")]
    async fn failed_or_panicking_writer_releases_the_slot() {
        let c = SaveCoordinator::new(1);
        let mut world = World::new();
        let e = player(&mut world, "char-d");
        assert!(request(&c, &mut world, e, "char-d", |_| async {
            Err::<HashMap<usize, i32>, _>("boom".to_string())
        }));
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        assert!(request(&c, &mut world, e, "char-d", |_| async {
            panic!("writer exploded");
            #[allow(unreachable_code)]
            Ok(HashMap::new())
        }));
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        assert_eq!(c.pending(), 0);
        assert!(request(&c, &mut world, e, "char-d", |_| async {
            Ok(HashMap::new())
        }));
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
    }

    /// Staggering: at most `limit` characters per scan, oldest first, only
    /// those whose own last save is at least `interval` old.
    #[test]
    fn autosave_due_staggers_oldest_first() {
        let c = SaveCoordinator::new(2);
        let online: Vec<String> = ["a", "b", "c", "d"]
            .iter()
            .map(ToString::to_string)
            .collect();
        // Fresh logins are not due before one interval has elapsed.
        assert!(
            c.autosave_due(&online, Duration::from_secs(60), 2)
                .is_empty()
        );
        // Backdate: a oldest, then b, c; d is recent.
        let now = Instant::now();
        for (cid, age) in [("a", 400), ("b", 300), ("c", 200), ("d", 1)] {
            *c.slot(cid).last_save.lock().unwrap() =
                now.checked_sub(Duration::from_secs(age)).unwrap();
        }
        let due = c.autosave_due(&online, Duration::from_secs(100), 2);
        assert_eq!(due, vec!["a".to_string(), "b".to_string()]);
        // A busy character is skipped.
        c.slot("a").in_flight.store(true, Ordering::SeqCst);
        let due = c.autosave_due(&online, Duration::from_secs(100), 2);
        assert_eq!(due, vec!["b".to_string(), "c".to_string()]);
        // Slots of characters that went offline are pruned.
        let _ = c.autosave_due(&["a".to_string()], Duration::from_secs(100), 2);
        assert_eq!(c.0.slots.lock().unwrap().len(), 1);
    }
}
