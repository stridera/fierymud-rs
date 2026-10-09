//! Mob wandering tick. Once every `WANDER_PERIOD_TICKS`, every
//! eligible mob picks a random open exit and walks one room. The
//! schema's `MobBehavior` flags gate participation:
//!
//! - `Sentinel` mobs never wander.
//! - `StayZone` mobs only walk through exits that stay in the
//!   same zone (zone match on the destination room's `WorldKey`).
//! - Mobs in combat (`Fighting` component) never wander.
//! - Servants (pets, charmed mobs) and mobs following a leader who is
//!   in the same room never wander — legacy `mob_movement` refuses
//!   charmed mobs outright, and a follower walking off would break
//!   the follow.
//! - Mounts being ridden (`RiddenBy`) never wander — the rider
//!   moves them via the cardinal-direction commands.
//!
//! Cadence is loose (~30 game seconds) so a player walking through
//! a populated zone doesn't see mobs constantly migrating; tight
//! enough that a long sit watches the world breathe.

use std::collections::{HashMap, HashSet};

use bevy_ecs::prelude::*;
use mud_db::enums::{ExitState, MobBehavior, MobTrait, ObjectRestriction, Sector};
use mud_world::{
    AttachedTriggers, Corpse, ExitData, Exits, Fighting, Follower, Item, Located, Mob,
    MobBehaviors, MobTraits, Named, ObjectRestrictions, Player, RiddenBy, RoomSector, WorldKey,
};

use crate::TickCount;
use crate::commands::{arrival_from, broadcast_room_except_players_rendered, direction_name};

/// One wander check every 300 ticks (= 30s real-time at 10Hz).
/// Each tick a fixed fraction of eligible mobs actually move —
/// see `WANDER_CHANCE_DENOM`.
const WANDER_PERIOD_TICKS: u64 = 300;
/// Per-eligible-mob chance to actually wander on a wander tick.
/// 1 in 4 means roughly one move every 2 minutes per mob —
/// ambient, not chaotic.
const WANDER_CHANCE_DENOM: u32 = 4;

/// Scavenger tick fires every 100 ticks (= 10s real-time), the legacy
/// `PULSE_MOBILE`. Each Scavenger-flagged mob in a room with floor loot
/// takes one item on a 50% roll — at most one per tick per mob, so a
/// busy zone doesn't see mobs hoover the floor in a single frame.
pub(crate) const SCAVENGER_PERIOD_TICKS: u64 = 100;

/// True when the room's sector counts as "water" for AQUATIC-mob
/// movement: SHALLOWS, WATER, UNDERWATER. Beach / swamp aren't water
/// — fish can't crawl onto a beach.
fn room_is_aquatic(world: &World, room: Entity) -> bool {
    world
        .get::<RoomSector>(room)
        .is_some_and(|s| matches!(s.0, Sector::Shallows | Sector::Water | Sector::Underwater))
}

/// True for a mob that must not wander: a servant (pet / charmed mob), or
/// any mob whose `Follower` leader is in the same room as it.
fn is_tethered(world: &World, mob: Entity, room: Entity) -> bool {
    if crate::commands::is_servant(world, mob) {
        return true;
    }
    world
        .get::<Follower>(mob)
        .and_then(|f| world.get::<Located>(f.0))
        .is_some_and(|l| l.0 == room)
}

