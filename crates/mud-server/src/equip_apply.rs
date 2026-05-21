//! Gear-on-wear stat application. The player wears a sword: its
//! `ObjectEffects` rows of `effect_type = "modify"` (carrying
//! `modifier_data = {"target": "<stat>", "amount": <int>}`) flow into
//! the wearer's `CombatStats` / `CoreStats` / `Health` / etc. via
//! `commands::apply_modify_delta`. Spell-like `ObjectEffects`
//! (sanctuary rings, etc.) spawn `EffectInstance` entities tagged
//! with `GrantedByItem` so unequip can despawn only this item's
//! grants. `ObjectResistance` rows roll into the wearer's
//! `Resistances` map. Symmetric `unapply_*` reverses every change.
//!
//! Hooks:
//! - `wear_into` → `apply_object_to_wearer`
//! - `cmd_remove` → `unapply_object_from_wearer`
//! - login `respawn_inventory_from_db` → `recompute_equipped_for`
//! - mob equipment loader pass → `recompute_equipped_for`
//!
//! Idempotent unapply guard: every apply records a `GrantedDelta`
//! companion on the item so unapply replays the exact same deltas
//! even if `Object.protos` change underfoot. Mirrors the legacy C++
//! invariant where `effect_modify(add=false)` walked the same APPLY
//! list the equip pass walked.
//!
//! History: predecessor table `ObjectAffects` (legacy `(location,
//! modifier)`) was retired on 2026-05-12 (Wave 3.4-3.7). Its rows
//! were backfilled into `ObjectEffects` via
//! `fierylib/scripts/migrate_object_affects.py`.

use bevy_ecs::prelude::*;
use mud_world::{
    AppliedTo, EffectInstance, EffectSource, EquippedSlot, GrantedByItem, ObjectGrantedEffect,
    ObjectPrototypes, Resistances, WorldKey,
};

use crate::commands::{apply_modify_delta, reverse_modify_delta, try_insert};

/// Per-item bookkeeping: the `(stat_key, applied_delta)` pairs we
/// pushed onto the wearer when the item was equipped. Stored on the
/// item entity so unequip can replay the exact same list — even if
/// the proto changes between equip and remove (admin reload, schema
/// edit, etc.).
#[derive(Component, Debug, Clone, Default)]
pub struct GrantedDeltas {
    pub deltas: Vec<(String, i32)>,
    /// EffectInstance entities spawned from spell-like grants while
    /// this item was worn. Despawned on remove.
    pub effects: Vec<Entity>,
    /// `(element, value)` rolled into the wearer's `Resistances` map.
    /// Subtracted on remove.
    pub resistances: Vec<(mud_db::enums::ElementType, i32)>,
}

