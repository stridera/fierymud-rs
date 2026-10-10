//! Flight: falling, falling to the ground, and the overweight-flier rule.
//!
//! Ports legacy `movement.cpp` (`gravity_event`, `falling_check`,
//! `gravity_assisted_landing`, `falling_yell`, `too_heavy_to_fly`) and the
//! `falltoground` / `overweight` events of `events.cpp`.
//!
//! Legacy flies by position (`POS_FLYING`); here the `Flying` marker is the
//! position. A fall is a [`Falling`] component (the `EVENT_GRAVITY` event):
//! it is inserted when an unsupported actor enters an air room
//! ([`mud_world::movement::begin_fall_if_unsupported`]) or when flight is
//! lost ([`on_flight_lost`], the `land` command, overweight), and
//! [`gravity_tick`] steps only the entities carrying it. Nothing scans the
//! world each tick.
//!
//! Data note: the landing-damage formula, the 95% flight-load limit and the
//! Safefall ability id are runtime invariants ported from legacy. Per-room
//! fall damage would need a builder-editable column that does not exist.

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, Sector};
use mud_world::{
    AppliedTo, EffectInstance, EffectSource, Exits, Falling, Flying, Ghost, Health, KnownAbilities,
    Located, MovementModeTag, ObjectPrototypes, Player, Posture, PostureKind, RoomSector, Sized,
    effective_level,
};

use crate::TickCount;
use crate::commands::{
    Prevent, apply_damage, broadcast_room_except_rendered, cap_sentence_start, carried_weight,
    carry_capacity, direction_name, effect_prevents, has_effect_named, name_of, opposite, send_to,
    try_insert, try_remove,
};

/// Legacy `LVL_IMMORT`: gods neither fall nor feel weight.
const LVL_IMMORT: i32 = 100;
/// Legacy `MAXIMUM_FLIGHT_LOAD`: `950 * CAN_CARRY_W / 1000`.
const FLIGHT_LOAD_FRACTION: f64 = 0.95;
/// Ticks between steps of a fall (legacy: 2 pulses, 4 under feather fall).
const FALL_STEP_TICKS: u64 = 2;
/// A fall that has dropped this many rooms ends where it is. The start-room
/// check only catches a loop back to the start; a cycle that skips it
/// (A -> B -> C -> B) would otherwise fall forever.
const MAX_FALL_ROOMS: u32 = 50;
const FEATHER_FALL_STEP_TICKS: u64 = 4;
/// Order legacy walks exits in (`falling_yell`).
const DIRECTIONS: [Direction; 10] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
    Direction::Up,
    Direction::Down,
    Direction::Northeast,
    Direction::Northwest,
    Direction::Southeast,
    Direction::Southwest,
];

fn sector_of(world: &World, room: Entity) -> Option<Sector> {
    world.get::<RoomSector>(room).map(|s| s.0)
}

fn is_air(world: &World, room: Entity) -> bool {
    sector_of(world, room) == Some(Sector::Air)
}

/// Legacy `IS_WATER`.
fn is_water(world: &World, room: Entity) -> bool {
    matches!(
        sector_of(world, room),
        Some(Sector::Shallows | Sector::Water | Sector::Underwater)
    )
}

/// Legacy `IS_SPLASHY`.
fn is_splashy(world: &World, room: Entity) -> bool {
    matches!(
        sector_of(world, room),
        Some(Sector::Shallows | Sector::Water | Sector::Swamp)
    )
}

/// Held aloft right now: the `Flying` marker, or a proto that flies.
fn is_held_aloft(world: &World, e: Entity) -> bool {
    world.get::<Flying>(e).is_some()
        || world
            .get::<MovementModeTag>(e)
            .is_some_and(|m| m.0 == mud_db::enums::MovementMode::Flying)
}

fn has_feather_fall(world: &mut World, e: Entity) -> bool {
    ["featherfall", "feather_fall", "feather fall"]
        .iter()
        .any(|n| has_effect_named(world, e, n))
}

