//! Staff `transfer` / `teleport` on mobs, and teleport destinations that
//! are a room id or another character / mob (issue #62). Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{UserRole, effective_rank};
use mud_world::{
    Account, Exits, Fighting, GodZone, Item, Keywords, Located, Mob, Named, Online, Player,
    Profile, Room, WizInvis, WorldKey, WorldKeyIndex, Zone,
};

use super::dispatch;
use super::test_support::{Rx, drain};
use crate::commands::Connection;

fn world() -> World {
    let mut world = World::new();
    world.insert_resource(WorldKeyIndex::default());
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::WeatherCatalog::default());
    world
}

fn zone(world: &mut World, id: i32, god: bool) -> Entity {
    let z = world
        .spawn((
            Zone,
            WorldKey { zone: id, id: 0 },
            Named {
                name: format!("Zone {id}"),
            },
        ))
        .id();
    if god {
        world.entity_mut(z).insert(GodZone);
    }
    world.resource_mut::<WorldKeyIndex>().zones.insert(id, z);
    z
}

fn room_in(world: &mut World, zone_entity: Entity, zone_id: i32, id: i32) -> Entity {
    let r = world
        .spawn((
            Room,
            WorldKey { zone: zone_id, id },
            Named {
                name: format!("Room {zone_id}:{id}"),
            },
            Located(zone_entity),
            Exits::default(),
        ))
        .id();
    world
        .resource_mut::<WorldKeyIndex>()
        .rooms
        .insert((zone_id, id), r);
    r
}

fn person(world: &mut World, name: &str, level: i32, room: Entity) -> (Entity, Rx) {
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
                gender: "neutral".into(),
            },
        ))
        .id();
    (e, rx)
}

fn mob(world: &mut World, name: &str, kw: &str, key: (i32, i32), room: Entity) -> Entity {
    world
        .spawn((
            Mob,
            Named { name: name.into() },
            Keywords(vec![kw.into()]),
            WorldKey {
                zone: key.0,
                id: key.1,
            },
            Located(room),
        ))
        .id()
}

fn at(world: &World, e: Entity) -> Entity {
    world.get::<Located>(e).unwrap().0
}

#[test]
fn transfer_pulls_a_mob_by_name_and_clears_its_fight() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let (here, lair) = (room_in(&mut w, z, 30, 1), room_in(&mut w, z, 30, 2));
    let (imm, mut rx) = person(&mut w, "Laoris", 102, here);
    let (_, mut watcher) = person(&mut w, "Watcher", 10, here);
    let (_, mut lair_rx) = person(&mut w, "Victim", 10, lair);
    let dragon = mob(&mut w, "a red dragon", "dragon red", (30, 9), lair);
    let (victim, _) = person(&mut w, "Fighter", 10, lair);
    w.entity_mut(dragon).insert(Fighting(victim));
    w.entity_mut(victim).insert(Fighting(dragon));

    dispatch(&mut w, imm, "transfer dragon");
    assert_eq!(at(&w, dragon), here, "{}", drain(&mut rx));
    assert!(w.get::<Fighting>(dragon).is_none(), "fight cleared");
    assert!(
        w.get::<Fighting>(victim).is_none(),
        "opponent's fight cleared"
    );
    assert!(
        drain(&mut watcher).contains("A red dragon appears, summoned by Laoris."),
        "arrival shown"
    );
    assert!(
        drain(&mut lair_rx).contains("A red dragon vanishes in a puff of smoke."),
        "departure shown"
    );
}