/// Apply every gear bonus from `item` onto `wearer`. Records the
/// applied deltas on the item via `GrantedDeltas` so `unapply` can
/// replay them exactly. Modify-type `ObjectEffects` (with
/// `modifier_data = {target, amount}`) call into
/// `apply_modify_delta`; non-modify effects spawn as `EffectInstance`s
/// tagged with `GrantedByItem(item)` for the despawn path.
/// Resistances accumulate into the wearer's `Resistances` map.
///
/// No-op when the item lacks a `WorldKey` (synthetic items),
/// when the proto is missing from the catalog (skipped at load
/// time), or when the wearer no longer exists.
pub fn apply_object_to_wearer(world: &mut World, item: Entity, wearer: Entity) {
    if world.get_entity(wearer).is_err() || world.get_entity(item).is_err() {
        return;
    }
    let key = match world.get::<WorldKey>(item).copied() {
        Some(k) => k,
        None => return,
    };
    let proto = world
        .get_resource::<ObjectPrototypes>()
        .and_then(|p| p.by_key.get(&(key.zone, key.id)).cloned());
    let Some(proto) = proto else {
        return;
    };
    // ---- Resistances ----
    let mut applied_resistances: Vec<(mud_db::enums::ElementType, i32)> = Vec::new();
    if !proto.resistances.is_empty() {
        // Ensure the wearer has a Resistances component (cheap lazy
        // init; avoid creating empty ones for non-resistant gear).
        if world.get::<Resistances>(wearer).is_none() {
            try_insert(world, wearer, Resistances::default());
        }
        if let Some(mut res) = world.get_mut::<Resistances>(wearer) {
            for (element, value, _allow_absorption) in &proto.resistances {
                if *value == 0 {
                    continue;
                }
                let entry = res.0.entry(*element).or_insert(0);
                *entry = entry.saturating_add(*value);
                applied_resistances.push((*element, *value));
            }
        }
    }
    // ---- Granted effects ----
    // Filter by wear_location first so a "ring of haste" only grants
    // when worn on a finger (not when wielded as a thrown weapon).
    let equipped_slot = world.get::<EquippedSlot>(item).map(|e| e.0);
    let granted_effects_to_spawn: Vec<ObjectGrantedEffect> = proto
        .granted_effects
        .iter()
        .filter(|grant| {
            let Some(needed_wear) = grant.wear_location else {
                return true; // any-slot grant
            };
            // Only fires when wear_location matches the item's
            // current equipped slot. The caller guarantees the item
            // already has EquippedSlot set; absent slot = skip
            // (carried-not-worn shouldn't grant a worn-only effect).
            let Some(slot) = equipped_slot else {
                return false;
            };
            crate::equip_apply::wear_flag_matches_slot(needed_wear, slot)
        })
        .cloned()
        .collect();
    let mut applied_deltas: Vec<(String, i32)> = Vec::new();
    let mut spawned_effect_entities: Vec<Entity> = Vec::new();
    // ---- Base armor (typed Objects.armor_pct column) ----
    // Distinct from apply-block bonuses (which flow through
    // ObjectEffects below): this is the per-slot armor mitigation
    // the item type itself provides, pre-scaled at fierylib import
    // time. Recorded in `applied_deltas` so unequip reverses it
    // through the same path apply-block deltas use.
    if proto.armor_pct != 0
        && apply_modify_delta(world, wearer, "armor_pct", proto.armor_pct)
    {
        applied_deltas.push(("armor_pct".to_string(), proto.armor_pct));
    }
    for grant in granted_effects_to_spawn {
        let effect_def = world
            .get_resource::<mud_world::EffectCatalog>()
            .and_then(|c| c.by_id.get(&grant.effect_id).cloned());
        let Some(def) = effect_def else {
            tracing::warn!(
                proto_zone = proto.zone_id,
                proto_id = proto.id,
                effect_id = grant.effect_id,
                "ObjectEffect references missing EffectCatalog row; skipped",
            );
            continue;
        };
        // Modify-type effects don't spawn an EffectInstance — they
        // call straight into apply_modify_delta with the
        // `(target, amount)` pulled from modifier_data. Recorded in
        // `applied_deltas` so unapply reverses them. This is the
        // post-Wave-3.7 successor to the legacy ObjectAffects path.
        if def.effect_type == "modify" {
            let target = grant
                .modifier_data
                .get("target")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let amount = grant
                .modifier_data
                .get("amount")
                .and_then(serde_json::Value::as_i64)
                .map(|n| n as i32);
            match (target, amount) {
                (Some(t), Some(a)) if a != 0 => {
                    if apply_modify_delta(world, wearer, &t, a) {
                        applied_deltas.push((t, a));
                    } else {
                        tracing::warn!(
                            proto_zone = proto.zone_id,
                            proto_id = proto.id,
                            target = %t,
                            amount = a,
                            "ObjectEffect modify: unsupported target, skipped"
                        );
                    }
                }
                (Some(_), Some(_)) => { /* zero delta — no-op */ }
                _ => {
                    tracing::warn!(
                        proto_zone = proto.zone_id,
                        proto_id = proto.id,
                        modifier_data = %grant.modifier_data,
                        "ObjectEffect modify: missing/invalid target or amount in modifier_data"
                    );
                }
            }
            continue;
        }
        // Spell-like effect: spawn an EffectInstance pinned to the
        // wearer for as long as the item is worn.
        let entity = world
            .spawn((
                EffectInstance {
                    kind: def.id,
                    name: def.name.clone(),
                    strength: grant.strength.max(1),
                    // Permanent — gear-granted effects last as long
                    // as the item is worn. The unequip path despawns
                    // them; effects_tick never decrements -1.
                    remaining_secs: -1,
                    source: EffectSource::Item,
                    ability_id: None,
                },
                AppliedTo(wearer),
                GrantedByItem(item),
            ))
            .id();
        spawned_effect_entities.push(entity);
    }
    // ---- Bookkeeping for unapply ----
    let bookkeeping = GrantedDeltas {
        deltas: applied_deltas,
        effects: spawned_effect_entities,
        resistances: applied_resistances,
    };
    if !bookkeeping.deltas.is_empty()
        || !bookkeeping.effects.is_empty()
        || !bookkeeping.resistances.is_empty()
    {
        try_insert(world, item, bookkeeping);
    }
}