/// Knows the monk Safefall skill (legacy `SKILL_SAFEFALL`), whose id is
/// resolved by name into `CoreAbilities`.
fn safefall_skill(world: &World, e: Entity) -> bool {
    let Some(safefall) = world
        .get_resource::<mud_world::CoreAbilities>()
        .and_then(|c| c.safefall)
    else {
        return false;
    };
    world
        .get::<KnownAbilities>(e)
        .is_some_and(|k| k.entries.iter().any(|(id, p, _)| *id == safefall && *p > 0))
}

fn gender_of(world: &World, e: Entity) -> String {
    if let Some(p) = world.get::<mud_world::Profile>(e) {
        return p.gender.to_ascii_lowercase();
    }
    world
        .get::<mud_world::WorldKey>(e)
        .and_then(|k| {
            world
                .get_resource::<mud_world::MobPrototypes>()
                .and_then(|p| p.by_key.get(&(k.zone, k.id)))
        })
        .map(|p| p.gender.to_ascii_lowercase())
        .unwrap_or_default()
}

fn reflexive(world: &World, e: Entity) -> &'static str {
    match gender_of(world, e).as_str() {
        "male" => "himself",
        "female" => "herself",
        _ => "itself",
    }
}

pub(crate) fn possessive(world: &World, e: Entity) -> &'static str {
    match gender_of(world, e).as_str() {
        "male" => "his",
        "female" => "her",
        _ => "its",
    }
}

/// Legacy `too_heavy_to_fly`: carrying (inventory plus worn gear) more than
/// 95% of the carry capacity. Gods and the empty-handed are never too heavy.
pub(crate) fn too_heavy_to_fly(world: &mut World, e: Entity) -> bool {
    if effective_level(world, e) >= LVL_IMMORT || !world.contains_resource::<ObjectPrototypes>() {
        return false;
    }
    let carried = carried_weight(world, e);
    let cap = carry_capacity(world, e);
    if carried < 1.0 || cap < 1.0 {
        return false;
    }
    carried > FLIGHT_LOAD_FRACTION * cap
}

/// Is `e` standing in `room` after its flight ended? The ordinary-room half of
/// losing flight: legacy `falltoground_event` (flight lost) and the
/// non-air branch of `overweight_event`.
fn drop_to_ground(world: &mut World, e: Entity, room: Entity, overweight: bool) {
    let name = cap_sentence_start(&name_of(world, e));
    let (to_char, to_room) = if is_splashy(world, room) {
        (
            "You fall into the water with a splash!\r\n",
            format!("{name} falls into the water with a splash.\r\n"),
        )
    } else if overweight {
        (
            "You fall down!\r\n",
            format!("{name} falls to the ground!\r\n"),
        )
    } else {
        (
            "You fall to the ground.\r\n",
            format!("{name} falls to the ground.\r\n"),
        )
    };
    send_to(world, e, to_char);
    broadcast_room_except_rendered(world, room, &[e], &to_room);
    if overweight
        && matches!(
            world.get::<Posture>(e).map(|p| p.0),
            None | Some(PostureKind::Standing)
        )
    {
        try_insert(world, e, Posture(PostureKind::Sitting));
    }
}

/// The `Flying` marker was just removed by an effect ending (expiry, dispel,
/// a fly item taken off) and nothing else holds `e` up. In an air room it
/// starts to fall; anywhere else it drops to the ground (legacy
/// `EVENT_FALLTOGROUND`). Callers check the marker was actually present.
pub(crate) fn on_flight_lost(world: &mut World, e: Entity) {
    if is_held_aloft(world, e)
        || world.get::<Ghost>(e).is_some()
        || world.get::<Health>(e).is_some_and(|h| h.hp <= 0)
    {
        return;
    }
    let Some(room) = world.get::<Located>(e).map(|l| l.0) else {
        return;
    };
    if is_air(world, room) {
        mud_world::movement::begin_fall_if_unsupported(world, e);
    } else {
        drop_to_ground(world, e, room, false);
    }
}

