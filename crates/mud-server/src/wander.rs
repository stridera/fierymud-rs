//! Mob wandering tick. Once every `WANDER_PERIOD_TICKS`, every
//! eligible mob picks a random open exit and walks one room. The
//! schema's `MobBehavior` flags gate participation:
//!
//! - `Sentinel` mobs never wander.
//! - `StayZone` mobs only walk through exits that stay in the
//!   same zone (zone match on the destination room's `WorldKey`).
//! - Mobs in combat (`Fighting` component) never wander.
//! - Mounts being ridden (`RiddenBy`) never wander — the rider
//!   moves them via the cardinal-direction commands.
//!
//! Cadence is loose (~30 game seconds) so a player walking through
//! a populated zone doesn't see mobs constantly migrating; tight
//! enough that a long sit watches the world breathe.

use bevy_ecs::prelude::*;
use mud_db::enums::{ExitState, MobBehavior, MobTrait, Sector};
use mud_world::{
    AttachedTriggers, Corpse, ExitData, Exits, Fighting, Item, Located, Mob, MobBehaviors,
    MobTraits, Named, RiddenBy, RoomSector, WorldKey,
};

use crate::TickCount;
use crate::commands::{broadcast_room_except_players_rendered, direction_name, opposite};

/// One wander check every 300 ticks (= 30s real-time at 10Hz).
/// Each tick a fixed fraction of eligible mobs actually move —
/// see `WANDER_CHANCE_DENOM`.
const WANDER_PERIOD_TICKS: u64 = 300;
/// Per-eligible-mob chance to actually wander on a wander tick.
/// 1 in 4 means roughly one move every 2 minutes per mob —
/// ambient, not chaotic.
const WANDER_CHANCE_DENOM: u32 = 4;

/// Scavenger pickup tick fires every 100 ticks (= 10s real-time).
/// Each Scavenger-flagged mob in a room with a free-floor item
/// picks one up — at most one per tick per mob, so a busy zone
/// doesn't see mobs hoover the floor in a single frame.
const SCAVENGER_PERIOD_TICKS: u64 = 100;

/// True when the room's sector counts as "water" for AQUATIC-mob
/// movement: SHALLOWS, WATER, UNDERWATER. Beach / swamp aren't water
/// — fish can't crawl onto a beach.
fn room_is_aquatic(world: &World, room: Entity) -> bool {
    world
        .get::<RoomSector>(room)
        .is_some_and(|s| matches!(s.0, Sector::Shallows | Sector::Water | Sector::Underwater))
}

