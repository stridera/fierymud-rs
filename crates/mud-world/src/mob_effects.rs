//! Permanent per-proto effects on spawned mobs (`MobDefaultEffects`) and
//! the effect-flag -> marker-component mapping shared with the
//! spell-effect `status` arm.

use bevy_ecs::prelude::*;

use crate::components::{AppliedTo, EffectInstance, EffectSource};
use crate::resources::{EffectCatalog, MobDefaultEffectCatalog, RaceEffectCatalog};

/// Install the marker component a `status` effect's `flag` stands for.
/// Returns true when `flag` maps to a plain marker. Flags that need
/// extra data (resistance, empowered) are handled by the cast path
/// itself; flags with no marker component yet (the detect_* family
/// other than `detect_invisible`, vision, debuffs, elemental shields,
/// ...) return false.
pub fn install_flag_marker(world: &mut World, target: Entity, flag: &str) -> bool {
    let Ok(mut em) = world.get_entity_mut(target) else {
        return false;
    };
    match flag {
        "hidden" | "sneak" | "concealment" => em.insert(crate::components::Stealth),
        "fly" => em.insert(crate::components::Flying),
        "bless" => em.insert(crate::components::Bless),
        "sanctuary" => em.insert(crate::components::Sanctuary),
        "detect_invisible" => em.insert(crate::components::DetectInvis),
        "haste" => em.insert(crate::components::Haste),
        _ => return false,
    };
    true
}

/// True when [`install_flag_marker`] has a marker component for `flag`
/// (kept in step with its match arms).
#[must_use]
pub fn install_flag_marker_known(flag: &str) -> bool {
    matches!(
        flag,
        "hidden"
            | "sneak"
            | "concealment"
            | "fly"
            | "bless"
            | "sanctuary"
            | "detect_invisible"
            | "haste"
    )
}

/// The effect flags a `MobDefaultEffects` row carries: the importer's
/// `modifier_data.flags` array, a singular `modifier_data.flag`, or
/// (when the row names neither) the effect's `default_params.flag`.
pub fn row_flags(
    modifier_data: &serde_json::Value,
    default_params: &serde_json::Value,
) -> Vec<String> {
    let mut flags: Vec<String> = modifier_data
        .get("flags")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    let single = modifier_data
        .get("flag")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            if flags.is_empty() {
                default_params
                    .get("flag")
                    .and_then(serde_json::Value::as_str)
            } else {
                None
            }
        });
    if let Some(f) = single {
        flags.push(f.to_ascii_lowercase());
    }
    flags.dedup();
    flags
}

/// Attach the proto's `MobDefaultEffects` to a freshly spawned `mob`.
/// Each flag of each row becomes one permanent `EffectInstance` named
/// after the flag, plus its marker, through the same
/// [`install_flag_marker`] the spell `status` arm uses.
///
/// * `invisible` installs `Invisible` and tags the instance
///   `InvisibleSource`, so `break_invisibility` strips it when the mob
///   attacks. Legacy `aggro_lose_spells` removes `EFF_INVISIBLE` from
///   mobs too, innate or not, so this matches legacy: the mob stays
///   visible until it respawns.
/// * Flags with no marker component (permanent debuffs such as
///   `blinded` / `poisoned` / `sleeping`, vision and the other detect_*
///   flags) are skipped with a debug log and spawn nothing, so no
///   source-less poison tick or fight with the mob's position can
///   arise; sleeping mobs get that from the proto's default position.
/// * `empowered` is a consume-on-cast charge, not a permanent state:
///   skipped.
///
/// Rows whose effect is missing from the catalog are skipped.
pub fn apply_mob_default_effects(world: &mut World, mob: Entity, proto_key: (i32, i32)) {
    let Some(rows) = world
        .get_resource::<MobDefaultEffectCatalog>()
        .and_then(|c| c.by_key.get(&proto_key).cloned())
    else {
        return;
    };
    for row in rows {
        let Some(def) = world
            .get_resource::<EffectCatalog>()
            .and_then(|c| c.by_id.get(&row.effect_id).cloned())
        else {
            tracing::warn!(
                zone = proto_key.0,
                id = proto_key.1,
                effect_id = row.effect_id,
                "MobDefaultEffects references missing Effect row; skipped"
            );
            continue;
        };
        for flag in row_flags(&row.modifier_data, &def.default_params) {
            let invisible = flag == "invisible";
            let installed = if invisible {
                world.entity_mut(mob).insert(crate::components::Invisible);
                true
            } else {
                install_flag_marker(world, mob, &flag)
            };
            if !installed {
                tracing::debug!(
                    zone = proto_key.0,
                    id = proto_key.1,
                    flag = %flag,
                    "MobDefaultEffects flag has no marker component; ignored"
                );
                continue;
            }
            let mut em = world.spawn((
                EffectInstance {
                    kind: def.id,
                    name: flag,
                    strength: row.strength.max(1),
                    remaining_secs: -1,
                    source: EffectSource::Other("mob_default".to_string()),
                    ability_id: None,
                },
                AppliedTo(mob),
            ));
            if invisible {
                em.insert(crate::components::InvisibleSource);
            }
        }
    }
}

/// `EffectSource::Other` tag on race-innate effect instances. They are
/// rebuilt from [`RaceEffectCatalog`] every login / spawn, so they are
/// never persisted with the character.
pub const RACE_EFFECT_SOURCE: &str = "race";