/// Legacy `overweight_check` + `overweight_event`: a flier who is too heavy
/// to fly is grounded (or starts falling over an air room). Cheap when
/// `e` is not flying.
pub(crate) fn check_overweight(world: &mut World, e: Entity) {
    if world.get::<Flying>(e).is_none() || !too_heavy_to_fly(world, e) {
        return;
    }
    let spell_backed = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world).any(|(i, a)| {
            a.0 == e && i.name.eq_ignore_ascii_case("fly") && i.source == EffectSource::Spell
        })
    };
    send_to(
        world,
        e,
        if spell_backed {
            "The spell supporting you falters, unable to bear your weight!\r\n"
        } else {
            "You cannot fly with so much weight!\r\n"
        },
    );
    try_remove::<Flying>(world, e);
    let Some(room) = world.get::<Located>(e).map(|l| l.0) else {
        return;
    };
    if is_air(world, room) {
        mud_world::movement::begin_fall_if_unsupported(world, e);
    } else {
        drop_to_ground(world, e, room, true);
    }
}

/// Post-command hook: the commanding actor's load may have changed (`get`,
/// `buy`, loot). One component lookup for everyone who is not flying.
pub(crate) fn after_command(world: &mut World, actor: Entity) {
    check_overweight(world, actor);
}

/// A fly effect was just applied to `target` and installed the marker. If
/// the target is too heavy the effect stays but the marker goes (legacy
/// `SPELL_FLY`: "You feel somewhat lighter." / "$N remains earthbound.").
pub(crate) fn refuse_heavy_flier(world: &mut World, caster: Entity, target: Entity) {
    if world.get::<Flying>(target).is_none() || !too_heavy_to_fly(world, target) {
        return;
    }
    try_remove::<Flying>(world, target);
    send_to(world, target, "You feel somewhat lighter.\r\n");
    if caster != target {
        let n = cap_sentence_start(&name_of(world, target));
        send_to(world, caster, format!("{n} remains earthbound.\r\n"));
    }
}

/// Legacy `falling_yell`: the surprised yell of someone who starts to fall
/// carries into every adjoining room.
fn falling_yell(world: &mut World, e: Entity, room: Entity) {
    if effect_prevents(world, e, Prevent::Speaking) {
        return;
    }
    let Some(exits) = world.get::<Exits>(room).cloned() else {
        return;
    };
    for dir in DIRECTIONS {
        let Some(other) = exits
            .0
            .get(&dir)
            .filter(|x| x.state == ExitState::Open)
            .and_then(|x| x.to)
        else {
            continue;
        };
        // Which way does the other room's exit back lead? Prefer the exit
        // opposite `dir` when it returns here, else any exit that does.
        let back_exits = world.get::<Exits>(other).cloned().unwrap_or_default();
        let opp = opposite(dir).unwrap_or(dir);
        let back = if back_exits.0.get(&opp).is_some_and(|x| x.to == Some(room)) {
            opp
        } else {
            DIRECTIONS
                .iter()
                .copied()
                .find(|d| back_exits.0.get(d).is_some_and(|x| x.to == Some(room)))
                .unwrap_or(opp)
        };
        let from = match back {
            Direction::Down => "below".to_string(),
            // "<person> falls screaming from above" covers this one.
            Direction::Up => continue,
            d => format!("the {}", direction_name(d)),
        };
        let adj = if rand::random_range(0..=10) < 5 {
            "surprised"
        } else {
            "sudden"
        };
        let noun = if rand::random_range(0..=10) < 6 {
            "shriek"
        } else {
            "yelp"
        };
        broadcast_room_except_rendered(
            world,
            other,
            &[],
            &format!("You hear a {adj} {noun} from {from}, which quickly fades.\r\n"),
        );
    }
}

/// Drive every due fall one step. Only entities carrying [`Falling`] are
/// visited, so this costs nothing while nobody falls.
pub fn gravity_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    let due: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Falling)>();
        q.iter(world)
            .filter(|(_, f)| f.due_tick <= tick)
            .map(|(e, _)| e)
            .collect()
    };
    for e in due {
        fall_step(world, e, tick);
    }
    item_gravity_tick(world, tick);
}

