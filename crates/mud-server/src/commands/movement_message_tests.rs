//! Movement broadcast wording (issue #52): legacy `do_simple_move` says
//! "X leaves north." / "X arrives from the south." (up and down read
//! "from below" / "from above"), and exit-keyword alternatives such as
//! "gate metal" only ever show their first word. Test-only.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, UserRole};
use mud_world::{Account, ExitData, Exits, Named, Room};

use crate::commands::test_support::{Rx, drain, player_in};
use crate::commands::{arrival_from, cmd_move, exit_noun_phrase};

fn exit(to: Option<Entity>, state: ExitState, keywords: &[&str]) -> ExitData {
    ExitData {
        to,
        state,
        key: None,
        description: None,
        keywords: keywords.iter().map(|s| (*s).to_string()).collect(),
        is_hidden: false,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    }
}

fn account() -> Account {
    Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    }
}

/// Mover "Tester" in room A, watcher "Bob" in A, watcher "Cara" in B,
/// joined by one exit `dir` (state/keywords as given).
fn setup(dir: Direction, state: ExitState, keywords: &[&str]) -> (World, Entity, Rx, Rx, Rx) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    let a = world.spawn((Room, Exits::default())).id();
    let b = world.spawn((Room, Exits::default())).id();
    world.entity_mut(a).insert(Exits(HashMap::from([(
        dir,
        exit(Some(b), state, keywords),
    )])));
    let (mover, mrx) = player_in(&mut world, a);
    world.entity_mut(mover).insert(account());
    let (bob, brx) = player_in(&mut world, a);
    world
        .entity_mut(bob)
        .insert((Named { name: "Bob".into() }, account()));
    let (cara, crx) = player_in(&mut world, b);
    world.entity_mut(cara).insert((
        Named {
            name: "Cara".into(),
        },
        account(),
    ));
    (world, mover, mrx, brx, crx)
}

fn assert_walk(dir: Direction, leaves: &str, arrives: &str) {
    let (mut world, mover, _mrx, mut brx, mut crx) = setup(dir, ExitState::Open, &[]);
    cmd_move(&mut world, mover, dir);
    let left = drain(&mut brx);
    assert!(
        left.contains(&format!("Tester leaves {leaves}.\r\n")),
        "{left:?}"
    );
    let came = drain(&mut crx);
    assert!(
        came.contains(&format!("Tester arrives from {arrives}.\r\n")),
        "{came:?}"
    );
}

#[test]
fn cardinal_moves_read_leaves_dir_and_arrives_from_the_opposite() {
    assert_walk(Direction::North, "north", "the south");
    assert_walk(Direction::South, "south", "the north");
    assert_walk(Direction::East, "east", "the west");
    assert_walk(Direction::West, "west", "the east");
}

#[test]
fn diagonal_moves_arrive_from_the_opposite_corner() {
    assert_walk(Direction::Northeast, "northeast", "the southwest");
    assert_walk(Direction::Northwest, "northwest", "the southeast");
    assert_walk(Direction::Southeast, "southeast", "the northwest");
    assert_walk(Direction::Southwest, "southwest", "the northeast");
}

#[test]
fn going_up_arrives_from_below_and_going_down_from_above() {
    assert_walk(Direction::Up, "up", "below");
    assert_walk(Direction::Down, "down", "above");
}

#[test]
fn arrival_phrase_never_leaks_a_debug_direction_name() {
    for d in [
        Direction::North,
        Direction::Up,
        Direction::Down,
        Direction::In,
        Direction::Out,
        Direction::Portal,
    ] {
        let p = arrival_from(d);
        assert_eq!(p, p.to_lowercase(), "{d:?} -> {p}");
        assert!(!p.contains("the up") && !p.contains("the down"), "{p}");
    }
    assert_eq!(arrival_from(Direction::Portal), "nearby");
}

#[test]
fn closed_exit_names_only_the_first_keyword_alternative() {
    // Issue #52: keyword "gate metal" must read "The gate", not
    // "The gate metal".
    let (mut world, mover, mut mrx, _b, _c) =
        setup(Direction::North, ExitState::Closed, &["gate metal"]);
    cmd_move(&mut world, mover, Direction::North);
    assert_eq!(drain(&mut mrx), "The gate is closed.\r\n");
}

#[test]
fn locked_exit_uses_first_word_of_first_keyword() {
    let (mut world, mover, mut mrx, _b, _c) =
        setup(Direction::Down, ExitState::Locked, &["grate iron", "other"]);
    cmd_move(&mut world, mover, Direction::Down);
    assert_eq!(drain(&mut mrx), "The grate is locked.\r\n");
}

