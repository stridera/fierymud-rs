//! Mob respawn tick. Walks `MobResetCatalog` periodically and spawns
//! exactly one mob per reset row, but only when the live world-count
//! of that proto is below the row's `max_instances` cap. The cap is a
//! global ceiling on how many of THIS prototype can exist anywhere
//! at once (the legacy `CircleMUD` `max_existing` semantic), not a
//! per-row instance count.
//!
//! Cycle pacing is `RESPAWN_PERIOD_TICKS` ticks (currently 6 seconds
//! at 10 Hz); a future enhancement could read the `reset_behavior`
//! text per-row to differentiate PERSISTENT from ONCE etc., but the
//! runtime today treats every row as PERSISTENT.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_world::{
    AttachedTriggers, CombatStats, Description, Health, Keywords, Located, Mob, MobPrototypes,
    MobResetCatalog, Mountable, Named, Posture, PostureKind, ShopCatalog, Shopkeeper,
    TriggerCatalog, WorldKey,
};
use mud_world::FromMobReset;
use tracing::info;

use crate::TickCount;

/// One refill cycle every 60 game ticks (= 6 seconds at 10 Hz).
/// Plenty often for testing; players' perception of "respawn" is
/// minutes anyway, but the actual cap on refills is `max_instances`
/// per reset so the cadence just controls how snappy a kill feels.
const RESPAWN_PERIOD_TICKS: u64 = 60;

#[allow(clippy::needless_pass_by_value)]
pub fn respawn_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(RESPAWN_PERIOD_TICKS) {
        return;
    }

    // Snapshot live world-counts per (zone, id) so the cap check is
    // global rather than per-reset-row. Walks every Mob entity once
    // up front; faster than re-querying inside the refill loop.
    let mut world_counts: HashMap<(i32, i32), i32> = HashMap::new();
    {
        let mut q = world.query_filtered::<&WorldKey, With<Mob>>();
        for wk in q.iter(world) {
            *world_counts.entry((wk.zone, wk.id)).or_insert(0) += 1;
        }
    }

    // Snapshot the catalog so we can mutate the world without holding a
    // borrow on the resource. Each entry owns its data; cloning is cheap
    // (a few i32s + Entity).
    let entries: Vec<mud_world::MobResetEntry> =
        world.resource::<MobResetCatalog>().entries.clone();

    let mut refilled = 0usize;
    // Track every freshly-spawned mob carrying triggers so the
    // dispatcher can fire LOAD on them after the respawn loop
    // exits — firing inside the loop would re-borrow World mid-spawn.
    let mut load_fire_queue: Vec<Entity> = Vec::new();
    for entry in &entries {
        let proto_key = (entry.mob_zone_id, entry.mob_id);
        let live = world_counts.get(&proto_key).copied().unwrap_or(0);
        let cap = entry.max_instances.max(1);
        if live >= cap {
            continue;
        }
        let proto = world.resource::<MobPrototypes>().by_key.get(&proto_key).cloned();
        let Some(proto) = proto else { continue };
        let hp = proto.rolled_hp();
        let dmg = proto.avg_damage();
        let shop_key = world
            .resource::<ShopCatalog>()
            .keeper_index
            .get(&proto_key)
            .copied();
        let trigger_keys = world
            .resource::<TriggerCatalog>()
            .mob_attachments
            .get(&proto_key)
            .cloned();
        // One spawn per reset row (only when the cap allows). The
        // running `world_counts` is incremented locally so subsequent
        // reset rows for the same proto see the new count and stop
        // when full.
        let mut em = world.spawn((
            Mob,
            Named { name: proto.name.clone() },
            Keywords(proto.keywords.clone()),
            Description(proto.room_description.clone()),
            WorldKey { zone: proto.zone_id, id: proto.id },
            Located(entry.room_entity),
            Health { hp, max: hp },
            CombatStats {
                hit_roll: proto.hit_roll,
                dmg_roll: dmg,
                ac: proto.armor_class,
                alignment: proto.alignment,
            },
            Posture(PostureKind::Standing),
            FromMobReset(entry.reset_id),
        ));
        if let Some((shop_zone_id, shop_id)) = shop_key {
            em.insert(Shopkeeper { shop_zone_id, shop_id });
        }
        if let Some(ref keys) = trigger_keys {
            em.insert(AttachedTriggers(keys.clone()));
            load_fire_queue.push(em.id());
        }
        if proto.keywords.iter().any(|k| {
            let lc = k.to_ascii_lowercase();
            lc.contains("horse")
                || lc.contains("steed")
                || lc.contains("mount")
                || lc.contains("donkey")
                || lc.contains("mare")
                || lc.contains("nightmare")
        }) {
            em.insert(Mountable);
        }
        *world_counts.entry(proto_key).or_insert(0) += 1;
        refilled += 1;
    }

    if refilled > 0 {
        info!(refilled, "respawn tick");
    }

    // Fire LOAD triggers for the just-spawned mobs. The respawn loop
    // queued any mob that received an AttachedTriggers component;
    // firing here (after the loop ends) keeps World borrows simple.
    for e in load_fire_queue {
        crate::triggers::fire_event(world, e, mud_world::TriggerEvent::Load);
    }
}
