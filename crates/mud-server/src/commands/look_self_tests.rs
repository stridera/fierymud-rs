//! `look` at yourself (issue #85): your own name resolves like any other
//! player's, and self-look shows the same block as looking at another player.

use bevy_ecs::prelude::*;
use mud_db::enums::Composition;
use mud_world::{
    ClassCatalog, Health, Keywords, Located, Named, Player, Profile, RaceCatalog, RaceDef, Room,
};

use super::info::cmd_look;
use super::test_support::{Rx, drain, player_in};

fn profile(race: &str, gender: &str) -> Profile {
    Profile {
        level: 10,
        class_id: None,
        race: race.to_string(),
        experience: 0,
        gender: gender.to_string(),
    }
}

fn setup() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.init_resource::<ClassCatalog>();
    let mut races = RaceCatalog::default();
    races.by_race.insert(
        "DRYAD".to_string(),
        RaceDef {
            race: "DRYAD".to_string(),
            default_size: "MEDIUM".to_string(),
            default_lifeforce: "LIFE".to_string(),
            default_composition: Composition::Plant,
            ..RaceDef::default()
        },
    );
    world.insert_resource(races);
    let room = world.spawn(Room).id();
    let (me, rx) = player_in(&mut world, room);
    world
        .entity_mut(me)
        .insert((profile("DRYAD", "female"), Health { hp: 10, max: 10 }));
    let other = world
        .spawn((
            Player,
            Named {
                name: "Bob".to_string(),
            },
            Keywords(vec!["bob".to_string()]),
            Located(room),
            profile("DRYAD", "male"),
            Health { hp: 10, max: 10 },
        ))
        .id();
    (world, me, other, rx)
}

fn plain(rx: &mut Rx) -> String {
    let raw = drain(rx);
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

#[test]
fn look_own_name_resolves_to_self() {
    let (mut world, me, _other, mut rx) = setup();
    cmd_look(&mut world, me, "tester");
    let out = plain(&mut rx);
    assert!(!out.contains("don't see"), "{out}");
    assert!(out.contains("Tester"), "{out}");
}

#[test]
fn look_me_and_own_name_show_the_same_block_as_looking_at_another_player() {
    let (mut world, me, _other, mut rx) = setup();
    cmd_look(&mut world, me, "me");
    let by_me = plain(&mut rx);
    cmd_look(&mut world, me, "self");
    let by_self = plain(&mut rx);
    cmd_look(&mut world, me, "Tester");
    let by_name = plain(&mut rx);
    assert_eq!(by_me, by_self);
    assert_eq!(by_me, by_name);
    // Same sections `look bob` prints: identity, condition, lifeforce,
    // size + composition.
    for needle in ["is a level 10 Dryad", "excellent shape"] {
        assert!(by_me.contains(needle), "missing {needle:?}: {by_me}");
    }
    cmd_look(&mut world, me, "bob");
    let bob = plain(&mut rx);
    assert!(bob.contains("is a level 10 Dryad"), "{bob}");
}

#[test]
fn another_player_is_found_before_yourself() {
    let (mut world, me, other, mut rx) = setup();
    world.entity_mut(other).insert(Named {
        name: "Testy".to_string(),
    });
    world
        .entity_mut(other)
        .insert(Keywords(vec!["testy".to_string()]));
    cmd_look(&mut world, me, "test");
    let out = plain(&mut rx);
    assert!(out.contains("Testy"), "{out}");
    assert!(!out.contains("yourself"), "{out}");
}