#[test]
fn noun_phrase_skips_blank_entries_and_falls_back_to_the_way() {
    let ed = exit(None, ExitState::Closed, &["  ", "curtain beads"]);
    assert_eq!(exit_noun_phrase(&ed), "The curtain");
    let plain = exit(None, ExitState::Closed, &[]);
    assert_eq!(exit_noun_phrase(&plain), "The way");
}

/// Legacy `do_simple_move` only drags along followers that are not
/// fighting; a fighting follower stays behind (and keeps its fight).
#[test]
fn a_fighting_follower_stays_behind_when_the_leader_moves() {
    let (mut world, leader, _mrx, _brx, _crx) = setup(Direction::North, ExitState::Open, &[]);
    let from = world.get::<mud_world::Located>(leader).unwrap().0;
    let (idle, _irx) = player_in(&mut world, from);
    let (fighter, _frx) = player_in(&mut world, from);
    let foe = world.spawn(mud_world::Located(from)).id();
    for f in [idle, fighter] {
        world
            .entity_mut(f)
            .insert((account(), mud_world::Follower(leader)));
    }
    world.entity_mut(fighter).insert(mud_world::Fighting(foe));
    cmd_move(&mut world, leader, Direction::North);
    let to = world.get::<mud_world::Located>(leader).unwrap().0;
    assert_ne!(to, from);
    assert_eq!(world.get::<mud_world::Located>(idle).unwrap().0, to);
    assert_eq!(world.get::<mud_world::Located>(fighter).unwrap().0, from);
    assert_eq!(
        world.get::<mud_world::Fighting>(fighter).map(|f| f.0),
        Some(foe)
    );
}

/// Legacy `do_simple_move` also requires `!CASTING(follower)`: a follower
/// mid-cast stays behind (and keeps its wind-up).
#[test]
fn a_casting_follower_stays_behind_when_the_leader_moves() {
    let (mut world, leader, _mrx, _brx, _crx) = setup(Direction::North, ExitState::Open, &[]);
    let from = world.get::<mud_world::Located>(leader).unwrap().0;
    let (idle, _irx) = player_in(&mut world, from);
    let (caster, _crx2) = player_in(&mut world, from);
    for f in [idle, caster] {
        world
            .entity_mut(f)
            .insert((account(), mud_world::Follower(leader)));
    }
    world.entity_mut(caster).insert(mud_world::Casting {
        ability_id: 1,
        ability_name: "Fireball".to_string(),
        args: String::new(),
        kind_label: "spell".to_string(),
        verb: "cast".to_string(),
        ticks_remaining: 8,
        ticks_total: 8,
        target: mud_world::CastTarget::Area,
        recognized_by: Vec::new(),
        slot_reservation: None,
    });
    cmd_move(&mut world, leader, Direction::North);
    let to = world.get::<mud_world::Located>(leader).unwrap().0;
    assert_ne!(to, from);
    assert_eq!(world.get::<mud_world::Located>(idle).unwrap().0, to);
    assert_eq!(world.get::<mud_world::Located>(caster).unwrap().0, from);
    assert!(world.get::<mud_world::Casting>(caster).is_some());
}

/// Legacy `do_simple_move` requires `GET_POS(follower) >= POS_STANDING`:
/// sitting, resting and sleeping followers stay behind.
#[test]
fn a_non_standing_follower_stays_behind_when_the_leader_moves() {
    use mud_world::{Posture, PostureKind};
    let (mut world, leader, _mrx, _brx, _crx) = setup(Direction::North, ExitState::Open, &[]);
    let from = world.get::<mud_world::Located>(leader).unwrap().0;
    let (standing, _srx) = player_in(&mut world, from);
    let mut sitters = Vec::new();
    for kind in [
        PostureKind::Sitting,
        PostureKind::Resting,
        PostureKind::Sleeping,
    ] {
        let (f, rx) = player_in(&mut world, from);
        world
            .entity_mut(f)
            .insert((account(), mud_world::Follower(leader), Posture(kind)));
        sitters.push((f, rx));
    }
    world
        .entity_mut(standing)
        .insert((account(), mud_world::Follower(leader)));
    cmd_move(&mut world, leader, Direction::North);
    let to = world.get::<mud_world::Located>(leader).unwrap().0;
    assert_ne!(to, from);
    assert_eq!(world.get::<mud_world::Located>(standing).unwrap().0, to);
    for (f, _) in &sitters {
        assert_eq!(world.get::<mud_world::Located>(*f).unwrap().0, from);
    }
}
