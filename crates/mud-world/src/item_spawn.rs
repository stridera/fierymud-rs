//! Shared "fresh item from prototype" spawn. Every path that mints a
//! new copy of an `ObjectProto` (boot and respawn resets, shop `buy`,
//! admin `spawn` / `loadobj`, quest rewards, conjure spells, Lua
//! scripts, login inventory, account chest, house items) goes through
//! [`spawn_item_from_proto`] so the full proto-derived component set
//! (flags, restrictions, triggers, hidden state, decay timer, charges,
//! board link, liquid, light fuel) is attached the same way everywhere.
//!
//! Per-instance state (saved charges, liquid level, customisation,
//! container open/lock state, curse deltas) is NOT part of this helper:
//! callers apply it afterwards, so a saved value always wins over the
//! proto default. Container open/closed/locked needs no component at
//! spawn: an instance with no `ContainerState` reads it from its proto.

use bevy_ecs::prelude::*;
use mud_db::enums::ObjectFlag;

use crate::components::{
    AttachedTriggers, BoardLink, Description, Item, ItemTimer, Keywords, LiquidContainer, Located,
    Mob, Named, ObjectFlags, ObjectRestrictions, Player, WorldKey,
};
use crate::resources::{ObjectProto, TriggerCatalog};

/// Legacy MUD-hour to wall seconds. Matches the constant used by
/// effect duration resolution; centralized here to keep the timer
/// math grounded in one place.
pub const SECS_PER_MUD_HOUR: i32 = 75;

/// If the proto has a positive `timer_hours` AND the object isn't
/// flagged PERMANENT, attach an `ItemTimer` to `entity`. Replaces any
/// timer already on the entity.
pub fn attach_timer_if_decaying(world: &mut World, entity: Entity, proto: &ObjectProto) {
    if proto.timer_hours <= 0 {
        return;
    }
    if proto.flags.contains(&ObjectFlag::Permanent) {
        return;
    }
    let remaining = proto.timer_hours.saturating_mul(SECS_PER_MUD_HOUR);
    let decompose = proto
        .decompose_timer
        .saturating_mul(SECS_PER_MUD_HOUR)
        .max(0);
    let Ok(mut em) = world.get_entity_mut(entity) else {
        return;
    };
    em.insert(ItemTimer {
        remaining_secs: remaining,
        decompose_window_secs: decompose,
    });
}

/// True when `holder` is a player or mob, or sits (through any depth of
/// nested containers) inside one.
fn held_by_creature_chain(world: &World, holder: Entity) -> bool {
    let mut at = holder;
    for _ in 0..64 {
        if world.get::<Player>(at).is_some() || world.get::<Mob>(at).is_some() {
            return true;
        }
        let Some(next) = world.get::<Located>(at).map(|l| l.0) else {
            return false;
        };
        at = next;
    }
    false
}

/// Spawn one instance of `proto` located in `location` (a room, a
/// carrier, or a container) with every proto-derived component.
pub fn spawn_item_from_proto(world: &mut World, proto: &ObjectProto, location: Entity) -> Entity {
    let trigger_keys = world.get_resource::<TriggerCatalog>().and_then(|c| {
        c.object_attachments
            .get(&(proto.zone_id, proto.id))
            .cloned()
    });
    let primary_slot = crate::loader::wear_flags_primary_slot(&proto.wear_flags);
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
        bundle.insert(crate::components::WearableIn(s));
    }
    if let Some(board_id) = proto.board_id {
        bundle.insert(BoardLink(board_id));
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
        bundle.insert(crate::components::LightFuel {
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
    // Concealment only means something for an item lying in a room or in a
    // container there. One minted straight into a creature's hands, pack or
    // worn slot (login restore, chest withdraw, buy, conjure) is carried, and
    // legacy `unhide_object` clears hiddenness on pickup, so it would only
    // leave the item hidden even from its owner once dropped.
    let carried = held_by_creature_chain(bundle.world(), location);
    if !carried && let Some(h) = proto.initial_hiddenness() {
        bundle.insert(h);
    }
    let spawned = bundle.id();
    crate::reset_gear::attach_proto_charges(world, spawned, proto.zone_id, proto.id);
    attach_timer_if_decaying(world, spawned, proto);
    spawned
}
