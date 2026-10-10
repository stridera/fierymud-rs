//! Posture and status in room listings (issues #105, #107, #108, #110):
//! a mob in its default state keeps its long description; asleep, seated,
//! flying, held or fighting mobs and every other player get a status line
//! ("A wolf is sleeping here."), the way legacy `print_char_to_char` does,
//! and the GMCP `Room.Mobs` / `Room.Players` frames carry the same state.
//! Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{Position, UserRole};
use mud_world::{
    Account, AppliedTo, Description, EffectInstance, EffectSource, Fighting, Flying, Located, Mob,
    MobPrototypes, Named, Player, Posture, PostureKind, Room, Stunned, Title, WorldKey,
};

use super::dispatch;
use super::gmcp_tests::{drain_bytes, frames, of};
use super::test_support::{Rx, mob_proto, player_in};

const LONG: &str = "A grey wolf prowls the road, watching you.";

fn setup() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(MobPrototypes::default());
    let room = world
        .spawn((
            Room,
            Named {
                name: "A quiet hall".into(),
            },
            mud_world::Exits::default(),
        ))
        .id();
    let (player, rx) = player_in(&mut world, room);
    world.entity_mut(player).insert(Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    });
    (world, room, player, rx)
}

fn wolf(world: &mut World, room: Entity) -> Entity {
    world
        .spawn((
            Mob,
            Named {
                name: "a grey wolf".into(),
            },
            Description(LONG.into()),
            Located(room),
            Posture(PostureKind::Standing),
        ))
        .id()
}

fn other_player(world: &mut World, room: Entity, name: &str) -> Entity {
    world
        .spawn((
            Player,
            Named { name: name.into() },
            Located(room),
            Posture(PostureKind::Standing),
        ))
        .id()
}

/// The visible text of `look` with GMCP and colour stripped.
fn look(world: &mut World, p: Entity, rx: &mut Rx) -> String {
    dispatch(world, p, "look");
    let bytes = drain_bytes(rx);
    let mut text = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(&[255, 250, 201]) {
            while i + 1 < bytes.len() && !(bytes[i] == 255 && bytes[i + 1] == 240) {
                i += 1;
            }
            i += 2;
        } else {
            text.push(bytes[i]);
            i += 1;
        }
    }
    let raw = String::from_utf8_lossy(&text).into_owned();
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
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