pub fn wander_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(WANDER_PERIOD_TICKS) {
        return;
    }
    // Snapshot every eligible mob with its current room. The full
    // gate runs in Rust here; the per-step exit roll happens later.
    let candidates: Vec<(Entity, Entity)> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, Option<&MobBehaviors>, Option<&AttachedTriggers>),
            (With<Mob>, Without<Fighting>, Without<RiddenBy>),
        >();
        q.iter(world)
            .filter(|(_, _, beh, _)| !beh.is_some_and(|b| b.has(MobBehavior::Sentinel)))
            .map(|(e, l, _, _)| (e, l.0))
            .collect()
    };
    if candidates.is_empty() {
        return;
    }
    let mut moves: Vec<(Entity, Entity, Entity, mud_db::enums::Direction)> = Vec::new();
    for (mob, room) in candidates {
        if rand::random_range(0..WANDER_CHANCE_DENOM) != 0 {
            continue;
        }
        // StayZone-aware exit pool: the destination must be in the
        // same zone for StayZone-flagged mobs. Other mobs walk
        // wherever an open exit leads.
        let stay_zone = world
            .get::<MobBehaviors>(mob)
            .is_some_and(|b| b.has(MobBehavior::StayZone));
        let mob_zone = world.get::<WorldKey>(mob).map(|k| k.zone);
        // AQUATIC trait (Wave 2.L): mob can only exist in water-class
        // sectors. Wander filter refuses any non-water target so a
        // shark stays in the ocean even when the cave next door is
        // an open exit.
        let aquatic = world
            .get::<MobTraits>(mob)
            .is_some_and(|t| t.has(MobTrait::Aquatic));
        let candidates_dir: Vec<(mud_db::enums::Direction, Entity)> = world
            .get::<Exits>(room)
            .map(|exits| {
                exits
                    .0
                    .iter()
                    .filter_map(|(dir, ed): (_, &ExitData)| {
                        if ed.state != ExitState::Open {
                            return None;
                        }
                        let to = ed.to?;
                        if stay_zone {
                            let target_zone = world.get::<WorldKey>(to).map(|k| k.zone);
                            if target_zone != mob_zone {
                                return None;
                            }
                        }
                        if aquatic && !room_is_aquatic(world, to) {
                            return None;
                        }
                        // NoMobsRoom: wandering mobs refuse to enter
                        // rooms flagged `allows_mobs = false`. Staff-
                        // placed mobs (via `load <zone> <id>`) bypass
                        // — the gate fires here, where the mob is
                        // *choosing* to step in.
                        if world.get::<mud_world::NoMobsRoom>(to).is_some() {
                            return None;
                        }
                        Some((*dir, to))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if candidates_dir.is_empty() {
            continue;
        }
        let pick = rand::random_range(0..candidates_dir.len());
        let (dir, target) = candidates_dir[pick];
        moves.push((mob, room, target, dir));
    }
    // Apply moves. Done in a separate pass so the candidate
    // snapshot's borrows are gone before we mutate `Located`.
    for (mob, from_room, target_room, dir) in moves {
        let mob_name = world
            .get::<Named>(mob)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        if mob_name.is_empty() {
            continue;
        }
        broadcast_room_except_players_rendered(
            world,
            from_room,
            &[mob],
            &format!(
                "{} leaves {}.\r\n",
                crate::commands::cap_sentence_start(&mob_name),
                direction_name(dir),
            ),
        );
        if let Some(mut l) = world.get_mut::<Located>(mob) {
            l.0 = target_room;
        }
        let arrival_dir =
            opposite(dir).map_or("nearby".to_string(), |d| format!("the {}", direction_name(d)));
        broadcast_room_except_players_rendered(
            world,
            target_room,
            &[mob],
            &format!(
                "{} arrives from {arrival_dir}.\r\n",
                crate::commands::cap_sentence_start(&mob_name),
            ),
        );
    }
}

/// Mob `Scavenger` behavior: every Scavenger-flagged mob picks up
/// one floor item per tick from its current room. Items already
/// inside containers (Located on something other than the room)
/// or worn (`EquippedSlot` set) are skipped. Cadence is 10s so
/// even a Scavenger-heavy zone doesn't strip the floor in one
/// frame; a player dropping a stack still gets to grab some back.
pub fn scavenger_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(SCAVENGER_PERIOD_TICKS) {
        return;
    }
    let scavengers: Vec<(Entity, Entity)> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &MobBehaviors),
            (With<Mob>, Without<Fighting>),
        >();
        q.iter(world)
            .filter(|(_, _, beh)| beh.has(MobBehavior::Scavenger))
            .map(|(e, l, _)| (e, l.0))
            .collect()
    };
    for (mob, room) in scavengers {
        // Pick the first free-floor item in the room — items
        // Located on other actors or inside containers stay put.
        // Corpses are skipped: a player who dies in a Scavenger-
        // patrolled room and respawns expects to find their own
        // body still on the floor, not vanished into a mob's
        // inventory and despawned with the mob's next tick.
        let target_item: Option<(Entity, String)> = {
            let mut q = world.query_filtered::<
                (Entity, &Located, &Named),
                (With<Item>, Without<Corpse>),
            >();
            q.iter(world)
                .find(|(_, l, _)| l.0 == room)
                .map(|(e, _, n)| (e, n.name.clone()))
        };
        let Some((item, item_name)) = target_item else {
            continue;
        };
        if let Some(mut l) = world.get_mut::<Located>(item) {
            l.0 = mob;
        }
        let mob_name = world
            .get::<Named>(mob)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        if !mob_name.is_empty() {
            broadcast_room_except_players_rendered(
                world,
                room,
                &[],
                &format!("{mob_name} picks up {item_name}.\r\n"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    //! Room-flag wander gating tests. Verifies the
    //! `Room.allows_mobs = false` flag (loaded as `NoMobsRoom`)
    //! keeps wandering mobs from migrating into wards.

    use super::*;
    use mud_db::enums::Direction;
    use mud_world::{ExitData, NoMobsRoom};

    fn make_room(world: &mut World) -> Entity {
        world.spawn_empty().id()
    }

    /// Build a single open exit `from -> to` in direction `dir`. The
    /// inverse exit is left off — these tests are one-directional so
    /// the wandering mob has exactly one place to consider going.
    fn link(world: &mut World, from: Entity, dir: Direction, to: Entity) {
        let mut exits = mud_world::Exits::default();
        exits.0.insert(
            dir,
            ExitData {
                to: Some(to),
                state: mud_db::enums::ExitState::Open,
                key: None,
                description: None,
                keywords: Vec::new(),
                is_hidden: false,
                is_pickproof: false,
            },
        );
        world.entity_mut(from).insert(exits);
    }

    fn make_mob(world: &mut World, room: Entity) -> Entity {
        world
            .spawn((
                Mob,
                Named { name: "test mob".to_string() },
                Located(room),
            ))
            .id()
    }

    /// The exit-pool filter inside `wander_tick` is the gate of
    /// interest. We invoke it indirectly: stub a single mob with a
    /// rooted exit pointing at a `NoMobsRoom` target, then run the
    /// tick on a wander-eligible tick number. After the tick the
    /// mob's `Located` must not have changed — the only available
    /// exit was filtered out, so it stayed put.
    #[test]
    fn no_mobs_room_blocks_wandering_into_it() {
        let mut world = World::new();
        let from = make_room(&mut world);
        let to = make_room(&mut world);
        world.entity_mut(to).insert(NoMobsRoom);
        link(&mut world, from, Direction::North, to);
        let mob = make_mob(&mut world, from);

        // Force the tick to a wander cadence so the gate runs at all.
        // Even when the per-mob RNG roll passes, the exit-pool filter
        // is the next gate, and it has no random component — so the
        // assertion is stable across runs.
        world.insert_resource(TickCount(WANDER_PERIOD_TICKS));
        // Run several times so the RNG eventually picks "move" (1-in-4)
        // and the exit-pool filter has a chance to be exercised.
        for _ in 0..50 {
            wander_tick(&mut world);
        }
        assert_eq!(
            world.get::<Located>(mob).map(|l| l.0),
            Some(from),
            "mob stayed in source room because NoMobsRoom filtered the only exit",
        );
    }

    /// Inverse: without the `NoMobsRoom` marker, the same setup
    /// eventually lets the mob wander through. Run a few iterations
    /// so the 1-in-4 RNG resolves — if the gate is wrongly tripped
    /// (e.g. swapped polarity), this fails fast.
    #[test]
    fn unflagged_room_lets_mob_wander_in() {
        let mut world = World::new();
        let from = make_room(&mut world);
        let to = make_room(&mut world);
        link(&mut world, from, Direction::North, to);
        let mob = make_mob(&mut world, from);

        world.insert_resource(TickCount(WANDER_PERIOD_TICKS));
        let mut moved = false;
        // Up to 50 ticks: 1 - (3/4)^50 ≈ 99.99...% chance the mob
        // moved at least once. Flake tolerance lives in the upper
        // bound; if this fails the gate is mis-applied.
        for _ in 0..50 {
            wander_tick(&mut world);
            if world.get::<Located>(mob).map(|l| l.0) == Some(to) {
                moved = true;
                break;
            }
        }
        assert!(moved, "mob should have wandered into the unflagged room");
    }

    /// AQUATIC trait (Wave 2.L) wander gate: a shark with
    /// `MobTrait::Aquatic` should refuse to step into a forest
    /// sector. Source room is `Water`, target room is `Forest`;
    /// after many ticks the shark stays in the water.
    #[test]
    fn aquatic_mob_refuses_non_water_target() {
        let mut world = World::new();
        let from = make_room(&mut world);
        let to = make_room(&mut world);
        world.entity_mut(from).insert(RoomSector(Sector::Water));
        world.entity_mut(to).insert(RoomSector(Sector::Forest));
        link(&mut world, from, Direction::North, to);
        let mob = make_mob(&mut world, from);
        world
            .entity_mut(mob)
            .insert(MobTraits(vec![MobTrait::Aquatic]));

        world.insert_resource(TickCount(WANDER_PERIOD_TICKS));
        for _ in 0..50 {
            wander_tick(&mut world);
        }
        assert_eq!(
            world.get::<Located>(mob).map(|l| l.0),
            Some(from),
            "AQUATIC mob should not wander into Forest",
        );
    }

    /// Inverse: same AQUATIC mob with a water-sector neighbor moves
    /// freely. Confirms the gate isn't paralyzing the mob in legitimate
    /// water rooms.
    #[test]
    fn aquatic_mob_wanders_into_water_target() {
        let mut world = World::new();
        let from = make_room(&mut world);
        let to = make_room(&mut world);
        world.entity_mut(from).insert(RoomSector(Sector::Water));
        world.entity_mut(to).insert(RoomSector(Sector::Shallows));
        link(&mut world, from, Direction::North, to);
        let mob = make_mob(&mut world, from);
        world
            .entity_mut(mob)
            .insert(MobTraits(vec![MobTrait::Aquatic]));

        world.insert_resource(TickCount(WANDER_PERIOD_TICKS));
        let mut moved = false;
        for _ in 0..50 {
            wander_tick(&mut world);
            if world.get::<Located>(mob).map(|l| l.0) == Some(to) {
                moved = true;
                break;
            }
        }
        assert!(moved, "AQUATIC mob should wander between water sectors");
    }
}