/// An item (a corpse, a dropped weapon, a thrown pouch) dropping through air
/// rooms: legacy `start_obj_falling` / `gravity_event` for objects.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct ItemFalling {
    start_room: Entity,
    distance: u32,
    due_tick: u64,
}

/// How often the world is scanned for items lying in air rooms. Legacy hooked
/// `obj_to_room`; here the scan only touches items when an air room exists,
/// and a drop waits at most this many ticks before it plummets.
const ITEM_SCAN_TICKS: u64 = 5;

fn item_can_fall(world: &World, item: Entity) -> bool {
    !world
        .get::<mud_world::ObjectFlags>(item)
        .is_some_and(|f| f.has(mud_db::enums::ObjectFlag::NoFall))
}

/// Start and advance item falls. Items lying loose in an air room with a
/// way down start to fall (unless flagged `NoFall`); every item already
/// falling steps once its delay has passed.
fn item_gravity_tick(world: &mut World, tick: u64) {
    if tick.is_multiple_of(ITEM_SCAN_TICKS) {
        let air_rooms: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &RoomSector), With<mud_world::Room>>();
            q.iter(world)
                .filter(|(_, s)| s.0 == Sector::Air)
                .map(|(e, _)| e)
                .collect()
        };
        if !air_rooms.is_empty() {
            let starters: Vec<(Entity, Entity)> = {
                let mut q = world
                    .query_filtered::<(Entity, &Located), (With<mud_world::Item>, Without<ItemFalling>)>();
                q.iter(world)
                    .filter(|(_, l)| air_rooms.contains(&l.0))
                    .map(|(e, l)| (e, l.0))
                    .collect()
            };
            for (item, room) in starters {
                if item_can_fall(world, item) && room_below(world, room).is_some() {
                    try_insert(
                        world,
                        item,
                        ItemFalling {
                            start_room: room,
                            distance: 0,
                            due_tick: 0,
                        },
                    );
                }
            }
        }
    }
    let due: Vec<Entity> = {
        let mut q = world.query::<(Entity, &ItemFalling)>();
        q.iter(world)
            .filter(|(_, f)| f.due_tick <= tick)
            .map(|(e, _)| e)
            .collect()
    };
    for item in due {
        item_fall_step(world, item, tick);
    }
}

/// One step of legacy `gravity_event` for an object.
fn item_fall_step(world: &mut World, item: Entity, tick: u64) {
    let Some(mut fall) = world.get::<ItemFalling>(item).copied() else {
        return;
    };
    let stop = |world: &mut World| try_remove::<ItemFalling>(world, item);
    let Some(room) = world.get::<Located>(item).map(|l| l.0) else {
        return stop(world);
    };
    // Picked up (the holder is not an air room) or no longer over thin air.
    if !is_air(world, room) || !item_can_fall(world, item) {
        return stop(world);
    }
    let Some(to_room) = room_below(world, room) else {
        return stop(world);
    };
    let what = cap_sentence_start(&name_of(world, item));
    if fall.distance == 0 {
        broadcast_room_except_rendered(
            world,
            room,
            &[],
            &format!("{what} <red>plummets</> <green>downward!</>\r\n"),
        );
    }
    mud_world::movement::move_to_room(world, item, to_room);
    broadcast_room_except_rendered(
        world,
        to_room,
        &[],
        &format!("{what} <red>falls from above.</>\r\n"),
    );
    if to_room == fall.start_room {
        return stop(world);
    }
    fall.distance += 1;
    if fall.distance < MAX_FALL_ROOMS
        && is_air(world, to_room)
        && room_below(world, to_room).is_some()
    {
        fall.due_tick = tick + FALL_STEP_TICKS;
        try_insert(world, item, fall);
        return;
    }
    // The bottom.
    stop(world);
    let landing = if is_splashy(world, to_room) {
        "<red>lands with a loud</> <red>SPLASH</><green>!</>"
    } else {
        "<red>lands with a dull</> <red>THUD</><green>!</>"
    };
    broadcast_room_except_rendered(world, to_room, &[], &format!("{what} {landing}\r\n"));
}

