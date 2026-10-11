//! Idle disconnect. Players who haven't typed in the idle window are
//! warned shortly before (`server.idle_warning_seconds`), then
//! get marked with `IdleKickPending`; the main tick loop drains the
//! marker after the schedule by sending a notice and routing each
//! through `ConnRouter::on_disconnect` (same path as a real telnet
//! drop, so `save_player` runs and the entity is despawned).
//!
//! Kept out of `commands::flush_prompts` so the disconnect notice
//! lands first; otherwise the player would receive the kick line
//! sandwiched between two prompts.

use std::time::Duration;

use bevy_ecs::prelude::*;
use mud_world::{Account, LastInputAt, LoggedInAt, Online, Player};

use crate::TickCount;

/// Check every 10s. Fine enough that the pre-kick warning lands within
/// ten seconds of its mark and a 30-minute idle gets booted within ten
/// seconds of the threshold; cheap enough that the scan doesn't churn
/// the world tick.
const IDLE_CHECK_PERIOD_TICKS: u64 = 100;
/// Default idle threshold. 30 minutes mirrors the conventional
/// MUD idle timer; immortals (any role above Player) are exempt
/// because admins routinely sit AFK observing. Precedence chain:
/// `server.connection_timeout_seconds` `GameConfig` row, then
/// `MUD_IDLE_KICK_SECS` env var, then this default.
const DEFAULT_IDLE_KICK_SECS: u64 = 30 * 60;
/// Default lead time of the idle warning: two minutes before the kick.
/// `server.idle_warning_seconds` `GameConfig` row overrides it; a
/// non-positive value disables the warning. No seed row is needed, the
/// default lives here (same as `connection_timeout_seconds`).
const DEFAULT_IDLE_WARNING_SECS: i32 = 120;

fn idle_kick_secs(world: &World) -> u64 {
    // GameConfig row wins so operators can re-tune at runtime
    // without restart. A non-positive value falls through to the
    // env var / hardcoded default.
    let cfg_secs = world
        .get_resource::<mud_world::RuntimeConfig>()
        .map_or(0, |c| c.get_i32("server", "connection_timeout_seconds", 0));
    if cfg_secs > 0 {
        return u64::try_from(cfg_secs).unwrap_or(DEFAULT_IDLE_KICK_SECS);
    }
    std::env::var("MUD_IDLE_KICK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_IDLE_KICK_SECS)
}

/// How long before the kick the warning is sent. Zero disables it.
fn idle_warning_secs(world: &World) -> u64 {
    let secs = world.resource::<mud_world::RuntimeConfig>().get_i32(
        "server",
        "idle_warning_seconds",
        DEFAULT_IDLE_WARNING_SECS,
    );
    u64::try_from(secs).unwrap_or(0)
}

/// Marker placed on a connected player whose `LastInputAt` (or
/// `LoggedInAt` if they never typed) elapsed past the kick window.
/// Drained from `main` after the schedule runs; never inspected
/// from a system.
#[derive(Component)]
pub struct IdleKickPending;

/// The player has already been sent the idle warning for this idle
/// stretch. Cleared as soon as they type again.
#[derive(Component)]
pub struct IdleWarned;

/// Last time the idle scan saw this player in combat. Fighting counts as
/// being active (a mid-fight kick leaves a linkdead character swinging),
/// so the idle clock restarts when the fight ends instead of resuming
/// from the pre-fight keystroke.
#[derive(Component)]
pub struct IdleCombatSeen(pub std::time::Instant);

/// Combat counts as activity only up to this many idle windows since the
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
/// (within [`COMBAT_IDLE_CAP_WINDOWS`] idle windows) for the fight to
/// count as activity.
fn combat_keeps_alive(world: &mut World, entity: Entity, threshold: Duration) -> bool {
    crate::commands::in_combat(world, entity)
        && since_last_input(world, entity) <= threshold * COMBAT_IDLE_CAP_WINDOWS
}

fn warning_text(remaining: Duration) -> String {
    let secs = remaining.as_secs().max(1);
    let when = if secs >= 90 {
        format!("{} minutes", secs.div_ceil(60))
    } else {
        format!("{secs} seconds")
    };
    format!(
        "\r\nYou have been idle for a while. You will be disconnected in about {when} \
         unless you type something.\r\n"
    )
}

pub fn idle_kick_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(IDLE_CHECK_PERIOD_TICKS) {
        return;
    }
    let threshold = Duration::from_secs(idle_kick_secs(world));
    // The warning must fire strictly before the kick, so a lead time
    // that swallows the whole window is clamped to half of it.
    let warn_lead = Duration::from_secs(idle_warning_secs(world)).min(threshold / 2);
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
            // is the effective rank (max of website role and level-derived
            // role), so unlinked L100+ god characters are exempt too.
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
        // Fighting is activity: never warn or kick mid-fight, unless the
        // player has been silent for the whole combat-idle cap.
        if combat_keeps_alive(world, entity, threshold) {
            if let Ok(mut e) = world.get_entity_mut(entity) {
                e.insert(IdleCombatSeen(std::time::Instant::now()));
                e.remove::<(IdleWarned, IdleKickPending)>();
            }
            continue;
        }
        // Past the cap the fight no longer counts: judge by keystrokes only.
        let elapsed = if crate::commands::in_combat(world, entity) {
            since_last_input(world, entity)
        } else {
            elapsed
        };
        if elapsed > threshold {
            if let Ok(mut e) = world.get_entity_mut(entity) {
                e.insert(IdleKickPending);
            }
        } else if !warn_lead.is_zero() && elapsed > threshold.saturating_sub(warn_lead) {
            if world.get::<IdleWarned>(entity).is_none() {
                crate::commands::send_to(
                    world,
                    entity,
                    warning_text(threshold.saturating_sub(elapsed)),
                );
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.insert(IdleWarned);
                }
            }
        } else if let Ok(mut e) = world.get_entity_mut(entity) {
            // Typed (or finished a fight) since the warning: re-arm it.
            e.remove::<IdleWarned>();
        }
    }
}

