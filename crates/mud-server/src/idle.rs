//! Idle disconnect. Players who haven't typed in `IDLE_KICK_SECS`
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

/// Check every 60s — fast enough that a 30-minute idle gets booted
/// within a minute of the threshold; slow enough that the scan
/// doesn't churn the world tick.
const IDLE_CHECK_PERIOD_TICKS: u64 = 600;
/// Default idle threshold. 30 minutes mirrors the conventional
/// MUD idle timer; immortals (any role above Player) are exempt
/// because admins routinely sit AFK observing. Precedence chain:
/// `server.connection_timeout_seconds` `GameConfig` row, then
/// `MUD_IDLE_KICK_SECS` env var, then this default.
const DEFAULT_IDLE_KICK_SECS: u64 = 30 * 60;

fn idle_kick_secs(world: &World) -> u64 {
    // GameConfig row wins so operators can re-tune at runtime
    // without restart. A non-positive value falls through to the
    // env var / hardcoded default.
    let cfg_secs = world.resource::<mud_world::RuntimeConfig>().get_i32(
        "server",
        "connection_timeout_seconds",
        0,
    );
    if cfg_secs > 0 {
        return u64::try_from(cfg_secs).unwrap_or(DEFAULT_IDLE_KICK_SECS);
    }
    std::env::var("MUD_IDLE_KICK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_IDLE_KICK_SECS)
}

/// Marker placed on a connected player whose `LastInputAt` (or
/// `LoggedInAt` if they never typed) elapsed past the kick window.
/// Drained from `main` after the schedule runs; never inspected
/// from a system.
#[derive(Component)]
pub struct IdleKickPending;

pub fn idle_kick_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(IDLE_CHECK_PERIOD_TICKS) {
        return;
    }
    let threshold = Duration::from_secs(idle_kick_secs(world));
    let to_kick: Vec<Entity> = {
        let mut q = world.query_filtered::<
            (Entity, Option<&LastInputAt>, Option<&LoggedInAt>, &Account),
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
            .filter(|(_, _, _, acct)| {
                use mud_db::enums::UserRole;
                acct.role.rank() <= UserRole::Player.rank()
            })
            .filter(|(_, last, login, _)| {
                let elapsed = last
                    .map(|l| l.0.elapsed())
                    .or_else(|| login.map(|l| l.0.elapsed()))
                    .unwrap_or(Duration::ZERO);
                elapsed > threshold
            })
            .map(|(e, _, _, _)| e)
            .collect()
    };
    for entity in to_kick {
        if let Ok(mut e) = world.get_entity_mut(entity) {
            e.insert(IdleKickPending);
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
pub async fn drain_idle_kicks(
    world: &mut World,
    router: &mut crate::login::ConnRouter,
    pool: &mud_db::sqlx::PgPool,
) {
    let pending: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, With<IdleKickPending>>();
        q.iter(world).collect()
    };
    for entity in pending {
        if let Some(conn_id) = router.find_conn(entity) {
            crate::commands::send_to(
                world,
                entity,
                "\r\nYou have been idle for too long. Disconnecting.\r\n",
            );
            router.on_disconnect(world, conn_id, pool).await;
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
}
