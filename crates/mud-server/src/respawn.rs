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
    AttachedTriggers, Description, Item, Keywords, LiquidContainer, Located, Mob, MobGearCatalog,
    MobPrototypes, MobResetCatalog, Named, ObjectPrototypes, ObjectResetCatalog, TriggerCatalog,
    WorldKey, fill_container, object_world_counts, outfit_mob,
};
use mud_world::{FromMobReset, FromObjectReset};
use tracing::info;

use crate::TickCount;
use crate::commands::broadcast_room_except_players_rendered;

/// One refill cycle every 60 game ticks (= 6 seconds at 10 Hz).
/// Plenty often to keep checks cheap; the actual respawn timing
/// is gated by per-row death timestamps + `MOB_RESPAWN_DELAY_TICKS`,
/// so this just controls the polling rate.
const RESPAWN_PERIOD_TICKS: u64 = 60;

/// Minimum gap (in 10 Hz ticks) between a mob's death and its
/// respawn through the same `MobResets` row (G3.4). Defaults to
/// 1800 ticks = 180 s = 3 min so trash mobs don't pop the moment
/// the player turns around. Overridable at runtime via
/// `world.mob_respawn_delay_seconds` `GameConfig`. Boss-tier delays
/// could later be authored per-row; for now this is a global floor.
const MOB_RESPAWN_DELAY_TICKS_DEFAULT: u64 = 1800;

