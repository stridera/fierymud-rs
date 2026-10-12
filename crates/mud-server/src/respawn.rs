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

use std::collections::{HashMap, HashSet};

use bevy_ecs::prelude::*;
use mud_db::enums::ResetMode;
use mud_world::{
    AttachedTriggers, ExitDefaults, Exits, Item, Located, Mob, MobGearCatalog, MobPrototypes,
    MobResetCatalog, ObjectPrototypes, ObjectResetCatalog, Player, Room, WorldKey, ZoneDoorReset,
    fill_container, object_world_counts, outfit_mob,
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

/// How soon a zone retries its door reset after skipping a door because a
/// player stood beside it (5 minutes of 10 Hz ticks).
const DOOR_RESET_RETRY_TICKS: u64 = 3000;

/// Most zones the door pass resets per respawn tick (one tick per 6 s).
/// Zones start with the same `last_reset`, so with equal lifespans nearly
/// all of them fall due on the same tick; resetting them all at once was a
/// 100-150 ms spike (debug) every 10 minutes. The rest stay due and are
/// picked up on the next ticks, so every door still resets, a few seconds
/// late at most.
const DOOR_RESET_ZONES_PER_PASS: usize = 6;

/// Restore every door in each zone whose `lifespan` has elapsed to its
/// as-authored state (`RoomExits.default_state`), like legacy `reset_zone`'s
/// `D` commands. Only due zones are touched; the per-pass cost for idle
/// zones is one comparison per zone entity.
///
/// Kinder than legacy: a door is left alone while a player stands in either
/// room it joins, and the zone retries after `DOOR_RESET_RETRY_TICKS`.
/// `ResetMode::Empty` zones wait until no player is inside; `Never` zones
/// (clan halls, quest zones) keep their doors as players left them.
pub(crate) fn door_reset_pass(world: &mut World, tick: u64) {
    // (zone, mode, period, how far past due)
    let mut due: Vec<(Entity, ResetMode, u64, u64)> = Vec::new();
    {
        let mut q = world.query::<(Entity, &ZoneDoorReset)>();
        for (zone, r) in q.iter(world) {
            let age = tick.saturating_sub(r.last_reset);
            if r.mode != ResetMode::Never && age >= r.period_ticks {
                due.push((zone, r.mode, r.period_ticks, age - r.period_ticks));
            }
        }
    }
    if due.is_empty() {
        return;
    }
    let occupied: HashSet<Entity> = world
        .query_filtered::<&Located, With<Player>>()
        .iter(world)
        .map(|l| l.0)
        .collect();
    // An `Empty` zone with a player inside cannot reset yet. Drop it before
    // the slice is cut, or a handful of long-occupied zones (most overdue
    // first) would fill every pass and starve the zones that can reset.
    let occupied_zones: HashSet<Entity> = occupied
        .iter()
        .filter_map(|&room| world.get::<Located>(room).map(|l| l.0))
        .collect();
    due.retain(|d| d.1 != ResetMode::Empty || !occupied_zones.contains(&d.0));
    // Most overdue first, a bounded slice per pass (see the constant).
    due.sort_by(|a, b| b.3.cmp(&a.3).then(a.0.cmp(&b.0)));
    due.truncate(DOOR_RESET_ZONES_PER_PASS);
    // One scan of the rooms for the whole slice, not one per zone.
    let batch: HashSet<Entity> = due.iter().map(|d| d.0).collect();
    let mut rooms_by_zone: HashMap<Entity, Vec<Entity>> = HashMap::new();
    for (room, located) in world
        .query_filtered::<(Entity, &Located), (With<Room>, With<ExitDefaults>)>()
        .iter(world)
    {
        if batch.contains(&located.0) {
            rooms_by_zone.entry(located.0).or_default().push(room);
        }
    }
    for (zone, _mode, period, _) in due {
        let rooms = rooms_by_zone.remove(&zone).unwrap_or_default();
        let skipped = reset_zone_doors(world, &rooms, &occupied);
        let next = if skipped {
            tick.saturating_sub(period)
                .saturating_add(DOOR_RESET_RETRY_TICKS.min(period))
        } else {
            tick
        };
        if let Some(mut r) = world.get_mut::<ZoneDoorReset>(zone) {
            r.last_reset = next;
        }
    }
}

/// Reset the doors of `rooms` (a zone's rooms with authored exit defaults).
/// Each room restores only its OWN sides from its own `ExitDefaults` entry (state and hp): the two
/// sides of a door may have different defaults (a locked side with a key
/// facing an open, keyless side), so one side must never be copied onto the
/// other. The far side is restored by the pass of its own room's zone;
/// restoring a side from its own defaults is idempotent, so order does not
/// matter. A side is left alone while a player is in, or next to, its room.
/// Returns true when a door was skipped because of a player.
fn reset_zone_doors(world: &mut World, rooms: &[Entity], occupied: &HashSet<Entity>) -> bool {
    let mut skipped = false;
    for &room in rooms {
        let Some(defaults) = world.get::<ExitDefaults>(room).map(|d| d.0.clone()) else {
            continue;
        };
        for (dir, (state, hp)) in defaults {
            let Some((cur_state, cur_hp, far)) = world
                .get::<Exits>(room)
                .and_then(|e| e.0.get(&dir))
                .map(|ed| (ed.state, ed.hit_points, ed.to))
            else {
                continue;
            };
            if cur_state == state && cur_hp == hp {
                continue;
            }
            let touches_player =
                occupied.contains(&room) || far.is_some_and(|f| occupied.contains(&f));
            if touches_player {
                skipped = true;
                continue;
            }
            if let Some(mut exits) = world.get_mut::<Exits>(room)
                && let Some(ed) = exits.0.get_mut(&dir)
            {
                ed.state = state;
                ed.hit_points = hp;
            }
        }
    }
    skipped
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn respawn_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(RESPAWN_PERIOD_TICKS) {
        return;
    }

    door_reset_pass(world, tick);

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
    // Staff-held copies (and their containers' contents) are not counted.
    let mut object_world_counts = object_world_counts(world);
    let mut reset_id_alive: std::collections::HashSet<i32> = std::collections::HashSet::new();
    {
        let mut q = world.query_filtered::<(Entity, &FromObjectReset), With<Item>>();
        let owned: Vec<(Entity, i32)> = q.iter(world).map(|(e, fr)| (e, fr.0)).collect();
        for (item, reset_id) in owned {
            // An item a staff member holds gives its reset slot back for good
            // (it is theirs now, not the world's): the row refills, and the
            // item does not claim the slot again if it is later dropped.
            if mud_world::reset_gear::held_by_staff(world, item) {
                world.entity_mut(item).remove::<FromObjectReset>();
                continue;
            }
            reset_id_alive.insert(reset_id);
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
        let spawned =
            mud_world::spawn_item_for_reset(world, &proto, entry.room_entity, entry.reset_id);
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
    use mud_db::mob_reset_equipment::{MobResetEquipment, MobResetEquipmentContent};
    use mud_db::object_reset_contents::ObjectResetContent;
    use mud_world::{
        ContentEntry, LightFuelProto, MobResetEntry, ObjectContentsCatalog, ObjectResetEntry,
        RuntimeConfig, ShopCatalog,
    };
    use mud_world::{Located, Named, TriggerCatalog};

    const MOB_KEY: (i32, i32) = (1, 1);
    const SWORD: i32 = 10;
    const HELM: i32 = 11;
    const BREAD: i32 = 12;
    const TORCH: i32 = 13;
    const CHEST: i32 = 20;
    const POUCH: i32 = 21;
    const GEM: i32 = 22;
    const KEY: i32 = 23;

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
        for id in [SWORD, HELM, BREAD, TORCH, CHEST, POUCH, GEM, KEY] {
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
        add_mob_reset_with_contents(world, room, reset_id, rows, &[]);
    }

    fn add_mob_reset_with_contents(
        world: &mut World,
        room: Entity,
        reset_id: i32,
        rows: &[MobResetEquipment],
        gear_contents: &[MobResetEquipmentContent],
    ) {
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
        let (fresh, _) = mud_world::reset_gear::build_gear_entries(
            rows,
            gear_contents,
            world.resource::<ObjectPrototypes>(),
        );
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
        assert!(crate::commands::room_has_light(&world, room));
        kill(&mut world, booted, room);
        // The torch went into the corpse; nothing lights the room.
        let lit: Vec<Entity> = {
            let mut q = world.query_filtered::<Entity, With<mud_world::Lit>>();
            q.iter(&world).collect()
        };
        for e in lit {
            world.despawn(e);
        }
        assert!(!crate::commands::room_has_light(&world, room));
        run_respawn(&mut world, 6000);
        assert!(mob_of(&mut world, 1).is_some());
        assert!(
            crate::commands::room_has_light(&world, room),
            "respawned mob's worn torch is lit"
        );
    }

    #[test]
    fn respawned_room_object_keeps_its_flags_and_authored_hiddenness() {
        let (mut world, room) = base_world();
        {
            let mut protos = world.resource_mut::<ObjectPrototypes>();
            let gem = protos.by_key.get_mut(&(1, GEM)).expect("gem proto");
            gem.concealment = 300;
            gem.flags = vec![mud_db::enums::ObjectFlag::NoFall];
        }
        world
            .resource_mut::<ObjectResetCatalog>()
            .entries
            .push(ObjectResetEntry {
                reset_id: 8,
                object_zone_id: 1,
                object_id: GEM,
                room_entity: room,
                max_instances: 1,
            });
        run_respawn(&mut world, 6000);
        let gem = {
            let mut q = world.query_filtered::<(Entity, &WorldKey), With<Item>>();
            q.iter(&world)
                .find(|(_, k)| k.id == GEM)
                .map(|(e, _)| e)
                .expect("gem respawned")
        };
        assert_eq!(
            world.get::<mud_world::Hiddenness>(gem).map(|h| h.0),
            Some(300)
        );
        assert!(
            world
                .get::<mud_world::ObjectFlags>(gem)
                .is_some_and(|f| f.has(mud_db::enums::ObjectFlag::NoFall))
        );
    }

    /// `base_world` plus a charged wand, an unbound-charge staff (a binding
    /// whose `charges` is NULL) and a plain sword's binding-less proto.
    fn add_charged_protos(world: &mut World) -> (i32, i32) {
        const WAND: i32 = 30;
        const STAFF: i32 = 31;
        {
            let mut objs = world.resource_mut::<ObjectPrototypes>();
            objs.by_key
                .insert((1, WAND), object_proto(1, WAND, ObjectType::Wand));
            objs.by_key
                .insert((1, STAFF), object_proto(1, STAFF, ObjectType::Staff));
        }
        let mut bindings = mud_world::ObjectAbilityCatalog::default();
        let bind = |charges| mud_world::resources::ObjectAbilityBinding {
            ability_id: 1,
            level: 10,
            charges,
        };
        bindings.by_key.insert((1, WAND), vec![bind(Some(3))]);
        bindings.by_key.insert((1, STAFF), vec![bind(None)]);
        world.insert_resource(bindings);
        (WAND, STAFF)
    }

    fn charges_of(world: &mut World, obj: i32) -> Vec<Option<i32>> {
        let mut q = world.query_filtered::<(&WorldKey, Option<&mud_world::Charges>), With<Item>>();
        q.iter(world)
            .filter(|(k, _)| k.id == obj)
            .map(|(_, c)| c.map(|c| c.0))
            .collect()
    }

    #[test]
    fn a_wand_respawned_by_an_object_reset_has_the_prototypes_charges() {
        let (mut world, room) = base_world();
        let (wand, staff) = add_charged_protos(&mut world);
        for (reset_id, obj) in [(40, wand), (41, staff), (42, SWORD)] {
            world
                .resource_mut::<ObjectResetCatalog>()
                .entries
                .push(ObjectResetEntry {
                    reset_id,
                    object_zone_id: 1,
                    object_id: obj,
                    room_entity: room,
                    max_instances: 1,
                });
        }
        run_respawn(&mut world, 6000);
        assert_eq!(charges_of(&mut world, wand), vec![Some(3)]);
        // No charge count in the data, or no binding at all: left unlimited
        // on purpose (no `Charges` component).
        assert_eq!(charges_of(&mut world, staff), vec![None]);
        assert_eq!(charges_of(&mut world, SWORD), vec![None]);
    }

    #[test]
    fn mob_gear_and_container_contents_get_charges_too() {
        let (mut world, room) = base_world();
        let (wand, _) = add_charged_protos(&mut world);
        add_mob_reset(
            &mut world,
            room,
            1,
            &[eq_row(1, 1, wand, Some("WIELD"), 0.99)],
        );
        let _mob = boot_mob(&mut world, room, 1);
        assert_eq!(charges_of(&mut world, wand), vec![Some(3)], "mob gear");

        let chest = world.spawn((Item, Located(room))).id();
        let rows = vec![ObjectResetContent {
            id: 1,
            reset_id: 9,
            parent_content_id: None,
            object_zone_id: 1,
            object_id: wand,
            quantity: 1,
            max_instances: 99,
        }];
        let entries = mud_world::reset_gear::build_content_entries(&rows)
            .remove(&9)
            .unwrap();
        world
            .resource_mut::<ObjectContentsCatalog>()
            .by_reset
            .insert(9, entries);
        let mut counts = std::collections::HashMap::new();
        mud_world::fill_container(&mut world, &[chest], 9, &mut counts);
        assert_eq!(
            charges_of(&mut world, wand),
            vec![Some(3), Some(3)],
            "container contents"
        );
    }

    fn gear_content(
        id: i32,
        equipment_id: i32,
        parent: Option<i32>,
        obj: i32,
        quantity: i32,
        max: i32,
    ) -> MobResetEquipmentContent {
        MobResetEquipmentContent {
            id,
            equipment_id,
            parent_content_id: parent,
            object_zone_id: 1,
            object_id: obj,
            quantity,
            max_instances: max,
        }
    }

    /// Sorted object ids sitting directly inside `parent`.
    fn items_inside(world: &mut World, parent: Entity) -> Vec<i32> {
        let mut q = world.query_filtered::<(&WorldKey, &Located), With<Item>>();
        let mut v: Vec<i32> = q
            .iter(world)
            .filter(|(_, l)| l.0 == parent)
            .map(|(k, _)| k.id)
            .collect();
        v.sort_unstable();
        v
    }

    /// The carried (unworn) item of prototype `obj` held by `mob`.
    fn carried_item(world: &mut World, mob: Entity, obj: i32) -> Entity {
        let mut q = world.query_filtered::<(Entity, &WorldKey, &Located), With<Item>>();
        q.iter(world)
            .find(|(_, k, l)| k.id == obj && l.0 == mob)
            .map_or_else(|| panic!("mob carries no object {obj}"), |(e, _, _)| e)
    }

    /// Legacy `M`, `G chest`, `P key chest` (and a pouch of gems in the
    /// chest): the mob carries the chest with the key inside, to any depth.
    #[test]
    fn mob_spawns_carrying_a_container_with_its_contents_nested_inside() {
        let (mut world, room) = base_world();
        add_mob_reset_with_contents(
            &mut world,
            room,
            1,
            &[eq_row(1, 1, CHEST, None, 0.99)],
            &[
                gear_content(1, 1, None, KEY, 1, 3),
                gear_content(2, 1, None, POUCH, 1, 99),
                gear_content(3, 1, Some(2), GEM, 2, 99),
            ],
        );
        let mob = boot_mob(&mut world, room, 1);
        // Only the chest is on the mob; the key and pouch are inside it.
        assert_eq!(gear_of(&mut world, mob), vec![(CHEST, None)]);
        let chest = carried_item(&mut world, mob, CHEST);
        assert_eq!(items_inside(&mut world, chest), vec![POUCH, KEY]);
        let pouch = {
            let mut q = world.query_filtered::<(Entity, &WorldKey, &Located), With<Item>>();
            q.iter(&world)
                .find(|(_, k, l)| k.id == POUCH && l.0 == chest)
                .map(|(e, _, _)| e)
                .unwrap()
        };
        assert_eq!(items_inside(&mut world, pouch), vec![GEM, GEM]);

        // Dies and respawns: the new mob comes back with the same nesting.
        kill(&mut world, mob, room);
        run_respawn(&mut world, 6000);
        let reborn = mob_of(&mut world, 1).expect("mob respawned");
        assert_ne!(reborn, mob);
        let chest = carried_item(&mut world, reborn, CHEST);
        assert_eq!(items_inside(&mut world, chest), vec![POUCH, KEY]);
    }

    /// Legacy `P` max: the key's world cap is shared by every carrier, and a
    /// carrier whose chest is skipped by the chest's own cap gets no key.
    #[test]
    fn mob_gear_contents_honour_the_world_cap() {
        let (mut world, room) = base_world();
        let contents = [gear_content(1, 1, None, KEY, 1, 1)];
        add_mob_reset_with_contents(
            &mut world,
            room,
            1,
            &[eq_row(1, 1, CHEST, None, 0.99)],
            &contents,
        );
        add_mob_reset_with_contents(
            &mut world,
            room,
            2,
            &[eq_row(2, 2, CHEST, None, 0.99)],
            &[gear_content(2, 2, None, KEY, 1, 1)],
        );
        let first = boot_mob(&mut world, room, 1);
        let second = boot_mob(&mut world, room, 2);
        let chest1 = carried_item(&mut world, first, CHEST);
        let chest2 = carried_item(&mut world, second, CHEST);
        assert_eq!(items_inside(&mut world, chest1), vec![KEY]);
        assert_eq!(
            items_inside(&mut world, chest2),
            Vec::<i32>::new(),
            "P max 1 already met"
        );
        let keys = *object_world_counts(&mut world).get(&(1, KEY)).unwrap_or(&0);
        assert_eq!(keys, 1);
    }

    fn level_profile(level: i32) -> mud_world::Profile {
        mud_world::Profile {
            level,
            class_id: None,
            race: String::new(),
            experience: 0,
            gender: String::new(),
        }
    }

    /// Issue #121: a limited item held by staff (directly or inside their
    /// containers) does not use up the world cap; one held by a mortal does.
    #[test]
    fn staff_held_copies_do_not_block_object_respawn() {
        use mud_db::enums::UserRole;
        use mud_world::Account;
        let holder = |role: UserRole| Account {
            user_id: String::new(),
            character_id: "c".into(),
            role,
            account_role: role,
            perms: vec![],
        };
        let keys_on_floor = |world: &mut World, room: Entity| {
            let mut q = world.query_filtered::<(&WorldKey, &Located), With<Item>>();
            q.iter(world)
                .filter(|(k, l)| k.id == KEY && l.0 == room)
                .count()
        };
        let entry = |room| ObjectResetEntry {
            reset_id: 9,
            object_zone_id: 1,
            object_id: KEY,
            room_entity: room,
            max_instances: 1,
        };

        // Staff holds the only key, inside a sack in their inventory.
        let (mut world, room) = base_world();
        let imm = world
            .spawn((
                Player,
                holder(UserRole::Immortal),
                level_profile(100),
                Located(room),
            ))
            .id();
        let sack = world
            .spawn((Item, WorldKey { zone: 1, id: 900 }, Located(imm)))
            .id();
        world.spawn((Item, WorldKey { zone: 1, id: KEY }, Located(sack)));
        world
            .resource_mut::<ObjectResetCatalog>()
            .entries
            .push(entry(room));
        run_respawn(&mut world, 6000);
        assert_eq!(keys_on_floor(&mut world, room), 1, "staff copy ignored");

        // A mortal holding it still blocks the spawn.
        let (mut world, room) = base_world();
        let mortal = world
            .spawn((
                Player,
                holder(UserRole::Player),
                level_profile(10),
                Located(room),
            ))
            .id();
        world.spawn((Item, WorldKey { zone: 1, id: KEY }, Located(mortal)));
        world
            .resource_mut::<ObjectResetCatalog>()
            .entries
            .push(entry(room));
        run_respawn(&mut world, 6000);
        assert_eq!(keys_on_floor(&mut world, room), 0, "mortal copy counts");
    }

    /// Issue #121 (round 4): a reset-spawned copy that staff pick up off the
    /// floor gives its reset slot back; a mortal holding it still keeps it.
    #[test]
    fn a_reset_item_staff_picked_up_releases_its_reset_slot() {
        use mud_db::enums::UserRole;
        use mud_world::Account;
        let holder = |role: UserRole| Account {
            user_id: String::new(),
            character_id: "c".into(),
            role,
            account_role: role,
            perms: vec![],
        };
        let keys_on_floor = |world: &mut World, room: Entity| {
            let mut q = world.query_filtered::<(&WorldKey, &Located), With<Item>>();
            q.iter(world)
                .filter(|(k, l)| k.id == KEY && l.0 == room)
                .count()
        };
        let setup = |role: UserRole, level: i32| {
            let (mut world, room) = base_world();
            world
                .resource_mut::<ObjectResetCatalog>()
                .entries
                .push(ObjectResetEntry {
                    reset_id: 9,
                    object_zone_id: 1,
                    object_id: KEY,
                    room_entity: room,
                    max_instances: 1,
                });
            // The reset fills its slot, then someone picks the key up.
            run_respawn(&mut world, 6000);
            assert_eq!(keys_on_floor(&mut world, room), 1, "reset spawned it");
            let who = world
                .spawn((Player, holder(role), level_profile(level), Located(room)))
                .id();
            let key = {
                let mut q = world.query_filtered::<(Entity, &WorldKey), With<Item>>();
                q.iter(&world).find(|(_, k)| k.id == KEY).unwrap().0
            };
            world.entity_mut(key).insert(Located(who));
            (world, room, key)
        };

        let (mut world, room, key) = setup(UserRole::Immortal, 100);
        run_respawn(&mut world, 12000);
        assert_eq!(keys_on_floor(&mut world, room), 1, "the row refilled");
        assert!(
            world.get::<FromObjectReset>(key).is_none(),
            "the imm's copy no longer owns the slot"
        );
        // Dropping it back does not make the row own two instances: it is a
        // loose extra and the row stays satisfied.
        world.entity_mut(key).insert(Located(room));
        run_respawn(&mut world, 18000);
        assert_eq!(keys_on_floor(&mut world, room), 2);

        let (mut world, room, key) = setup(UserRole::Player, 10);
        run_respawn(&mut world, 12000);
        assert_eq!(keys_on_floor(&mut world, room), 0, "mortal keeps the slot");
        assert!(world.get::<FromObjectReset>(key).is_some());

        // A low-level alt on a staff website account is not staff: it keeps
        // the slot, so the row does not refill (no duplicate to hand on).
        let (mut world, room, key) = setup(UserRole::Immortal, 10);
        run_respawn(&mut world, 12000);
        assert_eq!(
            keys_on_floor(&mut world, room),
            0,
            "staff-account alt keeps the slot"
        );
        assert!(world.get::<FromObjectReset>(key).is_some());
        assert_eq!(
            *object_world_counts(&mut world).get(&(1, KEY)).unwrap_or(&0),
            1,
            "the alt's copy still counts toward the cap"
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

    /// A respawn, then the mob AI pulse: mobs only pick fights on the pulse
    /// (legacy `mobile_activity`), never at spawn time.
    fn respawn_and_pulse(world: &mut World, tick: u64) {
        run_respawn(world, tick);
        crate::commands::aggro_pulse(world);
    }

    #[test]
    fn respawned_aggressive_mob_waits_for_the_pulse() {
        let (mut world, _room, player, _rx) = aggro_world(vec![]);
        run_respawn(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(fighting_target(&world, mob), None, "not at spawn time");
        crate::commands::aggro_pulse(&mut world);
        assert_eq!(fighting_target(&world, mob), Some(player));
    }

    #[test]
    fn respawned_aggressive_mob_attacks_an_awake_visible_player() {
        let (mut world, _room, player, _rx) = aggro_world(vec![]);
        respawn_and_pulse(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(fighting_target(&world, mob), Some(player));
    }

    #[test]
    fn respawned_mob_does_not_attack_a_player_it_cannot_see() {
        let (mut world, _room, player, _rx) = aggro_world(vec![]);
        world.entity_mut(player).insert(mud_world::Invisible);
        respawn_and_pulse(&mut world, 6000);
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
        respawn_and_pulse(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(fighting_target(&world, mob), Some(player));
    }

    #[test]
    fn respawned_wimpy_mob_leaves_an_awake_player_alone_but_hits_a_sleeper() {
        use mud_db::enums::MobBehavior;
        let (mut world, _room, _player, _rx) = aggro_world(vec![MobBehavior::Wimpy]);
        respawn_and_pulse(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(fighting_target(&world, mob), None, "awake player");

        let (mut world, _room, player, _rx) = aggro_world(vec![MobBehavior::Wimpy]);
        world
            .entity_mut(player)
            .insert(mud_world::Posture(mud_world::PostureKind::Sleeping));
        respawn_and_pulse(&mut world, 6000);
        let mob = mob_of(&mut world, 1).expect("respawned");
        assert_eq!(fighting_target(&world, mob), Some(player), "sleeping");
    }

    #[test]
    fn respawned_mob_spares_staff() {
        let (mut world, _room, player, _rx) = aggro_world(vec![]);
        world.get_mut::<mud_world::Account>(player).unwrap().role =
            mud_db::enums::UserRole::Immortal;
        respawn_and_pulse(&mut world, 6000);
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
        respawn_and_pulse(&mut world, 6000);
        assert_eq!(fighting_target(&world, pet), None, "pet spares its owner");

        // Control: the same evil mob without a master does attack.
        world.entity_mut(pet).remove::<mud_world::Follower>();
        world
            .resource_mut::<MobRespawnTimers>()
            .last_death_tick
            .clear();
        let mob = mob_of(&mut world, 1).expect("reset mob alive");
        world.entity_mut(mob).despawn();
        respawn_and_pulse(&mut world, 12000);
        assert_eq!(fighting_target(&world, pet), Some(player));
    }

    /// Timing guard at prod scale: 5500 reset rows with a live mob and
    /// 4000 gear items each, 40 rows whose mob just died. The steady pass
    /// (nothing to refill) and the refill pass must both stay cheap.
    /// Hard limit only enforced in release.
    #[test]
    fn respawn_tick_prod_scale_is_fast() {
        let (mut world, _room) = base_world();
        let rooms: Vec<Entity> = (0..10_000).map(|_| world.spawn_empty().id()).collect();
        let gear = standard_gear();
        let mut mobs = Vec::new();
        for reset_id in 1..=5500_i32 {
            let r = rooms[usize::try_from(reset_id).unwrap() * 7 % rooms.len()];
            let rows: Vec<MobResetEquipment> = gear
                .iter()
                .map(|g| MobResetEquipment {
                    reset_id,
                    ..g.clone()
                })
                .collect();
            add_mob_reset(
                &mut world,
                r,
                reset_id,
                &rows[..usize::from(reset_id % 3 == 0)],
            );
            mobs.push(boot_mob(&mut world, r, reset_id));
        }
        for &m in mobs.iter().take(40) {
            world.despawn(m);
        }
        let start = std::time::Instant::now();
        run_respawn(&mut world, 60_000);
        let refill = start.elapsed();
        let start = std::time::Instant::now();
        run_respawn(&mut world, 60_060);
        let steady = start.elapsed();
        eprintln!("respawn_tick 5500 mobs: refill(40)={refill:?} steady={steady:?}");
        if !cfg!(debug_assertions) {
            assert!(refill.as_millis() < 20, "refill pass took {refill:?}");
            assert!(steady.as_millis() < 20, "steady pass took {steady:?}");
        }
    }
}

#[cfg(test)]
mod door_reset_tests {
    use super::*;
    use mud_db::enums::{Direction, ExitState};
    use mud_world::{ExitData, Named};

    const PERIOD: u64 = 600 * 30;

    fn door(to: Entity, state: ExitState) -> ExitData {
        ExitData {
            to: Some(to),
            state,
            key: None,
            description: None,
            keywords: vec!["door".into()],
            is_hidden: false,
            is_pickproof: false,
            is_bashable: true,
            hit_points: Some(50),
        }
    }

    /// Two rooms joined by an east/west door, both authored Locked, in
    /// separate zones when `second_zone` is set.
    fn world_with_door(mode: ResetMode, second_zone: bool) -> (World, Entity, Entity, Entity) {
        let mut world = World::new();
        let zone = world
            .spawn((Named { name: "z".into() }, ZoneDoorReset::new(mode, 30, 0)))
            .id();
        let zone_b = if second_zone {
            world
                .spawn((Named { name: "y".into() }, ZoneDoorReset::new(mode, 30, 0)))
                .id()
        } else {
            zone
        };
        let a = world.spawn((Room, Located(zone))).id();
        let b = world.spawn((Room, Located(zone_b))).id();
        for (room, dir, to) in [(a, Direction::East, b), (b, Direction::West, a)] {
            let mut exits = Exits::default();
            exits.0.insert(dir, door(to, ExitState::Locked));
            let mut defaults = ExitDefaults::default();
            defaults.0.insert(dir, (ExitState::Locked, Some(50)));
            world.entity_mut(room).insert((exits, defaults));
        }
        (world, zone, a, b)
    }

    fn state(world: &World, room: Entity, dir: Direction) -> ExitState {
        world.get::<Exits>(room).unwrap().0[&dir].state
    }

    fn open_both(world: &mut World, a: Entity, b: Entity) {
        world
            .get_mut::<Exits>(a)
            .unwrap()
            .0
            .get_mut(&Direction::East)
            .unwrap()
            .state = ExitState::Open;
        let mut eb = world.get_mut::<Exits>(b).unwrap();
        let w = eb.0.get_mut(&Direction::West).unwrap();
        w.state = ExitState::Open;
        w.hit_points = Some(5);
    }

    #[test]
    fn not_due_until_the_lifespan_passes() {
        let (mut world, _z, a, b) = world_with_door(ResetMode::Normal, false);
        open_both(&mut world, a, b);
        door_reset_pass(&mut world, PERIOD - 1);
        assert_eq!(state(&world, a, Direction::East), ExitState::Open);
        door_reset_pass(&mut world, PERIOD);
        assert_eq!(state(&world, a, Direction::East), ExitState::Locked);
        assert_eq!(state(&world, b, Direction::West), ExitState::Locked);
        assert_eq!(
            world.get::<Exits>(b).unwrap().0[&Direction::West].hit_points,
            Some(50),
            "bash damage is repaired"
        );
    }

    #[test]
    fn one_sided_change_restores_only_that_side_across_zones() {
        let (mut world, zone, a, b) = world_with_door(ResetMode::Normal, true);
        // Only room `a` (zone 1) deviates; the far side is in zone 2 and
        // already at its own default.
        world
            .get_mut::<Exits>(a)
            .unwrap()
            .0
            .get_mut(&Direction::East)
            .unwrap()
            .state = ExitState::Closed;
        world.get_mut::<ZoneDoorReset>(zone).unwrap().last_reset = 0;
        door_reset_pass(&mut world, PERIOD);
        assert_eq!(state(&world, a, Direction::East), ExitState::Locked);
        assert_eq!(state(&world, b, Direction::West), ExitState::Locked);
    }

    #[test]
    fn a_player_in_either_room_defers_the_door() {
        for occupied_side in 0..2 {
            let (mut world, zone, a, b) = world_with_door(ResetMode::Normal, false);
            open_both(&mut world, a, b);
            let visitor = world
                .spawn((Player, Located(if occupied_side == 0 { a } else { b })))
                .id();
            door_reset_pass(&mut world, PERIOD);
            assert_eq!(state(&world, a, Direction::East), ExitState::Open);
            assert_eq!(state(&world, b, Direction::West), ExitState::Open);
            // Retried after the short delay, not a whole lifespan later.
            let sched = *world.get::<ZoneDoorReset>(zone).unwrap();
            assert_eq!(
                sched.last_reset + sched.period_ticks,
                PERIOD + DOOR_RESET_RETRY_TICKS
            );
            world.despawn(visitor);
            door_reset_pass(&mut world, PERIOD + DOOR_RESET_RETRY_TICKS - 1);
            assert_eq!(state(&world, a, Direction::East), ExitState::Open);
            door_reset_pass(&mut world, PERIOD + DOOR_RESET_RETRY_TICKS);
            assert_eq!(state(&world, a, Direction::East), ExitState::Locked);
            assert_eq!(state(&world, b, Direction::West), ExitState::Locked);
        }
    }

    #[test]
    fn empty_zones_wait_for_everyone_to_leave_and_never_zones_never_reset() {
        let (mut world, _z, a, b) = world_with_door(ResetMode::Empty, false);
        open_both(&mut world, a, b);
        // A player far from the door but inside the zone blocks an Empty zone.
        let elsewhere = world
            .spawn((Room, Located(world.get::<Located>(a).unwrap().0)))
            .id();
        let p = world.spawn((Player, Located(elsewhere))).id();
        door_reset_pass(&mut world, PERIOD);
        assert_eq!(state(&world, a, Direction::East), ExitState::Open);
        world.despawn(p);
        door_reset_pass(&mut world, PERIOD + 60);
        assert_eq!(state(&world, a, Direction::East), ExitState::Locked);

        let (mut world, _z, a, b) = world_with_door(ResetMode::Never, false);
        open_both(&mut world, a, b);
        door_reset_pass(&mut world, PERIOD * 100);
        assert_eq!(state(&world, a, Direction::East), ExitState::Open);
    }

    /// Occupied `Empty` zones stay due until their players leave. They must
    /// not fill the bounded slice (most overdue first) and starve the
    /// `Normal` zones that can reset right now.
    #[test]
    fn occupied_empty_zones_do_not_hog_the_slice() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let room_in = |world: &mut World, mode: ResetMode, last_reset: u64| {
            let zone = world
                .spawn((
                    Named { name: "z".into() },
                    ZoneDoorReset::new(mode, 10, last_reset),
                ))
                .id();
            let mut exits = Exits::default();
            exits
                .0
                .insert(Direction::East, door(target, ExitState::Open));
            let mut defaults = ExitDefaults::default();
            defaults
                .0
                .insert(Direction::East, (ExitState::Locked, Some(50)));
            world.spawn((Room, Located(zone), exits, defaults)).id()
        };
        // More overdue than the Normal zones (reset at 0, not 100).
        let occupied: Vec<Entity> = (0..DOOR_RESET_ZONES_PER_PASS)
            .map(|_| room_in(&mut world, ResetMode::Empty, 0))
            .collect();
        for &room in &occupied {
            world.spawn((Player, Located(room)));
        }
        let normal: Vec<Entity> = (0..3)
            .map(|_| room_in(&mut world, ResetMode::Normal, 100))
            .collect();
        door_reset_pass(&mut world, 10 * 600 + 100);
        for &room in &normal {
            assert_eq!(state(&world, room, Direction::East), ExitState::Locked);
        }
        for &room in &occupied {
            assert_eq!(state(&world, room, Direction::East), ExitState::Open);
        }
    }

    /// Prod scale (190 zones, 13k rooms, all on the same lifespan so they all
    /// fall due on one tick): a pass resets a bounded slice of zones, the
    /// rest follow on later passes, and every door ends up reset. Round 8
    /// saw 104-157 ms slow ticks every 10 minutes (debug) when every due
    /// zone was reset, with a full room scan each, in one tick.
    #[test]
    fn many_zones_falling_due_together_are_reset_in_slices() {
        const ZONES: usize = 190;
        const ROOMS_PER_ZONE: usize = 70;
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let mut doors: Vec<(Entity, Entity)> = Vec::new();
        for z in 0..ZONES {
            let zone = world
                .spawn((
                    Named {
                        name: format!("z{z}"),
                    },
                    ZoneDoorReset::new(ResetMode::Normal, 10, 0),
                ))
                .id();
            for _ in 0..ROOMS_PER_ZONE {
                let mut exits = Exits::default();
                exits
                    .0
                    .insert(Direction::East, door(target, ExitState::Open));
                let mut defaults = ExitDefaults::default();
                defaults
                    .0
                    .insert(Direction::East, (ExitState::Locked, Some(50)));
                let room = world.spawn((Room, Located(zone), exits, defaults)).id();
                doors.push((zone, room));
            }
        }
        let due_tick = 10 * 600;
        let locked = |world: &World| {
            doors
                .iter()
                .filter(|(_, r)| state(world, *r, Direction::East) == ExitState::Locked)
                .count()
        };
        assert_eq!(locked(&world), 0);

        let start = std::time::Instant::now();
        door_reset_pass(&mut world, due_tick);
        let first = start.elapsed();
        assert_eq!(
            locked(&world),
            DOOR_RESET_ZONES_PER_PASS * ROOMS_PER_ZONE,
            "one pass resets only its slice of zones"
        );

        let mut passes = 1;
        let mut worst = first;
        while locked(&world) < doors.len() {
            let start = std::time::Instant::now();
            door_reset_pass(&mut world, due_tick + passes * 60);
            worst = worst.max(start.elapsed());
            passes += 1;
            assert!(passes < 200, "zones must all be reset eventually");
        }
        eprintln!("door_reset_pass {ZONES} zones: first={first:?} worst={worst:?} passes={passes}");
        assert_eq!(
            usize::try_from(passes).unwrap(),
            ZONES.div_ceil(DOOR_RESET_ZONES_PER_PASS)
        );
        // Once everyone has been reset nothing is due again until the next lifespan.
        door_reset_pass(&mut world, due_tick + passes * 60 + 60);
        assert!(
            doors
                .iter()
                .all(|(z, _)| { world.get::<ZoneDoorReset>(*z).unwrap().last_reset >= due_tick })
        );
        // Loose bound (debug build, shared CI box): a slice is a few ms, not 100+.
        assert!(worst.as_millis() < 60, "slowest pass took {worst:?}");
    }

    #[test]
    fn only_the_due_zone_is_touched() {
        let (mut world, zone, a, b) = world_with_door(ResetMode::Normal, true);
        open_both(&mut world, a, b);
        // Zone of room `b` was reset just now; zone of `a` is due. Each zone
        // restores only its own rooms' sides, so `b` is left alone.
        let zb = world.get::<Located>(b).unwrap().0;
        world.get_mut::<ZoneDoorReset>(zb).unwrap().last_reset = PERIOD;
        door_reset_pass(&mut world, PERIOD);
        assert_eq!(state(&world, a, Direction::East), ExitState::Locked);
        assert_eq!(state(&world, b, Direction::West), ExitState::Open);
        assert_eq!(world.get::<ZoneDoorReset>(zone).unwrap().last_reset, PERIOD);
        assert_eq!(world.get::<ZoneDoorReset>(zb).unwrap().last_reset, PERIOD);
    }

    /// A door whose sides have different authored defaults (472:200 south
    /// locked with a key, 472:201 north open and keyless): each side is
    /// restored from its OWN defaults, whichever zone runs first.
    #[test]
    fn asymmetric_sides_each_restore_to_their_own_defaults() {
        for second_zone in [false, true] {
            let (mut world, _z, a, b) = world_with_door(ResetMode::Normal, second_zone);
            // `a` east: locked, hp 50 (the fixture). `b` west: open, keyless,
            // no hp.
            world
                .get_mut::<ExitDefaults>(b)
                .unwrap()
                .0
                .insert(Direction::West, (ExitState::Open, None));
            {
                let mut eb = world.get_mut::<Exits>(b).unwrap();
                let w = eb.0.get_mut(&Direction::West).unwrap();
                w.state = ExitState::Open;
                w.hit_points = None;
            }
            world
                .get_mut::<Exits>(a)
                .unwrap()
                .0
                .get_mut(&Direction::East)
                .unwrap()
                .key = Some((470, 19));
            // Players picked the lock and walked through; both sides now open.
            {
                let mut ea = world.get_mut::<Exits>(a).unwrap();
                let e = ea.0.get_mut(&Direction::East).unwrap();
                e.state = ExitState::Open;
                e.hit_points = Some(7);
            }
            door_reset_pass(&mut world, PERIOD);
            let ea = &world.get::<Exits>(a).unwrap().0[&Direction::East];
            assert_eq!(ea.state, ExitState::Locked, "locked side is locked again");
            assert_eq!(ea.key, Some((470, 19)), "still keyed");
            assert_eq!(ea.hit_points, Some(50));
            let eb = &world.get::<Exits>(b).unwrap().0[&Direction::West];
            assert_eq!(eb.state, ExitState::Open, "open side stays open");
            assert_eq!(eb.hit_points, None);
            // And a deviating open side is restored to Open, not to Locked.
            world
                .get_mut::<Exits>(b)
                .unwrap()
                .0
                .get_mut(&Direction::West)
                .unwrap()
                .state = ExitState::Closed;
            for z in [a, b] {
                let zone = world.get::<Located>(z).unwrap().0;
                world.get_mut::<ZoneDoorReset>(zone).unwrap().last_reset = 0;
            }
            door_reset_pass(&mut world, PERIOD * 2);
            assert_eq!(state(&world, b, Direction::West), ExitState::Open);
            assert_eq!(state(&world, a, Direction::East), ExitState::Locked);
        }
    }
}
