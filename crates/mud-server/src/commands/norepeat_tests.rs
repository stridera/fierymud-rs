//! `NoRepeat` (issue #51): legacy `PRF_NOREPEAT` replaces the echo of your
//! own say/tell/channel text with "Ok." and nothing else. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{PlayerFlag, UserRole};
use mud_world::{Account, Named, PlayerFlags, Room};

use crate::commands::dispatch;
use crate::commands::test_support::{Rx, drain, player_in};

fn account() -> Account {
    Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    }
}

/// The visible text line, without trailing GMCP frames (which carry the
/// chat-tab copy regardless of `NoRepeat`).
fn text(out: &str) -> &str {
    out.split("\r\n").next().unwrap_or("")
}

/// "Tester" (speaker) and "Bob" (listener) in one room.
fn setup(norepeat: bool) -> (World, Entity, Rx, Rx) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    let room = world.spawn(Room).id();
    let (speaker, srx) = player_in(&mut world, room);
    let flags = if norepeat {
        vec![PlayerFlag::NoRepeat]
    } else {
        Vec::new()
    };
    world
        .entity_mut(speaker)
        .insert((account(), PlayerFlags(flags)));
    let (bob, brx) = player_in(&mut world, room);
    world.entity_mut(bob).insert((
        Named { name: "Bob".into() },
        mud_world::Online,
        account(),
        PlayerFlags(Vec::new()),
    ));
    (world, speaker, srx, brx)
}

#[test]
fn say_echoes_by_default() {
    let (mut world, p, mut srx, mut brx) = setup(false);
    dispatch(&mut world, p, "say hello there");
    assert!(drain(&mut srx).contains("You say,"));
    assert!(drain(&mut brx).contains("Tester"));
}

#[test]
fn say_with_norepeat_answers_ok_but_still_reaches_others() {
    let (mut world, p, mut srx, mut brx) = setup(true);
    dispatch(&mut world, p, "say hello there");
    let own = drain(&mut srx);
    let own = text(&own);
    assert_eq!(own, "Ok.");
    assert!(drain(&mut brx).contains("hello there"));
}

#[test]
fn tell_with_norepeat_answers_ok() {
    let (mut world, p, mut srx, mut brx) = setup(true);
    dispatch(&mut world, p, "tell bob secret words");
    let own = drain(&mut srx);
    let own = text(&own);
    assert_eq!(own, "Ok.");
    assert!(drain(&mut brx).contains("secret words"));
}

#[test]
fn tell_without_norepeat_echoes() {
    let (mut world, p, mut srx, _brx) = setup(false);
    dispatch(&mut world, p, "tell bob secret words");
    assert!(drain(&mut srx).contains("You tell"));
}

#[test]
fn gossip_with_norepeat_answers_ok() {
    let (mut world, p, mut srx, mut brx) = setup(true);
    world.entity_mut(p).insert(mud_world::Online);
    dispatch(&mut world, p, "gossip big news");
    let own = drain(&mut srx);
    let own = text(&own);
    assert_eq!(own, "Ok.");
    let _ = drain(&mut brx);
}

#[test]
fn whisper_with_norepeat_answers_ok() {
    let (mut world, p, mut srx, mut brx) = setup(true);
    dispatch(&mut world, p, "whisper bob psst");
    let own = drain(&mut srx);
    let own = text(&own);
    assert_eq!(own, "Ok.");
    assert!(drain(&mut brx).contains("psst"));
}

#[test]
fn norepeat_toggle_shows_in_toggle_list() {
    let (mut world, p, mut srx, _brx) = setup(true);
    dispatch(&mut world, p, "toggle");
    let out = drain(&mut srx);
    let line = out.lines().find(|l| l.contains("NoRepeat")).expect("row");
    assert!(line.contains("ON"), "{line:?}");
}
