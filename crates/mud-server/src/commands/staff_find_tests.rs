//! `at`, `vnum` family, `vstat`, `vsearch` / `esearch` / `tsearch`,
//! `osearch` / `msearch` / `rsearch`.
//! Test-only.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::{
    Direction, ExitState, MobBehavior, MobProfession, ObjectFlag, ObjectType, Sector, UserRole,
    WearFlag, effective_rank,
};
use mud_world::{
    Account, Description, EffectCatalog, EffectDef, ExitData, Exits, Ghost, Located, Mob,
    MobPrototypes, Named, NoMagicRoom, ObjectGrantedEffect, ObjectPrototypes, Online, PeacefulRoom,
    Player, Profile, Room, RoomSector, TriggerAttach, TriggerCatalog, TriggerDef, WorldKey,
    WorldKeyIndex,
};

use super::super::dispatch;
use super::super::test_support::{Rx, drain, mob_proto, object_proto};
use crate::commands::Connection;

fn world() -> World {
    let mut world = World::new();
    world.insert_resource(WorldKeyIndex::default());
    world.insert_resource(MobPrototypes::default());
    world.insert_resource(ObjectPrototypes::default());
    world.insert_resource(TriggerCatalog::default());
    world.insert_resource(mud_world::WeatherCatalog::default());
    world.insert_resource(crate::commands::SocialRegistry::default());
    world.insert_resource(mud_world::ObjectAbilityCatalog::default());
    world
}

