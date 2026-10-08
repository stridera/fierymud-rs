//! Staff `where <name>` (issue #66): after players it also searches live
//! mobs and objects (legacy `perform_immort_where`), including carried,
//! worn and contained items. Mortal `where` is unchanged. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{UserRole, effective_rank};
use mud_world::{
    Account, EquippedSlot, Exits, GodZone, Item, Keywords, Located, Mob, Named, Online, Player,
    Profile, Room, Slot, WizInvis, WorldKey, WorldKeyIndex, Zone,
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

fn item(world: &mut World, name: &str, kw: &str, key: (i32, i32), holder: Entity) -> Entity {
    world
        .spawn((
            Item,
            Named { name: name.into() },
            Keywords(vec![kw.into()]),
            WorldKey {
                zone: key.0,
                id: key.1,
            },
            Located(holder),
        ))
        .id()
}

#[test]
fn staff_where_lists_players_then_mobs_then_objects() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let (sq, tavern) = (room_in(&mut w, z, 30, 1), room_in(&mut w, z, 30, 2));
    let (imm, mut rx) = person(&mut w, "Laoris", 100, sq);
    let _ = person(&mut w, "Goblinbane", 20, tavern);
    mob(&mut w, "a goblin", "goblin", (30, 5), tavern);
    item(&mut w, "a goblin tooth", "tooth goblin", (30, 9), sq);

    dispatch(&mut w, imm, "where goblin");
    let out = drain(&mut rx);
    let at_player = out.find("Goblinbane is in: Room 30:2 [30:2]").expect(&out);
    let at_mob = out
        .find("M  1. [30:5] a goblin")
        .unwrap_or_else(|| panic!("{out}"));
    let at_obj = out
        .find("O  1. [30:9] a goblin tooth")
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        at_player < at_mob && at_mob < at_obj,
        "players, then mobs, then objects: {out}"
    );
    assert!(out.contains("- Room 30:2 [30:2]"), "mob room shown: {out}");
    assert!(
        out.contains("- Room 30:1 [30:1]"),
        "object room shown: {out}"
    );
}

#[test]
fn staff_where_follows_carried_worn_and_contained_objects() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let sq = room_in(&mut w, z, 30, 1);
    let (imm, mut rx) = person(&mut w, "Laoris", 100, sq);
    let (hero, _h) = person(&mut w, "Hero", 20, sq);
    let guard = mob(&mut w, "a guard", "guard", (30, 6), sq);
    item(&mut w, "a ruby", "ruby", (30, 11), hero);
    let ring = item(&mut w, "a ruby ring", "ruby", (30, 12), guard);
    w.entity_mut(ring).insert(EquippedSlot(Slot::Neck));
    let bag = item(&mut w, "a leather bag", "bag", (30, 13), sq);
    item(&mut w, "a ruby gem", "ruby", (30, 14), bag);

    dispatch(&mut w, imm, "where ruby");
    let out = drain(&mut rx);
    assert!(out.contains("carried by Hero at Room 30:1 [30:1]"), "{out}");
    assert!(out.contains("worn by a guard at Room 30:1 [30:1]"), "{out}");
    assert!(
        out.contains("inside a leather bag at Room 30:1 [30:1]"),
        "{out}"
    );
}

#[test]
fn staff_where_with_no_match_says_so() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let sq = room_in(&mut w, z, 30, 1);
    let (imm, mut rx) = person(&mut w, "Laoris", 100, sq);
    dispatch(&mut w, imm, "where zzyzx");
    assert!(
        drain(&mut rx).contains("Couldn't find any such thing."),
        "legacy no-match line"
    );
}

#[test]
fn staff_where_skips_wizinvis_actors_and_what_they_carry() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let sq = room_in(&mut w, z, 30, 1);
    let (imm, mut rx) = person(&mut w, "Laoris", 100, sq);
    let (high, _h) = person(&mut w, "Zeus", 105, sq);
    w.entity_mut(high).insert(WizInvis(105));
    item(&mut w, "a thunderbolt", "bolt", (30, 20), high);
    let sneaky = mob(&mut w, "a bolt-thrower", "bolt", (30, 21), sq);
    w.entity_mut(sneaky).insert(WizInvis(105));

    dispatch(&mut w, imm, "where bolt");
    let out = drain(&mut rx);
    assert!(out.contains("Couldn't find any such thing."), "{out}");
}

#[test]
fn mortal_where_does_not_search_mobs_or_objects() {
    let mut w = world();
    let z = zone(&mut w, 30, false);
    let sq = room_in(&mut w, z, 30, 1);
    let (mortal, mut rx) = person(&mut w, "Newbie", 10, sq);
    mob(&mut w, "a goblin", "goblin", (30, 5), sq);
    item(&mut w, "a goblin tooth", "tooth", (30, 9), sq);
    dispatch(&mut w, mortal, "where goblin");
    let out = drain(&mut rx);
    assert!(out.contains("'goblin' isn't online."), "{out}");
    assert!(!out.contains("M  1."), "{out}");
}

#[test]
fn staff_where_finds_mobs_in_god_zones_while_mortals_never_do() {
    let mut w = world();
    let town = zone(&mut w, 30, false);
    let heavens = zone(&mut w, 12, true);
    let sq = room_in(&mut w, town, 30, 1);
    let hall = room_in(&mut w, heavens, 12, 4);
    let (imm, mut irx) = person(&mut w, "Laoris", 100, sq);
    let (mortal, mut mrx) = person(&mut w, "Newbie", 10, sq);
    mob(&mut w, "an angel", "angel", (12, 1), hall);

    dispatch(&mut w, imm, "where angel");
    let out = drain(&mut irx);
    assert!(out.contains("M  1. [12:1] an angel"), "{out}");
    assert!(out.contains("Room 12:4 [12:4]"), "{out}");

    dispatch(&mut w, mortal, "where angel");
    let out = drain(&mut mrx);
    assert!(!out.contains("angel is") && !out.contains("[12:"), "{out}");
}
