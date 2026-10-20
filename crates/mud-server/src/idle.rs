//! Idle handling, mirroring legacy `check_idling` (`limits.cpp`).
//!
//! Legacy counts 75-second MUD-hour ticks since the last input line:
//! at 8 ticks (10 minutes) an idle mortal is pulled into the void room
//! (their room is remembered, they are saved), at 12 ticks (15 minutes)
//! they are force-rented: disconnected and extracted. Typing anything
//! brings a voided character straight back. Staff (any rank above
//! Player) are exempt.
//!
//! Here the scan marks a connected player past the void window with
//! [`IdleVoided`] (and moves them to the void), and one past the rent
//! window with `IdleKickPending`; the main tick loop drains that marker
//! after the schedule by sending a notice and routing each through
//! `ConnRouter::on_disconnect` (same path as a real telnet drop, so
//! `save_player` runs and the entity is despawned).
//!
//! Kept out of `commands::flush_prompts` so the disconnect notice
//! lands first; otherwise the player would receive the kick line
//! sandwiched between two prompts.

use std::time::Duration;

use bevy_ecs::prelude::*;
use mud_world::{Account, LastInputAt, LoggedInAt, Online, Player, WorldKeyIndex};

use crate::TickCount;
use crate::query_cache::with_query;

/// Check every 10s. Fine enough that the void and the kick land within
/// ten seconds of their thresholds; cheap enough that the scan doesn't
/// churn the world tick.
const IDLE_CHECK_PERIOD_TICKS: u64 = 100;
/// Legacy `check_idling`: 8 ticks of 75 seconds before the void.
const DEFAULT_IDLE_VOID_SECS: u64 = 8 * 75;
/// Legacy `check_idling`: 12 ticks of 75 seconds before the force-rent.
const DEFAULT_IDLE_RENT_SECS: u64 = 12 * 75;
/// Legacy room 0, "The Void" (zone 0, id 0).
const VOID_ROOM: (i32, i32) = (0, 0);

/// Read a positive `server.<key>` `GameConfig` row, else `default`.
/// The row wins so operators can re-tune at runtime without restart.
fn config_secs(world: &World, key: &str, default: u64) -> u64 {
    let secs = world
        .get_resource::<mud_world::RuntimeConfig>()
        .map_or(0, |c| c.get_i32("server", key, 0));
    u64::try_from(secs)
        .ok()
        .filter(|s| *s > 0)
        .unwrap_or(default)
}

/// Idle seconds before a mortal is pulled into the void
/// (`server.idle_void_seconds`, legacy 10 minutes).
fn idle_void_secs(world: &World) -> u64 {
    config_secs(world, "idle_void_seconds", DEFAULT_IDLE_VOID_SECS)
}

/// Idle seconds before a mortal is disconnected
/// (`server.idle_rent_seconds`, legacy 15 minutes). Never below the void
/// window, so the void always comes first.
fn idle_rent_secs(world: &World) -> u64 {
    config_secs(world, "idle_rent_seconds", DEFAULT_IDLE_RENT_SECS).max(idle_void_secs(world))
}

/// Marker placed on a connected player whose `LastInputAt` (or
/// `LoggedInAt` if they never typed) elapsed past the rent window.
/// Drained from `main` after the schedule runs; never inspected
/// from a system.
#[derive(Component)]
pub struct IdleKickPending;

/// The player was idle past the void window and sits in the void room;
/// `was_in` is the room to put them back in (legacy `GET_WAS_IN`).
/// Removed, and the player returned, as soon as they type again.
#[derive(Component)]
pub struct IdleVoided {
    pub was_in: Entity,
}

/// Last time the idle scan saw this player in combat. Fighting counts as
/// being active (a mid-fight kick leaves a linkdead character swinging),
/// so the idle clock restarts when the fight ends instead of resuming
/// from the pre-fight keystroke.
#[derive(Component)]
pub struct IdleCombatSeen(pub std::time::Instant);

/// Combat counts as activity only up to this many void windows since the
/// player last typed. Without a cap, an AFK character in a room of
/// respawning aggressive mobs fights forever and never times out.
const COMBAT_IDLE_CAP_WINDOWS: u32 = 3;

