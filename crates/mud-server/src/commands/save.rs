//! `save` — force a checkpoint of the current character to the
//! database without disconnecting. The auto-save path runs only
//! on `on_disconnect` and graceful shutdown, so a paranoid
//! player who's about to attempt something risky (or who's
//! noticed flaky network) can use this to checkpoint.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;

use crate::autosave::SaveCoordinator;
use crate::commands::off_tick::{self, CommandJobs};
use crate::commands::{
    AsyncCommand, Category, Command, Help, PlayerUpdateInbox, cmd_mail_stub, drain_player_updates,
    send_to,
};
use crate::login::{PlayerSaveSnapshot, save_player, snapshot_player, write_snapshot};
use mud_world::Account;

inventory::submit! {
    Command {
        names: &["save"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "save",
            summary: "Force a checkpoint of your character to the database.",
            long: "Persists hp / stamina / xp / inventory / location / \
                   flags / prompt / hunger / thirst / drunk / known \
                   abilities / spell-slot cooldowns / ability cooldowns \
                   back to the schema. The same path runs automatically \
                   on disconnect and on graceful shutdown — 'save' lets \
                   you force it right now without logging out, useful \
                   before a risky encounter or when the network feels \
                   unstable.",
        },
        // The sync stub is never the real handler; the AsyncCommand
        // entry below claims `save` first via dispatch order. Reusing
        // `cmd_mail_stub` matches the convention used by `visit` /
        // other async commands.
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, _args| match head {
            "save" => Some(Box::pin(cmd_save(world, player, pool))),
            _ => None,
        },
    }
}

const NOTHING_TO_SAVE: &str =
    "Nothing to save — you don't have a character record on this entity.\r\n";
const SAVE_FAILED: &str = "Save failed — your last successful checkpoint is intact, \
     but this attempt rolled back. Try 'save' again, or ask \
     staff to investigate the syslog.\r\n";

/// How many times a save that finds the character's write turn taken (an
/// autosave is mid-write) waits for it and tries again before giving up.
const SAVE_TURN_RETRIES: u8 = 3;

async fn cmd_save(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    if world.contains_resource::<CommandJobs>() {
        let pool = pool.clone();
        let writer = move |snap: Arc<PlayerSaveSnapshot>| {
            let pool = pool.clone();
            async move {
                write_snapshot(&pool, &snap)
                    .await
                    .map_err(|e| e.to_string())
            }
        };
        start_save(world, player, writer, SAVE_TURN_RETRIES);
        return;
    }
    let outcome = save_player(world, player, pool).await;
    if outcome.aborted {
        // The aborted path means the entity isn't a Player at all
        // (no Account component) — caller is most likely a mob in
        // `switch` mode. Don't report a save when nothing was saved.
        send_to(world, player, NOTHING_TO_SAVE);
        return;
    }
    if outcome.committed {
        send_to(world, player, "Saved.\r\n");
    } else {
        // Tx rolled back; the full error went to tracing::warn for
        // staff diagnostics. Surface a one-liner pointing the player
        // at retry.
        send_to(world, player, SAVE_FAILED);
    }
}