/// Mark every connection that owns a logged-in player as authenticated
/// in `mud-net`, which otherwise closes connections that never finish
/// login (pre-login idle / total timeouts). Cheap: linear in online
/// players, run about once a second.
pub fn sync_authenticated(world: &mut World, router: &crate::login::ConnRouter) {
    let mut q = world.query_filtered::<Entity, With<Player>>();
    for entity in q.iter(world) {
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
    let pending: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, With<IdleKickPending>>();
        q.iter(world).collect()
    };
    let threshold = Duration::from_secs(idle_kick_secs(world));
    for entity in pending {
        // Marked before a fight broke out: kicking now would leave a
        // linkdead character still swinging. Let them stay, unless they
        // have been silent past the combat-idle cap.
        if combat_keeps_alive(world, entity, threshold) {
            if let Ok(mut e) = world.get_entity_mut(entity) {
                e.remove::<IdleKickPending>();
            }
            continue;
        }
        if let Some(conn_id) = router.find_conn(entity) {
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

    fn spawn_idle(world: &mut World, level: i32, account_role: UserRole) -> Entity {
        world
            .spawn((
                Player,
                Online,
                Account {
                    user_id: String::new(),
                    character_id: "c".into(),
                    role: effective_rank(level, account_role),
                    account_role,
                    perms: vec![],
                },
                // Idle for a day, well past the 30 minute default.
                LoggedInAt(
                    std::time::Instant::now()
                        .checked_sub(Duration::from_secs(86_400))
                        .expect("monotonic clock has > 1 day of uptime"),
                ),
            ))
            .id()
    }

    #[test]
    fn staff_by_level_are_exempt_from_idle_kick() {
        let mut world = World::new();
        world.insert_resource(TickCount(IDLE_CHECK_PERIOD_TICKS));
        world.insert_resource(mud_world::RuntimeConfig::default());
        let mortal = spawn_idle(&mut world, 99, UserRole::Player);
        // Unlinked god characters: Player account role, staff by level.
        let l100 = spawn_idle(&mut world, 100, UserRole::Player);
        let l105 = spawn_idle(&mut world, 105, UserRole::Player);
        // Linked staff account on a low-level character.
        let linked = spawn_idle(&mut world, 1, UserRole::Builder);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(mortal).is_some());
        assert!(world.get::<IdleKickPending>(l100).is_none());
        assert!(world.get::<IdleKickPending>(l105).is_none());
        assert!(world.get::<IdleKickPending>(linked).is_none());
    }

    #[test]
    fn linkdead_characters_are_not_marked_for_idle_kick() {
        let mut world = World::new();
        world.insert_resource(TickCount(IDLE_CHECK_PERIOD_TICKS));
        world.insert_resource(mud_world::RuntimeConfig::default());
        let connected = spawn_idle(&mut world, 10, UserRole::Player);
        let linkdead = spawn_idle(&mut world, 10, UserRole::Player);
        world
            .entity_mut(linkdead)
            .insert(crate::commands::Linkdead { since_tick: 0 });
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(connected).is_some());
        assert!(world.get::<IdleKickPending>(linkdead).is_none());
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

    fn timed_world() -> World {
        let mut world = World::new();
        world.insert_resource(TickCount(IDLE_CHECK_PERIOD_TICKS));
        world.insert_resource(mud_world::RuntimeConfig::default());
        set_config(&mut world, "connection_timeout_seconds", 600);
        world
    }

    #[test]
    fn fighting_players_are_not_kicked_and_get_a_fresh_clock() {
        let mut world = timed_world();
        let fighter = spawn_idle(&mut world, 10, UserRole::Player);
        // Typed 700s ago: past the 600s window, inside the combat cap.
        idle_for(&mut world, fighter, 700);
        let foe = world.spawn_empty().id();
        world.entity_mut(fighter).insert(mud_world::Fighting(foe));
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(fighter).is_none());
        assert!(world.get::<IdleWarned>(fighter).is_none());
        // Fight over: the idle clock restarted at the end of the fight
        // instead of resuming from the 700s-old keystroke.
        world.entity_mut(fighter).remove::<mud_world::Fighting>();
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(fighter).is_none());
    }

    #[test]
    fn stale_kick_marker_is_dropped_when_a_fight_starts() {
        let mut world = timed_world();
        let p = spawn_idle(&mut world, 10, UserRole::Player);
        idle_for(&mut world, p, 700);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(p).is_some());
        let foe = world.spawn_empty().id();
        world.entity_mut(p).insert(mud_world::Fighting(foe));
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(p).is_none());
    }

    #[test]
    fn combat_stops_counting_as_activity_after_three_idle_windows() {
        let mut world = timed_world();
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

    #[test]
    fn warning_is_sent_once_before_the_kick_and_rearms_on_input() {
        let mut world = timed_world();
        set_config(&mut world, "idle_warning_seconds", 120);
        let p = spawn_idle(&mut world, 10, UserRole::Player);
        // Well before the warning window (600 - 120 = 480s).
        idle_for(&mut world, p, 300);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleWarned>(p).is_none());
        assert!(world.get::<IdleKickPending>(p).is_none());
        // Inside the warning window: warned, not kicked.
        idle_for(&mut world, p, 500);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleWarned>(p).is_some());
        assert!(world.get::<IdleKickPending>(p).is_none());
        // Typed again: warning re-arms.
        idle_for(&mut world, p, 5);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleWarned>(p).is_none());
        // Past the threshold: kicked.
        idle_for(&mut world, p, 700);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleKickPending>(p).is_some());
    }

    #[test]
    fn warning_can_be_disabled_by_config() {
        let mut world = timed_world();
        set_config(&mut world, "idle_warning_seconds", 0);
        let p = spawn_idle(&mut world, 10, UserRole::Player);
        idle_for(&mut world, p, 590);
        idle_kick_tick(&mut world);
        assert!(world.get::<IdleWarned>(p).is_none());
        assert!(world.get::<IdleKickPending>(p).is_none());
    }

    #[test]
    fn warning_text_names_the_remaining_time() {
        assert!(warning_text(Duration::from_secs(120)).contains("2 minutes"));
        assert!(warning_text(Duration::from_secs(45)).contains("45 seconds"));
    }
}