/// The room below `room`, if the down exit is passable (legacy `CAN_GO`).
fn room_below(world: &World, room: Entity) -> Option<Entity> {
    world
        .get::<Exits>(room)?
        .0
        .get(&Direction::Down)
        .filter(|x| x.state == ExitState::Open)
        .and_then(|x| x.to)
}

/// The first step of a fall: the actor finds itself on thin air.
fn announce_fall_start(world: &mut World, e: Entity, room: Entity, feather: bool) {
    let who = cap_sentence_start(&name_of(world, e));
    let itself = reflexive(world, e);
    if feather {
        send_to(
            world,
            e,
            "<b:red>You find yourself in midair and begin descending.</>\r\n\r\n",
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[e],
            &format!("<b:red>{who} finds {itself} in midair and begins descending.</>\r\n"),
        );
    } else {
        send_to(
            world,
            e,
            "<b:red>You find yourself on thin air and fall</> <green>DOWN!</>\r\n\r\n",
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[e],
            &format!("<b:red>{who} finds {itself} on thin air and falls</> <green>DOWN!</>\r\n"),
        );
        falling_yell(world, e, room);
    }
}

/// One step of legacy `gravity_event`.
fn fall_step(world: &mut World, e: Entity, tick: u64) {
    let Some(mut fall) = world.get::<Falling>(e).copied() else {
        return;
    };
    let stop = |world: &mut World| try_remove::<Falling>(world, e);
    let Some(room) = world.get::<Located>(e).map(|l| l.0) else {
        return stop(world);
    };
    if !is_air(world, room)
        || world.get::<Ghost>(e).is_some()
        || is_held_aloft(world, e)
        || effective_level(world, e) >= LVL_IMMORT
    {
        return stop(world);
    }
    let Some(to_room) = room_below(world, room) else {
        return stop(world);
    };
    // A flying mount or rider holds the other up; otherwise they part.
    let mount = world.get::<mud_world::Mounted>(e).map(|m| m.0);
    let rider = world.get::<mud_world::RiddenBy>(e).map(|r| r.0);
    if mount.is_some_and(|m| is_held_aloft(world, m))
        || rider.is_some_and(|r| is_held_aloft(world, r))
    {
        return stop(world);
    }
    if let Some(m) = mount {
        try_remove::<mud_world::Mounted>(world, e);
        try_remove::<mud_world::RiddenBy>(world, m);
    }
    if let Some(r) = rider {
        try_remove::<mud_world::RiddenBy>(world, e);
        try_remove::<mud_world::Mounted>(world, r);
    }

    let feather = has_feather_fall(world, e);
    let who = cap_sentence_start(&name_of(world, e));
    let is_player = world.get::<Player>(e).is_some();

    if fall.distance == 0 {
        announce_fall_start(world, e, room, feather);
    }

    crate::combat::relocate(world, e, to_room);
    if is_player {
        crate::commands::broadcast_room_player_diff(world, room, e, "RemovePlayer");
        crate::commands::broadcast_room_player_diff(world, to_room, e, "AddPlayer");
        crate::commands::send_room_players_snapshot(world, e);
        crate::commands::mark_room_visited(world, e, to_room);
        crate::commands::note_room_entry(world, e, to_room);
    }

    let (to_char, to_room_msg) = if feather {
        (
            "\r\n<green>You float slowly downward.</>\r\n\r\n",
            format!("<green>{who} floats slowly down from above.</>\r\n"),
        )
    } else if safefall_skill(world, e) {
        (
            "\r\n<green>You fall gracefully DOWN!</>\r\n\r\n",
            format!("<green>{who} gracefully falls from above.</>\r\n"),
        )
    } else {
        (
            "\r\n<green>DOWN!</>\r\n\r\n",
            format!("<green>{who} falls screaming from above.</>\r\n"),
        )
    };
    send_to(world, e, to_char);
    broadcast_room_except_rendered(world, to_room, &[e], &to_room_msg);
    if is_player {
        crate::commands::cmd_look(world, e, "");
    }

    if to_room == fall.start_room {
        send_to(
            world,
            e,
            "\r\nParadoxically, you end up where you began.\r\n",
        );
        return stop(world);
    }

    fall.distance += 1;
    if fall.distance < MAX_FALL_ROOMS
        && is_air(world, to_room)
        && room_below(world, to_room).is_some()
    {
        fall.due_tick = tick
            + if feather {
                FEATHER_FALL_STEP_TICKS
            } else {
                FALL_STEP_TICKS
            };
        try_insert(world, e, fall);
        return;
    }

    // Nothing below (or the distance cap): this is the bottom.
    try_remove::<Falling>(world, e);
    land(world, e, to_room, fall.distance, feather);
}