/// Per-reset death timestamps for the respawn cooldown. Stamped by
/// `combat::handle_death` when the dying mob carries a
/// `FromMobReset`; consulted by `respawn_tick` to gate refills.
#[derive(Resource, Default, Debug)]
pub struct MobRespawnTimers {
    /// `reset_id` → `TickCount` at death.
    pub last_death_tick: HashMap<i32, u64>,
}

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
    // Every freshly-spawned mob, so worn gear gets the same
    // `recompute_equipped_for` pass boot-time mobs get (lit worn
    // lights, equipment bonuses) once the spawn loop is done.
    let mut spawned_mobs: Vec<Entity> = Vec::new();
    // (room, mob name) pairs for the post-loop announcement pass.
    // Same reason as load_fire_queue — the broadcast helper queries
    // the world, but the spawn block here holds an EntityWorldMut.
    let mut announce_queue: Vec<(Entity, String)> = Vec::new();
    // (mob, room) pairs for any aggro mob that just spawned —
    // post-loop, we look for a player in that room and start
    // hostilities. Reuses the same threshold the on-entry check
    // does so look / consider / spawn-engage all flip together.
    let mut aggro_rooms: Vec<Entity> = Vec::new();
    // Read the configurable per-row respawn delay once. Negative or
    // zero means "no delay" — useful for tests and for staff-tuned
    // dungeons that need snappy refills.
    let delay_ticks: u64 = {
        let cfg = world.resource::<mud_world::RuntimeConfig>();
        #[allow(clippy::cast_possible_truncation)]
        let raw = cfg.get_i32(
            "world",
            "mob_respawn_delay_seconds",
            (MOB_RESPAWN_DELAY_TICKS_DEFAULT / 10) as i32,
        );
        u64::try_from(raw.max(0)).unwrap_or(0).saturating_mul(10)
    };
    // Running world count of every object proto, built on the first
    // gear-carrying respawn and kept current across the loop so the
    // legacy item caps hold within a single tick too.
    let mut gear_counts: Option<HashMap<(i32, i32), i32>> = None;
    let timers_snapshot: HashMap<i32, u64> = world
        .get_resource::<MobRespawnTimers>()
        .map_or_else(HashMap::new, |t| t.last_death_tick.clone());
    for entry in &entries {
        if reset_id_alive.contains(&entry.reset_id) {
            continue;
        }
        // G3.4: per-row respawn cooldown. A freshly-killed mob waits
        // `delay_ticks` before the row eligible-fires again. Resets
        // we've never seen die (just loaded, or were killed before
        // the runtime started tracking) bypass the gate.
        if delay_ticks > 0
            && let Some(&death_tick) = timers_snapshot.get(&entry.reset_id)
            && tick.saturating_sub(death_tick) < delay_ticks
        {
            continue;
        }
        let proto_key = (entry.mob_zone_id, entry.mob_id);
        let live = world_counts.get(&proto_key).copied().unwrap_or(0);
        let cap = entry.max_instances.max(1);
        if live >= cap {
            continue;
        }
        let proto = world
            .resource::<MobPrototypes>()
            .by_key
            .get(&proto_key)
            .cloned();
        let Some(proto) = proto else { continue };
        // One spawn per reset row (only when the cap allows). The
        // running `world_counts` is incremented locally so subsequent
        // reset rows for the same proto see the new count and stop
        // when full. Same builder every other spawn path uses, so the
        // proto's default effects land here too.
        let new_mob =
            mud_world::spawn_mob_from_proto(world, &proto, entry.room_entity, Some(entry.reset_id));
        if world.get::<AttachedTriggers>(new_mob).is_some() {
            load_fire_queue.push(new_mob);
        }
        reset_id_alive.insert(entry.reset_id);
        spawned_mobs.push(new_mob);
        *world_counts.entry(proto_key).or_insert(0) += 1;
        announce_queue.push((entry.room_entity, proto.name.clone()));
        aggro_rooms.push(entry.room_entity);
        refilled += 1;
        // Re-run the reset's E / G commands (legacy `reset_zone`
        // re-equips on every reset): same function the boot loader
        // uses, so the respawn matches the original outfit, minus any
        // item whose world-wide cap is already met.
        let has_gear = world
            .get_resource::<MobGearCatalog>()
            .is_some_and(|c| c.by_reset.contains_key(&entry.reset_id));
        if has_gear {
            let counts = gear_counts.get_or_insert_with(|| object_world_counts(world));
            outfit_mob(world, new_mob, entry.reset_id, counts);
        }
    }

    if refilled > 0 {
        info!(refilled, "respawn tick");
    }
    for mob in spawned_mobs {
        crate::equip_apply::recompute_equipped_for(world, mob);
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
            &format!(
                "{} arrives.\r\n",
                crate::commands::cap_sentence_start(&name)
            ),
        );
    }

    // Aggro pass: a respawned mob attacks a player already in its room
    // through the same check a player walking in gets
    // (`recheck_aggro_in_room`: grudge, alignment / formula rule,
    // visibility, wimpy gating, staff exempt), so respawn and room
    // entry can't drift apart.
    aggro_rooms.sort_unstable();
    aggro_rooms.dedup();
    for room in aggro_rooms {
        crate::commands::aggro_room_players(world, room);
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
            Named {
                name: proto.name.clone(),
            },
            Keywords(proto.keywords.clone()),
            WorldKey {
                zone: proto.zone_id,
                id: proto.id,
            },
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
        let spawned = bundle.id();
        crate::item_decay::attach_timer_if_decaying(world, spawned, &proto);
        // A container that comes back gets its authored contents again,
        // minus any whose world-wide cap is already met. The container
        // is counted first (a container may hold copies of itself) and
        // the contents extend the same running map.
        *object_world_counts.entry(proto_key).or_insert(0) += 1;
        fill_container(world, &[spawned], entry.reset_id, &mut object_world_counts);
        reset_id_alive.insert(entry.reset_id);
        object_refilled += 1;
    }
    if object_refilled > 0 {
        info!(object_refilled, "object respawn tick");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{mob_proto, object_proto};
    use mud_db::enums::{MobProfession, ObjectType};
    use mud_db::mob_reset_equipment::MobResetEquipment;
    use mud_db::object_reset_contents::ObjectResetContent;
    use mud_world::{
        ContentEntry, LightFuelProto, MobResetEntry, ObjectContentsCatalog, ObjectResetEntry,
        RuntimeConfig, ShopCatalog,
    };

    const MOB_KEY: (i32, i32) = (1, 1);
    const SWORD: i32 = 10;
    const HELM: i32 = 11;
    const BREAD: i32 = 12;
    const TORCH: i32 = 13;
    const CHEST: i32 = 20;
    const POUCH: i32 = 21;
    const GEM: i32 = 22;

    fn eq_row(
        id: i32,
        reset_id: i32,
        obj: i32,
        slot: Option<&str>,
        prob: f64,
    ) -> MobResetEquipment {
        MobResetEquipment {
            id,
            reset_id,
            object_zone_id: 1,
            object_id: obj,
            wear_location: slot.map(str::to_string),
            max_instances: 1,
            probability: prob,
            decorative: false,
        }
    }

    /// A world with one room, the mob proto, and all gear protos.
    fn base_world() -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(TickCount(0));
        world.insert_resource(RuntimeConfig::default());
        world.insert_resource(ShopCatalog::default());
        world.insert_resource(TriggerCatalog::default());
        world.insert_resource(MobRespawnTimers::default());
        let mut mobs = MobPrototypes::default();
        mobs.by_key
            .insert(MOB_KEY, mob_proto(1, 1, MobProfession::Trainer));
        world.insert_resource(mobs);
        let mut objs = ObjectPrototypes::default();
        for id in [SWORD, HELM, BREAD, TORCH, CHEST, POUCH, GEM] {
            let mut p = object_proto(1, id, ObjectType::Other);
            p.name = format!("object {id}");
            if id == TORCH {
                p.r#type = ObjectType::Light;
                p.light_fuel = Some(LightFuelProto {
                    capacity: 100,
                    remaining: 100,
                });
            }
            objs.by_key.insert((1, id), p);
        }
        world.insert_resource(objs);
        world.insert_resource(MobResetCatalog::default());
        world.insert_resource(ObjectResetCatalog::default());
        world.insert_resource(MobGearCatalog::default());
        world.insert_resource(ObjectContentsCatalog::default());
        let room = world.spawn_empty().id();
        (world, room)
    }

    fn add_mob_reset(world: &mut World, room: Entity, reset_id: i32, rows: &[MobResetEquipment]) {
        world
            .resource_mut::<MobResetCatalog>()
            .entries
            .push(MobResetEntry {
                reset_id,
                mob_zone_id: 1,
                mob_id: 1,
                room_entity: room,
                max_instances: 10,
            });
        let (fresh, _) =
            mud_world::reset_gear::build_gear_entries(rows, world.resource::<ObjectPrototypes>());
        world
            .resource_mut::<MobGearCatalog>()
            .by_reset
            .extend(fresh);
    }

    /// What `load_from_db` does for one mob: spawn it for its reset,
    /// outfit it, then run the gear-bonus pass.
    fn boot_mob(world: &mut World, room: Entity, reset_id: i32) -> Entity {
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "a test mob".into(),
                },
                WorldKey { zone: 1, id: 1 },
                Located(room),
                FromMobReset(reset_id),
            ))
            .id();
        let mut counts = object_world_counts(world);
        outfit_mob(world, mob, reset_id, &mut counts);
        crate::equip_apply::recompute_equipped_for(world, mob);
        mob
    }

    fn mob_of(world: &mut World, reset_id: i32) -> Option<Entity> {
        let mut q = world.query_filtered::<(Entity, &FromMobReset), With<Mob>>();
        q.iter(world).find(|(_, f)| f.0 == reset_id).map(|(e, _)| e)
    }

    /// Sorted `(object id, worn slot)` of everything on `mob`.
    fn gear_of(world: &mut World, mob: Entity) -> Vec<(i32, Option<String>)> {
        let mut q = world
            .query_filtered::<(&WorldKey, &Located, Option<&mud_world::EquippedSlot>), With<Item>>(
            );
        let mut v: Vec<(i32, Option<String>)> = q
            .iter(world)
            .filter(|(_, l, _)| l.0 == mob)
            .map(|(k, _, s)| (k.id, s.map(|s| format!("{:?}", s.0))))
            .collect();
        v.sort();
        v
    }

    fn kill(world: &mut World, mob: Entity, room: Entity) {
        crate::combat::handle_death(world, mob, "a test mob", room);
    }

    fn run_respawn(world: &mut World, tick: u64) {
        world.insert_resource(TickCount(tick));
        respawn_tick(world);
    }

    fn standard_gear() -> Vec<MobResetEquipment> {
        vec![
            eq_row(1, 1, SWORD, Some("WIELD"), 0.99),
            eq_row(2, 1, HELM, Some("HEAD"), 0.99),
            eq_row(3, 1, BREAD, None, 0.99),
            eq_row(4, 1, BREAD, None, 0.99),
        ]
    }

    #[test]
    fn respawned_mob_has_same_equipment_and_inventory_as_boot() {
        let (mut world, room) = base_world();
        add_mob_reset(&mut world, room, 1, &standard_gear());
        let booted = boot_mob(&mut world, room, 1);
        let at_boot = gear_of(&mut world, booted);
        assert_eq!(at_boot.len(), 4, "boot outfits all four items: {at_boot:?}");

        kill(&mut world, booted, room);
        assert!(world.get_entity(booted).is_err(), "mob died");
        assert!(mob_of(&mut world, 1).is_none());

        run_respawn(&mut world, 6000);
        let reborn = mob_of(&mut world, 1).expect("mob respawned");
        assert_eq!(gear_of(&mut world, reborn), at_boot);
    }

    #[test]
    fn mob_reset_wrist_l_and_paired_labels_equip_both_sides() {
        let (mut world, room) = base_world();
        let rows = vec![
            eq_row(1, 1, SWORD, Some("WRIST_L"), 0.99),
            eq_row(2, 1, HELM, Some("WRIST_L"), 0.99),
            eq_row(3, 1, BREAD, Some("WRIST_L"), 0.99),
        ];
        add_mob_reset(&mut world, room, 1, &rows);
        let mob = boot_mob(&mut world, room, 1);
        // First WRIST_L takes the left wrist, the second spills to the
        // right wrist, the third finds both taken and stays carried.
        assert_eq!(
            gear_of(&mut world, mob),
            vec![
                (SWORD, Some("LeftWrist".to_string())),
                (HELM, Some("RightWrist".to_string())),
                (BREAD, None),
            ]
        );
    }

    #[test]
    fn respawned_gear_is_world_owned_not_character_items() {
        // Mob gear is parented to the mob, never to a player, so the
        // player-save snapshot (which walks a player's Located chain)
        // can't see it, and it carries no persistence marker.
        let (mut world, room) = base_world();
        add_mob_reset(&mut world, room, 1, &standard_gear());
        let booted = boot_mob(&mut world, room, 1);
        kill(&mut world, booted, room);
        run_respawn(&mut world, 6000);
        let reborn = mob_of(&mut world, 1).expect("respawned");
        let mut q =
            world.query_filtered::<(&Located, Has<mud_world::PersistedItemId>), With<Item>>();
        let on_mob: Vec<bool> = q
            .iter(&world)
            .filter(|(l, _)| l.0 == reborn)
            .map(|(_, p)| p)
            .collect();
        assert_eq!(on_mob.len(), 4);
        assert!(on_mob.iter().all(|persisted| !persisted));
        assert!(world.get::<mud_world::Player>(reborn).is_none());
    }

    #[test]
    fn capped_item_is_not_minted_again_while_a_copy_exists() {
        // SWORD prob 0.01 -> legacy max 1: one copy in the world.
        let (mut world, room) = base_world();
        let rows = vec![
            eq_row(1, 1, SWORD, Some("WIELD"), 0.01),
            eq_row(2, 1, BREAD, None, 0.99),
        ];
        add_mob_reset(&mut world, room, 1, &rows);
        let booted = boot_mob(&mut world, room, 1);
        assert_eq!(gear_of(&mut world, booted).len(), 2);

        // Death moves the sword into the corpse; it still exists, so
        // the respawn must not hand out a second one.
        kill(&mut world, booted, room);
        run_respawn(&mut world, 6000);
        let reborn = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(
            gear_of(&mut world, reborn)
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            vec![BREAD],
            "sword is capped while the corpse still holds it"
        );
        let swords = *object_world_counts(&mut world)
            .get(&(1, SWORD))
            .unwrap_or(&0);
        assert_eq!(swords, 1);

        // Once every copy is gone (corpse decayed), the next respawn
        // may load it again - still never more than one.
        let sword_entities: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &WorldKey), With<Item>>();
            q.iter(&world)
                .filter(|(_, k)| k.id == SWORD)
                .map(|(e, _)| e)
                .collect()
        };
        for e in sword_entities {
            world.despawn(e);
        }
        kill(&mut world, reborn, room);
        run_respawn(&mut world, 12_000);
        let again = mob_of(&mut world, 1).expect("respawned again");
        assert!(
            gear_of(&mut world, again)
                .iter()
                .any(|(id, _)| *id == SWORD)
        );
        assert_eq!(
            *object_world_counts(&mut world)
                .get(&(1, SWORD))
                .unwrap_or(&0),
            1
        );
    }

    #[test]
    fn cap_holds_across_reset_rows_in_the_same_tick() {
        // Two rows share a unique sword; only one mob may get it,
        // even when both respawn in the same tick.
        let (mut world, room) = base_world();
        let sword = [eq_row(1, 1, SWORD, Some("WIELD"), 0.01)];
        add_mob_reset(&mut world, room, 1, &sword);
        let sword2 = [eq_row(2, 2, SWORD, Some("WIELD"), 0.01)];
        add_mob_reset(&mut world, room, 2, &sword2);
        run_respawn(&mut world, 6000);
        assert!(mob_of(&mut world, 1).is_some() && mob_of(&mut world, 2).is_some());
        assert_eq!(
            *object_world_counts(&mut world)
                .get(&(1, SWORD))
                .unwrap_or(&0),
            1
        );
    }

    #[test]
    fn respawned_mobs_worn_light_lights_the_room() {
        let (mut world, room) = base_world();
        add_mob_reset(
            &mut world,
            room,
            1,
            &[eq_row(1, 1, TORCH, Some("HOLD"), 0.99)],
        );
        let booted = boot_mob(&mut world, room, 1);
        assert!(crate::commands::room_has_light(&mut world, room));
        kill(&mut world, booted, room);
        // The torch went into the corpse; nothing lights the room.
        let lit: Vec<Entity> = {
            let mut q = world.query_filtered::<Entity, With<mud_world::Lit>>();
            q.iter(&world).collect()
        };
        for e in lit {
            world.despawn(e);
        }
        assert!(!crate::commands::room_has_light(&mut world, room));
        run_respawn(&mut world, 6000);
        assert!(mob_of(&mut world, 1).is_some());
        assert!(
            crate::commands::room_has_light(&mut world, room),
            "respawned mob's worn torch is lit"
        );
    }

    #[test]
    fn respawned_container_is_refilled_with_nested_contents() {
        let (mut world, room) = base_world();
        world
            .resource_mut::<ObjectResetCatalog>()
            .entries
            .push(ObjectResetEntry {
                reset_id: 7,
                object_zone_id: 1,
                object_id: CHEST,
                room_entity: room,
                max_instances: 1,
            });
        let content_rows = vec![
            ObjectResetContent {
                id: 1,
                reset_id: 7,
                parent_content_id: None,
                object_zone_id: 1,
                object_id: POUCH,
                quantity: 1,
                max_instances: 99,
            },
            // Listed before its parent on purpose: order must not matter.
            ObjectResetContent {
                id: 3,
                reset_id: 7,
                parent_content_id: Some(1),
                object_zone_id: 1,
                object_id: GEM,
                quantity: 3,
                max_instances: 99,
            },
            ObjectResetContent {
                id: 2,
                reset_id: 7,
                parent_content_id: None,
                object_zone_id: 1,
                object_id: BREAD,
                quantity: 2,
                max_instances: 99,
            },
        ];
        let entries: Vec<ContentEntry> =
            mud_world::reset_gear::build_content_entries(&content_rows)
                .remove(&7)
                .unwrap();
        world
            .resource_mut::<ObjectContentsCatalog>()
            .by_reset
            .insert(7, entries);

        run_respawn(&mut world, 6000);
        let chest = {
            let mut q = world.query_filtered::<(Entity, &WorldKey), With<Item>>();
            q.iter(&world)
                .find(|(_, k)| k.id == CHEST)
                .map(|(e, _)| e)
                .expect("chest respawned")
        };
        let kids = |world: &mut World, parent: Entity| -> Vec<i32> {
            let mut q = world.query_filtered::<(&WorldKey, &Located), With<Item>>();
            let mut v: Vec<i32> = q
                .iter(world)
                .filter(|(_, l)| l.0 == parent)
                .map(|(k, _)| k.id)
                .collect();
            v.sort_unstable();
            v
        };
        assert_eq!(kids(&mut world, chest), vec![BREAD, BREAD, POUCH]);
        let pouch = {
            let mut q = world.query_filtered::<(Entity, &WorldKey, &Located), With<Item>>();
            q.iter(&world)
                .find(|(_, k, l)| k.id == POUCH && l.0 == chest)
                .map(|(e, _, _)| e)
                .unwrap()
        };
        assert_eq!(kids(&mut world, pouch), vec![GEM, GEM, GEM]);
    }

    #[test]
    fn container_contents_honour_the_world_cap() {
        let (mut world, room) = base_world();
        for reset_id in [7, 8] {
            world
                .resource_mut::<ObjectResetCatalog>()
                .entries
                .push(ObjectResetEntry {
                    reset_id,
                    object_zone_id: 1,
                    object_id: CHEST,
                    room_entity: room,
                    max_instances: 2,
                });
            let rows = vec![ObjectResetContent {
                id: reset_id,
                reset_id,
                parent_content_id: None,
                object_zone_id: 1,
                object_id: GEM,
                quantity: 2,
                // Legacy `P` max 3: two chests of 2 gems -> only 3 exist.
                max_instances: 3,
            }];
            let entries = mud_world::reset_gear::build_content_entries(&rows)
                .remove(&reset_id)
                .unwrap();
            world
                .resource_mut::<ObjectContentsCatalog>()
                .by_reset
                .insert(reset_id, entries);
        }
        run_respawn(&mut world, 6000);
        let gems = |world: &mut World| *object_world_counts(world).get(&(1, GEM)).unwrap_or(&0);
        assert_eq!(gems(&mut world), 3);
        // Further cycles do not mint more while the cap is met.
        run_respawn(&mut world, 12000);
        assert_eq!(gems(&mut world), 3);
    }

    // -- aggro on respawn: same path as room entry -------------------------

    /// A reset row for an evil mob with `behaviors`, plus an online,
    /// idle player standing in the room.
    fn aggro_world(
        behaviors: Vec<mud_db::enums::MobBehavior>,
    ) -> (World, Entity, Entity, crate::commands::test_support::Rx) {
        let (mut world, room) = base_world();
        let mut proto = mob_proto(1, 1, MobProfession::Trainer);
        proto.alignment = -1000;
        proto.behaviors = behaviors;
        world
            .resource_mut::<MobPrototypes>()
            .by_key
            .insert(MOB_KEY, proto);
        add_mob_reset(&mut world, room, 1, &[]);
        let (player, rx) = crate::commands::test_support::player_in(&mut world, room);
        crate::commands::test_support::make_aggro_target(&mut world, player);
        (world, room, player, rx)
    }

    fn fighting_target(world: &World, mob: Entity) -> Option<Entity> {
        world.get::<mud_world::Fighting>(mob).map(|f| f.0)
    }

    #[test]
    fn respawned_mob_gets_default_effects() {
        let (mut world, room) = base_world();
        add_mob_reset(&mut world, room, 1, &[]);
        crate::commands::test_support::grant_default_flags(&mut world, MOB_KEY, &["haste"]);
        run_respawn(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert!(world.get::<mud_world::Haste>(mob).is_some());
    }

    #[test]
    fn respawned_aggressive_mob_attacks_an_awake_visible_player() {
        let (mut world, _room, player, _rx) = aggro_world(vec![]);
        run_respawn(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(fighting_target(&world, mob), Some(player));
    }

    #[test]
    fn respawned_mob_does_not_attack_a_player_it_cannot_see() {
        let (mut world, _room, player, _rx) = aggro_world(vec![]);
        world.entity_mut(player).insert(mud_world::Invisible);
        run_respawn(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(fighting_target(&world, mob), None);
    }

    #[test]
    fn respawned_mob_with_detect_invisible_default_attacks_an_invisible_player() {
        // The default effects are installed before the aggro pass.
        let (mut world, _room, player, _rx) = aggro_world(vec![]);
        world.entity_mut(player).insert(mud_world::Invisible);
        crate::commands::test_support::grant_default_flags(
            &mut world,
            MOB_KEY,
            &["detect_invisible"],
        );
        run_respawn(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(fighting_target(&world, mob), Some(player));
    }

    #[test]
    fn respawned_wimpy_mob_leaves_an_awake_player_alone_but_hits_a_sleeper() {
        use mud_db::enums::MobBehavior;
        let (mut world, _room, _player, _rx) = aggro_world(vec![MobBehavior::Wimpy]);
        run_respawn(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(fighting_target(&world, mob), None, "awake player");

        let (mut world, _room, player, _rx) = aggro_world(vec![MobBehavior::Wimpy]);
        world
            .entity_mut(player)
            .insert(mud_world::Posture(mud_world::PostureKind::Sleeping));
        run_respawn(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(fighting_target(&world, mob), Some(player), "sleeping");
    }

    #[test]
    fn respawned_mob_spares_staff() {
        let (mut world, _room, player, _rx) = aggro_world(vec![]);
        world.get_mut::<mud_world::Account>(player).unwrap().role =
            mud_db::enums::UserRole::Immortal;
        run_respawn(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(fighting_target(&world, mob), None);
    }

    #[test]
    fn evil_pet_does_not_attack_its_owner_when_something_respawns() {
        // The reset mob is neutral, so the pet is the only aggro
        // candidate in the room.
        let (mut world, room, player, _rx) = aggro_world(vec![]);
        world
            .resource_mut::<MobPrototypes>()
            .by_key
            .get_mut(&MOB_KEY)
            .unwrap()
            .alignment = 0;
        let spawn_evil = |world: &mut World| {
            world
                .spawn((
                    Mob,
                    Named {
                        name: "a hellhound".into(),
                    },
                    Located(room),
                    mud_world::CombatStats {
                        alignment: -1000,
                        ..mud_world::CombatStats::default()
                    },
                    mud_world::Health { hp: 50, max: 50 },
                    mud_world::Posture(mud_world::PostureKind::Standing),
                ))
                .id()
        };
        let pet = spawn_evil(&mut world);
        world.entity_mut(pet).insert(mud_world::Follower(player));
        run_respawn(&mut world, 6000);
        assert_eq!(fighting_target(&world, pet), None, "pet spares its owner");

        // Control: the same evil mob without a master does attack.
        world.entity_mut(pet).remove::<mud_world::Follower>();
        world
            .resource_mut::<MobRespawnTimers>()
            .last_death_tick
            .clear();
        let mob = mob_of(&mut world, 1).expect("reset mob alive");
        world.entity_mut(mob).despawn();
        run_respawn(&mut world, 12000);
        assert_eq!(fighting_target(&world, pet), Some(player));
    }
}
