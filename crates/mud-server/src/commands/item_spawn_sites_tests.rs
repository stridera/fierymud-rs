//! Every inline item-spawn site (login inventory, admin spawn / `loadobj`,
//! quest reward, house items, account chest, Lua `spawn_object`) builds the
//! item through `item_spawn::spawn_item_from_proto`, so each gets the full
//! proto-derived component set. Saved per-instance state (charges, liquid,
//! fuel) is applied on top and wins over the prototype. Test-only.

use bevy_ecs::prelude::*;
use mud_db::character_items::CharacterItemRow;
use mud_db::enums::{ObjectFlag, ObjectRestriction, ObjectType};
use mud_world::{
    AttachedTriggers, Charges, Item, ItemTimer, Located, ObjectFlags, ObjectPrototypes,
    ObjectRestrictions, Room, TriggerCatalog, WorldKey, WorldKeyIndex,
};

use super::test_support::{drain, object_proto, player_in};
use crate::item_decay::SECS_PER_MUD_HOUR;

const KEY: (i32, i32) = (30, 7);

/// A world holding one wand prototype with flags, restrictions, a trigger,
/// a 2-hour lifetime and 3 starting charges.
fn world_with_wand() -> (World, Entity) {
    let mut world = World::new();
    world.insert_resource(WorldKeyIndex::default());
    let mut proto = object_proto(KEY.0, KEY.1, ObjectType::Wand);
    proto.name = "a flagged wand".into();
    proto.flags = vec![ObjectFlag::Soulbound];
    proto.restrictions = vec![ObjectRestriction::NoDrop];
    proto.timer_hours = 2;
    let mut protos = ObjectPrototypes::default();
    protos.by_key.insert(KEY, proto);
    world.insert_resource(protos);
    let mut bindings = mud_world::ObjectAbilityCatalog::default();
    bindings.by_key.insert(
        KEY,
        vec![mud_world::resources::ObjectAbilityBinding {
            ability_id: 1,
            level: 10,
            charges: Some(3),
        }],
    );
    world.insert_resource(bindings);
    let mut triggers = TriggerCatalog::default();
    triggers.object_attachments.insert(KEY, vec![(30, 99)]);
    world.insert_resource(triggers);
    let room = world.spawn((Room, WorldKey { zone: 30, id: 1 })).id();
    world
        .resource_mut::<WorldKeyIndex>()
        .rooms
        .insert((30, 1), room);
    (world, room)
}

fn items_of(world: &mut World, holder: Entity) -> Vec<Entity> {
    let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
    q.iter(world)
        .filter(|(_, l)| l.0 == holder)
        .map(|(e, _)| e)
        .collect()
}

fn assert_full_component_set(world: &World, item: Entity) {
    assert!(
        world
            .get::<ObjectFlags>(item)
            .is_some_and(|f| f.has(ObjectFlag::Soulbound)),
        "flags"
    );
    assert!(
        world
            .get::<ObjectRestrictions>(item)
            .is_some_and(|r| r.has(ObjectRestriction::NoDrop)),
        "restrictions"
    );
    assert_eq!(
        world.get::<AttachedTriggers>(item).map(|t| t.0.clone()),
        Some(vec![(30, 99)]),
        "triggers"
    );
    assert!(world.get::<ItemTimer>(item).is_some(), "decay timer");
}

fn row(id: i32, charges: i32) -> CharacterItemRow {
    CharacterItemRow {
        id,
        character_id: "c".into(),
        object_zone_id: KEY.0,
        object_id: KEY.1,
        container_id: None,
        equipped_location: None,
        charges,
        liquid_remaining: 0,
        liquid_type: None,
        lit: false,
        custom_name: None,
        custom_examine_description: None,
        custom_keywords: None,
        custom_curse: None,
        custom_container: None,
    }
}

