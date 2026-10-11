//! Reset-time gear: the single place that turns `MobResetEquipment`,
//! `MobResetEquipmentContents` and `ObjectResetContents` rows into live
//! item entities.
//!
//! Both the boot loader and the runtime respawn tick call into here,
//! so a mob that dies and respawns comes back wearing and carrying
//! exactly what it had at boot (legacy `reset_zone` re-ran the `E` /
//! `G` / `P` commands after every `M` load).
//!
//! Item limits follow legacy `reset_zone`: an `E` / `G` command only
//! loads when the world already holds fewer than `max` copies of the
//! object. The importer stores that legacy `max` as `probability *
//! 100` on `MobResetEquipment` (and always `1` in `max_instances`),
//! so the world-wide cap for a gear row is `round(probability * 100)`.
//! A unique sword (`max` 1) therefore exists once, no matter how many
//! times its carrier respawns. The row-ownership invariant for mobs
//! and room objects is untouched: one reset row still owns one mob.

use std::collections::{HashMap, HashSet};

use bevy_ecs::prelude::*;
use mud_db::{
    mob_reset_equipment::{MobResetEquipment, MobResetEquipmentContent},
    object_reset_contents::ObjectResetContent,
};

use crate::components::{
    AttachedTriggers, Description, EquippedSlot, Item, Keywords, LightFuel, Located, Named, Slot,
    WorldKey,
};
use crate::resources::{ObjectProto, ObjectPrototypes, TriggerCatalog};

/// One piece of gear a reset-spawned mob receives.
#[derive(Debug, Clone)]
pub struct MobGearEntry {
    /// `MobResetEquipment.id` of the row this entry came from.
    pub equipment_id: i32,
    pub object_zone_id: i32,
    pub object_id: i32,
    /// Worn slot; `None` means the item goes in the mob's inventory.
    pub slot: Option<Slot>,
    /// World-wide cap on live copies of this object (legacy `max`).
    pub cap: i32,
    /// Items that spawn inside this one when it is a container the mob
    /// carries or wears (`MobResetEquipmentContents`; legacy `P` after `G` /
    /// `E`). Same row shape and cap rules as `ObjectContentsCatalog`.
    pub contents: Vec<ContentEntry>,
}

/// Gear per `MobResets.id`, in `MobResetEquipment.id` order.
#[derive(Resource, Debug, Default)]
pub struct MobGearCatalog {
    pub by_reset: HashMap<i32, Vec<MobGearEntry>>,
}

/// One nested-content row of an `ObjectResets` container.
#[derive(Debug, Clone)]
pub struct ContentEntry {
    pub id: i32,
    pub parent_content_id: Option<i32>,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub quantity: i32,
    /// World-wide cap on live copies of this object (legacy `P` max).
    pub cap: i32,
}

/// Container contents per `ObjectResets.id`.
#[derive(Resource, Debug, Default)]
pub struct ObjectContentsCatalog {
    pub by_reset: HashMap<i32, Vec<ContentEntry>>,
}

/// Counts reported by [`outfit_mob`] / [`fill_container`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GearStats {
    pub spawned: usize,
    pub skipped: usize,
}

/// World-wide cap encoded by an equipment row's `probability`
/// (the importer's `legacy max / 100`). Always at least 1.
#[must_use]
pub fn gear_cap(probability: f64) -> i32 {
    #[allow(clippy::cast_possible_truncation)]
    let pct = (probability * 100.0).round() as i32;
    pct.max(1)
}