#[test]
fn transfer_by_indexed_name_and_by_zone_id_instance() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let (here, a, b) = (
        room_in(&mut w, z, 30, 1),
        room_in(&mut w, z, 30, 2),
        room_in(&mut w, z, 30, 3),
    );
    let (imm, mut rx) = person(&mut w, "Laoris", 102, here);
    let first = mob(&mut w, "a guard", "guard", (30, 7), a);
    let second = mob(&mut w, "a guard", "guard", (30, 7), b);
    let other = mob(&mut w, "a sergeant", "sergeant", (30, 8), a);

    // Elsewhere candidates are ordered (prototype, spawn order): 2.guard is the second.
    dispatch(&mut w, imm, "transfer 2.guard");
    assert_eq!(at(&w, second), here, "{}", drain(&mut rx));
    assert_eq!(at(&w, first), a);

    dispatch(&mut w, imm, "transfer 30:8");
    assert_eq!(at(&w, other), here, "zone:id selects a spawned instance");
    // The first guard moved in already and now sorts first (own room).
    dispatch(&mut w, imm, "transfer 2.30:7");
    assert_eq!(at(&w, first), here, "N.zone:id picks the Nth instance");
}

#[test]
fn transfer_prefers_a_mob_in_your_room_and_skips_unseen_actors() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let (here, far) = (room_in(&mut w, z, 30, 1), room_in(&mut w, z, 30, 2));
    let (imm, mut rx) = person(&mut w, "Laoris", 102, here);
    let (zeus, _z) = person(&mut w, "Zeus", 105, far);
    w.entity_mut(zeus).insert(WizInvis(105));
    dispatch(&mut w, imm, "transfer zeus");
    assert!(drain(&mut rx).contains("There is no one by that name here."));
    assert_eq!(at(&w, zeus), far);
}

#[test]
fn transfer_keeps_player_behaviour_and_the_level_guard() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let (here, far) = (room_in(&mut w, z, 30, 1), room_in(&mut w, z, 30, 2));
    let (imm, mut rx) = person(&mut w, "Laoris", 102, here);
    let (bob, mut brx) = person(&mut w, "Bob", 20, far);
    let (boss, _b) = person(&mut w, "Boss", 105, far);

    dispatch(&mut w, imm, "transfer bob");
    assert_eq!(at(&w, bob), here);
    assert!(drain(&mut rx).contains("You summon Bob."));
    let seen = drain(&mut brx);
    assert!(seen.contains("Laoris summons you."), "{seen}");
    assert!(
        seen.contains("Room 30:1"),
        "room look after arrival: {seen}"
    );

    dispatch(&mut w, imm, "transfer boss");
    assert!(drain(&mut rx).contains("Go transfer someone your own size."));
    assert_eq!(at(&w, boss), far);

    dispatch(&mut w, imm, "transfer laoris");
    assert!(drain(&mut rx).contains("That doesn't make much sense"));
}

#[test]
fn teleport_sends_a_mob_to_a_room_in_every_syntax() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let z40 = zone(&mut w, 40, false);
    let (here, r2) = (room_in(&mut w, z, 30, 1), room_in(&mut w, z, 30, 2));
    let r40 = room_in(&mut w, z40, 40, 5);
    let (imm, mut rx) = person(&mut w, "Laoris", 102, here);
    let rat = mob(&mut w, "a rat", "rat", (30, 3), here);

    dispatch(&mut w, imm, "teleport rat 40 5");
    assert_eq!(at(&w, rat), r40, "{}", drain(&mut rx));
    dispatch(&mut w, imm, "teleport rat 30:2");
    assert_eq!(at(&w, rat), r2);
    dispatch(&mut w, imm, "teleport rat 1");
    assert_eq!(at(&w, rat), here, "bare id = room in your zone");
    dispatch(&mut w, imm, "teleport 30:3 40:5");
    assert_eq!(at(&w, rat), r40, "zone:id subject");
    let out = drain(&mut rx);
    assert!(out.contains("You teleport a rat to (40, 5)."), "{out}");

    dispatch(&mut w, imm, "teleport rat 40 99");
    assert!(drain(&mut rx).contains("No room (40, 99)."));
    dispatch(&mut w, imm, "teleport rat");
    assert!(drain(&mut rx).contains("Where do you wish to send this person?"));
    assert_eq!(at(&w, rat), r40);
}

