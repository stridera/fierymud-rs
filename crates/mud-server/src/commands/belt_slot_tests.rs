//! Issue #92: waist and belt (legacy `WEAR_WAIST` / `WEAR_OBELT`) are two
//! different slots, and a belt item needs a waist item worn. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole, WearFlag};
use mud_world::{
    Account, EquippedSlot, Exits, Item, Keywords, Located, Named, ObjectPrototypes, Room, Slot,
    WearableIn, WorldKey, wear_flags_primary_slot,
};

use super::dispatch;
use super::test_support::{Rx, drain, object_proto, player_in};

fn setup() -> (World, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(ObjectPrototypes::default());
    world.init_resource::<mud_world::ObjectAbilityCatalog>();
    let room = world
        .spawn((
            Room,
            Named {
                name: "A quiet hall".into(),
            },
            Exits::default(),
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
    (world, player, rx)
}

/// Carry a piece of gear whose prototype has `flags`.
fn gear(
    world: &mut World,
    holder: Entity,
    id: i32,
    name: &str,
    keyword: &str,
    flags: &[WearFlag],
) -> Entity {
    let mut proto = object_proto(1, id, ObjectType::Armor);
    proto.name = name.into();
    proto.keywords = vec![keyword.into()];
    proto.wear_flags = flags.to_vec();
    world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .insert((1, id), proto);
    let mut item = world.spawn((
        Item,
        Named { name: name.into() },
        Keywords(vec![keyword.into()]),
        WorldKey { zone: 1, id },
        Located(holder),
    ));
    if let Some(slot) = wear_flags_primary_slot(flags) {
        item.insert(WearableIn(slot));
    }
    item.id()
}

fn worn_in(world: &World, item: Entity) -> Option<Slot> {
    world.get::<EquippedSlot>(item).map(|e| e.0)
}

#[test]
fn waist_and_belt_flags_resolve_to_distinct_slots() {
    assert_eq!(
        wear_flags_primary_slot(&[WearFlag::Waist]),
        Some(Slot::Waist)
    );
    assert_eq!(wear_flags_primary_slot(&[WearFlag::Belt]), Some(Slot::Belt));
}

#[test]
fn a_belt_item_needs_a_waist_item_first() {
    let (mut world, p, mut rx) = setup();
    let dagger = gear(
        &mut world,
        p,
        1,
        "a belt dagger",
        "dagger",
        &[WearFlag::Belt],
    );
    dispatch(&mut world, p, "wear dagger");
    let out = drain(&mut rx);
    assert!(out.contains("You'll need to wear a belt first."), "{out}");
    assert_eq!(worn_in(&world, dagger), None);
}

#[test]
fn waist_item_and_belt_item_are_worn_together() {
    let (mut world, p, mut rx) = setup();
    let girdle = gear(
        &mut world,
        p,
        1,
        "a leather girdle",
        "girdle",
        &[WearFlag::Waist],
    );
    let pouch = gear(&mut world, p, 2, "a belt pouch", "pouch", &[WearFlag::Belt]);
    dispatch(&mut world, p, "wear girdle");
    dispatch(&mut world, p, "wear pouch");
    let out = drain(&mut rx);
    assert_eq!(worn_in(&world, girdle), Some(Slot::Waist), "{out}");
    assert_eq!(worn_in(&world, pouch), Some(Slot::Belt), "{out}");
    assert!(
        out.contains("You attach a belt pouch to your belt."),
        "{out}"
    );
}

#[test]
fn removing_the_belt_drops_what_hangs_from_it() {
    let (mut world, p, mut rx) = setup();
    let girdle = gear(
        &mut world,
        p,
        1,
        "a leather girdle",
        "girdle",
        &[WearFlag::Waist],
    );
    let pouch = gear(&mut world, p, 2, "a belt pouch", "pouch", &[WearFlag::Belt]);
    dispatch(&mut world, p, "wear girdle");
    dispatch(&mut world, p, "wear pouch");
    drain(&mut rx);
    dispatch(&mut world, p, "remove girdle");
    let out = drain(&mut rx);
    assert_eq!(worn_in(&world, girdle), None, "{out}");
    assert_eq!(worn_in(&world, pouch), None, "{out}");
    assert!(out.contains("falls off as you remove your belt"), "{out}");
    assert_eq!(world.get::<Located>(pouch).map(|l| l.0), Some(p));
}

#[test]
fn remove_all_takes_the_belt_item_off_normally() {
    // Legacy `do_remove` walks the slots and strips `WEAR_OBELT` in the
    // loop like any other item; it does not "fall off" with the belt.
    let (mut world, p, mut rx) = setup();
    let girdle = gear(
        &mut world,
        p,
        1,
        "a leather girdle",
        "girdle",
        &[WearFlag::Waist],
    );
    let pouch = gear(&mut world, p, 2, "a belt pouch", "pouch", &[WearFlag::Belt]);
    dispatch(&mut world, p, "wear girdle");
    dispatch(&mut world, p, "wear pouch");
    drain(&mut rx);
    dispatch(&mut world, p, "remove all");
    let out = drain(&mut rx);
    assert_eq!(worn_in(&world, girdle), None, "{out}");
    assert_eq!(worn_in(&world, pouch), None, "{out}");
    assert!(out.contains("You remove a belt pouch"), "{out}");
    assert!(out.contains("You remove a leather girdle"), "{out}");
    assert!(!out.contains("falls off as you remove your belt"), "{out}");
    assert!(
        out.find("You remove a belt pouch") < out.find("You remove a leather girdle"),
        "belt item comes off before the waist item: {out}"
    );
}

#[test]
fn remove_all_fires_the_remove_trigger_on_a_belt_item() {
    use mud_world::{AttachedTriggers, TriggerAttach, TriggerCatalog, TriggerDef, TriggerEvent};
    let (mut world, p, mut rx) = setup();
    let mut catalog = TriggerCatalog::default();
    catalog.by_key.insert(
        (99, 1),
        TriggerDef {
            zone_id: 99,
            id: 1,
            name: "t".to_string(),
            attach_type: TriggerAttach::Object,
            commands: "return".to_string(),
            flags: vec![TriggerEvent::Remove],
            arg_list: vec![],
            num_args: 0,
        },
    );
    world.insert_resource(catalog);
    world.insert_resource(mud_script::LuaHost::new());
    let girdle = gear(
        &mut world,
        p,
        1,
        "a leather girdle",
        "girdle",
        &[WearFlag::Waist],
    );
    let pouch = gear(&mut world, p, 2, "a belt pouch", "pouch", &[WearFlag::Belt]);
    world
        .entity_mut(pouch)
        .insert(AttachedTriggers(vec![(99, 1)]));
    dispatch(&mut world, p, "wear girdle");
    dispatch(&mut world, p, "wear pouch");
    drain(&mut rx);
    dispatch(&mut world, p, "remove all");
    let out = drain(&mut rx);
    assert_eq!(worn_in(&world, girdle), None, "{out}");
    assert_eq!(worn_in(&world, pouch), None, "{out}");
    let fired = world
        .get_resource::<crate::triggers::TriggerStats>()
        .and_then(|s| s.by_event.get("Remove"))
        .map_or(0, |c| c.fired);
    assert_eq!(fired, 1, "belt item's Remove trigger fires once: {out}");
}

#[test]
fn any_unapply_of_the_waist_item_drops_the_belt_item() {
    // Quest take-items, mob unequip and the like call the shared
    // equip_apply entry points rather than `remove`.
    let (mut world, p, _rx) = setup();
    let girdle = gear(
        &mut world,
        p,
        1,
        "a leather girdle",
        "girdle",
        &[WearFlag::Waist],
    );
    let pouch = gear(&mut world, p, 2, "a belt pouch", "pouch", &[WearFlag::Belt]);
    dispatch(&mut world, p, "wear girdle");
    dispatch(&mut world, p, "wear pouch");
    crate::equip_apply::release_gear(&mut world, girdle);
    assert_eq!(worn_in(&world, pouch), None);
    assert_eq!(world.get::<Located>(pouch).map(|l| l.0), Some(p));
}
