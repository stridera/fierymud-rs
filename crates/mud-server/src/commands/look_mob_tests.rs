//! `look <mob>` (issue #46): mortals get the legacy appearance line
//! (size, composition) and condition but never the numeric level; staff
//! keep the level readout.

use bevy_ecs::prelude::*;
use mud_db::enums::{MobProfession, Sector, Size, UserRole};
use mud_world::{
    Account, Health, Keywords, Located, Mob, MobPrototypes, Named, Room, RoomSector, Sized,
    WorldKey,
};

use super::info::cmd_look;
use super::test_support::{Rx, drain, mob_proto, player_in};

fn world_with_mob(race: &str, gender: &str, size: Size) -> (World, Entity, Rx) {
    let mut world = World::new();
    world.init_resource::<mud_world::MudClock>();
    let mut proto = mob_proto(30, 5, MobProfession::Shopkeeper);
    proto.professions.clear();
    proto.name = "a gnarled treant".to_string();
    proto.keywords = vec!["treant".to_string()];
    proto.race = race.to_string();
    proto.gender = gender.to_string();
    proto.size = size;
    proto.level = 37;
    let mut protos = MobPrototypes::default();
    protos.by_key.insert((30, 5), proto);
    world.insert_resource(protos);
    let room = world.spawn((Room, RoomSector(Sector::Field))).id();
    world.spawn((
        Mob,
        Named {
            name: "a gnarled treant".to_string(),
        },
        Keywords(vec!["treant".to_string()]),
        WorldKey { zone: 30, id: 5 },
        Located(room),
        Health { hp: 10, max: 10 },
        Sized(size),
    ));
    let (player, rx) = player_in(&mut world, room);
    (world, player, rx)
}

/// Drain the connection and drop ANSI colour sequences.
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

fn make_staff(world: &mut World, player: Entity) {
    world.entity_mut(player).insert(Account {
        user_id: "u".to_string(),
        character_id: "c".to_string(),
        role: UserRole::Builder,
        account_role: UserRole::Builder,
        perms: Vec::new(),
    });
}

#[test]
fn mortal_look_at_mob_hides_level_and_shows_composition_and_condition() {
    let (mut world, player, mut rx) = world_with_mob("plant", "male", Size::Large);
    cmd_look(&mut world, player, "treant");
    let out = plain(&mut rx);
    assert!(!out.to_ascii_lowercase().contains("level"), "{out}");
    assert!(!out.contains("37"), "{out}");
    assert!(out.contains("He is large in size"), "{out}");
    assert!(out.contains("composed of plant material"), "{out}");
    assert!(out.contains("excellent shape"), "{out}");
}

#[test]
fn flesh_mob_gets_size_only_like_legacy() {
    let (mut world, player, mut rx) = world_with_mob("human", "female", Size::Medium);
    cmd_look(&mut world, player, "treant");
    let out = plain(&mut rx);
    assert!(out.contains("She is medium in size."), "{out}");
    assert!(!out.contains("composed of"), "{out}");
}

#[test]
fn staff_look_at_mob_still_sees_level() {
    let (mut world, player, mut rx) = world_with_mob("plant", "neutral", Size::Medium);
    make_staff(&mut world, player);
    cmd_look(&mut world, player, "treant");
    let out = plain(&mut rx);
    assert!(out.contains("is level 37"), "{out}");
    assert!(out.contains("It is medium in size"), "{out}");
}
