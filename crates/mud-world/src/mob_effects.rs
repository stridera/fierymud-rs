//! Permanent per-proto effects on spawned mobs (`MobDefaultEffects`) and
//! the effect-flag -> marker-component mapping shared with the
//! spell-effect `status` arm.

use bevy_ecs::prelude::*;

use crate::components::{AppliedTo, EffectInstance, EffectSource};
use crate::resources::{EffectCatalog, MobDefaultEffectCatalog};

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

/// The effect flags a `MobDefaultEffects` row carries: the importer's
/// `modifier_data.flags` array, a singular `modifier_data.flag`, or
/// (when the row names neither) the effect's `default_params.flag`.
fn row_flags(modifier_data: &serde_json::Value, default_params: &serde_json::Value) -> Vec<String> {
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
