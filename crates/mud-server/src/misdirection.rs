//! Misdirection (`EFF_MISDIRECTION`, legacy `act.movement.cpp`
//! `do_misdirected_move` / `do_move`, `mobact.cpp` `mob_movement`).
//!
//! While the `misdirection` status flag holds, a step can be followed by a
//! second direction: `north south` walks north but shows the room a figure
//! heading south. The false direction is acted out first (the "illusion"
//! leaves, nothing moves), then the real step is taken with its own leave
//! message withheld. `stay <dir>` fakes a departure without going anywhere,
//! `<dir> stay` walks off with no departure message at all. Staff of at
//! least the mover's level (and at least level 100) see through it: they get
//! "X's illusion leaves south" for the fake and the true leave message for
//! the real step. It is refused while riding or being ridden.
//!
//! A mob carrying the flag does it on its own whenever it wanders (see
//! [`crate::wander`]): one time in six it only pretends to leave, otherwise
//! it shows a random false direction and slips out unannounced.
//!
//! Not ported: legacy's confusion detour (the engine has no `confused`
//! behaviour yet) and the follower oddity where followers were dragged along
//! the false direction as well; followers here go where the leader really
//! goes.

use bevy_ecs::prelude::*;
use mud_db::enums::Direction;
use mud_world::{Located, Misdirecting, Misdirection, Mob, Player, Posture, PostureKind};

use crate::commands::{
    direction_name, may_start_move, name_of, parse_direction, race_movement_verb, room_has_light,
    room_is_dark, seen_name, sees_characters_in_dark, send_msg,
};

/// Legacy `LVL_IMMORT`: the lowest level that sees through the trick.
const LVL_IMMORT: i32 = 100;

/// The six directions legacy mobs pick a false direction from
/// (`NUM_OF_DIRS`).
pub(crate) const MOB_FAKE_DIRECTIONS: [Direction; 6] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
    Direction::Up,
    Direction::Down,
];

/// Legacy `SEES_THROUGH_MISDIRECTION`: an immortal at least as high as the
/// mover.
pub(crate) fn sees_through(world: &World, observer: Entity, mover: Entity) -> bool {
    let observer_level = mud_world::effective_level(world, observer);
    observer_level >= LVL_IMMORT && observer_level >= mud_world::effective_level(world, mover)
}

/// Players in `room` who must not be told about `mover`'s real departure:
/// everyone who cannot see through the misdirection.
pub(crate) fn fooled_observers(world: &World, room: Entity, mover: Entity) -> Vec<Entity> {
    crate::room_index::players_in(world, room)
        .into_iter()
        .filter(|&o| o != mover && !sees_through(world, o, mover))
        .collect()
}

/// Legacy `do_misdirected_move`: act out `actor` leaving `dir` without
/// moving. The actor sees "Your illusion leaves north."; awake bystanders
/// see the plain departure, staff who see through it see the illusion.
pub(crate) fn fake_move(world: &mut World, actor: Entity, dir: Direction) {
    let Some(room) = world.get::<Located>(actor).map(|l| l.0) else {
        return;
    };
    let verb = race_movement_verb(world, actor, false);
    let dir_name = direction_name(dir);
    if world.get::<Player>(actor).is_some() {
        send_msg(
            world,
            actor,
            format!("Your <magenta>illusion</> {verb} {dir_name}.\r\n"),
        );
    }
    let name = name_of(world, actor);
    let dark = room_is_dark(world, room) && !room_has_light(world, room);
    for observer in crate::room_index::player_listeners_in(world, room) {
        if observer == actor
            || world
                .get::<Posture>(observer)
                .is_some_and(|p| p.0 == PostureKind::Sleeping)
        {
            continue;
        }
        let who = if dark && !sees_characters_in_dark(world, observer) {
            "someone".to_string()
        } else {
            seen_name(world, observer, actor, &name)
        };
        let line = if sees_through(world, observer, actor) {
            format!("{who}'s <magenta>illusion</> {verb} {dir_name}.\r\n")
        } else {
            format!("{who} {verb} {dir_name}.\r\n")
        };
        send_msg(world, observer, line);
    }
}

/// A direction command with trailing text (`north south`). Without the
/// `misdirection` flag the text is ignored, as for any other step.
pub(crate) fn cmd_move_args(world: &mut World, player: Entity, dir: Direction, args: &str) {
    match args.split_whitespace().next() {
        Some(arg) if world.get::<Misdirection>(player).is_some() => {
            misdirect(world, player, Some(dir), arg);
        }
        _ => crate::commands::cmd_move(world, player, dir),
    }
}

/// `stay` / `stay <dir>`: with misdirection the second form fakes a
/// departure toward `<dir>`; otherwise it only says so.
pub(crate) fn cmd_stay(world: &mut World, player: Entity, args: &str) {
    match args.split_whitespace().next() {
        Some(arg) if world.get::<Misdirection>(player).is_some() => {
            misdirect(world, player, None, arg);
        }
        _ => send_msg(world, player, "Ok, you stay put.\r\n"),
    }
}

/// Legacy `do_move` with the misdirection argument. `real` is the direction
/// actually walked (`None` for `stay`); `arg` names the false one, or
/// `stay*` for none.
fn misdirect(world: &mut World, player: Entity, real: Option<Direction>, arg: &str) {
    if world.get::<mud_world::Mounted>(player).is_some()
        || world.get::<mud_world::RiddenBy>(player).is_some()
    {
        send_msg(
            world,
            player,
            "You can't misdirect people while riding.\r\n",
        );
        return;
    }
    let fake = if arg.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("st")) {
        None
    } else if let Some(dir) = parse_direction(arg) {
        Some(dir)
    } else {
        send_msg(world, player, "That isn't a direction.\r\n");
        return;
    };
    // A step that cannot be taken at all (asleep, held, fighting) shows
    // nothing either. `stay` is no step and needs no such gate.
    if real.is_some() && !may_start_move(world, player) {
        return;
    }
    if let Some(dir) = fake {
        fake_move(world, player, dir);
    }
    if let Some(dir) = real {
        world.entity_mut(player).insert(Misdirecting);
        crate::commands::cmd_move(world, player, dir);
        if let Ok(mut e) = world.get_entity_mut(player) {
            e.remove::<Misdirecting>();
        }
    }
}

/// How a wandering mob's step goes (legacy `mob_movement`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MobStep {
    /// No misdirection: leave normally.
    Normal,
    /// Misdirecting mob that only pretended to leave toward `dir`; it stays.
    Pretend,
    /// Misdirecting mob that showed a random false direction and now leaves
    /// without a departure message (staff who see through it excepted).
    Silent,
}

/// Act out a misdirecting mob's wander step toward `dir`: one time in six
/// it only pretends to leave, otherwise it shows a random false direction.
/// The caller moves the mob for [`MobStep::Silent`], keeping
/// [`fooled_observers`] out of the leave message.
pub(crate) fn mob_step(world: &mut World, mob: Entity, dir: Direction) -> MobStep {
    if world.get::<Mob>(mob).is_none() || world.get::<Misdirection>(mob).is_none() {
        return MobStep::Normal;
    }
    if rand::random_range(1..=MOB_FAKE_DIRECTIONS.len()) == 1 {
        fake_move(world, mob, dir);
        MobStep::Pretend
    } else {
        let fake = MOB_FAKE_DIRECTIONS[rand::random_range(0..MOB_FAKE_DIRECTIONS.len())];
        fake_move(world, mob, fake);
        MobStep::Silent
    }
}
