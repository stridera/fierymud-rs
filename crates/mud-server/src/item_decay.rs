//! Generic item lifetime + decay tick (B1, parity with legacy
//! `Object.timer` / `Object.decompose_timer`). Items spawned with
//! `ObjectProto.timer_hours > 0` AND without the PERMANENT flag get
//! an `ItemTimer` component at spawn time. The `item_decay_tick`
//! decrements every game-second and destroys the entity at zero.
//!
//! The DECOMPOSING two-phase mode (`decompose_window_secs` > 0) is
//! plumbed through the component but not yet activated — today the
//! tick just destroys at zero. A follow-up can split into "expired"
//! + "decomposing" states with separate flavor lines.

use bevy_ecs::prelude::*;
use mud_db::enums::ObjectFlag;
use mud_world::{Item, ItemTimer, Located, Named, ObjectProto};

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
        q.iter(world).map(|(e, t)| (e, t.remaining_secs)).collect()
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
            .map_or_else(|| String::from("an item"), |n| n.name.clone());
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
        release_contents(world, entity, holder_entity, &holder_kind);
        if let Ok(em) = world.get_entity_mut(entity) {
            em.despawn();
        }
    }
}

/// A decaying container must not orphan what it holds. Legacy
/// `extract_corpse` moves each item to the container's own container,
/// or to the room (the carrier's room when the container is carried).
/// A container with no resolvable place (unrooted) takes its contents
/// with it. Items nested deeper stay with their own parent.
pub(crate) fn release_contents(
    world: &mut World,
    container: Entity,
    holder: Entity,
    kind: &HolderKind,
) {
    let contents: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == container)
            .map(|(e, _)| e)
            .collect()
    };
    if contents.is_empty() {
        return;
    }
    let dest = match kind {
        HolderKind::Room | HolderKind::Container => Some(holder),
        HolderKind::Player => world.get::<Located>(holder).map(|l| l.0),
        HolderKind::Unknown => None,
    };
    for item in contents {
        match dest {
            Some(d) => {
                world.entity_mut(item).insert(Located(d));
            }
            None => {
                if let Ok(em) = world.get_entity_mut(item) {
                    em.despawn();
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum HolderKind {
    Player,
    Room,
    Container,
    Unknown,
}

/// Classify what an item is Located on so the decay tick picks
/// the right announcement path.
pub(crate) fn location_kind(world: &World, item: Entity) -> (Entity, HolderKind) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::{Keywords, WorldKey};

    fn item(world: &mut World, name: &str, at: Entity) -> Entity {
        world
            .spawn((
                Item,
                Named { name: name.into() },
                Keywords(vec![name.into()]),
                WorldKey { zone: 1, id: 1 },
                Located(at),
            ))
            .id()
    }

    fn decaying(world: &mut World, name: &str, at: Entity) -> Entity {
        let e = item(world, name, at);
        world.entity_mut(e).insert(ItemTimer {
            remaining_secs: 1,
            decompose_window_secs: 0,
        });
        e
    }

    #[test]
    fn decaying_container_on_the_floor_spills_into_the_room() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let bag = decaying(&mut world, "bag", room);
        let gem = item(&mut world, "gem", bag);
        let inner = item(&mut world, "pouch", bag);
        let coin = item(&mut world, "coin", inner);
        item_decay_tick(&mut world);
        assert!(world.get_entity(bag).is_err());
        assert_eq!(world.get::<Located>(gem).unwrap().0, room);
        assert_eq!(world.get::<Located>(inner).unwrap().0, room);
        // Deeper nesting stays with its own parent.
        assert_eq!(world.get::<Located>(coin).unwrap().0, inner);
    }

    #[test]
    fn decaying_container_inside_another_hands_contents_to_it() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let chest = item(&mut world, "chest", room);
        let bag = decaying(&mut world, "bag", chest);
        let gem = item(&mut world, "gem", bag);
        item_decay_tick(&mut world);
        assert!(world.get_entity(bag).is_err());
        assert_eq!(world.get::<Located>(gem).unwrap().0, chest);
    }

    #[test]
    fn decaying_carried_container_drops_contents_in_the_carriers_room() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let player = world.spawn((mud_world::Player, Located(room))).id();
        let bag = decaying(&mut world, "bag", player);
        let gem = item(&mut world, "gem", bag);
        item_decay_tick(&mut world);
        assert!(world.get_entity(bag).is_err());
        assert_eq!(world.get::<Located>(gem).unwrap().0, room);
    }
}
