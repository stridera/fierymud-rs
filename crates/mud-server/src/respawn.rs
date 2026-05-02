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
    AttachedTriggers, CombatStats, Description, Health, Item, Keywords, LiquidContainer, Located,
    Mob, MobPrototypes, MobResetCatalog, Mountable, Named, ObjectPrototypes, ObjectResetCatalog,
    Posture, PostureKind, ShopCatalog, Shopkeeper, TriggerCatalog, WorldKey,
};
use mud_world::{FromMobReset, FromObjectReset};
use tracing::info;

use crate::TickCount;
use crate::commands::broadcast_room_except_players_rendered;

/// One refill cycle every 60 game ticks (= 6 seconds at 10 Hz).
/// Plenty often for testing; players' perception of "respawn" is
/// minutes anyway, but the actual cap on refills is `max_instances`
/// per reset so the cadence just controls how snappy a kill feels.
const RESPAWN_PERIOD_TICKS: u64 = 60;

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn respawn_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(RESPAWN_PERIOD_TICKS) {
        return;
    }

    // Snapshot live world-counts per (zone, id) for the global cap
    // check, plus the set of reset_ids whose mob is alive. Same
    // semantic as the object respawn pass: each MobResets row owns
    // at most one live instance — we only re-fire a row when its
    // mob is gone. Multiple rows for the same proto in the same
    // room are legitimate (a guard post staffed by three guards).
    // `max_instances` remains the global ceiling.
    let mut world_counts: HashMap<(i32, i32), i32> = HashMap::new();
    let mut reset_id_alive: std::collections::HashSet<i32> = std::collections::HashSet::new();
    {
        let mut q = world.query_filtered::<(&WorldKey, Option<&FromMobReset>), With<Mob>>();
        for (wk, fr) in q.iter(world) {
            *world_counts.entry((wk.zone, wk.id)).or_insert(0) += 1;
            if let Some(fr) = fr {
                reset_id_alive.insert(fr.0);
            }
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
    // (room, mob name) pairs for the post-loop announcement pass.
    // Same reason as load_fire_queue — the broadcast helper queries
    // the world, but the spawn block here holds an EntityWorldMut.
    let mut announce_queue: Vec<(Entity, String)> = Vec::new();
    // (mob, room) pairs for any aggro mob that just spawned —
    // post-loop, we look for a player in that room and start
    // hostilities. Reuses the same threshold the on-entry check
    // does so look / consider / spawn-engage all flip together.
    let mut aggro_queue: Vec<(Entity, Entity)> = Vec::new();
    for entry in &entries {
        if reset_id_alive.contains(&entry.reset_id) {
            continue;
        }
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
        if !proto.behaviors.is_empty() {
            em.insert(mud_world::MobBehaviors(proto.behaviors.clone()));
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
        reset_id_alive.insert(entry.reset_id);
        *world_counts.entry(proto_key).or_insert(0) += 1;
        announce_queue.push((entry.room_entity, proto.name.clone()));
        if proto.alignment <= crate::commands::AGGRO_ALIGNMENT {
            aggro_queue.push((em.id(), entry.room_entity));
        }
        refilled += 1;
    }

    if refilled > 0 {
        info!(refilled, "respawn tick");
    }

    // Tell anyone watching that a mob just wandered in. Only fires
    // for *refills* — the initial world load doesn't go through
    // respawn_tick, so no flood at startup. Silent if the room has
    // no players.
    for (room, name) in announce_queue {
        broadcast_room_except_players_rendered(
            world,
            room,
            &[],
            &format!("{name} arrives.\r\n"),
        );
    }

    // Aggro pass: any hostile mob that just respawned tries to grab
    // a non-admin, non-fighting player in the same room. Same
    // threshold as the on-entry attack — and same one-attacker
    // semantic, since each respawn iteration owns at most one mob.
    for (mob, room) in aggro_queue {
        let defender: Option<Entity> = {
            let mut q = world.query_filtered::<
                (Entity, &Located, Option<&mud_world::Account>, Option<&mud_world::Fighting>),
                (With<mud_world::Player>, With<mud_world::Online>),
            >();
            q.iter(world)
                .filter(|(_, l, account, fighting)| {
                    l.0 == room
                        && fighting.is_none()
                        && account.is_some_and(|a| {
                            a.role.rank() <= mud_db::enums::UserRole::Player.rank()
                        })
                })
                .map(|(e, _, _, _)| e)
                .next()
        };
        if let Some(defender) = defender {
            crate::commands::engage_combat(world, mob, defender, room);
        }
    }

    // Fire LOAD triggers for the just-spawned mobs. The respawn loop
    // queued any mob that received an AttachedTriggers component;
    // firing here (after the loop ends) keeps World borrows simple.
    for e in load_fire_queue {
        crate::triggers::fire_event(world, e, mud_world::TriggerEvent::Load);
    }

    // Object respawn: each ObjectResets row owns at most one live
    // instance. We refire a row only when its prior instance has
    // despawned (picked up + destroyed, or the world has restarted
    // without it). Multiple reset rows for the same room are
    // legitimate (a basket of apples, a rose bush) — they each get
    // their own slot. `max_instances` is still enforced as the
    // global ceiling on this proto's world count, so a "unique
    // dagger" (cap=1) won't multiply across rows.
    let mut object_world_counts: std::collections::HashMap<(i32, i32), i32> =
        std::collections::HashMap::new();
    let mut reset_id_alive: std::collections::HashSet<i32> = std::collections::HashSet::new();
    {
        let mut q = world.query_filtered::<&WorldKey, With<Item>>();
        for wk in q.iter(world) {
            *object_world_counts.entry((wk.zone, wk.id)).or_insert(0) += 1;
        }
    }
    {
        let mut q = world.query_filtered::<&FromObjectReset, With<Item>>();
        for fr in q.iter(world) {
            reset_id_alive.insert(fr.0);
        }
    }
    let object_entries: Vec<mud_world::ObjectResetEntry> =
        world.resource::<ObjectResetCatalog>().entries.clone();
    let mut object_refilled = 0usize;
    for entry in &object_entries {
        if reset_id_alive.contains(&entry.reset_id) {
            continue;
        }
        let proto_key = (entry.object_zone_id, entry.object_id);
        let live = object_world_counts.get(&proto_key).copied().unwrap_or(0);
        let cap = entry.max_instances.max(1);
        if live >= cap {
            continue;
        }
        let proto = world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&proto_key)
            .cloned();
        let Some(proto) = proto else { continue };
        let trigger_keys = world
            .resource::<TriggerCatalog>()
            .object_attachments
            .get(&proto_key)
            .cloned();
        let primary_slot = mud_world::wear_flags_primary_slot(&proto.wear_flags);
        let mut bundle = world.spawn((
            Item,
            Named { name: proto.name.clone() },
            Keywords(proto.keywords.clone()),
            WorldKey { zone: proto.zone_id, id: proto.id },
            Located(entry.room_entity),
            FromObjectReset(entry.reset_id),
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
        reset_id_alive.insert(entry.reset_id);
        *object_world_counts.entry(proto_key).or_insert(0) += 1;
        object_refilled += 1;
    }
    if object_refilled > 0 {
        info!(object_refilled, "object respawn tick");
    }
}
