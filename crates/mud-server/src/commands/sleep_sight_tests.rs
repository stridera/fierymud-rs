//! A sleeper neither sees room activity nor can look around (issue #178):
//! legacy `act()` hands room-wide lines only to awake recipients, and
//! `look`, `scan`, `exits` ... need `STANCE_RESTING`. Test-only.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, UserRole};
use mud_world::{Account, ExitData, Exits, Named, Posture, PostureKind, Room};

use crate::commands::test_support::{Rx, drain, player_in};
use crate::commands::{broadcast_room_except_rendered, cmd_move, dispatch};

const DREAMS: &str = "In your dreams, or what?";

fn account() -> Account {
    Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    }
}

/// Mover "Tester" in room A with an open north exit to room B. B holds
/// an awake "Bob" and a sleeping "Sleepy".
fn arrival_setup() -> (World, Entity, Rx, Rx) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    let a = world.spawn((Room, Exits::default())).id();
    let b = world.spawn((Room, Exits::default())).id();
    world.entity_mut(a).insert(Exits(HashMap::from([(
        Direction::North,
        ExitData {
            to: Some(b),
            state: ExitState::Open,
            key: None,
            description: None,
            keywords: vec![],
            is_hidden: false,
            is_pickproof: false,
            is_bashable: false,
            hit_points: None,
        },
    )])));
    let (mover, _mrx) = player_in(&mut world, a);
    world.entity_mut(mover).insert(account());
    let (bob, brx) = player_in(&mut world, b);
    world
        .entity_mut(bob)
        .insert((Named { name: "Bob".into() }, account()));
    let (sleepy, srx) = player_in(&mut world, b);
    world.entity_mut(sleepy).insert((
        Named {
            name: "Sleepy".into(),
        },
        account(),
        Posture(PostureKind::Sleeping),
    ));
    (world, mover, brx, srx)
}

#[test]
fn a_sleeper_misses_the_arrival_but_an_awake_bystander_sees_it() {
    let (mut world, mover, mut brx, mut srx) = arrival_setup();
    cmd_move(&mut world, mover, Direction::North);
    let awake = drain(&mut brx);
    assert!(
        awake.contains("Tester arrives from the south."),
        "awake bystander: {awake:?}"
    );
    let asleep = drain(&mut srx);
    assert!(!asleep.contains("arrives"), "sleeper: {asleep:?}");
}

#[test]
fn a_sleeper_misses_any_room_broadcast_but_wakes_to_it() {
    let (mut world, _mover, mut brx, mut srx) = arrival_setup();
    let sleepy = entity_named(&mut world, "Sleepy");
    let b = world.get::<mud_world::Located>(sleepy).unwrap().0;
    broadcast_room_except_rendered(&mut world, b, &[], "A goat bleats loudly.\r\n");
    assert!(drain(&mut brx).contains("A goat bleats loudly."));
    assert!(drain(&mut srx).is_empty());
    // Once awake, the same broadcast reaches them.
    world
        .entity_mut(sleepy)
        .insert(Posture(PostureKind::Standing));
    broadcast_room_except_rendered(&mut world, b, &[], "A goat bleats again.\r\n");
    assert!(drain(&mut srx).contains("A goat bleats again."));
}

fn entity_named(world: &mut World, name: &str) -> Entity {
    let mut q = world.query::<(Entity, &Named)>();
    q.iter(world)
        .find(|(_, n)| n.name == name)
        .map(|(e, _)| e)
        .unwrap()
}

#[test]
fn a_sleeper_still_gets_lines_sent_to_them_directly() {
    let (mut world, _mover, _brx, mut srx) = arrival_setup();
    let sleepy = entity_named(&mut world, "Sleepy");
    crate::commands::send_to(&world, sleepy, "Bob shakes you.\r\n");
    assert!(drain(&mut srx).contains("Bob shakes you."));
}

#[test]
fn sleepers_cannot_look_or_scan_but_can_check_their_pack() {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    let room = world.spawn(Room).id();
    let (p, mut rx) = player_in(&mut world, room);
    world
        .entity_mut(p)
        .insert((account(), Posture(PostureKind::Sleeping)));
    for line in [
        "look",
        "l",
        "look bob",
        "examine bob",
        "exits",
        "scan",
        "glance",
    ] {
        dispatch(&mut world, p, line);
        let out = drain(&mut rx);
        assert!(out.contains(DREAMS), "{line}: {out}");
    }
    dispatch(&mut world, p, "inventory");
    assert!(!drain(&mut rx).contains(DREAMS));
}

#[test]
fn resting_characters_keep_their_sight() {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    let room = world.spawn(Room).id();
    let (p, mut rx) = player_in(&mut world, room);
    world
        .entity_mut(p)
        .insert((account(), Posture(PostureKind::Resting)));
    dispatch(&mut world, p, "exits");
    assert!(!drain(&mut rx).contains(DREAMS));
}