#[allow(clippy::too_many_lines)]
pub fn wander_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(WANDER_PERIOD_TICKS) {
        return;
    }
    // Snapshot every eligible mob with its current room. The full
    // gate runs in Rust here; the per-step exit roll happens later.
    let candidates: Vec<(Entity, Entity)> = {
        let mut q = world.query_filtered::<(
            Entity,
            &Located,
            Option<&MobBehaviors>,
            Option<&AttachedTriggers>,
        ), (With<Mob>, Without<Fighting>, Without<RiddenBy>)>();
        q.iter(world)
            .filter(|(_, _, beh, _)| !beh.is_some_and(|b| b.has(MobBehavior::Sentinel)))
            .map(|(e, l, _, _)| (e, l.0))
            .collect()
    };
    // Servants and mobs trailing a leader who is still in the room stay put.
    let candidates: Vec<(Entity, Entity)> = candidates
        .into_iter()
        .filter(|&(mob, room)| !is_tethered(world, mob, room))
        .collect();
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
                        // "Water, no swim": a mob without a boat, wings,
                        // waterwalk or the AQUATIC trait never wanders into
                        // or out of deep water (legacy mob_movement goes
                        // through the same do_simple_move gate).
                        if crate::room_access::deep_water_blocks(world, mob, room, to) {
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
    // Rooms a player stands in. Departure / arrival lines, hiding-perception
    // checks and life-sense rolls only ever reach players, so a move between
    // rooms nobody is in does none of that work. Players do not move during
    // this pass, so one snapshot serves every move.
    let player_rooms: HashSet<Entity> = {
        let mut q = world.query_filtered::<&Located, With<Player>>();
        q.iter(world).map(|l| l.0).collect()
    };
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
        // Mobs are mortals as far as entry restrictions go (legacy GODROOM
        // kept every sub-immortal mob out). Cheap unless the room has one.
        if !crate::room_access::entry_allowed(world, mob, target_room) {
            continue;
        }
        // A hiding mob wears its hiding down as it walks, and observers
        // who cannot see it through that hear nothing of it.
        crate::hiding::decay_on_move(world, mob, &mut |lo, hi| rand::random_range(lo..=hi));
        if player_rooms.contains(&from_room) {
            crate::commands::broadcast_room_visual(
                world,
                from_room,
                mob,
                &[mob],
                &format!(
                    "{} leaves {}.\r\n",
                    crate::commands::cap_sentence_start(&mob_name),
                    direction_name(dir),
                ),
            );
            crate::commands::senses::sense_departure(world, from_room, mob, &[mob]);
        }
        crate::combat::relocate(world, mob, target_room);
        if player_rooms.contains(&target_room) {
            let arrival_dir = arrival_from(dir);
            crate::commands::broadcast_room_visible(
                world,
                target_room,
                mob,
                &[mob],
                &format!(
                    "{} arrives from {arrival_dir}.\r\n",
                    crate::commands::cap_sentence_start(&mob_name),
                ),
            );
        }
    }
}

/// Mob auto-hide (legacy `mobile_activity`, mobact.cpp:181, every
/// `PULSE_MOBILE` = [`SCAVENGER_PERIOD_TICKS`]): a mob that knows `hide`
/// and is not hidden calls `do_hide`. Like legacy it skips mobs that are
/// fighting, `NO_CLASS_AI`, immortal-level, unable to act, or charmed
/// with their master elsewhere. Skills come from the prototype's class or
/// race ([`crate::mob_ai::mob_skill_pct`]); the cheap filters (not
/// hidden, not fighting) run in the query and the skill lookup only for
/// what is left, so a world without the `hide` ability does no work. The
/// `hide` wait state ([`crate::commands::info::hide_with_roll`]) stops a
/// mob re-rolling while it is still lagged.
pub fn mob_hide_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(SCAVENGER_PERIOD_TICKS) {
        return;
    }
    let Some(hide_id) = world
        .get_resource::<mud_world::AbilityCatalog>()
        .and_then(|c| c.by_name.get("hide"))
        .map(|d| d.id)
    else {
        return;
    };
    let candidates: Vec<Entity> = {
        let mut q = world.query_filtered::<
            (Entity, Option<&MobBehaviors>),
            (With<Mob>, Without<mud_world::Hiddenness>, Without<Fighting>),
        >();
        q.iter(world)
            .filter(|(_, beh)| !beh.is_some_and(|b| b.has(MobBehavior::NoClassAi)))
            .map(|(e, _)| e)
            .collect()
    };
    for mob in candidates {
        if !crate::mob_ai::mob_can_act(world, mob)
            || mud_world::effective_level(world, mob) >= IMMORTAL_LEVEL
            || crate::mob_ai::mob_skill_pct(world, mob, hide_id) == 0
        {
            continue;
        }
        if crate::commands::is_servant(world, mob) {
            let room = world.get::<Located>(mob).map(|l| l.0);
            let master_here = world
                .get::<Follower>(mob)
                .and_then(|f| world.get::<Located>(f.0))
                .map(|l| l.0)
                == room;
            if !master_here {
                continue;
            }
        }
        crate::commands::info::hide_with_roll(world, mob, &mut |lo, hi| {
            rand::random_range(lo..=hi)
        });
    }
}

/// Legacy `LVL_IMMORT`: mobs at or above it never run the rogue AI.
const IMMORTAL_LEVEL: i32 = 100;

