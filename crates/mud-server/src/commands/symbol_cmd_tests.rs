//! Issue #95: legacy `command_interpreter` splits a leading symbol command
//! (`'`, `:`, `.`, `;`) from its argument with no space. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, Named, PlayerFlags, Room};

use crate::commands::test_support::{Rx, drain, player_in};
use crate::commands::{dispatch, split_symbol_command};

fn setup() -> (World, Entity, Rx, Rx) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    let room = world.spawn(Room).id();
    let (speaker, srx) = player_in(&mut world, room);
    let acct = || Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    };
    world
        .entity_mut(speaker)
        .insert((acct(), PlayerFlags(Vec::new())));
    let (bob, brx) = player_in(&mut world, room);
    world.entity_mut(bob).insert((
        Named { name: "Bob".into() },
        mud_world::Online,
        acct(),
        PlayerFlags(Vec::new()),
    ));
    let _ = bob;
    (world, speaker, srx, brx)
}

#[test]
fn split_inserts_space_only_for_symbol_commands() {
    assert_eq!(split_symbol_command("'hello"), "' hello");
    assert_eq!(split_symbol_command(":waves"), ": waves");
    assert_eq!(split_symbol_command("' hello"), "' hello");
    assert_eq!(split_symbol_command("'"), "'");
    assert_eq!(split_symbol_command("say hi"), "say hi");
    assert_eq!(split_symbol_command("north"), "north");
}

#[test]
fn apostrophe_say_without_space() {
    let (mut world, p, mut srx, mut brx) = setup();
    dispatch(&mut world, p, "'hello there");
    assert!(drain(&mut srx).contains("You say,"));
    assert!(drain(&mut brx).contains("hello there"));
}

#[test]
fn apostrophe_say_with_space_still_works() {
    let (mut world, p, mut srx, _brx) = setup();
    dispatch(&mut world, p, "' hello there");
    assert!(drain(&mut srx).contains("You say,"));
}

#[test]
fn colon_emote_without_space() {
    let (mut world, p, mut srx, mut brx) = setup();
    dispatch(&mut world, p, ":waves hello");
    assert!(drain(&mut srx).contains("Tester waves hello"));
    assert!(drain(&mut brx).contains("Tester waves hello"));
}