/// Time since the player last typed (or logged in, if they never have).
fn since_last_input(world: &World, entity: Entity) -> Duration {
    world
        .get::<LastInputAt>(entity)
        .map(|l| l.0.elapsed())
        .or_else(|| world.get::<LoggedInAt>(entity).map(|l| l.0.elapsed()))
        .unwrap_or(Duration::ZERO)
}

/// True when `entity` is fighting and has typed recently enough
/// (within [`COMBAT_IDLE_CAP_WINDOWS`] void windows) for the fight to
/// count as activity.
fn combat_keeps_alive(world: &mut World, entity: Entity, window: Duration) -> bool {
    crate::commands::in_combat(world, entity)
        && since_last_input(world, entity) <= window * COMBAT_IDLE_CAP_WINDOWS
}

/// Legacy "pulled into a void": remember the current room, save the
/// player there, and move them to the void room. No-op when the world
/// has no void room, or the player is already standing in it (legacy
/// only voids from a real room).
fn pull_into_void(world: &mut World, entity: Entity) {
    let Some(void) = world
        .get_resource::<WorldKeyIndex>()
        .and_then(|k| k.rooms.get(&VOID_ROOM).copied())
    else {
        return;
    };
    let Some(here) = world.get::<mud_world::Located>(entity).map(|l| l.0) else {
        return;
    };
    if here == void {
        return;
    }
    let who = crate::commands::name_of(world, entity);
    crate::commands::broadcast_room_visible(
        world,
        here,
        entity,
        &[entity],
        &format!("{who} disappears into the void.\r\n"),
    );
    crate::commands::send_to(
        world,
        entity,
        "\r\nYou have been idle, and are pulled into a void.\r\n",
    );
    mud_world::movement::move_to_room(world, entity, void);
    if let Ok(mut e) = world.get_entity_mut(entity) {
        e.insert(IdleVoided { was_in: here });
    }
}

/// Put a voided player back where they were (legacy comm.cpp "pull char
/// back from void"). No-op for anyone not voided. If the old room is gone
/// the player stays put but is no longer marked.
pub(crate) fn return_from_void(world: &mut World, entity: Entity) {
    leave_void(world, entity, true);
}

/// [`return_from_void`] for a player about to be saved and dropped
/// (disconnect, kick): the character must be written out in its real room,
/// not the void, and nobody needs to hear it came back.
pub(crate) fn return_from_void_quietly(world: &mut World, entity: Entity) {
    leave_void(world, entity, false);
}

fn leave_void(world: &mut World, entity: Entity, announce: bool) {
    let Some(was_in) = world.get::<IdleVoided>(entity).map(|v| v.was_in) else {
        return;
    };
    if let Ok(mut e) = world.get_entity_mut(entity) {
        e.remove::<IdleVoided>();
    }
    if world.get_entity(was_in).is_err() {
        return;
    }
    mud_world::movement::move_to_room(world, entity, was_in);
    if !announce {
        return;
    }
    let who = crate::commands::name_of(world, entity);
    crate::commands::broadcast_room_visible(
        world,
        was_in,
        entity,
        &[entity],
        &format!("{who} has returned.\r\n"),
    );
}

