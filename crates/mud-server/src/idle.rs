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
/// Idle threshold. 30 minutes mirrors the conventional MUD idle
/// timer; immortals (any role above Player) are exempt because
/// admins routinely sit AFK observing.
const IDLE_KICK_SECS: u64 = 30 * 60;

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
    let threshold = Duration::from_secs(IDLE_KICK_SECS);
    let to_kick: Vec<Entity> = {
        let mut q = world.query_filtered::<
            (Entity, Option<&LastInputAt>, Option<&LoggedInAt>, &Account),
            (With<Player>, With<Online>),
        >();
        q.iter(world)
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
