//! `olist` / `mlist` / `rlist` (issue #65): catalog listings by zone and
//! id range, paged, god zones hidden from non-staff. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{MobProfession, ObjectType, UserRole, effective_rank};
use mud_world::{
    Account, Exits, GodZone, Located, MobPrototypes, Named, ObjectPrototypes, Online, Player,
    Profile, Room, WorldKey, WorldKeyIndex, Zone,
};

use super::dispatch;
use super::test_support::{Rx, drain, mob_proto, object_proto};
use super::vlist::{cmd_mlist, cmd_olist};
use crate::commands::Connection;

fn world() -> World {
    let mut world = World::new();
    world.insert_resource(WorldKeyIndex::default());
    world.insert_resource(MobPrototypes::default());
    world.insert_resource(ObjectPrototypes::default());
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
                name: format!("Hall {zone_id}:{id}"),
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

fn person(world: &mut World, level: i32, room: Entity) -> (Entity, Rx) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let e = world
        .spawn((
            Player,
            Online,
            Named {
                name: "Builder".into(),
            },
            Located(room),
            Connection(tx),
            Account {
                user_id: String::new(),
                character_id: "c-b".into(),
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

fn add_mob(world: &mut World, key: (i32, i32), name: &str) {
    let mut p = mob_proto(key.0, key.1, MobProfession::Banker);
    p.name = name.into();
    world.resource_mut::<MobPrototypes>().by_key.insert(key, p);
}

fn add_obj(world: &mut World, key: (i32, i32), name: &str) {
    let mut p = object_proto(key.0, key.1, ObjectType::Weapon);
    p.name = name.into();
    world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .insert(key, p);
}

#[test]
fn mlist_defaults_to_the_current_zone_sorted_by_id() {
    let mut w = world();
    let z30 = zone(&mut w, 30, false);
    let hall = room_in(&mut w, z30, 30, 1);
    let (b, mut rx) = person(&mut w, 102, hall);
    add_mob(&mut w, (30, 20), "a tall elf");
    add_mob(&mut w, (30, 3), "a goblin");
    add_mob(&mut w, (31, 1), "a far-off orc");

    dispatch(&mut w, b, "mlist");
    let out = drain(&mut rx);
    let goblin = out.find("a goblin").unwrap_or_else(|| panic!("{out}"));
    let elf = out.find("a tall elf").unwrap_or_else(|| panic!("{out}"));
    assert!(goblin < elf, "sorted by id: {out}");
    assert!(out.contains("[ 30:3   ]"), "{out}");
    assert!(!out.contains("far-off orc"), "other zone excluded: {out}");
    assert!(out.contains("2 mob prototype(s) in zone 30"), "{out}");
}

#[test]
fn mlist_zone_and_id_range_in_both_syntaxes() {
    let mut w = world();
    let z30 = zone(&mut w, 30, false);
    let hall = room_in(&mut w, z30, 30, 1);
    let (b, mut rx) = person(&mut w, 102, hall);
    for id in [1, 5, 10, 15] {
        add_mob(&mut w, (40, id), &format!("mob number {id}"));
    }
    add_mob(&mut w, (30, 5), "home zone mob");

    for cmd in ["mlist 40 5 10", "mlist 40:5 40:10", "mlist 40 from 5 to 10"] {
        dispatch(&mut w, b, cmd);
        let out = drain(&mut rx);
        assert!(out.contains("mob number 5"), "{cmd}: {out}");
        assert!(out.contains("mob number 10"), "{cmd}: {out}");
        assert!(!out.contains("mob number 1 "), "{cmd}: {out}");
        assert!(!out.contains("mob number 15"), "{cmd}: {out}");
        assert!(!out.contains("home zone mob"), "{cmd}: {out}");
        assert!(out.contains("zone 40, ids 5-10"), "{cmd}: {out}");
    }

    // One bound = "from there to the end of the zone".
    dispatch(&mut w, b, "mlist 40 10");
    let out = drain(&mut rx);
    assert!(
        out.contains("mob number 15") && !out.contains("mob number 5"),
        "{out}"
    );

    // `*` spans zones.
    dispatch(&mut w, b, "mlist *");
    let out = drain(&mut rx);
    assert!(
        out.contains("home zone mob") && out.contains("mob number 15"),
        "{out}"
    );
}

#[test]
fn mlist_rejects_cross_zone_ranges_and_junk() {
    let mut w = world();
    let z30 = zone(&mut w, 30, false);
    let hall = room_in(&mut w, z30, 30, 1);
    let (b, mut rx) = person(&mut w, 102, hall);
    dispatch(&mut w, b, "mlist 30:1 31:9");
    assert!(drain(&mut rx).contains("inside one zone"));
    dispatch(&mut w, b, "mlist banana");
    assert!(drain(&mut rx).contains("Usage:"));
    dispatch(&mut w, b, "mlist 30 1 2 3");
    assert!(drain(&mut rx).contains("Usage:"));
}

#[test]
fn olist_lists_object_prototypes_with_type_and_empty_zone_message() {
    let mut w = world();
    let z30 = zone(&mut w, 30, false);
    let hall = room_in(&mut w, z30, 30, 1);
    let (b, mut rx) = person(&mut w, 102, hall);
    add_obj(&mut w, (30, 7), "a rusty sword");

    dispatch(&mut w, b, "olist");
    let out = drain(&mut rx);
    assert!(out.contains("a rusty sword"), "{out}");
    assert!(out.contains("Weapon"), "{out}");
    assert!(out.contains("1 object prototype(s) in zone 30"), "{out}");

    dispatch(&mut w, b, "olist 99");
    assert!(drain(&mut rx).contains("No object prototype(s) found in zone 99."));
}

#[test]
fn rlist_lists_loaded_rooms_in_the_range() {
    let mut w = world();
    let z30 = zone(&mut w, 30, false);
    let hall = room_in(&mut w, z30, 30, 1);
    room_in(&mut w, z30, 30, 2);
    room_in(&mut w, z30, 30, 9);
    let (b, mut rx) = person(&mut w, 102, hall);

    dispatch(&mut w, b, "rlist 30 2 9");
    let out = drain(&mut rx);
    assert!(
        out.contains("Hall 30:2") && out.contains("Hall 30:9"),
        "{out}"
    );
    assert!(!out.contains("Hall 30:1\r"), "{out}");
    dispatch(&mut w, b, "rlist");
    let out = drain(&mut rx);
    assert!(out.contains("3 room(s) in zone 30"), "{out}");
}

#[test]
fn long_lists_are_paged_fifty_rows_at_a_time() {
    let mut w = world();
    let z30 = zone(&mut w, 30, false);
    let hall = room_in(&mut w, z30, 30, 1);
    let (b, mut rx) = person(&mut w, 102, hall);
    for id in 1..=120 {
        add_mob(&mut w, (50, id), &format!("clone {id}"));
    }

    dispatch(&mut w, b, "mlist 50");
    let out = drain(&mut rx);
    assert!(out.contains("120 mob prototype(s) in zone 50"), "{out}");
    assert!(
        out.contains("clone 50 ") && !out.contains("clone 51 "),
        "{out}"
    );
    assert!(out.contains("add 'page 2'"), "{out}");

    dispatch(&mut w, b, "mlist 50 page 3");
    let out = drain(&mut rx);
    assert!(
        out.contains("clone 120 ") && out.contains("clone 101 "),
        "{out}"
    );
    assert!(
        !out.contains("clone 100 ") && !out.contains("add 'page"),
        "{out}"
    );

    dispatch(&mut w, b, "mlist 50 page 4");
    assert!(drain(&mut rx).contains("only 3 page(s)"));
    dispatch(&mut w, b, "mlist 50 page 0");
    assert!(drain(&mut rx).contains("page must be a number"));
}

#[test]
fn god_zone_prototypes_are_hidden_from_non_staff_and_shown_to_staff() {
    let mut w = world();
    let z30 = zone(&mut w, 30, false);
    zone(&mut w, 12, true);
    let hall = room_in(&mut w, z30, 30, 1);
    let (staff, mut srx) = person(&mut w, 102, hall);
    let (mortal, mut mrx) = person(&mut w, 10, hall);
    add_mob(&mut w, (12, 1), "an archangel");
    add_mob(&mut w, (30, 1), "a villager");

    dispatch(&mut w, staff, "mlist *");
    let out = drain(&mut srx);
    assert!(
        out.contains("an archangel") && out.contains("a villager"),
        "{out}"
    );

    // The dispatcher refuses mortals outright; the handler also filters
    // god zones on its own so a staff-looking gate slip cannot leak.
    cmd_mlist(&mut w, mortal, "*");
    let out = drain(&mut mrx);
    assert!(
        out.contains("a villager") && !out.contains("an archangel"),
        "{out}"
    );
    cmd_mlist(&mut w, mortal, "12");
    assert!(drain(&mut mrx).contains("No mob prototype(s) found in zone 12."));
    add_obj(&mut w, (12, 2), "a halo");
    cmd_olist(&mut w, mortal, "12");
    assert!(!drain(&mut mrx).contains("halo"));
}

#[test]
fn listing_commands_are_builder_plus() {
    let mut w = world();
    let z30 = zone(&mut w, 30, false);
    let hall = room_in(&mut w, z30, 30, 1);
    let (mortal, mut rx) = person(&mut w, 10, hall);
    add_mob(&mut w, (30, 1), "a villager");
    for cmd in ["mlist", "olist", "rlist"] {
        dispatch(&mut w, mortal, cmd);
        let out = drain(&mut rx);
        assert!(
            !out.contains("villager") && !out.contains("Hall"),
            "{cmd}: {out}"
        );
    }
}