/// Legacy `gravity_assisted_landing`.
fn land(world: &mut World, e: Entity, room: Entity, distance: u32, feather: bool) {
    let who = cap_sentence_start(&name_of(world, e));
    let water = is_water(world, room);
    if feather {
        let (to_char, to_room) = if water {
            (
                "\r\nYou come to rest above the surface of the water.\r\n",
                format!("{who} comes to rest above the surface of the water.\r\n"),
            )
        } else {
            (
                "\r\nYou come to rest just above the ground.\r\n",
                format!("{who}'s descent ends just above the ground.\r\n"),
            )
        };
        send_to(world, e, to_char);
        broadcast_room_except_rendered(world, room, &[e], &to_room);
        return;
    }

    let safefall = safefall_skill(world, e);
    let mut posture = PostureKind::Sitting;
    if water {
        send_to(
            world,
            e,
            "\r\nYou land with a tremendous <blue>SPLASH</><green>!</>\r\n",
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[e],
            &format!("{who} lands with a tremendous <blue>SPLASH</><green>!</>\r\n"),
        );
    } else if safefall && distance <= 5 {
        posture = PostureKind::Standing;
        send_to(
            world,
            e,
            "\r\nYou tuck and roll, performing a beautiful landing!\r\n",
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[e],
            &format!("{who} tucks and rolls, performing a beautiful landing!\r\n"),
        );
    } else if safefall && distance < 15 {
        send_to(
            world,
            e,
            "\r\nYou gracefully land without taking too much damage.\r\n",
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[e],
            &format!("{who} gracefully lands without taking too much damage.\r\n"),
        );
    } else {
        let splat = "<red>S</><green>P</><red>L</><green>A</><red>T</><green>!</>";
        send_to(
            world,
            e,
            format!("\r\nYou land with a resounding {splat}\r\n"),
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[e],
            &format!("{who} lands with a resounding {splat}\r\n"),
        );
    }
    try_insert(world, e, Posture(posture));

    let damage = fall_damage(
        distance,
        world.get::<Sized>(e).map_or(2, |s| s.0.rank()),
        world.get::<Health>(e).map_or(0, |h| h.max),
        water,
        safefall,
    );
    if damage > 0 {
        let (dead, msg) = apply_damage(world, e, damage);
        if dead {
            let name = name_of(world, e);
            crate::combat::handle_death(world, e, &name, room);
            return;
        }
        if let Some(m) = msg {
            send_to(world, e, m);
        }
    }
}

/// Legacy fall damage: `distance * (size + 1) / 50` of max HP, a quarter of
/// that into water; Safefall zeroes it up to 5 rooms and scales it by
/// `distance / 15` up to 15.
#[allow(clippy::cast_possible_truncation)]
fn fall_damage(distance: u32, size_rank: i32, max_hp: i32, water: bool, safefall: bool) -> i32 {
    let d = f64::from(distance);
    let mut damage = ((d * f64::from(size_rank + 1)) / 50.0 * f64::from(max_hp)) as i32;
    if water {
        damage /= 4;
    }
    if safefall {
        if distance <= 5 {
            damage = 0;
        } else if distance < 15 {
            damage = (f64::from(damage) * d / 15.0) as i32;
        }
    }
    damage
}

#[cfg(test)]
#[path = "flight_tests.rs"]
mod tests;
