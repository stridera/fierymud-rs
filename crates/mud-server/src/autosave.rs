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
//! * **Failure retry.** A failed background write backdates the slot's
//!   `last_save` (and holds it off for `FAILED_SAVE_RETRY` so a dead
//!   database isn't hammered every scan) so the character is due again
//!   within seconds rather than a full autosave interval later. A failed
//!   quit-save hands its owned snapshot to
//!   [`SaveCoordinator::retry_failed_snapshot`], which keeps retrying off
//!   the tick (with its generation, so it can never overwrite a newer
//!   save) after the player entity is gone.
//! * **Relog barrier.** Every spawned background write / quit retry counts
//!   in its slot's `outstanding`. A character that logs back in while that
//!   is non-zero must not load from the database yet (it would read state
//!   older than the pending write, and the write would then land over the
//!   new session): [`SaveCoordinator::wait_settled`] lets login wait for it
//!   without holding up the tick.
//! * **Shutdown.** [`SaveCoordinator::flush`] waits for every spawned write;
//!   [`SaveCoordinator::unsettled_characters`] names whoever is still
//!   unsaved if it times out.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use tokio::sync::{Notify, OwnedMutexGuard, Semaphore};
use tracing::{error, info, warn};

use crate::login::{PlayerSaveSnapshot, apply_commit};

/// Max characters snapshotted per autosave scan (the scan runs once per
/// second). 2/s is ~600 players per default 5-minute interval — far above
/// the server's `max_connections` — while keeping the burst tiny.
pub(crate) const AUTOSAVE_PER_SCAN: usize = 2;
/// Concurrent background DB writers. Leaves most of the pool free for
/// login / command traffic.
pub(crate) const BACKGROUND_WRITERS: usize = 2;
/// After a failed background save the character is retried this soon
/// (not a whole autosave interval later).
pub(crate) const FAILED_SAVE_RETRY: Duration = Duration::from_secs(5);
/// Delays between retries of a failed quit-save. The player is already
/// gone, so this is the only chance their last state ever reaches the
/// database; ~2.5 minutes of attempts rides out a restart or failover.
pub(crate) const QUIT_RETRY_BACKOFF: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(20),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(30),
];

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
    /// Background writes own the slot's `in_flight` flag; a quit-save
    /// retry does not, so its completion must not clear it.
    owns_in_flight: bool,
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
    /// Spawned background writes plus quit-save retry tasks that have not
    /// finished (success, superseded, or given up).
    outstanding: AtomicUsize,
    last_save: Mutex<Instant>,
    /// Set after a failed background write: the character is not retried
    /// before this instant.
    retry_after: Mutex<Option<Instant>>,
    /// The character has a live entity in the world (set by login once the
    /// entity is spawned, under the character's turn). A write that targets
    /// an offline character's database rows checks this under the turn: a
    /// row inserted behind a live session's back is deleted by that
    /// session's next save.
    session_live: AtomicBool,
}

impl Slot {
    fn new() -> Self {
        Self {
            order: Arc::new(tokio::sync::Mutex::new(0)),
            next_generation: AtomicU64::new(1),
            in_flight: AtomicBool::new(false),
            outstanding: AtomicUsize::new(0),
            last_save: Mutex::new(Instant::now()),
            retry_after: Mutex::new(None),
            session_live: AtomicBool::new(false),
        }
    }

    fn touch(&self) {
        *self.last_save.lock().expect("last_save lock") = Instant::now();
        *self.retry_after.lock().expect("retry_after lock") = None;
    }

    /// A background write failed: backdate `last_save` so the character is
    /// due on the next scan after `FAILED_SAVE_RETRY`, whatever the
    /// autosave interval is.
    fn mark_failed(&self) {
        let now = Instant::now();
        *self.last_save.lock().expect("last_save lock") = now
            .checked_sub(Duration::from_secs(24 * 3600))
            .unwrap_or(now);
        *self.retry_after.lock().expect("retry_after lock") = Some(now + FAILED_SAVE_RETRY);
    }