/// Reverse `apply_object_to_wearer`. Reads the `GrantedDeltas`
/// companion off the item and replays it inverted. Despawns
/// gear-granted effects. Subtracts resistances. Removes the
/// `GrantedDeltas` component when done. No-op when the bookkeeping
/// is missing (item never went through `apply_object_to_wearer`).
pub fn unapply_object_from_wearer(world: &mut World, item: Entity, wearer: Entity) {
    if world.get_entity(item).is_err() {
        return;
    }
    let bookkeeping = world.get::<GrantedDeltas>(item).cloned();
    let Some(bookkeeping) = bookkeeping else {
        return;
    };
    // Stat deltas — reverse each.
    for (key, amount) in &bookkeeping.deltas {
        reverse_modify_delta(world, wearer, key, *amount);
    }
    // Resistances — subtract from the wearer's map. Drop entries
    // that fall back to 0 to keep the map sparse.
    if !bookkeeping.resistances.is_empty()
        && let Some(mut res) = world.get_mut::<Resistances>(wearer)
    {
        for (element, value) in &bookkeeping.resistances {
            if let Some(entry) = res.0.get_mut(element) {
                *entry = entry.saturating_sub(*value);
                if *entry == 0 {
                    res.0.remove(element);
                }
            }
        }
    }
    // Despawn gear-granted effects. The effects_tick path doesn't
    // care if the entity vanishes between ticks; AppliedTo is just
    // an edge.
    for effect_entity in &bookkeeping.effects {
        if let Ok(em) = world.get_entity_mut(*effect_entity) {
            em.despawn();
        }
    }
    if let Ok(mut e) = world.get_entity_mut(item) {
        e.remove::<GrantedDeltas>();
    }
}

/// Apply gear bonuses for *every* currently-equipped item on
/// `wearer`. Used by:
/// - login path (after items respawn from `CharacterItems`)
/// - mob equipment loader pass (after items spawn into mob slots)
///
/// Iterates equipped items and calls `apply_object_to_wearer` for
/// each. Skips items that already have a `GrantedDeltas` companion
/// to keep the call idempotent (a re-login that double-walks
/// shouldn't double-stack stats).
pub fn recompute_equipped_for(world: &mut World, wearer: Entity) {
    let equipped: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &mud_world::Located, &EquippedSlot), With<mud_world::Item>>();
        q.iter(world)
            .filter(|(_, l, _)| l.0 == wearer)
            .map(|(e, _, _)| e)
            .collect()
    };
    for item in equipped {
        if world.get::<GrantedDeltas>(item).is_some() {
            continue;
        }
        apply_object_to_wearer(world, item, wearer);
    }
}