pub fn idle_kick_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(IDLE_CHECK_PERIOD_TICKS) {
        return;
    }
    let void_after = Duration::from_secs(idle_void_secs(world));
    let rent_after = Duration::from_secs(idle_rent_secs(world));
    let candidates: Vec<(Entity, Duration)> = {
        let mut q = world.query_filtered::<
            (
                Entity,
                Option<&LastInputAt>,
                Option<&LoggedInAt>,
                Option<&IdleCombatSeen>,
                &Account,
            ),
            // Linkdead characters keep `Online` but have no connection to
            // kick; `drain_linkdead` retires them, so marking them here
            // only yields an orphaned marker every check.
            (
                With<Player>,
                With<Online>,
                Without<crate::commands::Linkdead>,
            ),
        >();
        q.iter(world)
            // Staff (any rank above Player) never idle out. `Account.role`
            // is the effective rank (level-derived), so unlinked L100+ god
            // characters are exempt too.
            .filter(|(.., acct)| {
                use mud_db::enums::UserRole;
                acct.role.rank() <= UserRole::Player.rank()
            })
            .map(|(e, last, login, combat, _)| {
                let elapsed = last
                    .map(|l| l.0.elapsed())
                    .or_else(|| login.map(|l| l.0.elapsed()))
                    .unwrap_or(Duration::ZERO);
                let elapsed = combat.map_or(elapsed, |c| elapsed.min(c.0.elapsed()));
                (e, elapsed)
            })
            .collect()
    };
    for (entity, elapsed) in candidates {
        // Fighting is activity: never void or kick mid-fight, unless the
        // player has been silent for the whole combat-idle cap.
        if combat_keeps_alive(world, entity, void_after) {
            if let Ok(mut e) = world.get_entity_mut(entity) {
                e.insert(IdleCombatSeen(std::time::Instant::now()));
                e.remove::<IdleKickPending>();
            }
            continue;
        }
        // Past the cap the fight no longer counts: judge by keystrokes only.
        let elapsed = if crate::commands::in_combat(world, entity) {
            since_last_input(world, entity)
        } else {
            elapsed
        };
        if elapsed > rent_after {
            if let Ok(mut e) = world.get_entity_mut(entity) {
                e.insert(IdleKickPending);
            }
        } else if elapsed > void_after {
            if world.get::<IdleVoided>(entity).is_none() {
                pull_into_void(world, entity);
            }
        } else {
            // Finished a fight (clock restarted) since the void: bring the
            // player back. Typing does this itself in `stamp_input`.
            return_from_void(world, entity);
        }
    }
}

/// Mark every connection that owns a logged-in player as authenticated
/// in `mud-net`, which otherwise closes connections that never finish
/// login (pre-login idle / total timeouts). Cheap: linear in online
/// players, run about once a second.
pub fn sync_authenticated(world: &mut World, router: &crate::login::ConnRouter) {
    for entity in crate::query_cache::entities::<With<Player>>(world) {
        if let Some(conn_id) = router.find_conn(entity) {
            mud_net::mark_authenticated(conn_id);
        }
    }
}