/// Fold equipment rows (and the `content_rows` nested inside the ones that
/// are containers) into catalog entries. Rows with a non-positive
/// probability are disabled; rows pointing at an unknown prototype are
/// dropped (returned count lets the loader report them).
#[must_use]
pub fn build_gear_entries(
    rows: &[MobResetEquipment],
    content_rows: &[MobResetEquipmentContent],
    protos: &ObjectPrototypes,
) -> (HashMap<i32, Vec<MobGearEntry>>, usize) {
    let mut contents_by_equipment: HashMap<i32, Vec<ContentEntry>> = HashMap::new();
    for c in content_rows {
        contents_by_equipment
            .entry(c.equipment_id)
            .or_default()
            .push(ContentEntry {
                id: c.id,
                parent_content_id: c.parent_content_id,
                object_zone_id: c.object_zone_id,
                object_id: c.object_id,
                quantity: c.quantity,
                cap: c.max_instances.max(1),
            });
    }
    let mut by_reset: HashMap<i32, Vec<MobGearEntry>> = HashMap::new();
    let mut skipped = 0usize;
    for eq in rows {
        if eq.probability <= 0.0 {
            continue;
        }
        if !protos
            .by_key
            .contains_key(&(eq.object_zone_id, eq.object_id))
        {
            skipped += 1;
            continue;
        }
        by_reset.entry(eq.reset_id).or_default().push(MobGearEntry {
            equipment_id: eq.id,
            object_zone_id: eq.object_zone_id,
            object_id: eq.object_id,
            slot: eq.wear_location.as_deref().and_then(Slot::from_label_warn),
            cap: gear_cap(eq.probability),
            contents: contents_by_equipment.remove(&eq.id).unwrap_or_default(),
        });
    }
    (by_reset, skipped)
}

/// Group content rows by their owning reset.
#[must_use]
pub fn build_content_entries(rows: &[ObjectResetContent]) -> HashMap<i32, Vec<ContentEntry>> {
    let mut by_reset: HashMap<i32, Vec<ContentEntry>> = HashMap::new();
    for r in rows {
        by_reset.entry(r.reset_id).or_default().push(ContentEntry {
            id: r.id,
            parent_content_id: r.parent_content_id,
            object_zone_id: r.object_zone_id,
            object_id: r.object_id,
            quantity: r.quantity,
            cap: r.max_instances.max(1),
        });
    }
    by_reset
}

/// Live copies of every object prototype currently in the world
/// (loose, carried, worn, in containers, in corpses). The `E` / `G`
/// cap check and the object respawn cap read this; callers keep it
/// current across a batch of [`outfit_mob`] calls instead of
/// re-scanning per mob.
///
/// Items held by staff (Immortal+, see [`crate::bypasses_restrictions`]),
/// directly or through any chain of containers, do not count: an imm
/// hoarding a limited key or quest item must not block it from spawning.
pub fn object_world_counts(world: &mut World) -> HashMap<(i32, i32), i32> {
    let mut counts: HashMap<(i32, i32), i32> = HashMap::new();
    let mut q = world.query_filtered::<(Entity, &WorldKey), With<Item>>();
    let items: Vec<(Entity, i32, i32)> = q.iter(world).map(|(e, wk)| (e, wk.zone, wk.id)).collect();
    for (entity, zone, id) in items {
        if held_by_staff(world, entity) {
            continue;
        }
        *counts.entry((zone, id)).or_insert(0) += 1;
    }
    counts
}

/// Whether the top-level holder of `item` (following `Located` up through
/// containers until a non-item) is a staff character.
fn held_by_staff(world: &World, item: Entity) -> bool {
    let mut cur = item;
    // Bounded walk: a malformed containment cycle must not hang the tick.
    for _ in 0..64 {
        let Some(parent) = world.get::<Located>(cur).map(|l| l.0) else {
            return false;
        };
        if world.get::<Item>(parent).is_some() {
            cur = parent;
            continue;
        }
        return crate::bypasses_restrictions(world, parent);
    }
    false
}

/// The finite charge count a fresh instance of object prototype
/// `(zone, id)` starts with: the first `ObjectAbilities` binding's
/// `charges`. `None` for items without a charged binding (scrolls,
/// potions, plain gear); wands, staves and instruments in the data
/// always carry one.
#[must_use]
pub fn proto_charges(world: &World, zone: i32, id: i32) -> Option<i32> {
    world
        .get_resource::<crate::resources::ObjectAbilityCatalog>()?
        .by_key
        .get(&(zone, id))
        .and_then(|v| v.first().and_then(|b| b.charges))
}