fn room(world: &mut World, zone: i32, id: i32, name: &str) -> Entity {
    let r = world
        .spawn((
            Room,
            WorldKey { zone, id },
            Named { name: name.into() },
            Description(format!("You see the {name}.")),
            Exits::default(),
        ))
        .id();
    world
        .resource_mut::<WorldKeyIndex>()
        .rooms
        .insert((zone, id), r);
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

fn where_is(world: &World, e: Entity) -> Entity {
    world.get::<Located>(e).unwrap().0
}

fn add_mob(world: &mut World, key: (i32, i32), name: &str, keywords: &[&str]) {
    let mut p = mob_proto(key.0, key.1, MobProfession::Banker);
    p.name = name.into();
    p.keywords = keywords.iter().map(|s| (*s).to_string()).collect();
    world.resource_mut::<MobPrototypes>().by_key.insert(key, p);
}

fn two_rooms() -> (World, Entity, Entity, Entity, Rx) {
    let mut w = world();
    let a = room(&mut w, 30, 1, "Origin Hall");
    let b = room(&mut w, 30, 2, "Far Cellar");
    let (p, rx) = person(&mut w, 102, a);
    (w, p, a, b, rx)
}

#[test]
fn at_looks_elsewhere_and_returns() {
    let (mut w, p, a, _b, mut rx) = two_rooms();
    dispatch(&mut w, p, "at 30:2 look");
    let out = drain(&mut rx);
    assert!(out.contains("Far Cellar"), "{out}");
    assert!(!out.contains("Origin Hall"), "{out}");
    assert_eq!(where_is(&w, p), a, "put back");
    // The other location forms goto takes.
    for cmd in ["at 2 look", "at 30 2 look"] {
        dispatch(&mut w, p, cmd);
        let out = drain(&mut rx);
        assert!(out.contains("Far Cellar"), "{cmd}: {out}");
        assert_eq!(where_is(&w, p), a, "{cmd}");
    }
}

#[test]
fn at_a_named_mob_uses_its_room() {
    let (mut w, p, a, b, mut rx) = two_rooms();
    w.spawn((
        Mob,
        Named {
            name: "a pale ghoul".into(),
        },
        mud_world::Keywords(vec!["ghoul".into()]),
        Located(b),
    ));
    dispatch(&mut w, p, "at ghoul look");
    let out = drain(&mut rx);
    assert!(out.contains("Far Cellar"), "{out}");
    assert_eq!(where_is(&w, p), a);
}

#[test]
fn at_with_a_failing_command_returns_you() {
    let (mut w, p, a, _b, mut rx) = two_rooms();
    dispatch(&mut w, p, "at 30:2 frobnicate the widget");
    let out = drain(&mut rx);
    assert!(out.contains("Unknown command"), "{out}");
    assert_eq!(where_is(&w, p), a);
}

#[test]
fn at_with_a_bad_location_or_no_command_stays_put() {
    let (mut w, p, a, _b, mut rx) = two_rooms();
    dispatch(&mut w, p, "at 30:99 look");
    assert!(drain(&mut rx).contains("No room"), "bad room");
    dispatch(&mut w, p, "at nobody-here look");
    assert!(drain(&mut rx).contains("No one named"), "bad name");
    dispatch(&mut w, p, "at 30:2");
    assert!(drain(&mut rx).contains("What do you want to do there?"));
    dispatch(&mut w, p, "at");
    assert!(drain(&mut rx).contains("room number or a name"));
    assert_eq!(where_is(&w, p), a);
}

#[test]
fn at_leaves_you_where_the_command_moved_you() {
    let (mut w, p, _a, _b, mut rx) = two_rooms();
    let c = room(&mut w, 30, 3, "Third Room");
    dispatch(&mut w, p, "at 30:2 goto 30:3");
    drain(&mut rx);
    assert_eq!(where_is(&w, p), c, "a goto inside at wins");
}

#[test]
fn at_leaves_a_ghost_where_it_died() {
    let (mut w, p, a, b, mut rx) = two_rooms();
    // A command that kills the caller in place: model it as the ghost state.
    super::return_from_at(&mut w, p, b, a, false);
    // Not in `location` (b): nothing to undo.
    assert_eq!(where_is(&w, p), a);
    w.entity_mut(p).insert((Located(b), Ghost));
    super::return_from_at(&mut w, p, b, a, false);
    assert_eq!(
        where_is(&w, p),
        b,
        "became a ghost during the command: stays"
    );
    w.entity_mut(p).remove::<Ghost>();
    super::return_from_at(&mut w, p, b, a, false);
    assert_eq!(where_is(&w, p), a, "living caller goes back");
    drain(&mut rx);
}

#[test]
fn at_returns_a_staffer_who_was_already_a_ghost() {
    let (mut w, p, a, b, _rx) = two_rooms();
    // Already a ghost before the command: dying is not what moved them, so
    // the return still happens.
    w.entity_mut(p).insert((Located(b), Ghost));
    super::return_from_at(&mut w, p, b, a, true);
    assert_eq!(where_is(&w, p), a, "an existing ghost still goes back");
}

#[test]
fn at_with_two_numbers_and_no_command_asks_what_to_do() {
    let (mut w, p, a, _b, mut rx) = two_rooms();
    dispatch(&mut w, p, "at 30 2");
    let out = drain(&mut rx);
    assert!(out.contains("What do you want to do there?"), "{out}");
    assert_eq!(where_is(&w, p), a);
}

#[test]
fn at_nests() {
    let (mut w, p, a, _b, mut rx) = two_rooms();
    room(&mut w, 30, 3, "Third Room");
    dispatch(&mut w, p, "at 30:2 at 30:3 look");
    let out = drain(&mut rx);
    assert!(out.contains("Third Room"), "{out}");
    assert_eq!(where_is(&w, p), a);
}

#[test]
fn mortals_are_refused() {
    let mut w = world();
    let a = room(&mut w, 30, 1, "Origin Hall");
    room(&mut w, 30, 2, "Far Cellar");
    add_mob(&mut w, (30, 5), "a goblin", &["goblin"]);
    let (p, mut rx) = person(&mut w, 50, a);
    for cmd in [
        "at 30:2 look",
        "vnum mob goblin",
        "mnum goblin",
        "onum sword",
        "rnum hall",
        "tnum greet",
        "vstat mob 30 5",
        "vsearch mob level 5",
        "esearch keyword door",
        "tsearch name greet",
        "osearch level 1",
        "msearch level 1",
        "rsearch name hall",
    ] {
        dispatch(&mut w, p, cmd);
        let out = drain(&mut rx);
        assert!(out.contains("You can't do that."), "{cmd}: {out}");
        assert!(!out.contains("goblin"), "{cmd}: {out}");
    }
    assert_eq!(where_is(&w, p), a);
}

#[test]
fn vnum_finds_a_proto_by_keyword_as_composite_id() {
    let (mut w, p, _a, _b, mut rx) = two_rooms();
    add_mob(
        &mut w,
        (30, 5),
        "a snarling goblin",
        &["goblin", "snarling"],
    );
    add_mob(&mut w, (31, 7), "a goblin chief", &["goblin", "chief"]);
    add_mob(&mut w, (30, 9), "an elf", &["elf"]);

    dispatch(&mut w, p, "vnum mobiles goblin");
    let out = drain(&mut rx);
    assert!(out.contains("30:5"), "{out}");
    assert!(out.contains("31:7"), "{out}");
    assert!(out.contains("a snarling goblin"), "{out}");
    assert!(!out.contains("an elf"), "{out}");
    assert!(out.contains("2 of 2"), "{out}");

    dispatch(&mut w, p, "mnum goblin chief");
    let out = drain(&mut rx);
    assert!(out.contains("31:7") && !out.contains("30:5"), "{out}");

    dispatch(&mut w, p, "mnum goblin in 30");
    let out = drain(&mut rx);
    assert!(out.contains("30:5") && !out.contains("31:7"), "{out}");

    dispatch(&mut w, p, "vnum mob dragon");
    assert!(drain(&mut rx).contains("No mob prototypes match"));
}

#[test]
fn vnum_covers_objects_rooms_and_triggers() {
    let (mut w, p, _a, _b, mut rx) = two_rooms();
    let mut o = object_proto(30, 12, ObjectType::Weapon);
    o.name = "a rusty sword".into();
    o.keywords = vec!["sword".into(), "rusty".into()];
    w.resource_mut::<ObjectPrototypes>()
        .by_key
        .insert((30, 12), o);
    w.resource_mut::<TriggerCatalog>().by_key.insert(
        (30, 3),
        TriggerDef {
            zone_id: 30,
            id: 3,
            name: "Greeter script".into(),
            attach_type: TriggerAttach::Mob,
            commands: "say hello there".into(),
            flags: vec![],
            arg_list: vec![],
            num_args: 0,
        },
    );

    dispatch(&mut w, p, "onum sword");
    assert!(drain(&mut rx).contains("30:12"));
    dispatch(&mut w, p, "vnum obj rusty");
    assert!(drain(&mut rx).contains("30:12"));
    dispatch(&mut w, p, "rnum cellar");
    assert!(drain(&mut rx).contains("30:2"));
    dispatch(&mut w, p, "vnum rooms far");
    assert!(drain(&mut rx).contains("30:2"));
    dispatch(&mut w, p, "tnum greeter");
    assert!(drain(&mut rx).contains("30:3"));
    dispatch(&mut w, p, "tsearch commands hello");
    assert!(drain(&mut rx).contains("30:3"));
    dispatch(&mut w, p, "tsearch intention object");
    assert!(drain(&mut rx).contains("No triggers match"));
}

#[test]
fn vsearch_filters_by_field() {
    let (mut w, p, _a, _b, mut rx) = two_rooms();
    add_mob(&mut w, (30, 5), "a weak imp", &["imp"]);
    add_mob(&mut w, (30, 6), "a strong troll", &["troll"]);
    w.resource_mut::<MobPrototypes>()
        .by_key
        .get_mut(&(30, 6))
        .unwrap()
        .level = 40;

    dispatch(&mut w, p, "vsearch mobiles level >20");
    let out = drain(&mut rx);
    assert!(out.contains("30:6") && !out.contains("30:5"), "{out}");
    dispatch(&mut w, p, "vsearch mobiles level 1..10");
    let out = drain(&mut rx);
    assert!(out.contains("30:5") && !out.contains("30:6"), "{out}");
    dispatch(&mut w, p, "vsearch mobiles short strong");
    assert!(drain(&mut rx).contains("30:6"));
    dispatch(&mut w, p, "vsearch rooms desc cellar");
    assert!(drain(&mut rx).contains("30:2"));

    // No field: list them. Bad field and bad number are explained.
    dispatch(&mut w, p, "vsearch mobiles");
    assert!(drain(&mut rx).contains("fields"));
    dispatch(&mut w, p, "vsearch mobiles bogus 1");
    assert!(drain(&mut rx).contains("Unrecognized search field"));
    dispatch(&mut w, p, "vsearch mobiles level lots");
    assert!(drain(&mut rx).contains("takes a number"));
    dispatch(&mut w, p, "vsearch shops name inn");
    assert!(drain(&mut rx).contains("Unrecognized vsearch mode"));
}

#[test]
fn esearch_finds_doors_by_keyword_and_target() {
    let (mut w, p, a, b, mut rx) = two_rooms();
    w.entity_mut(a).insert(Exits(HashMap::from([(
        Direction::North,
        ExitData {
            to: Some(b),
            state: ExitState::Closed,
            key: Some((30, 77)),
            description: None,
            keywords: vec!["iron".into(), "door".into()],
            is_hidden: false,
            is_pickproof: false,
            is_bashable: false,
            hit_points: None,
        },
    )])));
    dispatch(&mut w, p, "esearch keyword door");
    let out = drain(&mut rx);
    assert!(
        out.contains("30:1") && out.contains("north") && out.contains("-> 30:2"),
        "{out}"
    );
    dispatch(&mut w, p, "esearch key 77");
    assert!(drain(&mut rx).contains("30:1"));
    dispatch(&mut w, p, "esearch room 2");
    assert!(drain(&mut rx).contains("30:1"));
    dispatch(&mut w, p, "vsearch exits key >100");
    assert!(drain(&mut rx).contains("No exits match"));
}

#[test]
fn vstat_shows_a_mob_without_spawning_it() {
    let (mut w, p, _a, _b, mut rx) = two_rooms();
    add_mob(&mut w, (30, 5), "a snarling goblin", &["goblin"]);
    let mut o = object_proto(30, 12, ObjectType::Weapon);
    o.name = "a rusty sword".into();
    w.resource_mut::<ObjectPrototypes>()
        .by_key
        .insert((30, 12), o);

    for cmd in ["vstat mob 30 5", "vstat mob 30:5", "vstat m 5"] {
        dispatch(&mut w, p, cmd);
        let out = drain(&mut rx);
        assert!(out.contains("A snarling goblin"), "{cmd}: {out}");
        assert!(out.contains("(30, 5)"), "{cmd}: {out}");
    }
    let spawned = w.query_filtered::<Entity, With<Mob>>().iter(&w).count();
    assert_eq!(spawned, 0, "vstat must not spawn the mob");

    dispatch(&mut w, p, "vstat obj 30:12");
    assert!(drain(&mut rx).contains("A rusty sword"));
    dispatch(&mut w, p, "vstat mob 30 999");
    assert!(drain(&mut rx).contains("No mob proto"));
    dispatch(&mut w, p, "vstat mob goblin");
    assert!(drain(&mut rx).contains("Usage: vstat"));
    dispatch(&mut w, p, "vstat room 1");
    assert!(drain(&mut rx).contains("either 'obj' or 'mob'"));
    dispatch(&mut w, p, "vstat");
    assert!(drain(&mut rx).contains("Usage: vstat"));
}

fn add_object(
    w: &mut World,
    key: (i32, i32),
    name: &str,
    kind: ObjectType,
    wear: &[WearFlag],
    level: i32,
) {
    let mut o = object_proto(key.0, key.1, kind);
    o.name = name.into();
    o.keywords = name.split_whitespace().map(str::to_string).collect();
    o.wear_flags = wear.to_vec();
    o.level = level;
    w.resource_mut::<ObjectPrototypes>().by_key.insert(key, o);
}

fn effect(id: i32, name: &str) -> EffectDef {
    EffectDef {
        id,
        name: name.into(),
        description: None,
        effect_type: "status".into(),
        tags: vec![],
        presence_override: None,
        default_params: serde_json::json!({}),
        prevents_speaking: false,
        prevents_casting: false,
        prevents_movement: false,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    }
}

fn osearch_fx() -> (World, Entity, Rx) {
    let (mut w, p, _a, _b, rx) = two_rooms();
    add_object(
        &mut w,
        (30, 1),
        "a bronze helm",
        ObjectType::Armor,
        &[WearFlag::Head],
        10,
    );
    add_object(
        &mut w,
        (30, 2),
        "a gem collar",
        ObjectType::Armor,
        &[WearFlag::Neck, WearFlag::Head],
        25,
    );
    add_object(
        &mut w,
        (31, 3),
        "a rusty sword",
        ObjectType::Weapon,
        &[WearFlag::Mainhand],
        12,
    );
    add_object(
        &mut w,
        (31, 4),
        "a fire staff",
        ObjectType::Fireweapon,
        &[],
        40,
    );
    {
        let mut objs = w.resource_mut::<ObjectPrototypes>();
        let collar = objs.by_key.get_mut(&(30, 2)).unwrap();
        collar.flags = vec![ObjectFlag::Glow];
        collar.timer_hours = 6;
        collar.granted_effects = vec![ObjectGrantedEffect {
            effect_id: 9,
            strength: 1,
            modifier_data: serde_json::json!({}),
            wear_location: None,
        }];
    }
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(9, effect(9, "Sanctuary"));
    w.insert_resource(effects);
    (w, p, rx)
}

#[test]
fn osearch_filters_by_type_wear_flags_and_effects() {
    let (mut w, p, mut rx) = osearch_fx();
    // Type: whole-word prefix, so "weapon" is not Fireweapon.
    dispatch(&mut w, p, "osearch type weapon");
    let out = drain(&mut rx);
    assert!(out.contains("31:3") && !out.contains("31:4"), "{out}");
    dispatch(&mut w, p, "osearch type fire");
    assert!(drain(&mut rx).contains("31:4"));
    // Several wear flags must all be present; abbreviations work.
    dispatch(&mut w, p, "osearch wear head neck");
    let out = drain(&mut rx);
    assert!(out.contains("30:2") && !out.contains("30:1"), "{out}");
    dispatch(&mut w, p, "osearch wear hea");
    let out = drain(&mut rx);
    assert!(out.contains("30:1") && out.contains("30:2"), "{out}");
    // Item flags, granted effects, timer.
    dispatch(&mut w, p, "osearch extraflags glow");
    let out = drain(&mut rx);
    assert!(out.contains("30:2") && !out.contains("30:1"), "{out}");
    dispatch(&mut w, p, "osearch effflags sanctuary");
    let out = drain(&mut rx);
    assert!(out.contains("30:2") && !out.contains("30:1"), "{out}");
    dispatch(&mut w, p, "osearch timer >0");
    assert!(drain(&mut rx).contains("30:2"));
}

#[test]
fn osearch_filters_by_level_zone_and_name() {
    let (mut w, p, mut rx) = osearch_fx();
    // Level range, both grammars; zone scope.
    dispatch(&mut w, p, "osearch level 11..30");
    let out = drain(&mut rx);
    assert!(
        out.contains("30:2") && out.contains("31:3") && !out.contains("30:1"),
        "{out}"
    );
    dispatch(&mut w, p, "osearch level bt 11 and 30");
    let out = drain(&mut rx);
    assert!(
        out.contains("30:2") && out.contains("31:3") && !out.contains("30:1"),
        "{out}"
    );
    dispatch(&mut w, p, "osearch level gt 30");
    let out = drain(&mut rx);
    assert!(out.contains("31:4") && !out.contains("31:3"), "{out}");
    dispatch(&mut w, p, "osearch level 1..99 in 31");
    let out = drain(&mut rx);
    assert!(out.contains("31:3") && !out.contains("30:"), "{out}");
    // Name search, no matches, bad field, no field.
    dispatch(&mut w, p, "osearch name helm");
    assert!(drain(&mut rx).contains("30:1"));
    dispatch(&mut w, p, "osearch wear tail");
    assert!(drain(&mut rx).contains("No object prototypes match"));
    // Text that starts with no field is a name search, with the fields
    // listed when it finds nothing; `vsearch` still rejects it.
    dispatch(&mut w, p, "osearch rust");
    assert!(drain(&mut rx).contains("31:3"));
    dispatch(&mut w, p, "osearch bogus 1");
    let out = drain(&mut rx);
    assert!(
        out.contains("No object prototypes match") && out.contains("search fields"),
        "{out}"
    );
    dispatch(&mut w, p, "vsearch objects bogus 1");
    assert!(drain(&mut rx).contains("Unrecognized search field"));
    dispatch(&mut w, p, "osearch");
    assert!(drain(&mut rx).contains("search fields"));
}

#[test]
fn msearch_filters_by_flags_level_and_race() {
    let (mut w, p, _a, _b, mut rx) = two_rooms();
    add_mob(&mut w, (30, 5), "a sleepy guard", &["guard"]);
    add_mob(&mut w, (30, 6), "a twitchy scout", &["scout"]);
    add_mob(&mut w, (31, 7), "a plain clerk", &["clerk"]);
    {
        let mut mobs = w.resource_mut::<MobPrototypes>();
        let g = mobs.by_key.get_mut(&(30, 5)).unwrap();
        g.behaviors = vec![MobBehavior::Sentinel, MobBehavior::Wimpy];
        g.level = 30;
        g.race = "dwarf".into();
        let s = mobs.by_key.get_mut(&(30, 6)).unwrap();
        s.behaviors = vec![MobBehavior::Wimpy];
        s.level = 8;
    }
    dispatch(&mut w, p, "msearch flags sentinel");
    let out = drain(&mut rx);
    assert!(out.contains("30:5") && !out.contains("30:6"), "{out}");
    dispatch(&mut w, p, "msearch flags wimpy");
    let out = drain(&mut rx);
    assert!(out.contains("30:5") && out.contains("30:6"), "{out}");
    dispatch(&mut w, p, "msearch flags sentinel wimpy");
    let out = drain(&mut rx);
    assert!(out.contains("30:5") && !out.contains("30:6"), "{out}");
    // Profession is a mob flag too (the test proto is a banker).
    dispatch(&mut w, p, "msearch flags banker");
    let out = drain(&mut rx);
    assert!(out.contains("30:6") && out.contains("31:7"), "{out}");
    dispatch(&mut w, p, "msearch level 20..40");
    let out = drain(&mut rx);
    assert!(out.contains("30:5") && !out.contains("30:6"), "{out}");
    dispatch(&mut w, p, "msearch race dwarf");
    assert!(drain(&mut rx).contains("30:5"));
    dispatch(&mut w, p, "msearch name scout in 31");
    assert!(drain(&mut rx).contains("No mob prototypes match"));
    dispatch(&mut w, p, "msearch flags");
    assert!(drain(&mut rx).contains("Search flags for what"));
}

#[test]
fn rsearch_filters_by_sector_and_room_flags() {
    let (mut w, p, a, b, mut rx) = two_rooms();
    w.entity_mut(a).insert(RoomSector(Sector::City));
    w.entity_mut(b)
        .insert((RoomSector(Sector::Underwater), NoMagicRoom, PeacefulRoom));
    dispatch(&mut w, p, "rsearch sector city");
    let out = drain(&mut rx);
    assert!(out.contains("30:1") && !out.contains("30:2"), "{out}");
    dispatch(&mut w, p, "rsearch sector water");
    assert!(
        drain(&mut rx).contains("No rooms match"),
        "prefix, not substring"
    );
    dispatch(&mut w, p, "rsearch flags nomagic");
    let out = drain(&mut rx);
    assert!(out.contains("30:2") && !out.contains("30:1"), "{out}");
    dispatch(&mut w, p, "rsearch flags nomagic peaceful");
    assert!(drain(&mut rx).contains("30:2"));
    dispatch(&mut w, p, "rsearch flags nomagic deathtrap");
    assert!(drain(&mut rx).contains("No rooms match"));
    dispatch(&mut w, p, "rsearch name cellar");
    assert!(drain(&mut rx).contains("30:2"));
    dispatch(&mut w, p, "vsearch rooms flags peaceful");
    assert!(drain(&mut rx).contains("30:2"));
}

#[test]
fn searches_cap_at_100_rows_and_say_how_many_more() {
    let (mut w, p, _a, _b, mut rx) = two_rooms();
    for id in 1..=130 {
        add_object(
            &mut w,
            (40, id),
            "a copper coin",
            ObjectType::Treasure,
            &[],
            1,
        );
    }
    dispatch(&mut w, p, "osearch type treasure");
    let out = drain(&mut rx);
    assert!(out.contains("100 of 130"), "{out}");
    assert!(out.contains("30 more"), "{out}");
    assert!(out.contains("40:100") && !out.contains("40:101"), "{out}");
}
