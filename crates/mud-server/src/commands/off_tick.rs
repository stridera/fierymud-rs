//! Database work for a command without holding the world tick.
//!
//! The world loop is one task: a command that `.await`s Postgres inline
//! freezes every player for the round trip (a save, a house visit, a mail
//! write ...). [`run`] splits such a command in two:
//!
//! * the **work** future (owned, `Send + 'static`: copy what it needs out of
//!   the world first) is spawned and does the database I/O off the tick;
//! * the **finish** closure runs on a later tick, at the start of the next
//!   [`drain_command_jobs`], with the work's result. It re-checks the world
//!   (the player may have moved, died or left), mutates it and prints.
//!
//! While a job is outstanding the player carries [`AsyncBusy`]. Input typed
//! meanwhile is held in the player's input queue (`input_queue`) and released
//! one line per tick once the job has finished, so a player's commands still
//! run in the order they were typed and a command never starts before the
//! previous one's database work has landed. The gates (`pre_gate`, room
//! COMMAND triggers) run once, when the line is first dispatched, exactly as
//! before; the finish step must not run them again.
//!
//! A world without the [`CommandJobs`] resource (unit tests, tools) has no
//! tick to hand back to: [`run`] then awaits the work inline and finishes at
//! once, which is the old behaviour.

use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use bevy_ecs::prelude::*;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::send_to;

/// Longest a job's database work may take before the player is released
/// with an apology. Bounds how long a stuck database can hold a player's
/// input queue.
pub(crate) const JOB_TIMEOUT: Duration = Duration::from_secs(30);

type Finish = Box<dyn FnOnce(&mut World) + Send>;

/// Channel the spawned work reports back through; drained every tick.
/// Installed at boot (`main.rs`).
#[derive(Resource)]
pub(crate) struct CommandJobs {
    tx: UnboundedSender<Finish>,
    rx: Mutex<UnboundedReceiver<Finish>>,
}

impl Default for CommandJobs {
    fn default() -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            tx,
            rx: Mutex::new(rx),
        }
    }
}

/// The player has a command whose database work is still running. Later
/// input waits in the input queue until it is gone.
#[derive(Component)]
pub(crate) struct AsyncBusy;

/// Whether `player` is waiting on a job.
pub(crate) fn is_busy(world: &World, player: Entity) -> bool {
    world.get::<AsyncBusy>(player).is_some()
}

/// Run `work` (database I/O) off the tick and `finish` on a later one.
/// Without a [`CommandJobs`] resource both run right here, in order.
pub(crate) async fn run<T, W, F>(world: &mut World, player: Entity, work: W, finish: F)
where
    T: Send + 'static,
    W: Future<Output = T> + Send + 'static,
    F: FnOnce(&mut World, Entity, T) + Send + 'static,
{
    if world.contains_resource::<CommandJobs>() {
        spawn(world, player, work, finish);
    } else {
        let out = work.await;
        finish(world, player, out);
    }
}

/// [`run`] for a caller that is already on the tick and cannot await (a
/// finish step starting a follow-up job). Returns false, having done
/// nothing, when the world has no [`CommandJobs`].
pub(crate) fn spawn<T, W, F>(world: &mut World, player: Entity, work: W, finish: F) -> bool
where
    T: Send + 'static,
    W: Future<Output = T> + Send + 'static,
    F: FnOnce(&mut World, Entity, T) + Send + 'static,
{
    let Some(tx) = world.get_resource::<CommandJobs>().map(|j| j.tx.clone()) else {
        return false;
    };
    super::try_insert(world, player, AsyncBusy);
    let task = async move {
        // The work runs in its own task so a panic in it cannot strand the
        // player behind `AsyncBusy`: the outcome below always reaches the tick.
        let handle = tokio::spawn(work);
        let abort = handle.abort_handle();
        let outcome = match tokio::time::timeout(JOB_TIMEOUT, handle).await {
            Ok(Ok(out)) => Some(out),
            Ok(Err(e)) => {
                tracing::error!(error = %e, "command job panicked");
                None
            }
            Err(_) => {
                abort.abort();
                tracing::warn!("command job timed out");
                None
            }
        };
        let done: Finish = Box::new(move |world: &mut World| {
            // The player left (quit, link drop, death) while the work ran:
            // nothing to release, nobody to tell.
            if world.get_entity(player).is_err() {
                return;
            }
            world.entity_mut(player).remove::<AsyncBusy>();
            match outcome {
                Some(out) => finish(world, player, out),
                None => send_to(
                    world,
                    player,
                    "That took too long and was abandoned. Try again in a moment.\r\n",
                ),
            }
        });
        let _ = tx.send(done);
    };
    match world
        .get_resource::<crate::autosave::SaveCoordinator>()
        .cloned()
    {
        // Tracked, so a shutdown flush waits for the database work.
        Some(coordinator) => coordinator.spawn_tracked(task),
        None => {
            tokio::spawn(task);
        }
    }
    true
}

/// Run the finish step of every job whose work has completed. Called at the
/// start of every tick, before admin requests and the schedule.
pub(crate) fn drain_command_jobs(world: &mut World) {
    let done: Vec<Finish> = {
        let Some(jobs) = world.get_resource::<CommandJobs>() else {
            return;
        };
        let mut rx = jobs.rx.lock().expect("command jobs poisoned");
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    };
    for finish in done {
        finish(world);
    }
}