    fn retry_held_off(&self) -> bool {
        self.retry_after
            .lock()
            .expect("retry_after lock")
            .is_some_and(|t| Instant::now() < t)
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

    /// For a direct inventory-row write that is not a snapshot commit
    /// (account-chest deposit / withdraw): mark every snapshot taken so far
    /// stale, so a background write already queued behind this turn cannot
    /// land its out-of-date item list over the row change. Those saves are
    /// skipped, not lost: the next autosave carries the same state.
    pub(crate) fn supersede_earlier_snapshots(&mut self) {
        let generation = self.next_generation();
        *self.last_committed = (*self.last_committed).max(generation);
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
                owns_in_flight: true,
            });
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        if !self.finished {
            // Writer panicked or the runtime is shutting down.
            self.finish(Outcome::Failed("save task aborted".to_string()));
        }
        self.slot.outstanding.fetch_sub(1, Ordering::SeqCst);
        if self.shared.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.shared.idle.notify_waiters();
        }
    }
}

/// Keeps `pending` (and the slot's `outstanding`) accurate for a detached
/// retry task (see [`SaveCoordinator::retry_failed_snapshot`]) even if it
/// panics.
struct PendingGuard(Arc<Shared>, Arc<Slot>);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.1.outstanding.fetch_sub(1, Ordering::SeqCst);
        if self.0.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

/// Keeps `pending` accurate for a detached tracked task.
struct TrackGuard(Arc<Shared>);

impl Drop for TrackGuard {
    fn drop(&mut self) {
        if self.0.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

/// Poll `fut` once with a waker that does nothing. Used to join a lock's
/// FIFO queue immediately; the real waker is installed by the next poll.
fn poll_once<F: Future + Unpin>(fut: &mut F) -> Option<F::Output> {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match std::pin::Pin::new(fut).poll(&mut cx) {
        std::task::Poll::Ready(v) => Some(v),
        std::task::Poll::Pending => None,
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

    /// Take this character's write turn only if it is free right now.
    /// `None` means a background write, a queued ordered write or a retry
    /// attempt holds (or is waiting for) the turn. The world thread uses
    /// this where it must not wait: it can then snapshot with the turn in
    /// hand exactly as a foreground save would, or put the work off a tick.
    pub(crate) fn try_begin_ordered(&self, character_id: &str) -> Option<OrderedSave> {
        let slot = self.slot(character_id);
        let last_committed = Arc::clone(&slot.order).try_lock_owned().ok()?;
        Some(OrderedSave {
            slot,
            last_committed,
        })
    }

    /// Write a session-ending snapshot from a spawned task. The caller took
    /// the turn ([`Self::try_begin_ordered`]) and the snapshot with it, on
    /// the world thread, so nothing can slip between the two; this only
    /// moves the database write off the tick. The task counts as an
    /// unfinished write synchronously, before this returns, so the relog
    /// barrier and shutdown flush see it at once. A commit is folded into
    /// the world by the next [`Self::apply_completions`] (the entity is
    /// usually gone by then). A failed write registers the quit-save retry
    /// ([`Self::retry_failed_snapshot`]) before the turn is released, so no
    /// gap exists in which the character looks settled. Must be called
    /// inside a tokio runtime.
    pub(crate) fn spawn_final_save<W, Fut>(
        &self,
        mut ordered: OrderedSave,
        snapshot: PlayerSaveSnapshot,
        writer: W,
    ) where
        W: Fn(Arc<PlayerSaveSnapshot>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<HashMap<usize, i32>, String>> + Send + 'static,
    {
        let slot = Arc::clone(&ordered.slot);
        let shared = Arc::clone(&self.0);
        shared.pending.fetch_add(1, Ordering::SeqCst);
        slot.outstanding.fetch_add(1, Ordering::SeqCst);
        let guard = PendingGuard(Arc::clone(&shared), Arc::clone(&slot));
        let snap = Arc::new(snapshot);
        let coordinator = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            match writer(Arc::clone(&snap)).await {
                Ok(assigned) => {
                    ordered.record_commit(snap.generation);
                    shared.done.lock().expect("done lock").push(Completion {
                        snapshot: Arc::clone(&snap),
                        slot: Arc::clone(&slot),
                        outcome: Outcome::Committed(assigned),
                        owns_in_flight: false,
                    });
                }
                Err(e) => {
                    error!(character_id = %snap.character_id, error = %e,
                        "final save FAILED; handing the snapshot to the background writer \
                         for retry");
                    coordinator.retry_snapshot(Arc::clone(&snap), writer, QUIT_RETRY_BACKOFF);
                }
            }
            drop(ordered);
        });
    }

    /// Queue a character's write turn NOW and run `work` once it is granted.
    ///
    /// Unlike spawning a task that calls [`Self::begin_ordered`] itself, the
    /// turn is requested synchronously, in call order: a foreground save
    /// issued after this call (a quit) queues behind `work` instead of
    /// possibly winning the turn first. The task counts as an unfinished
    /// write for [`Self::flush`] and the relog barrier, so shutdown and
    /// relog wait for it. Must be called inside a tokio runtime.
    pub(crate) fn spawn_ordered<W, Fut>(&self, character_id: &str, work: W)
    where
        W: FnOnce(OrderedSave) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let slot = self.slot(character_id);
        let mut lock = Box::pin(Arc::clone(&slot.order).lock_owned());
        // Joins the lock's FIFO queue now (or takes the turn outright).
        let granted = poll_once(&mut lock);
        let shared = Arc::clone(&self.0);
        shared.pending.fetch_add(1, Ordering::SeqCst);
        slot.outstanding.fetch_add(1, Ordering::SeqCst);
        let guard = PendingGuard(shared, Arc::clone(&slot));
        tokio::spawn(async move {
            let _guard = guard;
            let last_committed = match granted {
                Some(g) => g,
                None => lock.await,
            };
            work(OrderedSave {
                slot,
                last_committed,
            })
            .await;
        });
    }

    /// Run a detached task that shutdown ([`Self::flush`]) waits for, for
    /// writes that are not part of any character's save order (a retried
    /// row delete). Must be called inside a tokio runtime.
    pub(crate) fn spawn_tracked<Fut>(&self, fut: Fut)
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        let shared = Arc::clone(&self.0);
        shared.pending.fetch_add(1, Ordering::SeqCst);
        let guard = TrackGuard(shared);
        tokio::spawn(async move {
            let _guard = guard;
            fut.await;
        });
    }