/// Drain `IdleKickPending` markers: tell each player they're being
/// kicked, run the canonical `on_disconnect` save flow, then close the
/// socket via `mud_net::close_connection` (queued notice is flushed
/// before the FIN). Without the close the session would be detached
/// but the TCP connection would linger until the client dropped it.
pub fn drain_idle_kicks(
    world: &mut World,
    router: &mut crate::login::ConnRouter,
    pool: &mud_db::sqlx::PgPool,
) {
    let pending: Vec<Entity> =
        with_query::<Entity, With<IdleKickPending>, _>(world, |world, q| q.iter(world).collect());
    let window = Duration::from_secs(idle_void_secs(world));
    for entity in pending {
        // Marked before a fight broke out: kicking now would leave a
        // linkdead character still swinging. Let them stay, unless they
        // have been silent past the combat-idle cap.
        if combat_keeps_alive(world, entity, window) {
            if let Ok(mut e) = world.get_entity_mut(entity) {
                e.remove::<IdleKickPending>();
            }
            continue;
        }
        if let Some(conn_id) = router.find_conn(entity) {
            return_from_void_quietly(world, entity);
            crate::commands::send_to(
                world,
                entity,
                "\r\nYou have been idle for too long. Disconnecting.\r\n",
            );
            router.on_disconnect(world, conn_id, pool);
            // A character that survives the disconnect must not carry a
            // stale marker into its next session.
            if let Ok(mut e) = world.get_entity_mut(entity) {
                e.remove::<IdleKickPending>();
            }
            mud_net::close_connection(conn_id);
        } else if let Ok(mut e) = world.get_entity_mut(entity) {
            // Orphaned marker (e.g. the player despawned mid-tick
            // somehow) — drop it so the next pass doesn't keep retrying.
            e.remove::<IdleKickPending>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_db::enums::{UserRole, effective_rank};
    use mud_world::Located;

    fn spawn_idle(world: &mut World, level: i32, account_role: UserRole) -> Entity {
        world
            .spawn((
                Player,
                Online,
                Account {
                    user_id: String::new(),
                    character_id: "c".into(),
                    role: effective_rank(level),
                    account_role,
                    perms: vec![],
                },
                // Idle for a day, well past the 15 minute default.
                LoggedInAt(
                    std::time::Instant::now()
                        .checked_sub(Duration::from_secs(86_400))
                        .expect("monotonic clock has > 1 day of uptime"),
                ),
            ))
            .id()
    }

    fn set_config(world: &mut World, key: &str, value: i64) {
        world
            .resource_mut::<mud_world::RuntimeConfig>()
            .by_key
            .insert(
                ("server".to_string(), key.to_string()),
                mud_world::ConfigValue::Int(value),
            );
    }

    fn idle_for(world: &mut World, entity: Entity, secs: u64) {
        let t = std::time::Instant::now()
            .checked_sub(Duration::from_secs(secs))
            .expect("monotonic clock has enough uptime");
        world.entity_mut(entity).insert(LastInputAt(t));
    }

    /// A world with the legacy defaults and a void room at (0, 0).
    fn timed_world() -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(TickCount(IDLE_CHECK_PERIOD_TICKS));
        world.insert_resource(mud_world::RuntimeConfig::default());
        let void = world.spawn(mud_world::Room).id();
        let mut keys = WorldKeyIndex::default();
        keys.rooms.insert(VOID_ROOM, void);
        world.insert_resource(keys);
        (world, void)
    }

    #[test]
    fn staff_by_level_are_exempt_from_idle_kick() {
        let (mut world, _) = timed_world();
        let mortal = spawn_idle(&mut world, 99, UserRole::Player);
        // Unlinked god characters: Player account role, staff by level.
        let l100 = spawn_idle(&mut world, 100, UserRole::Player);
        let l105 = spawn_idle(&mut world, 105, UserRole::Player);
        // Low-level character on a staff account: mortal in game.
        let linked = spawn_idle(&mut world, 1, UserRole::Builder);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(mortal).is_some());
        assert!(world.get::<IdleKickPending>(l100).is_none());
        assert!(world.get::<IdleKickPending>(l105).is_none());
        assert!(world.get::<IdleKickPending>(linked).is_some());
    }

    #[test]
    fn linkdead_characters_are_not_marked_for_idle_kick() {
        let (mut world, _) = timed_world();
        let connected = spawn_idle(&mut world, 10, UserRole::Player);
        let linkdead = spawn_idle(&mut world, 10, UserRole::Player);
        world
            .entity_mut(linkdead)
            .insert(crate::commands::Linkdead { since_tick: 0 });
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(connected).is_some());
        assert!(world.get::<IdleKickPending>(linkdead).is_none());
    }

    /// Issue #154: legacy `check_idling` voids at 8 ticks and force-rents at
    /// 12 ticks of 75 seconds. The stale seeded `connection_timeout_seconds`
    /// row (300) is not an idle setting and must not shorten them.
    #[test]
    fn defaults_are_the_legacy_ten_and_fifteen_minutes() {
        let (mut world, _) = timed_world();
        assert_eq!(idle_void_secs(&world), 600);
        assert_eq!(idle_rent_secs(&world), 900);
        set_config(&mut world, "connection_timeout_seconds", 300);
        assert_eq!(idle_void_secs(&world), 600);
        assert_eq!(idle_rent_secs(&world), 900);
        set_config(&mut world, "idle_void_seconds", 1200);
        set_config(&mut world, "idle_rent_seconds", 100);
        assert_eq!(idle_void_secs(&world), 1200);
        assert_eq!(idle_rent_secs(&world), 1200, "rent never precedes the void");
    }

    #[test]
    fn five_minutes_idle_is_left_alone() {
        let (mut world, _) = timed_world();
        let room = world.spawn(mud_world::Room).id();
        let p = spawn_idle(&mut world, 10, UserRole::Player);
        world.entity_mut(p).insert(Located(room));
        idle_for(&mut world, p, 310);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleVoided>(p).is_none());
        assert!(world.get::<IdleKickPending>(p).is_none());
        assert_eq!(world.get::<Located>(p).unwrap().0, room);
    }

    #[test]
    fn ten_minutes_idle_pulls_into_the_void_and_input_brings_back() {
        let (mut world, void) = timed_world();
        let room = world.spawn(mud_world::Room).id();
        let p = spawn_idle(&mut world, 10, UserRole::Player);
        world.entity_mut(p).insert(Located(room));
        idle_for(&mut world, p, 650);
        idle_kick_tick(&mut world);
        assert_eq!(world.get::<Located>(p).unwrap().0, void);
        assert_eq!(world.get::<IdleVoided>(p).unwrap().was_in, room);
        assert!(world.get::<IdleKickPending>(p).is_none());
        // A second scan while still voided changes nothing.
        idle_kick_tick(&mut world);
        assert_eq!(world.get::<IdleVoided>(p).unwrap().was_in, room);
        // Typing returns them to where they were.
        return_from_void(&mut world, p);
        assert_eq!(world.get::<Located>(p).unwrap().0, room);
        assert!(world.get::<IdleVoided>(p).is_none());
    }

    #[test]
    fn fifteen_minutes_idle_is_kicked_and_saved_from_the_real_room() {
        let (mut world, void) = timed_world();
        let room = world.spawn(mud_world::Room).id();
        let p = spawn_idle(&mut world, 10, UserRole::Player);
        world.entity_mut(p).insert(Located(room));
        idle_for(&mut world, p, 650);
        idle_kick_tick(&mut world);
        assert_eq!(world.get::<Located>(p).unwrap().0, void);
        idle_for(&mut world, p, 950);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(p).is_some());
        // The drain puts them back before the save.
        return_from_void_quietly(&mut world, p);
        assert_eq!(world.get::<Located>(p).unwrap().0, room);
    }

    #[test]
    fn a_player_already_in_the_void_is_not_voided() {
        let (mut world, void) = timed_world();
        let p = spawn_idle(&mut world, 10, UserRole::Player);
        world.entity_mut(p).insert(Located(void));
        idle_for(&mut world, p, 650);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleVoided>(p).is_none());
        assert!(world.get::<IdleKickPending>(p).is_none());
    }

    #[test]
    fn fighting_players_are_not_kicked_and_get_a_fresh_clock() {
        let (mut world, _) = timed_world();
        let fighter = spawn_idle(&mut world, 10, UserRole::Player);
        // Typed 1000s ago: past the 900s rent window, inside the combat cap.
        idle_for(&mut world, fighter, 1000);
        let foe = world.spawn_empty().id();
        world.entity_mut(fighter).insert(mud_world::Fighting(foe));
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(fighter).is_none());
        assert!(world.get::<IdleVoided>(fighter).is_none());
        // Fight over: the idle clock restarted at the end of the fight
        // instead of resuming from the 1000s-old keystroke.
        world.entity_mut(fighter).remove::<mud_world::Fighting>();
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(fighter).is_none());
    }

    #[test]
    fn stale_kick_marker_is_dropped_when_a_fight_starts() {
        let (mut world, _) = timed_world();
        let p = spawn_idle(&mut world, 10, UserRole::Player);
        idle_for(&mut world, p, 1000);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(p).is_some());
        let foe = world.spawn_empty().id();
        world.entity_mut(p).insert(mud_world::Fighting(foe));
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(p).is_none());
    }

    #[test]
    fn combat_stops_counting_as_activity_after_three_void_windows() {
        let (mut world, _) = timed_world();
        let afk = spawn_idle(&mut world, 10, UserRole::Player);
        let foe = world.spawn_empty().id();
        world.entity_mut(afk).insert(mud_world::Fighting(foe));
        // 3 windows = 1800s. Silent for 1900s while a respawning mob keeps
        // them "in combat": kicked anyway.
        idle_for(&mut world, afk, 1900);
        // A prior fight tick bumped the combat clock to "just now".
        world
            .entity_mut(afk)
            .insert(IdleCombatSeen(std::time::Instant::now()));
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(afk).is_some());
        // Same fight, but they typed 1700s ago (inside the cap): spared.
        let active = spawn_idle(&mut world, 10, UserRole::Player);
        world.entity_mut(active).insert(mud_world::Fighting(foe));
        idle_for(&mut world, active, 1700);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(active).is_none());
    }
}