#[test]
fn login_restores_saved_charges_and_keeps_one_proto_timer_and_one_trigger_set() {
    let (mut world, room) = world_with_wand();
    let player = world.spawn((mud_world::Player, Located(room))).id();
    // Row 1 was half-spent (1 of 3); row 2 sits at the schema default -1.
    let rows = [row(1, 1), row(2, -1)];
    assert_eq!(crate::login::spawn_inventory(&mut world, player, &rows), 2);
    let items = items_of(&mut world, player);
    assert_eq!(items.len(), 2);
    let charges_of = |w: &World, id: i32| {
        items
            .iter()
            .find(|e| w.get::<mud_world::PersistedItemId>(**e).map(|p| p.0) == Some(id))
            .and_then(|e| w.get::<Charges>(*e))
            .map(|c| c.0)
    };
    assert_eq!(
        charges_of(&world, 1),
        Some(1),
        "saved charges beat the proto"
    );
    assert_eq!(charges_of(&world, 2), Some(3), "no saved value: proto pool");
    for item in items {
        assert_full_component_set(&world, item);
        // Instance timers are not persisted: a restored decaying item
        // restarts exactly the proto's clock, once.
        assert_eq!(
            world.get::<ItemTimer>(item).map(|t| t.remaining_secs),
            Some(2 * SECS_PER_MUD_HOUR)
        );
    }
}

#[test]
fn login_applies_saved_liquid_over_the_proto_fill() {
    let (mut world, room) = world_with_wand();
    {
        let mut protos = world.resource_mut::<ObjectPrototypes>();
        let proto = protos.by_key.get_mut(&KEY).unwrap();
        proto.r#type = ObjectType::Drinkcontainer;
        proto.liquid = Some(mud_world::resources::LiquidProto {
            liquid: "WATER".into(),
            capacity: 10,
            remaining: 10,
            poisoned: false,
        });
    }
    let player = world.spawn((mud_world::Player, Located(room))).id();
    let mut r = row(1, -1);
    r.liquid_type = Some("WINE".into());
    r.liquid_remaining = 4;
    assert_eq!(crate::login::spawn_inventory(&mut world, player, &[r]), 1);
    let item = items_of(&mut world, player)[0];
    let lc = world.get::<mud_world::LiquidContainer>(item).unwrap();
    assert_eq!((lc.liquid.as_str(), lc.remaining), ("WINE", 4));
}

#[test]
fn an_admin_spawned_item_has_flags_restrictions_triggers_and_timer() {
    let (mut world, room) = world_with_wand();
    crate::admin::spawn_into(&mut world, "object", KEY.0, KEY.1, 30, 1).unwrap();
    let items = items_of(&mut world, room);
    assert_eq!(items.len(), 1);
    assert_full_component_set(&world, items[0]);
    assert_eq!(world.get::<Charges>(items[0]).map(|c| c.0), Some(3));
}

#[test]
fn a_loadobj_item_has_flags_restrictions_triggers_and_timer() {
    let (mut world, room) = world_with_wand();
    let (player, mut rx) = player_in(&mut world, room);
    super::admin_world::cmd_loadobj(&mut world, player, "30 7");
    assert!(drain(&mut rx).contains("Loaded"));
    let items = items_of(&mut world, player);
    assert_eq!(items.len(), 1);
    assert_full_component_set(&world, items[0]);
    assert_eq!(world.get::<Charges>(items[0]).map(|c| c.0), Some(3));
}

#[test]
fn a_house_item_has_the_full_set_and_a_stored_charge_count_wins() {
    let (mut world, room) = world_with_wand();
    let custom = mud_db::housing::HouseItemCustom {
        charges: Some(1),
        ..Default::default()
    };
    super::spawn_house_item(&mut world, 5, KEY.0, KEY.1, room, &custom);
    let items = items_of(&mut world, room);
    assert_eq!(items.len(), 1);
    assert_full_component_set(&world, items[0]);
    assert!(world.get::<mud_world::HouseItem>(items[0]).is_some());
    assert_eq!(world.get::<Charges>(items[0]).map(|c| c.0), Some(1));
}

#[test]
fn a_lua_spawned_object_has_the_full_component_set() {
    let (mut world, room) = world_with_wand();
    let (player, _rx) = player_in(&mut world, room);
    let mut host = mud_script::LuaHost::default();
    host.exec_for_listener_with_extras(&mut world, player, player, "self:spawn_object(30, 7)", &[])
        .expect("script runs");
    let items = items_of(&mut world, player);
    assert_eq!(items.len(), 1);
    assert_full_component_set(&world, items[0]);
    assert_eq!(world.get::<Charges>(items[0]).map(|c| c.0), Some(3));
}