/// True for an `EffectInstance` created by [`apply_race_effects`].
#[must_use]
pub fn is_race_effect(source: &EffectSource) -> bool {
    matches!(source, EffectSource::Other(s) if s == RACE_EFFECT_SOURCE)
}

/// `EffectSource::Other` tag on effect instances a worn item grants
/// (`ObjectEffects` status flags). Like race innates they live exactly
/// as long as their source: rebuilt from the equipped items at login /
/// spawn, torn down by source on unequip, never persisted and never
/// touched by dispel or cleanse.
pub const WORN_ITEM_EFFECT_SOURCE: &str = "worn_item";

/// True for an `EffectInstance` created by wearing an item.
#[must_use]
pub fn is_worn_item_effect(source: &EffectSource) -> bool {
    matches!(source, EffectSource::Other(s) if s == WORN_ITEM_EFFECT_SOURCE)
}

/// True for an effect whose lifetime belongs to its source (race
/// innate or worn item), not to a timer: never saved, never dispelled.
#[must_use]
pub fn is_innate_effect(source: &EffectSource) -> bool {
    is_race_effect(source) || is_worn_item_effect(source)
}

/// True when `flag` is a passive perception flag that has no marker
/// component yet but is still shown as a permanent effect.
#[must_use]
pub fn is_display_only_flag(flag: &str) -> bool {
    DISPLAY_ONLY_FLAGS.contains(&flag)
}

/// Passive perception flags with no marker component yet: a race still
/// carries them as a permanent, display-only `EffectInstance` so
/// `effects` / `score` show the innate ("infravision (permanent)").
/// Anything else without a marker (permanent debuffs such as
/// `poisoned`) is skipped, as for mobs.
const DISPLAY_ONLY_FLAGS: &[&str] = &[
    "infravision",
    "ultravision",
    "detect_poison",
    "detect_life",
    "detect_align",
    "detect_magic",
    "detect_hidden",
];

/// Give `entity` (a player or a freshly spawned mob) the permanent
/// innate effects of `race` from [`RaceEffectCatalog`] (`RaceEffects`).
///
/// Each flag becomes one permanent (`remaining_secs = -1`)
/// `EffectInstance` named after the flag with source
/// [`RACE_EFFECT_SOURCE`], plus its marker through
/// [`install_flag_marker`]. Idempotent: race-sourced instances already
/// on `entity` are replaced, and a flag the entity already carries as a
/// permanent effect (e.g. from its `MobDefaultEffects`) is not doubled.
/// Race effects are never saved with a character, so relogging cannot
/// stack them. Rows whose effect is missing from the catalog are skipped.
pub fn apply_race_effects(world: &mut World, entity: Entity, race: &str) {
    // Drop the previous application first so calling twice never doubles.
    let stale: Vec<Entity> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, inst, applied)| applied.0 == entity && is_race_effect(&inst.source))
            .map(|(e, _, _)| e)
            .collect()
    };
    for e in stale {
        world.despawn(e);
    }
    let Some(rows) = world
        .get_resource::<RaceEffectCatalog>()
        .and_then(|c| c.get(race).cloned())
    else {
        return;
    };
    for row in rows {
        let Some(def) = world
            .get_resource::<EffectCatalog>()
            .and_then(|c| c.by_id.get(&row.effect_id).cloned())
        else {
            tracing::warn!(
                race,
                effect_id = row.effect_id,
                "RaceEffects references missing Effect row; skipped"
            );
            continue;
        };
        for flag in row_flags(&row.modifier_data, &def.default_params) {
            let already = {
                let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                q.iter(world).any(|(inst, applied)| {
                    applied.0 == entity
                        && inst.remaining_secs < 0
                        && !is_worn_item_effect(&inst.source)
                        && inst.name.eq_ignore_ascii_case(&flag)
                })
            };
            if already {
                continue;
            }
            let marked = install_flag_marker(world, entity, &flag);
            if !marked && !DISPLAY_ONLY_FLAGS.contains(&flag.as_str()) {
                tracing::debug!(race, flag = %flag, "RaceEffects flag has no marker component; ignored");
                continue;
            }
            world.spawn((
                EffectInstance {
                    kind: def.id,
                    name: flag,
                    strength: row.strength.max(1),
                    remaining_secs: -1,
                    source: EffectSource::Other(RACE_EFFECT_SOURCE.to_string()),
                    ability_id: None,
                },
                AppliedTo(entity),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `install_flag_marker_known` must list exactly the flags
    /// `install_flag_marker` installs a marker for.
    #[test]
    fn known_marker_flags_match_the_installer() {
        let mut world = World::new();
        for flag in [
            "hidden",
            "sneak",
            "concealment",
            "fly",
            "bless",
            "sanctuary",
            "detect_invisible",
            "haste",
            "infravision",
            "poisoned",
            "invisible",
            "",
        ] {
            let target = world.spawn_empty().id();
            assert_eq!(
                install_flag_marker(&mut world, target, flag),
                install_flag_marker_known(flag),
                "flag {flag:?}"
            );
        }
    }

    #[test]
    fn worn_item_effects_are_innate_like_race_effects() {
        let worn = EffectSource::Other(WORN_ITEM_EFFECT_SOURCE.to_string());
        let race = EffectSource::Other(RACE_EFFECT_SOURCE.to_string());
        assert!(is_innate_effect(&worn) && is_innate_effect(&race));
        assert!(is_worn_item_effect(&worn) && !is_worn_item_effect(&race));
        assert!(!is_innate_effect(&EffectSource::Spell));
        assert!(!is_innate_effect(&EffectSource::Item));
    }
}