/// Give a freshly spawned instance of prototype `(zone, id)` its starting
/// `Charges`. Every proto-based spawn site calls this: an item with no
/// `Charges` component is treated as unlimited, so a spawn that skipped it
/// would hand out an endless wand.
pub fn attach_proto_charges(world: &mut World, item: Entity, zone: i32, id: i32) {
    if let Some(charges) = proto_charges(world, zone, id)
        && let Ok(mut e) = world.get_entity_mut(item)
    {
        e.insert(crate::components::Charges(charges));
    }
}

/// Spawn one item instance of `proto` inside `parent` (a mob, or a
/// container). Worn when `slot` is set. Never gives the item any
/// persistence marker: items on mobs belong to the world, not to a
/// character, and are never written to `CharacterItems`.
fn spawn_item(
    world: &mut World,
    proto: &ObjectProto,
    parent: Entity,
    slot: Option<Slot>,
) -> Entity {
    let trigger_keys = world.get_resource::<TriggerCatalog>().and_then(|t| {
        t.object_attachments
            .get(&(proto.zone_id, proto.id))
            .cloned()
    });
    let on_mob = world.get::<crate::components::Mob>(parent).is_some();
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
        Located(parent),
    ));
    if let Some(desc) = proto.examine_description.clone() {
        bundle.insert(Description(desc));
    }
    if let Some(s) = slot {
        bundle.insert(EquippedSlot(s));
    }
    // A worn light needs its fuel state: without it the light stays
    // dark (no fuel data is never assumed infinite).
    if let Some(fuel) = proto.light_fuel {
        bundle.insert(LightFuel {
            capacity: fuel.capacity,
            remaining: fuel.remaining,
        });
    }
    if let Some(keys) = trigger_keys {
        bundle.insert(AttachedTriggers(keys));
    }
    if !proto.flags.is_empty() {
        bundle.insert(crate::components::ObjectFlags(proto.flags.clone()));
    }
    // A hidden prototype stays hidden inside a container; on a mob it is
    // carried, and carried things are never hidden.
    if let Some(h) = proto.initial_hiddenness()
        && !on_mob
    {
        bundle.insert(h);
    }
    if !proto.restrictions.is_empty() {
        bundle.insert(crate::components::ObjectRestrictions(
            proto.restrictions.clone(),
        ));
    }
    let item = bundle.id();
    attach_proto_charges(world, item, proto.zone_id, proto.id);
    item
}

/// Equip and stock `mob` (spawned by `MobResets` row `reset_id`) from
/// the [`MobGearCatalog`]: worn slots, then inventory. Items whose
/// world-wide cap is already reached are skipped, like legacy
/// `reset_zone`. `counts` is the running world count from
/// [`object_world_counts`]; it is updated for every spawned item.
///
/// Callers must run `recompute_equipped_for` on the mob afterwards so
/// gear bonuses and worn lights take effect (boot does this for every
/// mob in one pass; the respawn tick does it for each new mob).
#[allow(clippy::implicit_hasher)]
pub fn outfit_mob(
    world: &mut World,
    mob: Entity,
    reset_id: i32,
    counts: &mut HashMap<(i32, i32), i32>,
) -> GearStats {
    let mut stats = GearStats::default();
    let Some(entries) = world
        .get_resource::<MobGearCatalog>()
        .and_then(|c| c.by_reset.get(&reset_id).cloned())
    else {
        return stats;
    };
    // Slots already filled on this mob by earlier entries: a second item
    // for a paired position (two `EARS`/`WRIST` rows) takes the other
    // side; with nothing free it stays in inventory instead of doubling
    // up in an occupied slot.
    let mut worn: HashSet<Slot> = HashSet::new();
    for entry in entries {
        let key = (entry.object_zone_id, entry.object_id);
        let proto = world
            .get_resource::<ObjectPrototypes>()
            .and_then(|p| p.by_key.get(&key).cloned());
        let Some(proto) = proto else {
            stats.skipped += 1 + entry.contents.len();
            continue;
        };
        if counts.get(&key).copied().unwrap_or(0) >= entry.cap {
            stats.skipped += 1 + entry.contents.len();
            continue;
        }
        let slot = entry.slot.and_then(|s| s.first_free(|c| worn.contains(&c)));
        if let Some(slot) = slot {
            worn.insert(slot);
        }
        let item = spawn_item(world, &proto, mob, slot);
        *counts.entry(key).or_insert(0) += 1;
        stats.spawned += 1;
        // A carried/worn container brings its contents along (legacy `P`
        // after `G` / `E`): nested inside the item, to any depth.
        if !entry.contents.is_empty() {
            let inner = fill_from_rows(world, &[item], &entry.contents, counts);
            stats.spawned += inner.spawned;
            stats.skipped += inner.skipped;
        }
    }
    stats
}