    /// Mark whether `character_id` has a live entity (see
    /// [`Slot::session_live`]).
    pub(crate) fn set_session_live(&self, character_id: &str, live: bool) {
        self.slot(character_id)
            .session_live
            .store(live, Ordering::SeqCst);
    }

    /// Whether `character_id` currently has a live entity.
    pub(crate) fn session_live(&self, character_id: &str) -> bool {
        self.0
            .slots
            .lock()
            .expect("slots lock")
            .get(character_id)
            .is_some_and(|s| s.session_live.load(Ordering::SeqCst))
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
                        "background save failed; retrying shortly");
                    c.slot.mark_failed();
                    // A Lua `actor:save()` request was consumed when this
                    // write started; it is only satisfied once a write
                    // lands, so put the marker back.
                    if c.snapshot.resume_pending_save
                        && let Ok(mut em) = world.get_entity_mut(c.snapshot.entity)
                    {
                        em.insert(mud_world::PendingSave);
                    }
                }
            }
            if c.owns_in_flight {
                c.slot.in_flight.store(false, Ordering::SeqCst);
            }
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
        slot.outstanding.fetch_add(1, Ordering::SeqCst);
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

    /// Keep retrying a snapshot whose foreground write (quit / idle-kick /
    /// shutdown) failed, off the tick, after the player entity is gone.
    /// Each attempt takes the character's turn and is skipped (and the
    /// retry ends) once a newer generation has committed, so it can never
    /// overwrite a later save. Gives up with an ERROR after `schedule` is
    /// exhausted. Must be called inside a tokio runtime.
    pub(crate) fn retry_failed_snapshot<W, Fut>(
        &self,
        snapshot: PlayerSaveSnapshot,
        writer: W,
        schedule: &'static [Duration],
    ) where
        W: Fn(Arc<PlayerSaveSnapshot>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<HashMap<usize, i32>, String>> + Send + 'static,
    {
        self.retry_snapshot(Arc::new(snapshot), writer, schedule);
    }

    fn retry_snapshot<W, Fut>(
        &self,
        snap: Arc<PlayerSaveSnapshot>,
        writer: W,
        schedule: &'static [Duration],
    ) where
        W: Fn(Arc<PlayerSaveSnapshot>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<HashMap<usize, i32>, String>> + Send + 'static,
    {
        let slot = self.slot(&snap.character_id);
        let shared = Arc::clone(&self.0);
        shared.pending.fetch_add(1, Ordering::SeqCst);
        slot.outstanding.fetch_add(1, Ordering::SeqCst);
        let guard = PendingGuard(Arc::clone(&shared), Arc::clone(&slot));
        tokio::spawn(async move {
            let _guard = guard;
            let mut attempt = 0usize;
            loop {
                {
                    let mut last_committed = Arc::clone(&slot.order).lock_owned().await;
                    if snap.generation <= *last_committed {
                        info!(character_id = %snap.character_id,
                            "save retry superseded by a newer committed save");
                        return;
                    }
                    let _permit = shared.writers.acquire().await;
                    match writer(Arc::clone(&snap)).await {
                        Ok(assigned) => {
                            *last_committed = snap.generation;
                            info!(character_id = %snap.character_id, attempt,
                                "save retry succeeded");
                            // Fold the commit into the world on the next
                            // tick even though the player is gone: a death
                            // committed here still has to give its corpse
                            // entity its `PlayerCorpseId`.
                            shared.done.lock().expect("done lock").push(Completion {
                                snapshot: Arc::clone(&snap),
                                slot: Arc::clone(&slot),
                                outcome: Outcome::Committed(assigned),
                                owns_in_flight: false,
                            });
                            return;
                        }
                        Err(e) => {
                            warn!(error = %e, character_id = %snap.character_id, attempt,
                                "save retry failed");
                        }
                    }
                }
                let Some(delay) = schedule.get(attempt).copied() else {
                    error!(character_id = %snap.character_id,
                        "save retry exhausted; this character's last state was NOT persisted");
                    return;
                };
                attempt += 1;
                tokio::time::sleep(delay).await;
            }
        });
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
            if slot.in_flight.load(Ordering::SeqCst) || slot.retry_held_off() {
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

    /// True while a background write or quit-save retry for this character
    /// has not finished: the database does not yet hold its latest state.
    pub(crate) fn has_unsettled_saves(&self, character_id: &str) -> bool {
        self.0
            .slots
            .lock()
            .expect("slots lock")
            .get(character_id)
            .is_some_and(|s| s.outstanding.load(Ordering::SeqCst) > 0)
    }

    /// Wait (without holding the tick: call from a spawned task) until this
    /// character has no unfinished write or retry. `false` on timeout, i.e.
    /// the database may still be behind the character's last session.
    pub(crate) async fn wait_settled(&self, character_id: &str, timeout: Duration) -> bool {
        let poll = async {
            while self.has_unsettled_saves(character_id) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::time::timeout(timeout, poll).await.is_ok()
    }

    /// Ids of characters with a write or retry still unfinished; logged by
    /// shutdown when its flush times out.
    pub(crate) fn unsettled_characters(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .0
            .slots
            .lock()
            .expect("slots lock")
            .iter()
            .filter(|(_, s)| s.outstanding.load(Ordering::SeqCst) > 0)
            .map(|(k, _)| k.clone())
            .collect();
        ids.sort();
        ids
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

    /// A turn queued with `spawn_ordered` is ahead of any turn requested
    /// after the call returns, even before the spawned task first runs: a
    /// quit save issued right after a house placement queues behind it.
    #[tokio::test(flavor = "current_thread")]
    async fn spawn_ordered_turn_is_queued_before_the_call_returns() {
        let c = SaveCoordinator::new(2);
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let holder = c.begin_ordered("char-q").await;
        let log_w = Arc::clone(&log);
        c.spawn_ordered("char-q", move |ordered| async move {
            let _turn = ordered;
            tokio::time::sleep(Duration::from_millis(30)).await;
            log_w.lock().unwrap().push("placement");
        });
        // Requested after, before the spawned task has been polled.
        let mut quit = Box::pin(c.begin_ordered("char-q"));
        assert!(poll_once(&mut quit).is_none());
        drop(holder);
        let _quit_turn = quit.await;
        log.lock().unwrap().push("quit");
        assert_eq!(*log.lock().unwrap(), vec!["placement", "quit"]);
        let mut world = World::new();
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
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

    /// A failed background write makes the character due again within
    /// seconds (not a full autosave interval), once the short hold-off
    /// passes.
    #[tokio::test(flavor = "current_thread")]
    async fn failed_background_save_is_due_again_within_seconds() {
        let c = SaveCoordinator::new(1);
        let mut world = World::new();
        let e = player(&mut world, "char-f");
        let online = vec!["char-f".to_string()];
        let interval = Duration::from_secs(300);
        assert!(request(&c, &mut world, e, "char-f", |_| async {
            Err::<HashMap<usize, i32>, _>("db down".to_string())
        }));
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        // Held off briefly so a dead database isn't hammered every scan...
        assert!(c.autosave_due(&online, interval, 2).is_empty());
        assert!(FAILED_SAVE_RETRY <= Duration::from_secs(10));
        // ...then due again, though only moments passed vs a 300s interval.
        *c.slot("char-f").retry_after.lock().unwrap() = Some(Instant::now());
        assert_eq!(c.autosave_due(&online, interval, 2), online);
        // A successful write restores the normal cadence.
        assert!(request(&c, &mut world, e, "char-f", |_| async {
            Ok(HashMap::new())
        }));
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        assert!(c.autosave_due(&online, interval, 2).is_empty());
    }

    static FAST_RETRY: &[Duration] = &[Duration::from_millis(5), Duration::from_millis(5)];
    static SLOW_RETRY: &[Duration] = &[Duration::from_millis(80), Duration::from_millis(80)];
    static NEVER_RETRY: &[Duration] = &[Duration::from_secs(30)];

    /// The owned snapshot of a failed quit-save is retried off the tick
    /// until it lands, and `flush` (shutdown) waits for it.
    #[tokio::test(flavor = "current_thread")]
    async fn failed_quit_snapshot_is_retried_until_it_commits() {
        let c = SaveCoordinator::new(1);
        let mut world = World::new();
        let e = player(&mut world, "char-q");
        let snap = snapshot_player(&mut world, e, 7).unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_w = Arc::clone(&attempts);
        c.retry_failed_snapshot(
            snap,
            move |_| {
                let n = attempts_w.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 2 {
                        Err("still down".to_string())
                    } else {
                        Ok(HashMap::new())
                    }
                }
            },
            FAST_RETRY,
        );
        assert_eq!(c.pending(), 1);
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        // Committed with its generation: an older snapshot can't undo it.
        assert_eq!(*c.slot("char-q").order.lock().await, 7);
    }

    /// A retry gives up (rather than looping forever) once its schedule is
    /// exhausted, and is dropped as stale if a newer save already landed.
    #[tokio::test(flavor = "current_thread")]
    async fn quit_snapshot_retry_gives_up_and_yields_to_newer_saves() {
        let c = SaveCoordinator::new(1);
        let mut world = World::new();
        let e = player(&mut world, "char-r");
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_w = Arc::clone(&attempts);
        c.retry_failed_snapshot(
            snapshot_player(&mut world, e, 1).unwrap(),
            move |_| {
                attempts_w.fetch_add(1, Ordering::SeqCst);
                async { Err::<HashMap<usize, i32>, _>("down".to_string()) }
            },
            FAST_RETRY,
        );
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        assert_eq!(attempts.load(Ordering::SeqCst), 3, "1 try + 2 retries");

        // A newer generation has committed: the old snapshot never writes.
        {
            let mut ordered = c.begin_ordered("char-r").await;
            ordered.record_commit(5);
        }
        let wrote = Arc::new(AtomicBool::new(false));
        let wrote_w = Arc::clone(&wrote);
        c.retry_failed_snapshot(
            snapshot_player(&mut world, e, 2).unwrap(),
            move |_| {
                wrote_w.store(true, Ordering::SeqCst);
                async { Ok(HashMap::new()) }
            },
            FAST_RETRY,
        );
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        assert!(!wrote.load(Ordering::SeqCst));
    }

    /// A relogging character waits for its pending quit-save retry: the
    /// barrier stays up through the retry's sleeps (when no order lock is
    /// held) and drops once the write lands.
    #[tokio::test(flavor = "current_thread")]
    async fn wait_settled_blocks_until_the_pending_retry_lands() {
        let c = SaveCoordinator::new(1);
        let mut world = World::new();
        let e = player(&mut world, "char-w");
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_w = Arc::clone(&attempts);
        assert!(!c.has_unsettled_saves("char-w"));
        c.retry_failed_snapshot(
            snapshot_player(&mut world, e, 1).unwrap(),
            move |_| {
                let n = attempts_w.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 2 {
                        Err("down".to_string())
                    } else {
                        Ok(HashMap::new())
                    }
                }
            },
            SLOW_RETRY,
        );
        assert!(c.has_unsettled_saves("char-w"));
        assert_eq!(c.unsettled_characters(), vec!["char-w".to_string()]);
        // Too short a wait: still unsettled.
        assert!(!c.wait_settled("char-w", Duration::from_millis(20)).await);
        // A full wait returns only once the write has landed.
        assert!(c.wait_settled("char-w", Duration::from_secs(5)).await);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(!c.has_unsettled_saves("char-w"));
        assert!(c.unsettled_characters().is_empty());
    }

    /// A retry that keeps failing keeps the barrier up past the wait, and
    /// an in-flight background write holds it too.
    #[tokio::test(flavor = "current_thread")]
    async fn wait_settled_times_out_while_saves_keep_failing() {
        let c = SaveCoordinator::new(1);
        let mut world = World::new();
        let e = player(&mut world, "char-x");
        c.retry_failed_snapshot(
            snapshot_player(&mut world, e, 1).unwrap(),
            |_| async { Err::<HashMap<usize, i32>, _>("down".to_string()) },
            NEVER_RETRY,
        );
        assert!(!c.wait_settled("char-x", Duration::from_millis(150)).await);

        let e2 = player(&mut world, "char-y");
        let (gate_tx, gate_rx) = oneshot::channel::<()>();
        assert!(request(&c, &mut world, e2, "char-y", move |_| async move {
            let _ = gate_rx.await;
            Ok(HashMap::new())
        }));
        assert!(c.has_unsettled_saves("char-y"));
        assert_eq!(
            c.unsettled_characters(),
            vec!["char-x".to_string(), "char-y".to_string()]
        );
        gate_tx.send(()).unwrap();
        assert!(c.wait_settled("char-y", Duration::from_secs(5)).await);
    }

    /// A direct row write (chest deposit) supersedes snapshots taken before
    /// it: a queued background write never reaches the database.
    #[tokio::test(flavor = "current_thread")]
    async fn superseded_snapshot_is_skipped() {
        let c = SaveCoordinator::new(2);
        let mut world = World::new();
        let e = player(&mut world, "char-s");
        let wrote = Arc::new(AtomicBool::new(false));
        let wrote_w = Arc::clone(&wrote);
        assert!(request(&c, &mut world, e, "char-s", move |_| async move {
            wrote_w.store(true, Ordering::SeqCst);
            Ok(HashMap::new())
        }));
        {
            let mut ordered = c.begin_ordered("char-s").await;
            ordered.supersede_earlier_snapshots();
        }
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        assert!(!wrote.load(Ordering::SeqCst));
        // Snapshots taken afterwards are unaffected.
        let wrote_w = Arc::clone(&wrote);
        assert!(request(&c, &mut world, e, "char-s", move |_| async move {
            wrote_w.store(true, Ordering::SeqCst);
            Ok(HashMap::new())
        }));
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        assert!(wrote.load(Ordering::SeqCst));
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

    /// A death committed by the quit-save retry (the player is gone by
    /// then) still gives the in-world corpse its `PlayerCorpseId`, so it
    /// can be looted and drags and decays through the database.
    #[tokio::test(flavor = "current_thread")]
    async fn retry_committed_death_stamps_the_corpse_entity() {
        const NO_RETRIES: &[Duration] = &[];
        let c = SaveCoordinator::new(2);
        let mut world = World::new();
        let e = player(&mut world, "char-dead");
        let room = world.get::<Located>(e).unwrap().0;
        let corpse = world
            .spawn((
                Item,
                mud_world::Corpse,
                mud_world::PlayerCorpse,
                Located(room),
            ))
            .id();
        world
            .entity_mut(e)
            .insert(crate::corpses::PendingDeath(corpse));
        let snap = snapshot_player(&mut world, e, 5).expect("snapshot");
        assert!(snap.death.is_some());
        world.entity_mut(e).despawn();
        c.retry_failed_snapshot(
            snap,
            |snap| async move {
                snap.committed_corpse_id
                    .store(77, std::sync::atomic::Ordering::SeqCst);
                Ok(HashMap::new())
            },
            NO_RETRIES,
        );
        assert!(c.flush(&mut world, Duration::from_secs(5)).await);
        assert_eq!(
            world.get::<mud_world::PlayerCorpseId>(corpse).map(|i| i.0),
            Some(77)
        );
    }
}
