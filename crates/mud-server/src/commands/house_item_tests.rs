//! Items stored in a player house keep their per-instance state (label,
//! enchantment, curse) across a save/reload. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{Alignment, ObjectFlag, ObjectRestriction, ObjectType};
use mud_db::housing::HouseItemCustom;
use mud_world::components::{ItemApplies, ItemBarredAlignments};
use mud_world::{
    Item, ItemCustomization, Keywords, Located, Named, ObjectFlags, ObjectPrototypes,
    ObjectRestrictions, WorldKey,
};

use super::test_support::object_proto;
use super::{house_item_custom, spawn_house_item};

fn world_with_sword() -> World {
    let mut world = World::new();
    let mut protos = ObjectPrototypes::default();
    let mut proto = object_proto(30, 7, ObjectType::Weapon);
    proto.name = "a plain sword".into();
    proto.keywords = vec!["sword".into()];
    proto.restrictions = vec![ObjectRestriction::NoSell];
    protos.by_key.insert((30, 7), proto);
    world.insert_resource(protos);
    world
}

/// A sword that has been enchanted, cursed and labeled in play.
fn enchanted_sword(world: &mut World) -> Entity {
    let item = world
        .spawn((
            Item,
            Named {
                name: "a plain sword".into(),
            },
            Keywords(vec!["sword".into()]),
            WorldKey { zone: 30, id: 7 },
            ObjectRestrictions(vec![ObjectRestriction::NoSell, ObjectRestriction::NoDrop]),
            ItemApplies(vec![("accuracy".into(), 2), ("attack_power".into(), 5)]),
            ItemBarredAlignments(vec![Alignment::Evil]),
        ))
        .id();
    crate::item_alter::add_flag(world, item, ObjectFlag::Magic);
    crate::item_alter::mark_dirty(world, item);
    crate::item_custom::install(
        world,
        item,
        ItemCustomization {
            name: Some("a plain sword (Grim)".into()),
            examine: Some("It hums.".into()),
            keywords: Some(vec!["sword".into(), "grim".into()]),
            dirty: true,
        },
    );
    item
}

#[test]
fn stored_columns_round_trip_the_instance_state() {
    let mut world = world_with_sword();
    let item = enchanted_sword(&mut world);
    let custom = house_item_custom(&world, item);
    assert_eq!(custom.name.as_deref(), Some("a plain sword (Grim)"));
    let alter = custom.alter.clone().expect("alter captured");
    assert_eq!(alter.applies.len(), 2);

    // What the DB row holds: two text columns and the JSON.
    let back = HouseItemCustom::from_columns(
        custom.name.clone(),
        custom.examine.clone(),
        &custom.values(),
    );
    assert_eq!(back, custom);
}

#[test]
fn enchanted_item_reloaded_from_the_house_keeps_its_enchant_once() {
    let mut world = world_with_sword();
    let item = enchanted_sword(&mut world);
    let custom = house_item_custom(&world, item);
    let before = crate::item_alter::snapshot(&world, item);

    // A fresh world is the "reload": only the prototype plus the stored row.
    let mut fresh = world_with_sword();
    let room = fresh.spawn_empty().id();
    spawn_house_item(&mut fresh, 11, 30, 7, room, &custom);
    let reloaded = {
        let mut q = fresh.query_filtered::<Entity, With<mud_world::HouseItem>>();
        q.single(&fresh).unwrap()
    };

    assert_eq!(fresh.get::<Located>(reloaded).unwrap().0, room);
    assert_eq!(
        fresh.get::<Named>(reloaded).unwrap().name,
        "a plain sword (Grim)"
    );
    let applies = &fresh.get::<ItemApplies>(reloaded).unwrap().0;
    assert_eq!(
        applies,
        &vec![("accuracy".to_string(), 2), ("attack_power".to_string(), 5)],
        "enchant applied once, not doubled"
    );
    let flags = &fresh.get::<ObjectFlags>(reloaded).unwrap().0;
    assert_eq!(
        flags.iter().filter(|f| **f == ObjectFlag::Magic).count(),
        1,
        "{flags:?}"
    );
    assert_eq!(
        fresh.get::<ItemBarredAlignments>(reloaded).unwrap().0,
        vec![Alignment::Evil]
    );
    let r = &fresh.get::<ObjectRestrictions>(reloaded).unwrap().0;
    assert!(r.contains(&ObjectRestriction::NoDrop) && r.contains(&ObjectRestriction::NoSell));
    assert_eq!(crate::item_alter::snapshot(&fresh, reloaded), before);

    // Stored again and reloaded again: still the same single enchant.
    let again = house_item_custom(&fresh, reloaded);
    let mut third = world_with_sword();
    let room3 = third.spawn_empty().id();
    spawn_house_item(&mut third, 12, 30, 7, room3, &again);
    let e3 = {
        let mut q = third.query_filtered::<Entity, With<mud_world::HouseItem>>();
        q.single(&third).unwrap()
    };
    assert_eq!(crate::item_alter::snapshot(&third, e3), before);
}

#[test]
fn a_plain_item_has_no_stored_state() {
    let mut world = world_with_sword();
    let item = world
        .spawn((
            Item,
            Named {
                name: "a plain sword".into(),
            },
            WorldKey { zone: 30, id: 7 },
            ObjectRestrictions(vec![ObjectRestriction::NoSell]),
        ))
        .id();
    let custom = house_item_custom(&world, item);
    assert_eq!(custom, HouseItemCustom::default());
    assert_eq!(custom.values(), serde_json::json!({}));
}