#[test]
fn teleport_destination_can_be_another_character_mob_or_object() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let (here, den, vault, attic) = (
        room_in(&mut w, z, 30, 1),
        room_in(&mut w, z, 30, 2),
        room_in(&mut w, z, 30, 3),
        room_in(&mut w, z, 30, 4),
    );
    let (imm, mut rx) = person(&mut w, "Laoris", 102, here);
    let (bob, mut brx) = person(&mut w, "Bob", 20, here);
    mob(&mut w, "a wolf", "wolf", (30, 6), den);
    let (_carol, _c) = person(&mut w, "Carol", 20, vault);
    w.spawn((
        Item,
        Named {
            name: "a brass lamp".into(),
        },
        Keywords(vec!["lamp".into()]),
        Located(attic),
    ));

    dispatch(&mut w, imm, "teleport bob wolf");
    assert_eq!(at(&w, bob), den, "{}", drain(&mut rx));
    assert!(
        drain(&mut brx).contains("Room 30:2"),
        "bob sees the new room"
    );
    dispatch(&mut w, imm, "teleport bob carol");
    assert_eq!(at(&w, bob), vault);
    dispatch(&mut w, imm, "teleport bob lamp");
    assert_eq!(at(&w, bob), attic, "object lying in a room");
    dispatch(&mut w, imm, "teleport bob nonesuch");
    assert!(drain(&mut rx).contains("No such creature or object around."));
    assert_eq!(at(&w, bob), attic);
}

#[test]
fn teleport_refuses_self_equal_rank_and_inbound_teleport_rooms() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let (here, sealed) = (room_in(&mut w, z, 30, 1), room_in(&mut w, z, 30, 2));
    w.entity_mut(sealed).insert(mud_world::NoTeleportRoom);
    let (imm, mut rx) = person(&mut w, "Laoris", 102, here);
    let (peer, _p) = person(&mut w, "Peer", 102, here);
    let rat = mob(&mut w, "a rat", "rat", (30, 3), here);

    dispatch(&mut w, imm, "teleport laoris 30:2");
    assert!(drain(&mut rx).contains("Use 'goto' to teleport yourself."));
    dispatch(&mut w, imm, "teleport peer 30:2");
    assert!(drain(&mut rx).contains("Maybe you shouldn't do that."));
    assert_eq!(at(&w, peer), here);
    dispatch(&mut w, imm, "teleport rat 30:2");
    assert!(drain(&mut rx).contains("refuses inbound teleports"));
    assert_eq!(at(&w, rat), here);
}

#[test]
fn teleport_into_a_god_zone_needs_a_god_rank_destination_check() {
    // Destinations resolved from a target in a god zone go through the
    // same entry gate as `goto` (staff bypass inside `entry_allowed`).
    let mut w = world();
    let town = zone(&mut w, 30, false);
    let heavens = zone(&mut w, 12, true);
    let here = room_in(&mut w, town, 30, 1);
    let hall = room_in(&mut w, heavens, 12, 4);
    let (imm, mut rx) = person(&mut w, "Laoris", 102, here);
    let rat = mob(&mut w, "a rat", "rat", (30, 3), here);
    mob(&mut w, "an angel", "angel", (12, 1), hall);
    dispatch(&mut w, imm, "teleport rat angel");
    assert_eq!(at(&w, rat), hall, "{}", drain(&mut rx));
}

#[test]
fn transfer_and_teleport_are_builder_plus() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let (here, far) = (room_in(&mut w, z, 30, 1), room_in(&mut w, z, 30, 2));
    let (mortal, _rx) = person(&mut w, "Newbie", 10, here);
    let rat = mob(&mut w, "a rat", "rat", (30, 3), far);
    dispatch(&mut w, mortal, "transfer rat");
    dispatch(&mut w, mortal, "teleport rat 30:1");
    assert_eq!(at(&w, rat), far);
}