/// Best-effort match between an `ObjectEffects.wear_location`
/// `WearFlag` and the runtime `Slot` an item is occupying. The
/// schema's WearFlag is finer-grained than the runtime Slot
/// (Mainhand/Offhand/Twohand all collapse onto Slot::Wield in
/// the runtime), so we collapse on the Slot side. Returns true
/// when the worn slot satisfies the grant's restriction.
#[must_use]
pub fn wear_flag_matches_slot(flag: mud_db::enums::WearFlag, slot: mud_world::Slot) -> bool {
    use mud_db::enums::WearFlag::*;
    use mud_world::Slot;
    match (flag, slot) {
        (Finger, Slot::LeftFinger | Slot::RightFinger) => true,
        (Neck, Slot::Neck) => true,
        (Ear, Slot::Ears) => true,
        (Wrist, Slot::Wrist) => true,
        (Head, Slot::Head) => true,
        (Eyes, Slot::Eyes) => true,
        (Face, Slot::Face) => true,
        (Body, Slot::Body) => true,
        (About, Slot::About) => true,
        (Arms, Slot::Arms) => true,
        (Hands, Slot::Hands) => true,
        (Waist | Belt, Slot::Waist) => true,
        (Legs, Slot::Legs) => true,
        (Feet, Slot::Feet) => true,
        (Mainhand | Offhand | Twohand, Slot::Wield | Slot::Hold) => true,
        (Badge, Slot::Badge) => true,
        (Hover, Slot::Hover) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::{
        CombatStats, CoreStats, EffectCatalog, EffectDef, Health, Item, Located, Named,
        ObjectProto, ObjectPrototypes, Slot, Stamina, WorldKey,
    };

    /// Build a minimal EffectCatalog containing the "modify" effect
    /// (id=3 in the live DB; arbitrary here as long as it matches
    /// what the test proto references).
    fn make_catalog_with_modify(modify_id: i32) -> EffectCatalog {
        let mut catalog = EffectCatalog::default();
        catalog.by_id.insert(
            modify_id,
            EffectDef {
                id: modify_id,
                name: "modify".into(),
                effect_type: "modify".into(),
                description: None,
                tags: Vec::new(),
                presence_override: None,
                default_params: serde_json::Value::Object(serde_json::Map::new()),
                prevents_speaking: false,
                prevents_casting: false,
                prevents_movement: false,
                on_apply: None,
                on_tick: None,
                on_remove: None,
            },
        );
        catalog
    }

    /// End-to-end verification: equip an item granting `+5 accuracy`,
    /// `+3 attack_power`, `+50 armor_pct`, `+25 max_hp`, `+2 str_bonus`
    /// via post-migration `ObjectEffects` modify rows. Confirm the
    /// wearer's stats moved by exactly those deltas, then unequip and
    /// confirm they returned to baseline.
    #[test]
    fn apply_then_unapply_round_trips_stats_modify() {
        let mut world = World::new();
        let modify_id = 3;
        world.insert_resource(make_catalog_with_modify(modify_id));
        let mut protos = ObjectPrototypes::default();
        let granted_effects: Vec<ObjectGrantedEffect> = vec![
            ObjectGrantedEffect {
                effect_id: modify_id,
                strength: 1,
                modifier_data: serde_json::json!({"target": "accuracy", "amount": 10}),
                wear_location: None,
            },
            ObjectGrantedEffect {
                effect_id: modify_id,
                strength: 1,
                modifier_data: serde_json::json!({"target": "attack_power", "amount": 15}),
                wear_location: None,
            },
            ObjectGrantedEffect {
                effect_id: modify_id,
                strength: 1,
                modifier_data: serde_json::json!({"target": "armor_pct", "amount": 50}),
                wear_location: None,
            },
            ObjectGrantedEffect {
                effect_id: modify_id,
                strength: 1,
                modifier_data: serde_json::json!({"target": "max_hp", "amount": 25}),
                wear_location: None,
            },
            ObjectGrantedEffect {
                effect_id: modify_id,
                strength: 1,
                modifier_data: serde_json::json!({"target": "str_bonus", "amount": 2}),
                wear_location: None,
            },
        ];
        protos.by_key.insert(
            (1, 1),
            ObjectProto {
                zone_id: 1,
                id: 1,
                r#type: mud_db::enums::ObjectType::Armor,
                name: "test ring".into(),
                keywords: vec!["ring".into()],
                room_description: String::new(),
                examine_description: None,
                weight: 0.0,
                level: 1,
                wear_flags: vec![mud_db::enums::WearFlag::Finger],
                weapon_dice_num: 0,
                weapon_dice_size: 0,
                weapon_dice_bonus: 0,
                weapon_damage_type: None,
                cost: 0,
                portal_destination_vnum: None,
                board_id: None,
                liquid: None,
                light_fuel: None,
                armor_pct: 0,
                restricted_alignments: vec![],
                restricted_class_ids: vec![],
                restricted_races: vec![],
                extras: vec![],
                resistances: vec![],
                granted_effects,
                flags: vec![],
                restrictions: vec![],
                timer_hours: 0,
                decompose_timer: 0,
                allowed_races: vec![],
                min_size: None,
                max_size: None,
                camp_kit_tier: None,
            },
        );
        world.insert_resource(protos);

        let wearer = world
            .spawn((
                Named { name: "Wearer".into() },
                Health { hp: 100, max: 100 },
                Stamina { current: 100, max: 100 },
                CombatStats::default(),
                CoreStats {
                    strength: 13,
                    dexterity: 13,
                    constitution: 13,
                    intelligence: 13,
                    wisdom: 13,
                    charisma: 13,
                },
            ))
            .id();
        let item = world
            .spawn((
                Item,
                Named { name: "test ring".into() },
                Located(wearer),
                WorldKey { zone: 1, id: 1 },
                mud_world::EquippedSlot(Slot::LeftFinger),
            ))
            .id();

        apply_object_to_wearer(&mut world, item, wearer);

        let cs = world.get::<CombatStats>(wearer).unwrap();
        assert_eq!(cs.accuracy, 10, "accuracy +10 applied");
        assert_eq!(cs.attack_power, 15, "attack_power +15 applied");
        assert_eq!(cs.armor_pct, 50, "armor_pct +50 applied");
        let hp = world.get::<Health>(wearer).unwrap();
        assert_eq!(hp.max, 125, "max_hp +25 raised max HP");
        assert_eq!(hp.hp, 125, "max_hp +25 also bumped current HP");
        let core = world.get::<CoreStats>(wearer).unwrap();
        assert_eq!(core.strength, 15, "str_bonus +2 raised strength");

        // Unapply path: every delta reverses cleanly.
        unapply_object_from_wearer(&mut world, item, wearer);
        let cs = world.get::<CombatStats>(wearer).unwrap();
        assert_eq!(cs.accuracy, 0, "accuracy reverted on unequip");
        assert_eq!(cs.attack_power, 0, "attack_power reverted on unequip");
        assert_eq!(cs.armor_pct, 0, "armor_pct reverted on unequip");
        let hp = world.get::<Health>(wearer).unwrap();
        assert_eq!(hp.max, 100, "max_hp reverted on unequip");
        // hp drops back to 100 because max dropped to 100.
        assert!(
            hp.hp <= 100,
            "current hp clamped to new max ({} > 100)",
            hp.hp
        );
        let core = world.get::<CoreStats>(wearer).unwrap();
        assert_eq!(core.strength, 13, "strength reverted on unequip");
        assert!(
            world.get::<GrantedDeltas>(item).is_none(),
            "GrantedDeltas removed after unapply"
        );
    }

    /// Wear-location restriction: a "ring of accuracy" with
    /// `wear_location = Finger` should fire on a finger slot…
    #[test]
    fn modify_grant_with_wear_location_fires_on_matching_slot() {
        let mut world = World::new();
        let modify_id = 3;
        world.insert_resource(make_catalog_with_modify(modify_id));
        let mut protos = ObjectPrototypes::default();
        protos.by_key.insert(
            (1, 2),
            ObjectProto {
                zone_id: 1,
                id: 2,
                r#type: mud_db::enums::ObjectType::Armor,
                name: "ring of accuracy".into(),
                keywords: vec!["ring".into()],
                room_description: String::new(),
                examine_description: None,
                weight: 0.0,
                level: 1,
                wear_flags: vec![mud_db::enums::WearFlag::Finger],
                weapon_dice_num: 0,
                weapon_dice_size: 0,
                weapon_dice_bonus: 0,
                weapon_damage_type: None,
                cost: 0,
                portal_destination_vnum: None,
                board_id: None,
                liquid: None,
                light_fuel: None,
                armor_pct: 0,
                restricted_alignments: vec![],
                restricted_class_ids: vec![],
                restricted_races: vec![],
                extras: vec![],
                resistances: vec![],
                granted_effects: vec![ObjectGrantedEffect {
                    effect_id: modify_id,
                    strength: 1,
                    modifier_data: serde_json::json!({"target": "accuracy", "amount": 5}),
                    wear_location: Some(mud_db::enums::WearFlag::Finger),
                }],
                flags: vec![],
                restrictions: vec![],
                timer_hours: 0,
                decompose_timer: 0,
                allowed_races: vec![],
                min_size: None,
                max_size: None,
                camp_kit_tier: None,
            },
        );
        world.insert_resource(protos);

        let wearer = world
            .spawn((
                Named { name: "Wearer".into() },
                Health { hp: 100, max: 100 },
                Stamina { current: 100, max: 100 },
                CombatStats::default(),
                CoreStats {
                    strength: 13,
                    dexterity: 13,
                    constitution: 13,
                    intelligence: 13,
                    wisdom: 13,
                    charisma: 13,
                },
            ))
            .id();
        let item = world
            .spawn((
                Item,
                Named { name: "ring of accuracy".into() },
                Located(wearer),
                WorldKey { zone: 1, id: 2 },
                mud_world::EquippedSlot(Slot::LeftFinger),
            ))
            .id();

        apply_object_to_wearer(&mut world, item, wearer);

        let cs = world.get::<CombatStats>(wearer).unwrap();
        assert_eq!(cs.accuracy, 5, "wear_location=Finger fired on finger slot");
    }
}
