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
