//! Shared "fresh item from prototype" spawn. Every path that mints a
//! new copy of an `ObjectProto` (reset respawn, shop `buy`, shop
//! inspection) goes through [`spawn_item_from_proto`] so the full
//! proto-derived component set (flags, restrictions, triggers, hidden
//! state, decay timer, charges) is attached the same way everywhere.

use bevy_ecs::prelude::*;
use mud_world::{
    AttachedTriggers, Description, Item, Keywords, LiquidContainer, Located, Named, ObjectFlags,
    ObjectProto, ObjectRestrictions, TriggerCatalog, WorldKey,
};

/// Spawn one instance of `proto` located in `location` (a room, a
/// carrier, or a container) with every proto-derived component.
pub(crate) fn spawn_item_from_proto(
    world: &mut World,
    proto: &ObjectProto,
    location: Entity,
) -> Entity {
    let trigger_keys = world.get_resource::<TriggerCatalog>().and_then(|c| {
        c.object_attachments
            .get(&(proto.zone_id, proto.id))
            .cloned()
    });
    let primary_slot = mud_world::wear_flags_primary_slot(&proto.wear_flags);
    let mut bundle = world.spawn((
        Item,
        Named {
            name: proto.name.clone(),
        },
        Keywords(proto.keywords.clone()),
        WorldKey {
            zone: proto.zone_id,
            id: proto.id,
        },
        Located(location),
    ));
    if let Some(desc) = proto.examine_description.clone() {
        bundle.insert(Description(desc));
    }
    if let Some(s) = primary_slot {
        bundle.insert(mud_world::WearableIn(s));
    }
    if let Some(board_id) = proto.board_id {
        bundle.insert(mud_world::BoardLink(board_id));
    }
    if let Some(liq) = proto.liquid.clone() {
        bundle.insert(LiquidContainer {
            liquid: liq.liquid,
            capacity: liq.capacity,
            remaining: liq.remaining,
            poisoned: liq.poisoned,
        });
    }
    if let Some(fuel) = proto.light_fuel {
        bundle.insert(mud_world::LightFuel {
            capacity: fuel.capacity,
            remaining: fuel.remaining,
        });
    }
    if let Some(keys) = trigger_keys {
        bundle.insert(AttachedTriggers(keys));
    }
    if !proto.flags.is_empty() {
        bundle.insert(ObjectFlags(proto.flags.clone()));
    }
    if !proto.restrictions.is_empty() {
        bundle.insert(ObjectRestrictions(proto.restrictions.clone()));
    }
    if let Some(h) = proto.initial_hiddenness() {
        bundle.insert(h);
    }
    let spawned = bundle.id();
    mud_world::attach_proto_charges(world, spawned, proto.zone_id, proto.id);
    crate::item_decay::attach_timer_if_decaying(world, spawned, proto);
    spawned
}