/// Mob `Scavenger` behavior (legacy `mobile_activity`, every
/// `PULSE_MOBILE` = 10 s = [`SCAVENGER_PERIOD_TICKS`]): each
/// Scavenger-flagged, non-illusory mob that can act rolls 50% and, on a
/// hit, takes the single most valuable gettable item off the floor of its
/// room (`mob_ai::mob_scavenge`: `appraise_item` + `CAN_GET_OBJ`); then it
/// wears whatever it carries that beats what is worn
/// (`mob_ai::mob_attempt_equip`). Both run even while the mob is
/// fighting, as in legacy. Items inside containers or worn (`Located` on
/// something other than the room) and !TAKE fixtures are never candidates.
///
/// Cost is O(scavengers + items): one pass over the items builds a
/// per-room list of floor loot, restricted to rooms a scavenger actually
/// stands in. (The earlier per-mob scan was O(scavengers x items) and
/// built a fresh query for every mob, ~150-220 ms at 5.5k mobs / 4.1k
/// items.) A mob only appraises the few items of its own room, and only
/// after its 50% roll; mobs sharing a room take successive best items
/// from that room's list.
pub fn scavenger_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(SCAVENGER_PERIOD_TICKS) {
        return;
    }
    let scavengers: Vec<(Entity, Entity)> = {
        let mut q = world
            .query_filtered::<(Entity, &Located, &MobBehaviors, Option<&MobTraits>), With<Mob>>();
        q.iter(world)
            .filter(|(_, _, beh, traits)| {
                beh.has(MobBehavior::Scavenger)
                    && !traits.is_some_and(|t| t.has(MobTrait::Illusion))
            })
            .map(|(e, l, _, _)| (e, l.0))
            .collect()
    };
    let scavengers: Vec<(Entity, Entity)> = scavengers
        .into_iter()
        .filter(|&(mob, _)| crate::mob_ai::mob_can_act(world, mob))
        .collect();
    if scavengers.is_empty() {
        return;
    }
    // Player house rooms are off limits: nothing a player placed at home is
    // loot, whoever happens to follow its owner in. (No world exit leads
    // there, so only a follower can be inside; legacy `ROOM_HOUSE`.)
    let scavenger_rooms: HashSet<Entity> = scavengers
        .iter()
        .map(|&(_, room)| room)
        .filter(|&room| world.get::<mud_world::HouseRoom>(room).is_none())
        .collect();
    // Free-floor items per scavenger room. Items Located on other actors
    // or inside containers never match a room key. Corpses are skipped: a
    // player who dies in a Scavenger-patrolled room and respawns expects
    // to find their own body still on the floor, not vanished into a mob's
    // inventory and despawned with the mob's next tick.
    let mut floor: HashMap<Entity, Vec<Entity>> = HashMap::new();
    {
        let mut q = world.query_filtered::<(Entity, &Located, Option<&ObjectRestrictions>), (
            With<Item>,
            With<Named>,
            Without<Corpse>,
            Without<mud_world::HouseItem>,
        )>();
        for (item, loc, restrictions) in q.iter(world) {
            // Same gate as the `get` command: a !TAKE item is fixed in
            // place for everyone, mobs included.
            if restrictions.is_some_and(|r| r.has(ObjectRestriction::NoTake)) {
                continue;
            }
            if scavenger_rooms.contains(&loc.0) {
                floor.entry(loc.0).or_default().push(item);
            }
        }
    }
    // The pickup message only reaches players, so skip it in rooms that
    // have none (the common case).
    let player_rooms: HashSet<Entity> = if floor.is_empty() {
        HashSet::new()
    } else {
        let mut q = world.query_filtered::<&Located, With<Player>>();
        q.iter(world).map(|l| l.0).collect()
    };
    for (mob, room) in scavengers {
        if let Some(items) = floor.get_mut(&room)
            && !items.is_empty()
            && crate::mob_ai::scavenge_roll()
            && let Some((_, item_name)) = crate::mob_ai::mob_scavenge(world, mob, items)
            && player_rooms.contains(&room)
        {
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
        crate::mob_ai::mob_attempt_equip(world, mob);
    }
}

/// Mob assist pulse (legacy `mobile_activity` -> `mob_assist`, every
/// `PULSE_MOBILE` = [`SCAVENGER_PERIOD_TICKS`]): lets helper, protector and
/// peacekeeper mobs join fights already underway in their room.
pub fn assist_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(SCAVENGER_PERIOD_TICKS) {
        return;
    }
    crate::commands::mob_assist_pulse(world);
}

