//! Camp lifecycle: setup, tick, completion. The legacy `do_camp`
//! pattern was a "safe wilderness logout" — pitch a tent, wait,
//! save, disconnect. The Rust port treats it as a "long rest with
//! checkpoint" instead: the auto-save-on-disconnect path already
//! covers logout safety, but a structured rest that takes a
//! commitment of in-game time and ends with a save is still
//! useful (and matches the legacy outdoors-only restriction the
//! user called out).

use bevy_ecs::prelude::*;
use mud_db::enums::Sector;
use mud_world::{Camping, Fighting, Located, PendingSave};

use crate::TickCount;
use crate::commands::{broadcast_room_except_players_rendered, name_of, send_rendered};

/// Sectors a player may camp in. Mirrors the legacy refusal
/// pattern: indoor (Structure), city, water variants, and air
/// are all out. Caves / underdark / planes are also refused —
/// not "outdoors" in any meaningful sense.
#[must_use]
pub fn sector_allows_camp(sector: Sector) -> bool {
    matches!(
        sector,
        Sector::Field
            | Sector::Forest
            | Sector::Hills
            | Sector::Mountain
            | Sector::Road
            | Sector::Grasslands
            | Sector::Beach
            | Sector::Swamp
            | Sector::Ruins
    )
}

/// Ticks elapsed before camp completes. At 10Hz this is 35 real
/// seconds — short enough to feel responsive, long enough that
/// camping in unsafe terrain still mattered if a wandering mob
/// shows up.
pub const CAMP_DURATION_TICKS: u64 = 350;

/// Per-tick walk over `Camping` players: cancels on combat or room
/// movement, completes when the deadline is reached. Completion
/// is a no-op gameplay-wise; the player just gets a flavor line
/// and a `PendingSave` marker so the next save loop checkpoints
/// them. Movement / combat aborts also clear the `Camping`
/// component.
pub fn camp_tick(world: &mut World) {
    let now_tick = world.resource::<TickCount>().0;
    let snapshot: Vec<(Entity, Camping)> = {
        let mut q = world.query::<(Entity, &Camping)>();
        q.iter(world)
            .map(|(e, c)| (e, *c))
            .collect()
    };
    for (entity, camp) in snapshot {
        // Combat-cancel: mid-camp ambush wakes you up.
        if world.get::<Fighting>(entity).is_some() {
            cancel(
                world,
                entity,
                "Combat shatters your half-finished camp.",
            );
            continue;
        }
        // Movement-cancel: leaving the campsite ends it.
        let now_in = world.get::<Located>(entity).map(|l| l.0);
        if now_in != Some(camp.started_in) {
            cancel(
                world,
                entity,
                "You wander away from your half-pitched campsite.",
            );
            continue;
        }
        // Completion: deadline reached.
        if now_tick.saturating_sub(camp.since_tick) >= CAMP_DURATION_TICKS {
            complete(world, entity);
        }
    }
}

fn cancel(world: &mut World, entity: Entity, msg: &str) {
    if let Ok(mut em) = world.get_entity_mut(entity) {
        em.remove::<Camping>();
    }
    send_rendered(world, entity, &format!("{msg}\r\n"));
}

fn complete(world: &mut World, entity: Entity) {
    if let Ok(mut em) = world.get_entity_mut(entity) {
        em.remove::<Camping>();
        em.insert(PendingSave);
    }
    let player_name = name_of(world, entity);
    let room = world.get::<Located>(entity).map(|l| l.0);
    send_rendered(
        world,
        entity,
        "<b:cyan>You complete your campsite, settle in, and rest for a while.</>\r\n",
    );
    if let Some(room) = room {
        broadcast_room_except_players_rendered(
            world,
            room,
            &[entity],
            &format!("<dim>{player_name} finishes pitching camp and settles in.</>\r\n"),
        );
    }
}
