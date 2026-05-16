//! Generic item lifetime + decay tick (B1, parity with legacy
//! `Object.timer` / `Object.decompose_timer`). Items spawned with
//! `ObjectProto.timer_hours > 0` AND without the PERMANENT flag get
//! an `ItemTimer` component at spawn time. The `item_decay_tick`
//! decrements every game-second and destroys the entity at zero.
//!
//! The DECOMPOSING two-phase mode (decompose_window_secs > 0) is
//! plumbed through the component but not yet activated — today the
//! tick just destroys at zero. A follow-up can split into "expired"
//! + "decomposing" states with separate flavor lines.

use bevy_ecs::prelude::*;
use mud_db::enums::ObjectFlag;
use mud_world::{ItemTimer, Item, Located, Named, ObjectProto};

use crate::commands::{broadcast_room_except_rendered, send_to};

/// Legacy MUD-hour to wall seconds. Matches the constant used by
/// effect duration resolution; centralized here to keep the timer
/// math grounded in one place.
const SECS_PER_MUD_HOUR: i32 = 75;

/// Run-at-spawn hook: if the proto has a positive `timer_hours`
/// AND the object isn't flagged PERMANENT, attach an `ItemTimer`
/// to the entity. Caller passes the freshly-spawned entity + its
/// proto. Safe to call after any `world.spawn(...)` that produced
/// an Item entity.
pub fn attach_timer_if_decaying(world: &mut World, entity: Entity, proto: &ObjectProto) {
    if proto.timer_hours <= 0 {
        return;
    }
    if proto.flags.contains(&ObjectFlag::Permanent) {
        return;
    }
    let remaining = proto.timer_hours.saturating_mul(SECS_PER_MUD_HOUR);
    let decompose = proto
        .decompose_timer
        .saturating_mul(SECS_PER_MUD_HOUR)
        .max(0);
    let Ok(mut em) = world.get_entity_mut(entity) else {
        return;
    };
    em.insert(ItemTimer {
        remaining_secs: remaining,
        decompose_window_secs: decompose,
    });
}

/// Decrement every `ItemTimer` by 1 second per call. Items hitting
/// zero are destroyed; when the holder is a player or the item is
/// on the floor of a populated room, a flavor line announces the
/// disappearance so players aren't left wondering where their
/// torch went. Runs at the same 1-Hz cadence as corpse decay.
pub fn item_decay_tick(world: &mut World) {
    // Snapshot first so we can both mutate timers AND despawn
    // without re-borrowing the query.
    let snapshots: Vec<(Entity, i32)> = {
        let mut q = world.query_filtered::<(Entity, &ItemTimer), With<Item>>();
        q.iter(world)
            .map(|(e, t)| (e, t.remaining_secs))
            .collect()
    };
    let mut destroyed: Vec<Entity> = Vec::new();
    for (entity, current) in snapshots {
        let next = current.saturating_sub(1);
        if next <= 0 {
            destroyed.push(entity);
            continue;
        }
        if let Some(mut t) = world.get_mut::<ItemTimer>(entity) {
            t.remaining_secs = next;
        }
    }
    for entity in destroyed {
        // Look up where the item sits BEFORE despawn so we can
        // route the flavor message. Items can be Located on a
        // room (floor), a player (carried/equipped), or another
        // item (inside a container).
        let (holder_entity, holder_kind) = location_kind(world, entity);
        let item_name = world
            .get::<Named>(entity)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| String::from("an item"));
        match holder_kind {
            HolderKind::Player => {
                send_to(
                    world,
                    holder_entity,
                    format!("<dim>{item_name} crumbles to dust in your hands.</>\r\n"),
                );
            }
            HolderKind::Room => {
                broadcast_room_except_rendered(
                    world,
                    holder_entity,
                    &[],
                    &format!("<dim>{item_name} crumbles to dust.</>\r\n"),
                );
            }
            HolderKind::Container | HolderKind::Unknown => {
                // Inside a container or unrooted — silent destroy.
            }
        }
        if let Ok(em) = world.get_entity_mut(entity) {
            em.despawn();
        }
    }
}

#[derive(Debug)]
enum HolderKind {
    Player,
    Room,
    Container,
    Unknown,
}

/// Classify what an item is Located on so the decay tick picks
/// the right announcement path.
fn location_kind(world: &World, item: Entity) -> (Entity, HolderKind) {
    let Some(loc) = world.get::<Located>(item).map(|l| l.0) else {
        return (item, HolderKind::Unknown);
    };
    if world.get::<mud_world::Player>(loc).is_some() {
        return (loc, HolderKind::Player);
    }
    if world.get::<mud_world::Room>(loc).is_some() {
        return (loc, HolderKind::Room);
    }
    (loc, HolderKind::Container)
}