fn spell_effect(world: &mut World, target: Entity, name: &str) {
    world.spawn((
        EffectInstance {
            kind: 1,
            name: name.into(),
            strength: 1,
            remaining_secs: 60,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(target),
    ));
}

#[test]
fn a_mob_in_its_default_posture_keeps_its_long_description() {
    let (mut world, room, p, mut rx) = setup();
    wolf(&mut world, room);
    let out = look(&mut world, p, &mut rx);
    assert!(out.contains(LONG), "{out}");
    assert!(!out.contains("is standing here"), "{out}");
    assert!(!out.contains("Also here"), "{out}");
}

#[test]
fn a_mob_out_of_its_default_posture_is_listed_by_name_and_posture() {
    for (posture, line) in [
        (PostureKind::Sleeping, "A grey wolf is sleeping here."),
        (PostureKind::Resting, "A grey wolf is resting here."),
        (PostureKind::Sitting, "A grey wolf is sitting here."),
        (PostureKind::Kneeling, "A grey wolf is kneeling here."),
    ] {
        let (mut world, room, p, mut rx) = setup();
        let wolf = wolf(&mut world, room);
        world.entity_mut(wolf).insert(Posture(posture));
        let out = look(&mut world, p, &mut rx);
        assert!(out.contains(line), "{posture:?}: {out}");
        assert!(!out.contains(LONG), "{posture:?}: {out}");
    }
}

#[test]
fn a_mob_whose_default_is_sleeping_keeps_its_long_description_asleep() {
    let (mut world, room, p, mut rx) = setup();
    let mut proto = mob_proto(30, 7, mud_db::enums::MobProfession::Banker);
    proto.default_position = Position::Sleeping;
    world
        .resource_mut::<MobPrototypes>()
        .by_key
        .insert((30, 7), proto);
    let wolf = wolf(&mut world, room);
    world
        .entity_mut(wolf)
        .insert((WorldKey { zone: 30, id: 7 }, Posture(PostureKind::Sleeping)));
    let out = look(&mut world, p, &mut rx);
    assert!(out.contains(LONG), "{out}");
    // Woken (or knocked sitting), it stops matching its default.
    world
        .entity_mut(wolf)
        .insert(Posture(PostureKind::Standing));
    let out = look(&mut world, p, &mut rx);
    assert!(out.contains("A grey wolf is standing here."), "{out}");
}

#[test]
fn a_flying_mob_is_listed_flying_unless_it_always_flies() {
    let (mut world, room, p, mut rx) = setup();
    let wolf = wolf(&mut world, room);
    world.entity_mut(wolf).insert(Flying);
    let out = look(&mut world, p, &mut rx);
    assert!(out.contains("A grey wolf is flying here."), "{out}");
    assert!(!out.contains(LONG), "{out}");
    // An innate flier (prototype default effect) keeps its description.
    world.spawn((
        EffectInstance {
            kind: 1,
            name: "fly".into(),
            strength: 1,
            remaining_secs: -1,
            source: EffectSource::Other("mob_default".into()),
            ability_id: None,
        },
        AppliedTo(wolf),
    ));
    let out = look(&mut world, p, &mut rx);
    assert!(out.contains(LONG), "{out}");
}

#[test]
fn held_mobs_say_how_they_are_held() {
    for (effect, line) in [
        (
            Some("paralyzed"),
            "A grey wolf is standing here, completely motionless.",
        ),
        (
            Some("mesmerized"),
            "A grey wolf is here, gazing carefully at a point in front of its nose.",
        ),
        (Some("stun"), "A grey wolf is here, stunned."),
        // The bare marker (no backing instance) still reads as held.
        (None, "A grey wolf is here, stunned."),
    ] {
        let (mut world, room, p, mut rx) = setup();
        let wolf = wolf(&mut world, room);
        world.entity_mut(wolf).insert(Stunned);
        if let Some(effect) = effect {
            spell_effect(&mut world, wolf, effect);
        }
        let out = look(&mut world, p, &mut rx);
        assert!(out.contains(line), "{effect:?}: {out}");
        assert!(!out.contains(LONG), "{effect:?}: {out}");
    }
}

#[test]
fn a_fighting_mob_says_who_it_is_fighting() {
    let (mut world, room, p, mut rx) = setup();
    let wolf = wolf(&mut world, room);
    let bob = other_player(&mut world, room, "Bob");
    world.entity_mut(wolf).insert(Fighting(p));
    let out = look(&mut world, p, &mut rx);
    assert!(out.contains("A grey wolf is here, fighting YOU!"), "{out}");
    world.entity_mut(wolf).insert(Fighting(bob));
    let out = look(&mut world, p, &mut rx);
    assert!(out.contains("A grey wolf is here, fighting Bob!"), "{out}");
}

#[test]
fn players_get_a_status_line_each_instead_of_also_here() {
    let (mut world, room, p, mut rx) = setup();
    let bob = other_player(&mut world, room, "Bob");
    let cy = other_player(&mut world, room, "Cy");
    world.entity_mut(bob).insert(Posture(PostureKind::Sleeping));
    world.entity_mut(cy).insert(Title("the Quick".into()));
    let out = look(&mut world, p, &mut rx);
    assert!(!out.contains("Also here"), "{out}");
    assert!(out.contains("Bob is sleeping here.\r\n"), "{out}");
    assert!(out.contains("Cy the Quick is standing here.\r\n"), "{out}");
    // Newest arrival first, one line apiece.
    assert!(out.find("Cy the Quick").unwrap() < out.find("Bob is").unwrap());
}

#[test]
fn players_show_posture_flight_fights_and_holds() {
    let (mut world, room, p, mut rx) = setup();
    let bob = other_player(&mut world, room, "Bob");
    for (setup, line) in [
        (
            Box::new(|w: &mut World| {
                w.entity_mut(bob).insert(Posture(PostureKind::Resting));
            }) as Box<dyn Fn(&mut World)>,
            "Bob is resting here.",
        ),
        (
            Box::new(|w: &mut World| {
                w.entity_mut(bob).insert(Posture(PostureKind::Standing));
                w.entity_mut(bob).insert(Flying);
            }),
            "Bob is flying here.",
        ),
        (
            Box::new(|w: &mut World| {
                w.entity_mut(bob).remove::<Flying>();
                w.entity_mut(bob).insert(Fighting(p));
            }),
            "Bob is here, fighting YOU!",
        ),
        (
            Box::new(|w: &mut World| {
                w.entity_mut(bob).remove::<Fighting>();
                w.entity_mut(bob).insert(Stunned);
                spell_effect(w, bob, "paralyzed");
            }),
            "Bob is standing here, completely motionless.",
        ),
    ] {
        setup(&mut world);
        let out = look(&mut world, p, &mut rx);
        assert!(out.contains(line), "{line}: {out}");
    }
}

#[test]
fn gmcp_frames_carry_the_same_posture_and_hold_as_the_room_text() {
    let (mut world, room, p, mut rx) = setup();
    let wolf = wolf(&mut world, room);
    let bob = other_player(&mut world, room, "Bob");
    world
        .entity_mut(wolf)
        .insert(Posture(PostureKind::Sleeping));
    world.entity_mut(bob).insert(Stunned);
    spell_effect(&mut world, bob, "paralyzed");
    dispatch(&mut world, p, "look");
    let fr = frames(&drain_bytes(&mut rx));
    let mobs: serde_json::Value = serde_json::from_str(&of(&fr, "Room.Mobs")[0]).unwrap();
    assert_eq!(mobs[0]["posture"], "sleeping", "{mobs}");
    assert!(mobs[0].get("status").is_none(), "{mobs}");
    let players: serde_json::Value = serde_json::from_str(&of(&fr, "Room.Players")[0]).unwrap();
    assert_eq!(players[0]["name"], "Bob");
    assert_eq!(players[0]["posture"], "standing", "{players}");
    assert_eq!(players[0]["status"], "stunned", "{players}");
    assert_eq!(players[0]["hold"], "paralyzed", "{players}");
    // Flying reads the same in the frame as in the text.
    world
        .entity_mut(wolf)
        .insert((Posture(PostureKind::Standing), Flying));
    dispatch(&mut world, p, "look");
    let fr = frames(&drain_bytes(&mut rx));
    let mobs: serde_json::Value = serde_json::from_str(&of(&fr, "Room.Mobs")[0]).unwrap();
    assert_eq!(mobs[0]["posture"], "flying", "{mobs}");
}