/// The autosave way of saving, for the live tick: take the character's write
/// turn and snapshot them right here (pure in-memory reads), hand only the
/// database write to a spawned task, and say "Saved." from a later tick once
/// it has committed. The player is busy until then, so their next command
/// waits for the answer. When a background write holds the turn, wait for it
/// off the tick and look again rather than waiting here.
fn start_save<W, Fut>(world: &mut World, player: Entity, writer: W, retries: u8)
where
    W: Fn(Arc<PlayerSaveSnapshot>) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Result<HashMap<usize, i32>, String>> + Send + 'static,
{
    let Some(character_id) = world.get::<Account>(player).map(|a| a.character_id.clone()) else {
        send_to(world, player, NOTHING_TO_SAVE);
        return;
    };
    let coordinator = world
        .get_resource::<SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    let Some(ordered) = coordinator.try_begin_ordered(&character_id) else {
        if retries == 0 {
            send_to(
                world,
                player,
                "A save of your character is already under way. Try again in a moment.\r\n",
            );
            return;
        }
        let turns = coordinator;
        off_tick::spawn(
            world,
            player,
            async move { drop(turns.begin_ordered(&character_id).await) },
            move |world, player, ()| start_save(world, player, writer, retries - 1),
        );
        return;
    };
    // Same prelude as `save_player`: fold in the write that just released
    // the turn, then the database-first updates not yet on the character.
    coordinator.apply_completions(world);
    if world.contains_resource::<PlayerUpdateInbox>() {
        drain_player_updates(world);
    }
    let generation = ordered.next_generation();
    let Some(snapshot) = snapshot_player(world, player, generation) else {
        send_to(world, player, NOTHING_TO_SAVE);
        return;
    };
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    coordinator.spawn_checkpoint(ordered, snapshot, writer, move |committed| {
        let _ = done_tx.send(committed);
    });
    off_tick::spawn(
        world,
        player,
        async move { done_rx.await.unwrap_or(false) },
        move |world, player, committed| {
            // The commit is in the coordinator's completion list; fold its
            // item-id stamps in now, as the foreground save does.
            coordinator.apply_completions(world);
            send_to(
                world,
                player,
                if committed { "Saved.\r\n" } else { SAVE_FAILED },
            );
        },
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mud_db::enums::UserRole;
    use mud_world::{Exits, Named, Room};

    use super::*;
    use crate::commands::off_tick::{self, drain_command_jobs};
    use crate::commands::test_support::{Rx, drain, player_in};
    use crate::commands::{dispatch_with_async, run_queued_input};

    const WRITE_DELAY: Duration = Duration::from_millis(300);

    fn pool() -> mud_db::sqlx::PgPool {
        mud_db::sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap()
    }

    /// A player in a named room, with the resources an off-tick save uses.
    fn world_with_player() -> (World, Entity, Rx) {
        let mut world = World::new();
        world.init_resource::<mud_world::SocialRegistry>();
        world.init_resource::<CommandJobs>();
        world.init_resource::<SaveCoordinator>();
        let room = world
            .spawn((
                Room,
                Named {
                    name: "A quiet hall".into(),
                },
                Exits::default(),
            ))
            .id();
        let (player, rx) = player_in(&mut world, room);
        world.entity_mut(player).insert(Account {
            user_id: "u".into(),
            character_id: "c-save-test".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        });
        (world, player, rx)
    }

    type WriteFuture =
        std::pin::Pin<Box<dyn Future<Output = Result<HashMap<usize, i32>, String>> + Send>>;

    /// A database write that takes `WRITE_DELAY` and then commits / fails.
    fn slow_writer(
        commit: bool,
    ) -> impl Fn(Arc<PlayerSaveSnapshot>) -> WriteFuture + Clone + Send + 'static {
        move |_snap| {
            Box::pin(async move {
                tokio::time::sleep(WRITE_DELAY).await;
                if commit {
                    Ok(HashMap::new())
                } else {
                    Err("connection reset".to_string())
                }
            })
        }
    }

    async fn settle() {
        tokio::time::sleep(WRITE_DELAY + Duration::from_millis(150)).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_write_does_not_hold_the_tick() {
        let (mut world, player, mut rx) = world_with_player();
        let started = std::time::Instant::now();
        start_save(&mut world, player, slow_writer(true), SAVE_TURN_RETRIES);
        assert!(
            started.elapsed() < WRITE_DELAY / 3,
            "start_save waited on the write: {:?}",
            started.elapsed()
        );
        assert!(off_tick::is_busy(&world, player));
        // The tick carries on: nothing to finish yet, nothing printed.
        drain_command_jobs(&mut world);
        assert_eq!(drain(&mut rx), "");
        assert!(off_tick::is_busy(&world, player));
        settle().await;
        drain_command_jobs(&mut world);
        assert!(drain(&mut rx).contains("Saved."));
        assert!(!off_tick::is_busy(&world, player));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saved_is_only_printed_after_the_write_commits() {
        let (mut world, player, mut rx) = world_with_player();
        start_save(&mut world, player, slow_writer(true), SAVE_TURN_RETRIES);
        tokio::time::sleep(WRITE_DELAY / 2).await;
        drain_command_jobs(&mut world);
        assert!(!drain(&mut rx).contains("Saved."), "printed mid-write");
        settle().await;
        drain_command_jobs(&mut world);
        assert!(drain(&mut rx).contains("Saved."));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_write_says_so_and_frees_the_player() {
        let (mut world, player, mut rx) = world_with_player();
        start_save(&mut world, player, slow_writer(false), SAVE_TURN_RETRIES);
        settle().await;
        drain_command_jobs(&mut world);
        let out = drain(&mut rx);
        assert!(out.contains("Save failed"), "{out}");
        assert!(!out.contains("Saved."), "{out}");
        assert!(!off_tick::is_busy(&world, player));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_player_who_is_not_a_character_has_nothing_to_save() {
        let (mut world, player, mut rx) = world_with_player();
        world.entity_mut(player).remove::<Account>();
        start_save(&mut world, player, slow_writer(true), SAVE_TURN_RETRIES);
        assert!(drain(&mut rx).contains("Nothing to save"));
        assert!(!off_tick::is_busy(&world, player));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_busy_write_turn_is_waited_for_off_the_tick() {
        let (mut world, player, mut rx) = world_with_player();
        let coordinator = world.resource::<SaveCoordinator>().clone();
        // An autosave holds the character's turn.
        let held = coordinator.try_begin_ordered("c-save-test").unwrap();
        start_save(&mut world, player, slow_writer(true), SAVE_TURN_RETRIES);
        tokio::time::sleep(Duration::from_millis(50)).await;
        drain_command_jobs(&mut world);
        assert_eq!(
            drain(&mut rx),
            "",
            "the save must not run under the held turn"
        );
        drop(held);
        // The wait ends, the tick retries, the write runs and commits.
        tokio::time::sleep(Duration::from_millis(50)).await;
        drain_command_jobs(&mut world);
        settle().await;
        drain_command_jobs(&mut world);
        assert!(drain(&mut rx).contains("Saved."));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn commands_typed_during_a_save_run_after_it_in_order() {
        let (mut world, player, mut rx) = world_with_player();
        let pool = pool();
        start_save(&mut world, player, slow_writer(true), SAVE_TURN_RETRIES);
        // Typed while the save is in flight: held, not run, not refused.
        dispatch_with_async(&mut world, player, &pool, "look").await;
        dispatch_with_async(&mut world, player, &pool, "save").await;
        assert_eq!(drain(&mut rx), "", "ran ahead of the save");
        // Nothing is released while the write is still going.
        assert!(!run_queued_input(&mut world, player, &pool).await);
        settle().await;
        drain_command_jobs(&mut world);
        // The save answered first; then the lines run one per tick, in order.
        assert!(run_queued_input(&mut world, player, &pool).await);
        let out = drain(&mut rx);
        let saved = out.find("Saved.").expect("saved");
        let look = out.find("A quiet hall").expect("look ran after the save");
        assert!(saved < look, "{out}");
        // One line per tick: the second `save` is still waiting its turn,
        // and when it runs it is a save job of its own.
        assert!(crate::commands::has_queued_input(&world, player));
        assert!(run_queued_input(&mut world, player, &pool).await);
        assert!(!crate::commands::has_queued_input(&world, player));
        assert!(off_tick::is_busy(&world, player));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_panicking_job_releases_the_player() {
        let (mut world, player, mut rx) = world_with_player();
        off_tick::spawn(
            &mut world,
            player,
            async { panic!("database driver blew up") },
            |_world, _player, ()| unreachable!("finish must not run"),
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        drain_command_jobs(&mut world);
        assert!(!off_tick::is_busy(&world, player));
        assert!(drain(&mut rx).contains("took too long"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_job_for_a_player_who_left_is_dropped() {
        let (mut world, player, _rx) = world_with_player();
        off_tick::spawn(&mut world, player, async {}, |_world, _player, ()| {
            unreachable!("the player is gone")
        });
        world.despawn(player);
        tokio::time::sleep(Duration::from_millis(50)).await;
        drain_command_jobs(&mut world);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn without_a_job_channel_the_work_runs_inline() {
        let (mut world, player, mut rx) = world_with_player();
        world.remove_resource::<CommandJobs>();
        off_tick::run(&mut world, player, async { 7 }, |world, player, n| {
            send_to(world, player, format!("got {n}\r\n"));
        })
        .await;
        assert_eq!(drain(&mut rx), "got 7\r\n");
        assert!(!off_tick::is_busy(&world, player));
    }
}
