//! Terminal escape injection (screen clears, OSC 52 clipboard writes,
//! OSC 8 links) must not reach other players through player-set text.
//! Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{PlayerFlag, UserRole};
use mud_world::{Account, Description, Named, PlayerFlags, Room, Title};

use super::dispatch;
use super::test_support::{Rx, drain, player_in};

const CLEAR: &str = "\x1b[2J";
const OSC52: &str = "\x1b]52;c;QUJD\x07";
const OSC8: &str = "\x1b]8;;http://evil.example\x07link\x1b]8;;\x07";

fn account() -> Account {
    Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    }
}

fn setup() -> (World, Entity, Rx, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    let room = world.spawn(Room).id();
    let (a, rx_a) = player_in(&mut world, room);
    world
        .entity_mut(a)
        .insert((account(), PlayerFlags(Vec::<PlayerFlag>::new())));
    let (b, rx_b) = player_in(&mut world, room);
    world.entity_mut(b).insert((
        Named { name: "Bob".into() },
        mud_world::Online,
        account(),
        PlayerFlags(Vec::new()),
    ));
    (world, a, rx_a, b, rx_b)
}

fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t'))
        // The server's own colour (SGR) escapes are fine; only non-SGR
        // sequences matter.
        && (s.contains(CLEAR) || s.contains("\x1b]") || s.contains('\x07'))
}

#[test]
fn title_and_description_are_stored_without_escapes() {
    let (mut world, a, _rx_a, _b, _rx_b) = setup();
    dispatch(&mut world, a, &format!("title the {CLEAR}Bold{OSC52} one"));
    let title = world.get::<Title>(a).expect("title set").0.clone();
    assert!(
        !title.contains('\x1b') && !title.contains('\x07'),
        "{title:?}"
    );
    assert!(title.starts_with("the "), "{title:?}");

    dispatch(
        &mut world,
        a,
        &format!("description A tall {OSC8} figure{CLEAR}"),
    );
    let desc = world
        .get::<Description>(a)
        .expect("description set")
        .0
        .clone();
    assert!(!desc.contains('\x1b') && !desc.contains('\x07'), "{desc:?}");
    assert!(desc.contains("tall") && desc.contains("figure"), "{desc:?}");
}

#[test]
fn say_with_escapes_arrives_stripped() {
    let (mut world, a, _rx_a, _b, mut rx_b) = setup();
    dispatch(&mut world, a, &format!("say hi {CLEAR}there{OSC52}{OSC8}"));
    let got = drain(&mut rx_b);
    assert!(got.contains("hi"), "{got:?}");
    assert!(!has_control(&got), "escape reached the listener: {got:?}");

    // And the wire encoder strips them even if text got past the game layer.
    let raw = format!("Bob says, 'x{CLEAR}y{OSC52}z{OSC8}'\r\n");
    let wire =
        mud_net::output::encode_text(&raw, mud_net::ColorDepth::Ansi16, mud_net::Charset::Utf8);
    assert_eq!(wire, "Bob says, 'xyzlink'\r\n");
}
