//! `ptell` and `page`. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{PlayerFlag, UserRole, effective_rank};
use mud_world::{
    Account, Located, MailDraft, Named, Online, Player, PlayerFlags, Posture, PostureKind, Profile,
    Room,
};

use super::dispatch;
use super::test_support::{Rx, drain as drain_raw};
use crate::commands::Connection;

/// What the player was sent, minus ANSI colour sequences.
fn drain(rx: &mut Rx) -> String {
    let raw = drain_raw(rx);
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for n in chars.by_ref() {
                if n == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn world() -> (World, Entity) {
    let mut world = World::new();
    let room = world
        .spawn((
            Room,
            Named {
                name: "A room".into(),
            },
        ))
        .id();
    (world, room)
}

fn person(world: &mut World, room: Entity, name: &str, level: i32) -> (Entity, Rx) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let e = world
        .spawn((
            Player,
            Online,
            Named { name: name.into() },
            Located(room),
            Connection(tx),
            Account {
                user_id: String::new(),
                character_id: format!("c-{name}"),
                role: effective_rank(level, UserRole::Player),
                account_role: UserRole::Player,
                perms: vec![],
            },
            Profile {
                level,
                class_id: None,
                race: "Human".into(),
                experience: 0,
                gender: "male".into(),
            },
        ))
        .id();
    (e, rx)
}

#[test]
fn ptell_reaches_the_petitioner_and_other_staff() {
    let (mut w, room) = world();
    let (imm, mut imm_rx) = person(&mut w, room, "Imm", 100);
    let (_other, mut other_rx) = person(&mut w, room, "Other", 102);
    let (_pet, mut pet_rx) = person(&mut w, room, "Pet", 20);

    dispatch(&mut w, imm, "ptell pet try the north door");
    assert!(
        drain(&mut pet_rx).contains("Imm responds to your petition, 'try the north door'"),
        "petitioner"
    );
    assert!(drain(&mut imm_rx).contains("You respond to Pet, 'try the north door'"));
    assert!(drain(&mut other_rx).contains("Imm responds to Pet's petition, 'try the north door'"));
}

#[test]
fn ptell_refusals() {
    let (mut w, room) = world();
    let (imm, mut rx) = person(&mut w, room, "Imm", 100);
    let (pet, mut pet_rx) = person(&mut w, room, "Pet", 20);
    let (_god, _g) = person(&mut w, room, "God", 102);

    dispatch(&mut w, imm, "ptell");
    assert!(drain(&mut rx).contains("Who do you wish to ptell??"));
    dispatch(&mut w, imm, "ptell pet");
    assert!(drain(&mut rx).contains("Who do you wish to ptell??"));
    dispatch(&mut w, imm, "ptell nobody hi");
    assert!(drain(&mut rx).contains("There is no one by that name here."));
    dispatch(&mut w, imm, "ptell imm hi");
    assert!(drain(&mut rx).contains("You need mental help."));
    dispatch(&mut w, imm, "ptell god hi");
    assert!(drain(&mut rx).contains("Just use wiznet!"));

    w.entity_mut(pet).insert(MailDraft {
        recipient_user_id: String::new(),
        recipient_label: String::new(),
        subject: None,
        body: vec![],
    });
    dispatch(&mut w, imm, "ptell pet hi");
    assert!(drain(&mut rx).contains("he's writing a message right now"));
    assert!(drain(&mut pet_rx).is_empty());
}

#[test]
fn ptell_notes_afk_and_honours_norepeat() {
    let (mut w, room) = world();
    let (imm, mut rx) = person(&mut w, room, "Imm", 100);
    let (pet, mut pet_rx) = person(&mut w, room, "Pet", 20);
    w.entity_mut(pet).insert(PlayerFlags(vec![PlayerFlag::Afk]));
    w.entity_mut(imm)
        .insert(PlayerFlags(vec![PlayerFlag::NoRepeat]));
    dispatch(&mut w, imm, "ptell pet hello");
    let out = drain(&mut rx);
    assert!(out.contains("Ok."), "{out}");
    assert!(out.contains("That person is AFK right now but received your message."));
    let theirs = drain(&mut pet_rx);
    assert!(theirs.contains("You received the previous message while AFK."));
    assert!(theirs.contains("responds to your petition, 'hello'"));
}

#[test]
fn ptell_is_immortal_only() {
    let (mut w, room) = world();
    let (pet, mut rx) = person(&mut w, room, "Pet", 20);
    let (_imm, mut imm_rx) = person(&mut w, room, "Imm", 100);
    dispatch(&mut w, pet, "ptell imm hi");
    assert!(drain(&mut rx).contains("You can't do that."));
    assert!(drain(&mut imm_rx).is_empty());
}

#[test]
fn page_reaches_its_target_and_echoes() {
    let (mut w, room) = world();
    let (god, mut god_rx) = person(&mut w, room, "God", 101);
    let (_bob, mut bob_rx) = person(&mut w, room, "Bob", 20);
    dispatch(&mut w, god, "page bob come here");
    assert!(drain(&mut bob_rx).contains("\x07\x07*God* come here"));
    assert!(drain(&mut god_rx).contains("\x07\x07*God* come here"));
}

#[test]
fn the_page_bells_survive_the_output_encoder() {
    use mud_net::output::encode_frame;
    use mud_net::{Charset, ColorDepth};
    let (mut w, room) = world();
    let (god, _god_rx) = person(&mut w, room, "God", 101);
    let (_bob, mut bob_rx) = person(&mut w, room, "Bob", 20);
    dispatch(&mut w, god, "page bob come here");
    let mut wire = Vec::new();
    while let Ok(frame) = bob_rx.try_recv() {
        wire.extend(encode_frame(frame, ColorDepth::Ansi16, Charset::Utf8));
    }
    let text = String::from_utf8(wire).unwrap();
    assert!(text.starts_with("\x07\x07*God* come here"), "{text:?}");
}

#[test]
fn page_refusals_and_rules() {
    let (mut w, room) = world();
    let (god, mut god_rx) = person(&mut w, room, "God", 101);
    let (bob, mut bob_rx) = person(&mut w, room, "Bob", 20);
    dispatch(&mut w, god, "page");
    assert!(drain(&mut god_rx).contains("Whom do you wish to page?"));
    dispatch(&mut w, god, "page nobody hi");
    assert!(drain(&mut god_rx).contains("There is no such person in the game!"));
    // `page all` needs a level above 101.
    dispatch(&mut w, god, "page all hi");
    assert!(drain(&mut god_rx).contains("You will never be godly enough to do that!"));
    assert!(drain(&mut bob_rx).is_empty());

    // A sleeping target does not get it (legacy act TO_VICT).
    w.entity_mut(bob).insert(Posture(PostureKind::Sleeping));
    dispatch(&mut w, god, "page bob wake up");
    assert!(drain(&mut bob_rx).is_empty());

    // NOREPEAT: "Ok." instead of the echo.
    w.entity_mut(god)
        .insert(PlayerFlags(vec![PlayerFlag::NoRepeat]));
    w.entity_mut(bob).insert(Posture(PostureKind::Standing));
    drain(&mut god_rx);
    dispatch(&mut w, god, "page bob now");
    assert!(drain(&mut bob_rx).contains("*God* now"));
    assert_eq!(drain(&mut god_rx), "Ok.\r\n");
}

#[test]
fn page_all_reaches_everyone_from_level_102() {
    let (mut w, room) = world();
    let (grgod, mut rx) = person(&mut w, room, "Grgod", 102);
    let (_bob, mut bob_rx) = person(&mut w, room, "Bob", 20);
    let (_amy, mut amy_rx) = person(&mut w, room, "Amy", 5);
    dispatch(&mut w, grgod, "page all reboot soon");
    for rx in [&mut bob_rx, &mut amy_rx, &mut rx] {
        assert!(drain(rx).contains("*Grgod* reboot soon"));
    }
}

#[test]
fn page_is_builder_and_up() {
    let (mut w, room) = world();
    let (imm, mut rx) = person(&mut w, room, "Imm", 100);
    let (_bob, mut bob_rx) = person(&mut w, room, "Bob", 20);
    dispatch(&mut w, imm, "page bob hi");
    assert!(drain(&mut rx).contains("You can't do that."));
    assert!(drain(&mut bob_rx).is_empty());
}
