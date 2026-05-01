//! Mob respawn tick. Walks `MobResetCatalog` periodically and refills
//! any reset that's below `max_instances`. Each refill spawns a fresh
//! mob with the same shape Pass 5 of the loader produces, plus the
//! `FromMobReset` tag so the next tick can count it.
//!
//! Cycle pacing is `RESPAWN_PERIOD_TICKS` ticks (currently 1 minute at
//! 10 Hz); a future enhancement could read the `reset_behavior` text
//! per-row to differentiate PERSISTENT from ONCE etc., but the runtime
//! today treats every row as PERSISTENT.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_world::{
    AttachedTriggers, CombatStats, Description, FromMobReset, Health, Keywords, Located, Mob,
    MobPrototypes, MobResetCatalog, Mountable, Named, Posture, PostureKind, ShopCatalog,
    Shopkeeper, TriggerCatalog, WorldKey,
};
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

    // Snapshot live counts per reset_id by walking entities tagged with
    // FromMobReset. Doing it once up front avoids re-querying inside the
    // refill loop.
    let mut counts: HashMap<i32, i32> = HashMap::new();
    {
        let mut q = world.query::<&FromMobReset>();
        for r in q.iter(world) {
            *counts.entry(r.0).or_insert(0) += 1;
        }
    }

    // Snapshot the catalog so we can mutate the world without holding a
    // borrow on the resource. Each entry owns its data; cloning is cheap
    // (a few i32s + Entity).
    let entries: Vec<mud_world::MobResetEntry> =
        world.resource::<MobResetCatalog>().entries.clone();

    let mut refilled = 0usize;
    for entry in &entries {
        let live = counts.get(&entry.reset_id).copied().unwrap_or(0);
        let want = entry.max_instances.max(1);
        if live >= want {
            continue;
        }
        let proto = world
            .resource::<MobPrototypes>()
            .by_key
            .get(&(entry.mob_zone_id, entry.mob_id))
            .cloned();
        let Some(proto) = proto else { continue };
        let hp = proto.rolled_hp();
        let dmg = proto.avg_damage();
        let shop_key = world
            .resource::<ShopCatalog>()
            .keeper_index
            .get(&(proto.zone_id, proto.id))
            .copied();
        let trigger_keys = world
            .resource::<TriggerCatalog>()
            .mob_attachments
            .get(&(proto.zone_id, proto.id))
            .cloned();
        for _ in 0..(want - live) {
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
            refilled += 1;
        }
    }

    if refilled > 0 {
        info!(refilled, "respawn tick");
    }
}
