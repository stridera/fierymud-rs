//! Gear-on-wear stat application. The player wears a sword: its
//! `ObjectEffects` rows of `effect_type = "modify"` (carrying
//! `modifier_data = {"target": "<stat>", "amount": <int>}`) flow into
//! the wearer's `CombatStats` / `CoreStats` / `Health` / etc. via
//! `commands::apply_modify_delta`. Spell-like `ObjectEffects`
//! (sanctuary rings, etc.) are `status` rows whose `modifier_data.flags`
//! become one permanent `EffectInstance` per flag, sourced
//! `worn_item` and tagged with `GrantedByItem`, with the flag's marker
//! component installed through `install_flag_marker` (the path race and
//! mob-default effects use). A `globe` row spawns a permanent "globe"
//! instance (circle in `strength`) and raises `MaxAbsorbCircle` to the
//! highest circle of any source. Unequip despawns only this item's
//! instances and tears the markers down like an expiry would. `ObjectResistance` rows roll into the wearer's
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
use mud_world::mob_effects::{WORN_ITEM_EFFECT_SOURCE, is_instance_only_flag, row_flags};
use mud_world::{
    AppliedTo, CoreStats, EffectInstance, EffectSource, EquippedSlot, GrantedByItem, Health,
    ObjectGrantedEffect, ObjectPrototypes, Resistances, Stamina, WorldKey,
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
    /// `EffectInstance` entities spawned from spell-like grants while
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
#[allow(clippy::too_many_lines)]
pub fn apply_object_to_wearer(world: &mut World, item: Entity, wearer: Entity) {
    if world.get_entity(wearer).is_err() || world.get_entity(item).is_err() {
        return;
    }
    let Some(key) = world.get::<WorldKey>(item).copied() else {
        return;
    };
    let proto = world
        .get_resource::<ObjectPrototypes>()
        .and_then(|p| p.by_key.get(&(key.zone, key.id)).cloned());
    let Some(proto) = proto else {
        return;
    };
    // ---- Light sources ----
    // A player's worn / held light is NOT lit by wearing it: they must
    // use the `light` command. Permanent lights (`remaining < 0`) need
    // no marker, `mud_world::is_lit` treats them as always lit. Mobs
    // can't type `light`, so a light a mob wears is lit here (unless
    // spent: `remaining == 0`; no fuel data is never assumed infinite).
    if proto.r#type == mud_db::enums::ObjectType::Light
        && world.get::<mud_world::Mob>(wearer).is_some()
        && world.get::<mud_world::Lit>(item).is_none()
        && world
            .get::<mud_world::LightFuel>(item)
            .is_some_and(|f| f.remaining != 0)
    {
        try_insert(world, item, mud_world::Lit);
    }
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
    if proto.armor_pct != 0 && apply_modify_delta(world, wearer, "armor_pct", proto.armor_pct) {
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
            #[allow(clippy::cast_possible_truncation)]
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
        // `globe` effect: a permanent "globe" instance carrying the
        // highest circle absorbed in `strength`, the shape the
        // MINOR/MAJOR_GLOBE spells spawn. The marker is the max over every
        // source, so wearing a minor globe under a cast major one changes
        // nothing; removal recomputes it from what remains.
        if def.effect_type == "globe" {
            let circle = globe_circle(&grant, &def.default_params);
            let existing = world
                .get::<mud_world::MaxAbsorbCircle>(wearer)
                .map_or(0, |m| m.0);
            try_insert(
                world,
                wearer,
                mud_world::MaxAbsorbCircle(existing.max(circle)),
            );
            let entity = world
                .spawn((
                    EffectInstance {
                        kind: def.id,
                        name: def.name.clone(),
                        strength: circle,
                        remaining_secs: -1,
                        source: EffectSource::Other(WORN_ITEM_EFFECT_SOURCE.to_string()),
                        ability_id: None,
                    },
                    AppliedTo(wearer),
                    GrantedByItem(item),
                ))
                .id();
            spawned_effect_entities.push(entity);
            continue;
        }
        // `status` effect: one permanent instance per flag, with the
        // flag's marker installed through the same path race and
        // mob-default effects use. Other effect types have no
        // permanent-while-worn meaning.
        let flags = if def.effect_type == "status" {
            row_flags(&grant.modifier_data, &def.default_params)
        } else {
            Vec::new()
        };
        if flags.is_empty() {
            tracing::debug!(
                proto_zone = proto.zone_id,
                proto_id = proto.id,
                effect = %def.name,
                "ObjectEffect has no wearable status flag; skipped"
            );
            continue;
        }
        for flag in flags {
            let marked = mud_world::mob_effects::install_flag_marker(world, wearer, &flag);
            if !marked && !is_instance_only_flag(&flag) {
                tracing::debug!(
                    proto_zone = proto.zone_id,
                    proto_id = proto.id,
                    flag = %flag,
                    "ObjectEffect flag has no marker component; ignored"
                );
                continue;
            }
            let entity = world
                .spawn((
                    EffectInstance {
                        kind: def.id,
                        name: flag.clone(),
                        strength: grant.strength.max(1),
                        // Permanent: lasts as long as the item is worn.
                        // Unequip despawns it; effects_tick never
                        // decrements -1.
                        remaining_secs: -1,
                        source: EffectSource::Other(WORN_ITEM_EFFECT_SOURCE.to_string()),
                        ability_id: None,
                    },
                    AppliedTo(wearer),
                    GrantedByItem(item),
                ))
                .id();
            mud_world::mob_effects::tag_flag_instance(world, entity, &flag);
            spawned_effect_entities.push(entity);
        }
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

/// Highest circle a worn `globe` row absorbs: `modifier_data.maxCircle`,
/// else the row's `strength` (importer data: 3 minor, 6 major; the column
/// defaults to 1, which is not a circle), else the effect's
/// `default_params.maxCircle`, else 3.
fn globe_circle(grant: &ObjectGrantedEffect, default_params: &serde_json::Value) -> i32 {
    let from = |v: &serde_json::Value| {
        v.get("maxCircle")
            .and_then(serde_json::Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
    };
    from(&grant.modifier_data)
        .or_else(|| (grant.strength > 1).then_some(grant.strength))
        .or_else(|| from(default_params))
        .unwrap_or(3)
        .max(1)
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
    let mut torn_down: Vec<String> = Vec::new();
    for effect_entity in &bookkeeping.effects {
        if let Some(inst) = world.get::<EffectInstance>(*effect_entity) {
            torn_down.push(inst.name.clone());
        }
        if let Ok(em) = world.get_entity_mut(*effect_entity) {
            em.despawn();
        }
    }
    // Drop each flag's marker unless a spell or race innate still backs
    // it, exactly as an expiring effect does.
    torn_down.sort();
    torn_down.dedup();
    for name in torn_down {
        crate::effects::teardown_markers_after_removal(world, wearer, &name);
    }
    if let Ok(mut e) = world.get_entity_mut(item) {
        e.remove::<GrantedDeltas>();
    }
}

/// What an item grants, as player-facing text. `applies` mirrors legacy
/// identify's `Apply: +2 to strength` lines; `provides` its
/// `Item provides:` effect-flag list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemGrantText {
    pub applies: Vec<String>,
    pub provides: Vec<String>,
}

/// Human label for an apply target (`str_bonus` -> `strength`).
fn apply_label(key: &str) -> String {
    match key {
        "str" | "str_bonus" => "strength".to_string(),
        "dex" | "dex_bonus" => "dexterity".to_string(),
        "con" | "con_bonus" => "constitution".to_string(),
        "int" | "int_bonus" => "intelligence".to_string(),
        "wis" | "wis_bonus" => "wisdom".to_string(),
        "cha" | "cha_bonus" => "charisma".to_string(),
        "max_hp" => "max hit points".to_string(),
        "max_move" | "max_stamina" | "stamina_max" => "max stamina".to_string(),
        "max_mana" => "max mana".to_string(),
        "ward" | "ward_pct" => "ward".to_string(),
        other => other.replace('_', " "),
    }
}

/// List `proto`'s stat applies, resistances and granted effect flags.
/// Reads the same data `apply_object_to_wearer` applies, so identify can
/// never promise something wearing the item doesn't deliver.
#[must_use]
pub fn describe_item_grants(world: &World, proto: &mud_world::ObjectProto) -> ItemGrantText {
    let mut out = ItemGrantText::default();
    let catalog = world.get_resource::<mud_world::EffectCatalog>();
    for g in &proto.granted_effects {
        let at = g
            .wear_location
            .map(|w| format!(" (worn on: {})", w.label().to_lowercase()))
            .unwrap_or_default();
        let Some(def) = catalog.and_then(|c| c.by_id.get(&g.effect_id)) else {
            continue;
        };
        if def.effect_type == "modify" {
            let target = g
                .modifier_data
                .get("target")
                .and_then(serde_json::Value::as_str);
            let amount = g
                .modifier_data
                .get("amount")
                .and_then(serde_json::Value::as_i64);
            if let (Some(t), Some(a)) = (target, amount)
                && a != 0
            {
                out.applies.push(format!("{a:+} to {}{at}", apply_label(t)));
            }
        } else if def.effect_type == "globe" {
            out.provides.push(format!(
                "spell globe, absorbs up to circle {}{at}",
                globe_circle(g, &def.default_params)
            ));
        } else if def.effect_type == "status" {
            for flag in row_flags(&g.modifier_data, &def.default_params) {
                if mud_world::mob_effects::install_flag_marker_known(&flag)
                    || is_instance_only_flag(&flag)
                {
                    out.provides.push(format!("{}{at}", flag.replace('_', " ")));
                }
            }
        }
    }
    for (element, value, _) in &proto.resistances {
        if *value != 0 {
            out.applies.push(format!(
                "{value:+}% resistance to {}",
                format!("{element:?}").to_lowercase()
            ));
        }
    }
    out
}

/// Take `item` off whoever is wearing it: reverses its gear bonuses and
/// worn effects (when it has any applied). Call this BEFORE removing
/// `EquippedSlot` / re-locating the item at any site that strips worn
/// gear without going through `remove` (death, disarm, fear, banish), so
/// stats and markers never outlive the item and a stale `GrantedDeltas`
/// can't stop the next wearer's bonuses from applying.
pub fn release_gear(world: &mut World, item: Entity) {
    if world.get::<GrantedDeltas>(item).is_none() {
        return;
    }
    let Some(wearer) = world.get::<mud_world::Located>(item).map(|l| l.0) else {
        // Nobody to reverse against: drop the stale bookkeeping and
        // the orphaned effects.
        if let Some(b) = world.get::<GrantedDeltas>(item).cloned() {
            for e in b.effects {
                if let Ok(em) = world.get_entity_mut(e) {
                    em.despawn();
                }
            }
        }
        if let Ok(mut e) = world.get_entity_mut(item) {
            e.remove::<GrantedDeltas>();
        }
        return;
    };
    unapply_object_from_wearer(world, item, wearer);
}

/// What worn gear currently adds to values that are saved with the
/// character. Gear is re-applied from the equipped items at login, so
/// the save must write the value WITHOUT it or every relog would stack
/// the bonus again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GearOffsets {
    pub strength: i32,
    pub dexterity: i32,
    pub constitution: i32,
    pub intelligence: i32,
    pub wisdom: i32,
    pub charisma: i32,
    pub max_hp: i32,
    pub max_stamina: i32,
}

/// Sum the persisted-value deltas of every applied item on `wearer`.
#[must_use]
pub fn gear_offsets(world: &World, wearer: Entity) -> GearOffsets {
    let mut off = GearOffsets::default();
    let Some(contents) = world.get::<mud_world::Contents>(wearer) else {
        return off;
    };
    for item in contents.iter() {
        let Some(applied) = world.get::<GrantedDeltas>(item) else {
            continue;
        };
        for (key, amount) in &applied.deltas {
            let slot = match key.as_str() {
                "str" | "strength" | "str_bonus" => &mut off.strength,
                "dex" | "dexterity" | "dex_bonus" => &mut off.dexterity,
                "con" | "constitution" | "con_bonus" => &mut off.constitution,
                "int" | "intelligence" | "int_bonus" => &mut off.intelligence,
                "wis" | "wisdom" | "wis_bonus" => &mut off.wisdom,
                "cha" | "charisma" | "cha_bonus" => &mut off.charisma,
                "max_hp" => &mut off.max_hp,
                "max_move" | "max_stamina" | "stamina_max" => &mut off.max_stamina,
                _ => continue,
            };
            *slot = slot.saturating_add(*amount);
        }
    }
    off
}

/// `CoreStats` as they should be saved: current values minus gear.
#[must_use]
pub fn base_core_stats(world: &World, wearer: Entity) -> Option<CoreStats> {
    let mut stats = world.get::<CoreStats>(wearer).copied()?;
    let off = gear_offsets(world, wearer);
    stats.strength = stats.strength.saturating_sub(off.strength);
    stats.dexterity = stats.dexterity.saturating_sub(off.dexterity);
    stats.constitution = stats.constitution.saturating_sub(off.constitution);
    stats.intelligence = stats.intelligence.saturating_sub(off.intelligence);
    stats.wisdom = stats.wisdom.saturating_sub(off.wisdom);
    stats.charisma = stats.charisma.saturating_sub(off.charisma);
    Some(stats)
}

/// A current-points value (hp, stamina) as it should be saved: capped at
/// the gear-less maximum (`max - max_offset`), never below 1 for a living
/// wearer. Login re-applies the gear WITHOUT topping current points up
/// (`recompute_equipped_keeping_vitals`), so the saved value comes back
/// exactly; a wearer at full health returns at the gear-less maximum and
/// regenerates the rest.
#[must_use]
pub fn base_current(current: i32, max: i32, max_offset: i32) -> i32 {
    if current <= 0 {
        return current;
    }
    current.min(max.saturating_sub(max_offset)).max(1)
}

/// Destroy an item entity, reversing its worn-gear effects first. Every
/// site that despawns an item that might be worn (decay, consumption,
/// sale, purge, script destroy) goes through this so bonuses and markers
/// never outlive the item.
pub fn despawn_item(world: &mut World, item: Entity) {
    release_gear(world, item);
    if let Ok(em) = world.get_entity_mut(item) {
        em.despawn();
    }
}

/// True for the apply targets stored in the character row (`CoreStats`).
/// A stat-buff `ModifyDelta` on one of these is baked into the saved row;
/// every other target is rebuilt from base columns at login.
#[must_use]
pub fn is_persisted_stat_key(key: &str) -> bool {
    matches!(
        key,
        "str"
            | "strength"
            | "str_bonus"
            | "dex"
            | "dexterity"
            | "dex_bonus"
            | "con"
            | "constitution"
            | "con_bonus"
            | "int"
            | "intelligence"
            | "int_bonus"
            | "wis"
            | "wisdom"
            | "wis_bonus"
            | "cha"
            | "charisma"
            | "cha_bonus"
    )
}

/// `recompute_equipped_for` for a loaded player: the saved current
/// hp / stamina already are what the player had, so applying the gear
/// must not top them up the way equipping does.
pub fn recompute_equipped_keeping_vitals(world: &mut World, wearer: Entity) {
    let hp = world.get::<Health>(wearer).map(|h| h.hp);
    let stamina = world.get::<Stamina>(wearer).map(|s| s.current);
    recompute_equipped_for(world, wearer);
    if let Some(hp) = hp
        && let Some(mut h) = world.get_mut::<Health>(wearer)
    {
        h.hp = hp.min(h.max);
    }
    if let Some(cur) = stamina
        && let Some(mut s) = world.get_mut::<Stamina>(wearer)
    {
        s.current = cur.min(s.max);
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
    // Walk the wearer's `Contents` index (O(carried), not O(world items))
    // so this stays cheap when called per respawned mob.
    let equipped: Vec<Entity> = world
        .get::<mud_world::Contents>(wearer)
        .map(|c| {
            c.iter()
                .filter(|e| {
                    world.get::<mud_world::Item>(*e).is_some()
                        && world.get::<EquippedSlot>(*e).is_some()
                })
                .collect()
        })
        .unwrap_or_default();
    for item in equipped {
        if world.get::<GrantedDeltas>(item).is_some() {
            continue;
        }
        apply_object_to_wearer(world, item, wearer);
    }
}

/// Best-effort match between an `ObjectEffects.wear_location`
/// `WearFlag` and the runtime `Slot` an item is occupying. The
/// schema's `WearFlag` is finer-grained than the runtime Slot
/// (Mainhand/Offhand/Twohand all collapse onto `Slot::Wield` in
/// the runtime), so we collapse on the Slot side. Returns true
/// when the worn slot satisfies the grant's restriction.
#[must_use]
pub fn wear_flag_matches_slot(flag: mud_db::enums::WearFlag, slot: mud_world::Slot) -> bool {
    use mud_db::enums::WearFlag::{
        About, Arms, Badge, Belt, Body, Ear, Eyes, Face, Feet, Finger, Hands, Head, Hover, Legs,
        Mainhand, Neck, Offhand, Twohand, Waist, Wrist,
    };
    use mud_world::Slot;
    matches!(
        (flag, slot),
        (Finger, Slot::LeftFinger | Slot::RightFinger)
            | (Neck, Slot::Neck | Slot::SecondNeck)
            | (Ear, Slot::LeftEar | Slot::RightEar)
            | (Wrist, Slot::LeftWrist | Slot::RightWrist)
            | (Head, Slot::Head)
            | (Eyes, Slot::Eyes)
            | (Face, Slot::Face)
            | (Body, Slot::Body)
            | (About, Slot::About)
            | (Arms, Slot::Arms)
            | (Hands, Slot::Hands)
            | (Waist | Belt, Slot::Waist)
            | (Legs, Slot::Legs)
            | (Feet, Slot::Feet)
            | (Mainhand | Offhand | Twohand, Slot::Wield | Slot::Hold)
            | (Badge, Slot::Badge)
            | (Hover, Slot::Hover)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::{
        CombatStats, CoreStats, EffectCatalog, EffectDef, Health, Item, Located, Named,
        ObjectProto, ObjectPrototypes, Slot, Stamina, WorldKey,
    };

    /// Build a minimal `EffectCatalog` containing the "modify" effect
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
    #[allow(clippy::too_many_lines)]
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
                weight_reduction: 0.0,
                recall_rooms: None,
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
                Named {
                    name: "Wearer".into(),
                },
                Health { hp: 100, max: 100 },
                Stamina {
                    current: 100,
                    max: 100,
                },
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
                Named {
                    name: "test ring".into(),
                },
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
                weight_reduction: 0.0,
                recall_rooms: None,
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
                Named {
                    name: "Wearer".into(),
                },
                Health { hp: 100, max: 100 },
                Stamina {
                    current: 100,
                    max: 100,
                },
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
                Named {
                    name: "ring of accuracy".into(),
                },
                Located(wearer),
                WorldKey { zone: 1, id: 2 },
                mud_world::EquippedSlot(Slot::LeftFinger),
            ))
            .id();

        apply_object_to_wearer(&mut world, item, wearer);

        let cs = world.get::<CombatStats>(wearer).unwrap();
        assert_eq!(cs.accuracy, 5, "wear_location=Finger fired on finger slot");
    }

    /// Proto for a torch-like light with the given initial fuel.
    fn light_proto(id: i32) -> ObjectProto {
        let mut p =
            crate::commands::test_support::object_proto(1, id, mud_db::enums::ObjectType::Light);
        p.name = "a torch".into();
        p
    }

    fn light_world(
        fuel: Option<mud_world::LightFuel>,
        slot: Option<Slot>,
    ) -> (World, Entity, Entity, Entity) {
        let mut world = World::new();
        let mut protos = ObjectPrototypes::default();
        protos.by_key.insert((1, 30), light_proto(30));
        world.insert_resource(protos);
        let room = world.spawn_empty().id();
        let wearer = world
            .spawn((mud_world::Player, Located(room), Named { name: "W".into() }))
            .id();
        let mut item = world.spawn((
            Item,
            Named {
                name: "a torch".into(),
            },
            Located(wearer),
            WorldKey { zone: 1, id: 30 },
        ));
        if let Some(f) = fuel {
            item.insert(f);
        }
        if let Some(s) = slot {
            item.insert(mud_world::EquippedSlot(s));
        }
        let item = item.id();
        (world, room, wearer, item)
    }

    fn torch_fuel(remaining: i32) -> mud_world::LightFuel {
        mud_world::LightFuel {
            capacity: remaining.max(150),
            remaining,
        }
    }

    #[test]
    fn wearing_a_light_does_not_light_it() {
        let (mut world, room, wearer, item) = light_world(Some(torch_fuel(150)), Some(Slot::Hold));
        apply_object_to_wearer(&mut world, item, wearer);
        recompute_equipped_for(&mut world, wearer);
        assert!(world.get::<mud_world::Lit>(item).is_none());
        assert!(!mud_world::is_lit(&world, item));
        assert!(!crate::commands::room_has_light(&mut world, room));
    }

    #[test]
    fn light_command_lights_a_worn_torch_and_the_room_sees_it() {
        let (mut world, room, wearer, item) = light_world(Some(torch_fuel(150)), Some(Slot::Hold));
        world
            .entity_mut(item)
            .insert(mud_world::Keywords(vec!["torch".into()]));
        apply_object_to_wearer(&mut world, item, wearer);
        assert!(!crate::commands::room_has_light(&mut world, room));
        crate::commands::info::cmd_light(&mut world, wearer, "torch");
        assert!(world.get::<mud_world::Lit>(item).is_some());
        assert!(crate::commands::room_has_light(&mut world, room));
        // Extinguishing a normal torch works and darkens the room again.
        crate::commands::info::cmd_extinguish(&mut world, wearer, "torch");
        assert!(world.get::<mud_world::Lit>(item).is_none());
        assert!(!crate::commands::room_has_light(&mut world, room));
    }

    #[test]
    fn light_command_refuses_a_spent_torch() {
        let (mut world, room, wearer, item) = light_world(Some(torch_fuel(0)), Some(Slot::Hold));
        world
            .entity_mut(item)
            .insert(mud_world::Keywords(vec!["torch".into()]));
        crate::commands::info::cmd_light(&mut world, wearer, "torch");
        assert!(world.get::<mud_world::Lit>(item).is_none());
        assert!(!crate::commands::room_has_light(&mut world, room));
    }

    #[test]
    fn permanent_light_is_always_lit_and_cannot_be_extinguished() {
        let (mut world, room, wearer, item) = light_world(Some(torch_fuel(-1)), Some(Slot::Hold));
        world
            .entity_mut(item)
            .insert(mud_world::Keywords(vec!["torch".into()]));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
        world
            .entity_mut(wearer)
            .insert(crate::commands::Connection(tx));
        // Lit with no marker and no `light` command, even unworn.
        assert!(mud_world::is_lit(&world, item));
        assert!(crate::commands::room_has_light(&mut world, room));
        crate::commands::info::cmd_extinguish(&mut world, wearer, "torch");
        let mut out = String::new();
        while let Ok(b) = rx.try_recv() {
            out.push_str(&String::from_utf8_lossy(&b));
        }
        assert!(out.contains("You can't put out a torch."), "{out}");
        assert!(mud_world::is_lit(&world, item));
        assert!(crate::commands::room_has_light(&mut world, room));
        // `light` on it is just "already lit".
        crate::commands::info::cmd_light(&mut world, wearer, "torch");
        assert!(world.get::<mud_world::Lit>(item).is_none());
    }

    #[test]
    fn mob_worn_light_is_lit_since_mobs_cannot_use_light() {
        let (mut world, room, wearer, item) = light_world(Some(torch_fuel(150)), Some(Slot::Hold));
        world
            .entity_mut(wearer)
            .remove::<mud_world::Player>()
            .insert(mud_world::Mob);
        apply_object_to_wearer(&mut world, item, wearer);
        assert!(world.get::<mud_world::Lit>(item).is_some());
        assert!(crate::commands::room_has_light(&mut world, room));
    }

    #[test]
    fn light_with_no_fuel_data_is_not_assumed_infinite() {
        let (mut world, room, wearer, item) = light_world(None, Some(Slot::Hold));
        apply_object_to_wearer(&mut world, item, wearer);
        assert!(world.get::<mud_world::Lit>(item).is_none());
        assert!(!crate::commands::room_has_light(&mut world, room));
    }

    #[test]
    fn recompute_keeps_permanent_light_lit_without_a_marker() {
        let (mut world, room, wearer, item) = light_world(
            Some(mud_world::LightFuel {
                capacity: -1,
                remaining: -1,
            }),
            Some(Slot::Hold),
        );
        recompute_equipped_for(&mut world, wearer);
        assert!(world.get::<mud_world::Lit>(item).is_none());
        assert!(mud_world::is_lit(&world, item));
        assert!(crate::commands::room_has_light(&mut world, room));
    }

    #[test]
    fn spent_light_stays_dark_when_worn() {
        let (mut world, room, wearer, item) = light_world(
            Some(mud_world::LightFuel {
                capacity: 150,
                remaining: 0,
            }),
            Some(Slot::Hold),
        );
        apply_object_to_wearer(&mut world, item, wearer);
        assert!(world.get::<mud_world::Lit>(item).is_none());
        assert!(!crate::commands::room_has_light(&mut world, room));
    }

    #[test]
    fn non_light_items_are_never_lit() {
        let (mut world, _room, wearer, item) = light_world(None, Some(Slot::Hold));
        let mut protos = ObjectPrototypes::default();
        let mut p = light_proto(30);
        p.r#type = mud_db::enums::ObjectType::Other;
        protos.by_key.insert((1, 30), p);
        world.insert_resource(protos);
        apply_object_to_wearer(&mut world, item, wearer);
        assert!(world.get::<mud_world::Lit>(item).is_none());
    }
}