/// Mob aggro pulse (legacy `mobile_activity` -> `find_aggr_target` /
/// `mob_memory_check` -> `mob_attack`, every `PULSE_MOBILE` =
/// [`SCAVENGER_PERIOD_TICKS`], mobact.cpp:283): the only place a mob decides to
/// start a fight with a player standing in its room. The mob that engages
/// strikes in the same tick, so entering a room never costs a free blow and a
/// player who leaves before the pulse is never hit.
pub fn aggro_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(SCAVENGER_PERIOD_TICKS) {
        return;
    }
    crate::commands::aggro_pulse(world);
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
                is_bashable: false,
                hit_points: None,
            },
        );
        world.entity_mut(from).insert(exits);
    }

    fn make_mob(world: &mut World, room: Entity) -> Entity {
        world
            .spawn((
                Mob,
                Named {
                    name: "test mob".to_string(),
                },
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

    #[derive(Component)]
    struct M0;
    #[derive(Component)]
    struct M1;
    #[derive(Component)]
    struct M2;
    #[derive(Component)]
    struct M3;
    #[derive(Component)]
    struct M4;
    #[derive(Component)]
    struct M5;
    #[derive(Component)]
    struct M6;
    #[derive(Component)]
    struct M7;

    /// Attach a bit-pattern of marker components so the world spans up to
    /// 256 archetypes per entity kind, as a live world with many
    /// component combinations does.
    fn scatter_archetype(world: &mut World, e: Entity, bits: usize) {
        let mut ent = world.entity_mut(e);
        if bits & 1 != 0 {
            ent.insert(M0);
        }
        if bits & 2 != 0 {
            ent.insert(M1);
        }
        if bits & 4 != 0 {
            ent.insert(M2);
        }
        if bits & 8 != 0 {
            ent.insert(M3);
        }
        if bits & 16 != 0 {
            ent.insert(M4);
        }
        if bits & 32 != 0 {
            ent.insert(M5);
        }
        if bits & 64 != 0 {
            ent.insert(M6);
        }
        if bits & 128 != 0 {
            ent.insert(M7);
        }
    }

    /// Prod-scale scavenger scenario: 5000 mobs (1200 scavengers), 4000
    /// items over 1500 rooms. Floor loot is concentrated in rooms
    /// 1000..1500, so most scavengers stand in a room with nothing to take
    /// (the worst case for a scan-per-mob). Some items are corpses or
    /// carried by mobs, and a few marker components spread the entities
    /// over several archetypes like a live world. Returns
    /// `(world, rooms, scavengers, corpses, carried)`.
    fn scavenger_world() -> (World, Vec<Entity>, Vec<Entity>, Vec<Entity>, Vec<Entity>) {
        const ROOMS: usize = 1500;
        const MOBS: usize = 5000;
        const SCAVENGERS: usize = 1200;
        const ITEMS: usize = 4000;
        let mut world = World::new();
        world.insert_resource(TickCount(SCAVENGER_PERIOD_TICKS));
        let rooms: Vec<Entity> = (0..ROOMS).map(|_| make_room(&mut world)).collect();
        let mut mobs = Vec::with_capacity(MOBS);
        let mut scavengers = Vec::new();
        for i in 0..MOBS {
            let mob = make_mob(&mut world, rooms[i % ROOMS]);
            if i < SCAVENGERS {
                world
                    .entity_mut(mob)
                    .insert(MobBehaviors(vec![MobBehavior::Scavenger]));
                scavengers.push(mob);
            }
            scatter_archetype(&mut world, mob, i);
            mobs.push(mob);
        }
        let (mut corpses, mut carried) = (Vec::new(), Vec::new());
        for i in 0..ITEMS {
            let named = Named {
                name: format!("item {i}"),
            };
            let floor = rooms[1000 + i % 500];
            let item = match i % 10 {
                // Carried by a (non-scavenger) mob: never floor loot.
                0 => {
                    let holder = mobs[SCAVENGERS + i % (MOBS - SCAVENGERS)];
                    let e = world.spawn((Item, named, Located(holder))).id();
                    carried.push(e);
                    e
                }
                1 => {
                    let e = world.spawn((Item, Corpse, named, Located(floor))).id();
                    corpses.push(e);
                    e
                }
                _ => world.spawn((Item, named, Located(floor))).id(),
            };
            scatter_archetype(&mut world, item, i);
        }
        (world, rooms, scavengers, corpses, carried)
    }

    /// Each scavenger standing on floor loot lifts exactly one item,
    /// corpses and carried items are never touched, and scavengers in
    /// bare rooms stay empty-handed.
    #[test]
    fn scavenger_tick_picks_one_floor_item_each() {
        crate::mob_ai::force_scavenge_roll(Some(true));
        let (mut world, rooms, scavengers, corpses, carried) = scavenger_world();
        let floor_before: usize = {
            let mut q = world.query_filtered::<&Located, (With<Item>, Without<Corpse>)>();
            q.iter(&world).filter(|l| rooms.contains(&l.0)).count()
        };
        // Scavengers standing in a room that holds a non-corpse floor item.
        let loot_rooms: std::collections::HashSet<Entity> = {
            let mut q = world.query_filtered::<&Located, (With<Item>, Without<Corpse>)>();
            q.iter(&world)
                .map(|l| l.0)
                .filter(|r| rooms.contains(r))
                .collect()
        };
        let expected: Vec<Entity> = scavengers
            .iter()
            .copied()
            .filter(|&m| loot_rooms.contains(&world.get::<Located>(m).unwrap().0))
            .collect();
        assert_eq!(expected.len(), 160);
        scavenger_tick(&mut world);
        let mut holding = std::collections::HashMap::<Entity, usize>::new();
        let mut q = world.query_filtered::<&Located, With<Item>>();
        for l in q.iter(&world) {
            if scavengers.contains(&l.0) {
                *holding.entry(l.0).or_default() += 1;
            }
        }
        assert!(holding.values().all(|&n| n == 1), "one item per mob");
        assert_eq!(holding.len(), expected.len());
        assert!(expected.iter().all(|m| holding.contains_key(m)));
        for c in corpses {
            assert!(
                rooms.contains(&world.get::<Located>(c).unwrap().0),
                "corpse taken"
            );
        }
        for c in carried {
            assert!(!rooms.contains(&world.get::<Located>(c).unwrap().0));
        }
        let floor_after = {
            let mut q = world.query_filtered::<&Located, (With<Item>, Without<Corpse>)>();
            q.iter(&world).filter(|l| rooms.contains(&l.0)).count()
        };
        assert_eq!(floor_before - floor_after, expected.len());
    }

    /// A !TAKE fixture on the floor stays put; a takeable item beside it
    /// is picked up.
    #[test]
    fn scavenger_leaves_untakeable_items_alone() {
        crate::mob_ai::force_scavenge_roll(Some(true));
        let mut world = World::new();
        world.insert_resource(TickCount(SCAVENGER_PERIOD_TICKS));
        let room = make_room(&mut world);
        let mob = make_mob(&mut world, room);
        world
            .entity_mut(mob)
            .insert(MobBehaviors(vec![MobBehavior::Scavenger]));
        let named = |n: &str| Named { name: n.into() };
        let statue = world
            .spawn((
                Item,
                named("a marble statue"),
                Located(room),
                ObjectRestrictions(vec![ObjectRestriction::NoTake]),
            ))
            .id();
        scavenger_tick(&mut world);
        assert_eq!(world.get::<Located>(statue).map(|l| l.0), Some(room));

        let coin = world.spawn((Item, named("a coin"), Located(room))).id();
        scavenger_tick(&mut world);
        assert_eq!(world.get::<Located>(coin).map(|l| l.0), Some(mob));
        assert_eq!(world.get::<Located>(statue).map(|l| l.0), Some(room));
    }

    /// Nothing in a player's house is loot: neither items lying in a house
    /// room (a follower of its owner can stand there) nor placed house
    /// items that somehow lie on an ordinary floor.
    #[test]
    fn scavenger_leaves_player_houses_alone() {
        crate::mob_ai::force_scavenge_roll(Some(true));
        let mut world = World::new();
        world.insert_resource(TickCount(SCAVENGER_PERIOD_TICKS));
        let house = make_room(&mut world);
        world.entity_mut(house).insert(mud_world::HouseRoom {
            house_id: 1,
            local_index: 0,
        });
        let street = make_room(&mut world);
        let named = |n: &str| Named { name: n.into() };
        let pet = make_mob(&mut world, house);
        let thief = make_mob(&mut world, street);
        for m in [pet, thief] {
            world
                .entity_mut(m)
                .insert(MobBehaviors(vec![MobBehavior::Scavenger]));
        }
        let vase = world.spawn((Item, named("a vase"), Located(house))).id();
        let placed = world
            .spawn((
                Item,
                named("a lamp"),
                mud_world::HouseItem(7),
                Located(street),
            ))
            .id();
        scavenger_tick(&mut world);
        assert_eq!(world.get::<Located>(vase).map(|l| l.0), Some(house));
        assert_eq!(world.get::<Located>(placed).map(|l| l.0), Some(street));
        // An ordinary item beside them is still fair game.
        let coin = world.spawn((Item, named("a coin"), Located(street))).id();
        scavenger_tick(&mut world);
        assert_eq!(world.get::<Located>(coin).map(|l| l.0), Some(thief));
    }

    /// Timing guard at prod scale. Prod saw 138-223 ms per pass with the
    /// old per-mob item scan; the indexed pass must stay far below the
    /// 100 ms slow-tick threshold. Hard limit only enforced in release.
    #[test]
    fn scavenger_tick_prod_scale_is_fast() {
        crate::mob_ai::force_scavenge_roll(Some(true));
        let (mut world, ..) = scavenger_world();
        let start = std::time::Instant::now();
        scavenger_tick(&mut world);
        let first = start.elapsed();
        // Steady state: nothing left for most scavengers to do.
        let start = std::time::Instant::now();
        scavenger_tick(&mut world);
        let second = start.elapsed();
        eprintln!("scavenger_tick 5000 mobs / 4000 items: first={first:?} second={second:?}");
        if !cfg!(debug_assertions) {
            assert!(first.as_millis() < 5, "first pass took {first:?}");
            assert!(second.as_millis() < 5, "second pass took {second:?}");
        }
    }

    /// Prod-scale wander scenario: a 100x100 torus of rooms (10k) with
    /// caves (dark), deep water and underwater rooms mixed in, 5500 mobs
    /// (a third sentinel), 4000 items (carried and on the floor), and no
    /// players, at night.
    fn wander_world() -> World {
        const SIDE: usize = 100;
        const MOBS: usize = 5500;
        const ITEMS: usize = 4000;
        let mut world = World::new();
        world.insert_resource(TickCount(WANDER_PERIOD_TICKS));
        // Worst case: night, when every outdoor wilderness room is dark.
        world.insert_resource(mud_world::MudClock {
            hour: 23,
            ..Default::default()
        });
        let rooms: Vec<Entity> = (0..SIDE * SIDE).map(|_| make_room(&mut world)).collect();
        for (i, &room) in rooms.iter().enumerate() {
            let (x, y) = (i % SIDE, i / SIDE);
            let at = |x: usize, y: usize| rooms[(y % SIDE) * SIDE + x % SIDE];
            let sector = match i % 20 {
                0..=2 => Sector::Cave,
                3 => Sector::Water,
                4 => Sector::Underwater,
                5 => Sector::Shallows,
                _ => Sector::Field,
            };
            world.entity_mut(room).insert(RoomSector(sector));
            for (dir, to) in [
                (Direction::North, at(x, y + SIDE - 1)),
                (Direction::South, at(x, y + 1)),
                (Direction::East, at(x + 1, y)),
                (Direction::West, at(x + SIDE - 1, y)),
            ] {
                if let Some(mut e) = world.get_mut::<mud_world::Exits>(room) {
                    e.0.insert(dir, open_exit(to));
                } else {
                    let mut exits = mud_world::Exits::default();
                    exits.0.insert(dir, open_exit(to));
                    world.entity_mut(room).insert(exits);
                }
            }
        }
        let mut mobs = Vec::with_capacity(MOBS);
        for i in 0..MOBS {
            let mob = make_mob(&mut world, rooms[(i * 7) % rooms.len()]);
            if i % 3 == 0 {
                world
                    .entity_mut(mob)
                    .insert(MobBehaviors(vec![MobBehavior::Sentinel]));
            }
            scatter_archetype(&mut world, mob, i);
            mobs.push(mob);
        }
        for i in 0..ITEMS {
            let holder = if i % 2 == 0 {
                mobs[i % MOBS]
            } else {
                rooms[(i * 13) % rooms.len()]
            };
            let item = world
                .spawn((
                    Item,
                    Named {
                        name: format!("item {i}"),
                    },
                    Located(holder),
                ))
                .id();
            scatter_archetype(&mut world, item, i);
        }
        world
    }

    fn open_exit(to: Entity) -> ExitData {
        ExitData {
            to: Some(to),
            state: mud_db::enums::ExitState::Open,
            key: None,
            description: None,
            keywords: Vec::new(),
            is_hidden: false,
            is_pickproof: false,
            is_bashable: false,
            hit_points: None,
        }
    }

    /// Timing guard at prod scale (5500 mobs, 4000 items, 10k rooms, no
    /// players). Prod saw 600-700 ms per pass when every move scanned the
    /// whole item table for light sources and re-built player queries for
    /// observers that cannot exist. Hard limit only enforced in release.
    #[test]
    fn wander_tick_prod_scale_is_fast() {
        let mut world = wander_world();
        let start = std::time::Instant::now();
        wander_tick(&mut world);
        let first = start.elapsed();
        let start = std::time::Instant::now();
        wander_tick(&mut world);
        let second = start.elapsed();
        eprintln!(
            "wander_tick 5500 mobs / 4000 items / 10k rooms: first={first:?} second={second:?}"
        );
        if !cfg!(debug_assertions) {
            assert!(first.as_millis() < 20, "first pass took {first:?}");
            assert!(second.as_millis() < 20, "second pass took {second:?}");
        }
    }

    /// Run the wander tick enough times that an unblocked mob would
    /// certainly have stepped through its only exit; report where it ended.
    fn wander_many(world: &mut World, mob: Entity) -> Option<Entity> {
        world.insert_resource(TickCount(WANDER_PERIOD_TICKS));
        for _ in 0..200 {
            wander_tick(world);
        }
        world.get::<Located>(mob).map(|l| l.0)
    }

    /// A pet (mob following a player) stays put, even when its leader is
    /// elsewhere; an ordinary mob in the same setup still wanders.
    #[test]
    fn pet_does_not_wander_but_ordinary_mob_does() {
        let mut world = World::new();
        let from = make_room(&mut world);
        let to = make_room(&mut world);
        link(&mut world, from, Direction::North, to);
        let owner = world.spawn((mud_world::Player, Located(to))).id();
        let pet = make_mob(&mut world, from);
        world.entity_mut(pet).insert(Follower(owner));
        assert_eq!(wander_many(&mut world, pet), Some(from), "pet wandered");

        let ordinary = make_mob(&mut world, from);
        assert_eq!(wander_many(&mut world, ordinary), Some(to));
    }

    /// A mob following a (non-player) leader in the same room stays; once
    /// the leader is in another room it is free to wander.
    #[test]
    fn follower_with_leader_in_room_stays() {
        let mut world = World::new();
        let from = make_room(&mut world);
        let to = make_room(&mut world);
        link(&mut world, from, Direction::North, to);
        // Sentinel leader so only the follower's own gate is under test.
        let leader = make_mob(&mut world, from);
        world
            .entity_mut(leader)
            .insert(MobBehaviors(vec![MobBehavior::Sentinel]));
        let follower = make_mob(&mut world, from);
        world.entity_mut(follower).insert(Follower(leader));
        assert_eq!(wander_many(&mut world, follower), Some(from));

        world.entity_mut(leader).insert(Located(to));
        assert_eq!(wander_many(&mut world, follower), Some(to));
    }

    /// Legacy mobs walk through the same deep-water gate as players: a plain
    /// mob never wanders into a `Sector::Water` room, but an AQUATIC one, a
    /// flier and a waterwalker do.
    #[test]
    fn deep_water_blocks_wandering_unless_the_mob_can_cross() {
        let mut world = World::new();
        let from = make_room(&mut world);
        let lake = make_room(&mut world);
        world.entity_mut(lake).insert(RoomSector(Sector::Water));
        link(&mut world, from, Direction::North, lake);

        let plain = make_mob(&mut world, from);
        assert_eq!(wander_many(&mut world, plain), Some(from), "plain mob");

        let fish = make_mob(&mut world, from);
        world
            .entity_mut(fish)
            .insert(MobTraits(vec![MobTrait::Aquatic]));
        assert_eq!(wander_many(&mut world, fish), Some(lake), "aquatic");

        let bird = make_mob(&mut world, from);
        world.entity_mut(bird).insert(mud_world::Flying);
        assert_eq!(wander_many(&mut world, bird), Some(lake), "flying");

        let walker = make_mob(&mut world, from);
        world.entity_mut(walker).insert(mud_world::WaterWalk);
        assert_eq!(wander_many(&mut world, walker), Some(lake), "waterwalk");
    }
}