/// Materialize the [`ObjectContentsCatalog`] rows of `reset_id` inside
/// each of `containers` (the entity spawned by that `ObjectResets`
/// row). Handles arbitrary nesting: a row whose parent content row has
/// not been spawned yet waits for a later sweep. Rows whose parent or
/// prototype cannot be resolved, and copies that would exceed the row's
/// world-wide `cap`, are counted as skipped (legacy `P` only loads
/// while the live count is below `max`). `counts` is the running world
/// count from [`object_world_counts`]; it is updated for every spawn.
#[allow(clippy::implicit_hasher)]
pub fn fill_container(
    world: &mut World,
    containers: &[Entity],
    reset_id: i32,
    counts: &mut HashMap<(i32, i32), i32>,
) -> GearStats {
    let Some(rows) = world
        .get_resource::<ObjectContentsCatalog>()
        .and_then(|c| c.by_reset.get(&reset_id).cloned())
    else {
        return GearStats::default();
    };
    fill_from_rows(world, containers, &rows, counts)
}

/// Spawn `rows` (a content tree: `parent_content_id` `None` = directly in
/// each of `containers`) through the shared
/// [`spawn_item_from_proto`](crate::item_spawn::spawn_item_from_proto).
/// The engine behind [`fill_container`] and the contents of a mob's
/// carried/worn containers in [`outfit_mob`].
#[allow(clippy::implicit_hasher)]
fn fill_from_rows(
    world: &mut World,
    containers: &[Entity],
    rows: &[ContentEntry],
    counts: &mut HashMap<(i32, i32), i32>,
) -> GearStats {
    let mut stats = GearStats::default();
    let mut spawned_by_content: HashMap<i32, Vec<Entity>> = HashMap::with_capacity(rows.len());
    let mut pending: Vec<&ContentEntry> = rows.iter().collect();
    while !pending.is_empty() {
        let before = pending.len();
        let mut still_pending: Vec<&ContentEntry> = Vec::new();
        for row in pending {
            let parents: Vec<Entity> = match row.parent_content_id {
                None => containers.to_vec(),
                Some(pcid) => {
                    let Some(p) = spawned_by_content.get(&pcid) else {
                        still_pending.push(row);
                        continue;
                    };
                    p.clone()
                }
            };
            let key = (row.object_zone_id, row.object_id);
            let proto = world
                .get_resource::<ObjectPrototypes>()
                .and_then(|p| p.by_key.get(&key).cloned());
            let Some(proto) = proto else {
                stats.skipped += 1;
                continue;
            };
            let qty = usize::try_from(row.quantity.max(1)).unwrap_or(1);
            let mut made: Vec<Entity> = Vec::with_capacity(parents.len() * qty);
            for parent in parents {
                for _ in 0..qty {
                    if counts.get(&key).copied().unwrap_or(0) >= row.cap {
                        stats.skipped += 1;
                        continue;
                    }
                    made.push(crate::item_spawn::spawn_item_from_proto(
                        world, &proto, parent,
                    ));
                    *counts.entry(key).or_insert(0) += 1;
                    stats.spawned += 1;
                }
            }
            spawned_by_content.insert(row.id, made);
        }
        if still_pending.len() == before {
            // Parent row never materialized (missing proto, cycle).
            stats.skipped += still_pending.len();
            break;
        }
        pending = still_pending;
    }
    stats
}
