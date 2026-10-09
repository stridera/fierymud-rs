//! `attach` / `detach`: a runtime-attached trigger fires through the normal
//! dispatcher and a detached one stops. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{EntityType, UserRole};
use mud_world::{
    Account, AttachedTriggers, EntityVariableCache, Health, Item, Keywords, Located, Mob, Named,
    Online, Player, Posture, PostureKind, Profile, Room, TriggerAttach, TriggerCatalog, TriggerDef,
    TriggerEvent, WorldKey, WorldKeyIndex,
};

use super::dispatch;
use super::test_support::{Rx, drain};
use crate::commands::Connection;

const BODY: &str = "self:setvar('ran', 1)";

fn def(id: i32, name: &str, attach_type: TriggerAttach) -> TriggerDef {
    TriggerDef {
        zone_id: 99,
        id,
        name: name.into(),
        attach_type,
        commands: BODY.into(),
        flags: vec![TriggerEvent::Load],
        arg_list: vec![],
        num_args: 0,
    }
}

struct Fixture {
    world: World,
    room: Entity,
    mob: Entity,
    staff: Entity,
    rx: Rx,
}

fn fixture() -> Fixture {
    let mut world = World::new();
    let mut catalog = TriggerCatalog::default();
    catalog
        .by_key
        .insert((99, 1), def(1, "guard greeter", TriggerAttach::Mob));
    catalog
        .by_key
        .insert((99, 2), def(2, "second hook", TriggerAttach::Mob));
    catalog
        .by_key
        .insert((99, 3), def(3, "lamp flicker", TriggerAttach::Object));
    catalog
        .by_key
        .insert((99, 4), def(4, "room hum", TriggerAttach::World));
    world.insert_resource(catalog);
    world.insert_resource(mud_script::LuaHost::new());
    world.insert_resource(WorldKeyIndex::default());
    let room = world
        .spawn((
            Room,
            WorldKey { zone: 99, id: 5 },
            Named {
                name: "A room".into(),
            },
        ))
        .id();
    world
        .resource_mut::<WorldKeyIndex>()
        .rooms
        .insert((99, 5), room);
    let mob = world
        .spawn((
            Mob,
            Named {
                name: "sleeper".into(),
            },
            Health { hp: 10, max: 10 },
            WorldKey { zone: 99, id: 1 },
            Located(room),
            Posture(PostureKind::Standing),
        ))
        .id();
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let staff = world
        .spawn((
            Player,
            Online,
            Named {
                name: "Boss".into(),
            },
            Located(room),
            Connection(tx),
            Account {
                user_id: String::new(),
                character_id: "c-boss".into(),
                role: UserRole::Implementor,
                account_role: UserRole::Implementor,
                perms: vec![],
            },
            Profile {
                level: 105,
                class_id: None,
                race: "Human".into(),
                experience: 0,
                gender: "neutral".into(),
            },
        ))
        .id();
    Fixture {
        world,
        room,
        mob,
        staff,
        rx,
    }
}

/// Did the Lua body run since the last call? Resets the observation.
fn ran(world: &mut World) -> bool {
    let seen = world
        .get_resource::<EntityVariableCache>()
        .is_some_and(|c| c.get(EntityType::Mob, 99, 1, "ran").is_some());
    world.insert_resource(EntityVariableCache::default());
    seen
}

fn load(f: &mut Fixture) -> bool {
    crate::triggers::fire_event(&mut f.world, f.mob, TriggerEvent::Load);
    ran(&mut f.world)
}

fn attached(f: &Fixture, e: Entity) -> Vec<(i32, i32)> {
    f.world
        .get::<AttachedTriggers>(e)
        .map(|a| a.0.clone())
        .unwrap_or_default()
}

#[test]
fn attach_makes_a_trigger_fire_and_detach_stops_it() {
    let mut f = fixture();
    assert!(!load(&mut f), "nothing attached yet");

    dispatch(&mut f.world, f.staff, "attach mtr 99:1 sleeper");
    let out = drain(&mut f.rx);
    assert!(
        out.contains("Trigger 99:1 (guard greeter) attached to sleeper."),
        "{out}"
    );
    assert_eq!(attached(&f, f.mob), vec![(99, 1)]);
    assert!(load(&mut f), "the attached trigger fires");

    dispatch(&mut f.world, f.staff, "detach mob sleeper 99:1");
    assert!(drain(&mut f.rx).contains("Trigger removed."));
    assert!(f.world.get::<AttachedTriggers>(f.mob).is_none());
    assert!(!load(&mut f), "the detached trigger no longer fires");
}

