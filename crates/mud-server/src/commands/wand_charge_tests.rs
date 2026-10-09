//! A wand spawned by a zone-reset style path (`fill_container`, the code the
//! reset, respawn and mob-gear paths share) starts with the prototype's
//! charges and runs dry. Before `attach_proto_charges` such a wand had no
//! `Charges` component, which reads as unlimited. Test-only.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::ObjectType;
use mud_db::object_reset_contents::ObjectResetContent;
use mud_world::{Charges, Item, Keywords, Located, ObjectContentsCatalog, WorldKey};

use super::blindness_tests::{MODIFY, caster_with_spells};
use super::dispatch;
use super::test_support::{drain, object_proto};

const WAND: i32 = 30;

#[test]
fn a_reset_spawned_wand_has_the_prototypes_charges_and_runs_out() {
    let (mut fx, p, mut rx) = caster_with_spells(vec![(
        1,
        "Zap",
        vec![(
            MODIFY,
            Some(serde_json::json!({
                "target": "hiddenness",
                "amount": "10",
                "duration": "1",
                "durationUnit": "hours"
            })),
        )],
    )]);
    let mut proto = object_proto(1, WAND, ObjectType::Wand);
    proto.name = "a zapping wand".into();
    proto.keywords = vec!["wand".into()];
    let mut protos = mud_world::ObjectPrototypes::default();
    protos.by_key.insert((1, WAND), proto);
    fx.world.insert_resource(protos);
    let mut bindings = mud_world::ObjectAbilityCatalog::default();
    bindings.by_key.insert(
        (1, WAND),
        vec![mud_world::resources::ObjectAbilityBinding {
            ability_id: 1,
            level: 10,
            charges: Some(2),
        }],
    );
    fx.world.insert_resource(bindings);
    let rows = vec![ObjectResetContent {
        id: 1,
        reset_id: 5,
        parent_content_id: None,
        object_zone_id: 1,
        object_id: WAND,
        quantity: 1,
        max_instances: 99,
    }];
    let entries = mud_world::reset_gear::build_content_entries(&rows)
        .remove(&5)
        .unwrap();
    fx.world.insert_resource(ObjectContentsCatalog::default());
    fx.world
        .resource_mut::<ObjectContentsCatalog>()
        .by_reset
        .insert(5, entries);
    let mut counts = HashMap::new();
    mud_world::fill_container(&mut fx.world, &[p], 5, &mut counts);

    let wand = {
        let mut q = fx
            .world
            .query_filtered::<(Entity, &WorldKey, &Located, &Keywords), With<Item>>();
        q.iter(&fx.world)
            .find(|(_, k, l, _)| k.id == WAND && l.0 == p)
            .map(|(e, ..)| e)
            .expect("the wand reached the player's pack")
    };
    assert_eq!(fx.world.get::<Charges>(wand).map(|c| c.0), Some(2));

    let mut zap = |fx: &mut super::gmcp_tests::Fx| {
        let _ = drain(&mut rx);
        dispatch(&mut fx.world, p, "wave wand caster");
        for _ in 0..10 {
            crate::casting::casting_tick(&mut fx.world);
        }
        drain(&mut rx)
    };
    let first = zap(&mut fx);
    assert_eq!(
        fx.world.get::<Charges>(wand).map(|c| c.0),
        Some(1),
        "first wave spends a charge: {first}"
    );
    let second = zap(&mut fx);
    assert!(
        fx.world.get_entity(wand).is_err(),
        "the last charge crumbles the wand: {second}"
    );
    assert!(second.contains("crumbles to dust"), "{second}");
}

fn wand_world() -> World {
    let mut world = World::new();
    let mut proto = object_proto(1, WAND, ObjectType::Wand);
    proto.name = "a zapping wand".into();
    proto.keywords = vec!["wand".into()];
    let mut protos = mud_world::ObjectPrototypes::default();
    protos.by_key.insert((1, WAND), proto);
    world.insert_resource(protos);
    let mut bindings = mud_world::ObjectAbilityCatalog::default();
    bindings.by_key.insert(
        (1, WAND),
        vec![mud_world::resources::ObjectAbilityBinding {
            ability_id: 1,
            level: 10,
            charges: Some(3),
        }],
    );
    world.insert_resource(bindings);
    world
}

#[test]
fn an_emptied_wand_placed_in_a_house_stays_empty_on_reload() {
    let mut world = wand_world();
    let wand = world
        .spawn((
            Item,
            mud_world::Named {
                name: "a zapping wand".into(),
            },
            WorldKey { zone: 1, id: WAND },
            Charges(0),
        ))
        .id();
    let custom = super::house_item_custom(&world, wand);
    assert_eq!(custom.charges, Some(0));
    // Through the stored row: the JSON the house item keeps.
    let stored = mud_db::housing::HouseItemCustom::from_columns(None, None, &custom.values());
    assert_eq!(stored.charges, Some(0));

    // A fresh world is the reload: prototype (3 charges) plus the stored row.
    let mut fresh = wand_world();
    let room = fresh.spawn_empty().id();
    super::spawn_house_item(&mut fresh, 5, 1, WAND, room, &stored);
    let reloaded = {
        let mut q = fresh.query_filtered::<Entity, With<mud_world::HouseItem>>();
        q.single(&fresh).unwrap()
    };
    assert_eq!(fresh.get::<Charges>(reloaded).map(|c| c.0), Some(0));

    // A row without a stored count still gets the prototype's pool.
    let plain = mud_db::housing::HouseItemCustom::default();
    assert_eq!(plain.values(), serde_json::json!({}));
    let room2 = fresh.spawn_empty().id();
    super::spawn_house_item(&mut fresh, 6, 1, WAND, room2, &plain);
    let second = {
        let mut q = fresh.query_filtered::<(Entity, &mud_world::HouseItem), With<Item>>();
        q.iter(&fresh)
            .find(|(_, h)| h.0 == 6)
            .map(|(e, _)| e)
            .unwrap()
    };
    assert_eq!(fresh.get::<Charges>(second).map(|c| c.0), Some(3));
}

#[test]
fn a_wand_withdrawn_from_the_chest_without_a_stored_count_gets_the_prototypes() {
    use super::account_chest::spawn_withdrawn_item;
    let mut world = wand_world();
    let room = world.spawn_empty().id();
    let p = world.spawn((mud_world::Player, Located(room))).id();
    let row = |custom: Option<serde_json::Value>| mud_db::account_items::AccountItemRow {
        id: 1,
        user_id: String::new(),
        slot: 0,
        object_zone_id: 1,
        object_id: WAND,
        quantity: 1,
        custom_data: custom,
        stored_by_character_id: None,
        stored_at: chrono::Utc::now().naive_utc(),
    };
    spawn_withdrawn_item(&mut world, p, &row(None), 77);
    spawn_withdrawn_item(
        &mut world,
        p,
        &row(Some(serde_json::json!({"charges": 1}))),
        78,
    );
    let mut counts: Vec<i32> = {
        let mut q = world.query_filtered::<&Charges, With<Item>>();
        q.iter(&world).map(|c| c.0).collect()
    };
    counts.sort_unstable();
    assert_eq!(
        counts,
        vec![1, 3],
        "stored count wins, else the prototype's"
    );
}