#[test]
fn attach_refuses_a_duplicate_a_missing_trigger_and_a_wrong_type() {
    let mut f = fixture();
    dispatch(&mut f.world, f.staff, "attach mtr 99:1 sleeper");
    drain(&mut f.rx);
    dispatch(&mut f.world, f.staff, "attach mtr 99:1 sleeper");
    assert!(drain(&mut f.rx).contains("already attached"));
    dispatch(&mut f.world, f.staff, "attach mtr 99:77 sleeper");
    assert!(drain(&mut f.rx).contains("That trigger does not exist."));
    dispatch(&mut f.world, f.staff, "attach mtr 99:3 sleeper");
    assert!(drain(&mut f.rx).contains("attach it with 'otr'"));
    dispatch(&mut f.world, f.staff, "attach mtr 99:2 nobody");
    assert!(drain(&mut f.rx).contains("That mob does not exist."));
    assert_eq!(attached(&f, f.mob), vec![(99, 1)]);
}

#[test]
fn attach_position_orders_the_list() {
    let mut f = fixture();
    dispatch(&mut f.world, f.staff, "attach mtr 99:1 sleeper");
    dispatch(&mut f.world, f.staff, "attach mtr 99:2 sleeper 0");
    assert_eq!(attached(&f, f.mob), vec![(99, 2), (99, 1)]);
}

#[test]
fn attach_to_objects_and_rooms() {
    let mut f = fixture();
    let lamp = f
        .world
        .spawn((
            Item,
            Named {
                name: "a brass lamp".into(),
            },
            Keywords(vec!["lamp".into()]),
            Located(f.room),
        ))
        .id();
    dispatch(&mut f.world, f.staff, "attach otr 99:3 lamp");
    assert!(drain(&mut f.rx).contains("attached to a brass lamp"));
    assert_eq!(attached(&f, lamp), vec![(99, 3)]);

    dispatch(&mut f.world, f.staff, "attach wtr 99:4 99:5");
    assert!(drain(&mut f.rx).contains("attached to room 99:5"));
    assert_eq!(attached(&f, f.room), vec![(99, 4)]);

    dispatch(&mut f.world, f.staff, "detach room all");
    assert!(drain(&mut f.rx).contains("All triggers removed from room."));
    assert!(attached(&f, f.room).is_empty());

    // Short form finds the object itself.
    dispatch(&mut f.world, f.staff, "detach lamp 1");
    assert!(drain(&mut f.rx).contains("Trigger removed."));
    assert!(attached(&f, lamp).is_empty());
}

#[test]
fn detach_by_position_name_and_all() {
    let mut f = fixture();
    for t in ["99:1", "99:2"] {
        dispatch(&mut f.world, f.staff, &format!("attach mtr {t} sleeper"));
    }
    drain(&mut f.rx);
    dispatch(&mut f.world, f.staff, "detach mob sleeper 9");
    assert!(drain(&mut f.rx).contains("That trigger was not found."));
    dispatch(&mut f.world, f.staff, "detach mob sleeper hook");
    assert!(drain(&mut f.rx).contains("Trigger removed."));
    assert_eq!(attached(&f, f.mob), vec![(99, 1)]);
    dispatch(&mut f.world, f.staff, "attach mtr 99:2 sleeper");
    dispatch(&mut f.world, f.staff, "detach mob sleeper 1");
    assert_eq!(attached(&f, f.mob), vec![(99, 2)]);
    dispatch(&mut f.world, f.staff, "detach mob sleeper all");
    assert!(drain(&mut f.rx).contains("All triggers removed from sleeper."));
    assert!(f.world.get::<AttachedTriggers>(f.mob).is_none());
    dispatch(&mut f.world, f.staff, "detach mob sleeper all");
    assert!(drain(&mut f.rx).contains("That mob doesn't have any triggers."));
}

#[test]
fn attach_is_implementor_only() {
    let mut f = fixture();
    f.world.get_mut::<Account>(f.staff).unwrap().role = UserRole::Coder;
    dispatch(&mut f.world, f.staff, "attach mtr 99:1 sleeper");
    assert!(drain(&mut f.rx).contains("You can't do that."));
    assert!(attached(&f, f.mob).is_empty());
}

#[test]
fn parse_key_reads_composite_and_legacy_numbers() {
    use super::script_attach::parse_key;
    assert_eq!(parse_key("30:45"), Some((30, 45)));
    assert_eq!(parse_key("3045"), Some((30, 45)));
    assert_eq!(parse_key("45"), Some((1000, 45)));
    assert_eq!(parse_key("x"), None);
}
