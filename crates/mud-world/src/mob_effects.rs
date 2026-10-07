//! Permanent per-proto effects on spawned mobs (`MobDefaultEffects`) and
//! the effect-flag -> marker-component mapping shared with the
//! spell-effect `status` arm.

use bevy_ecs::prelude::*;

use crate::components::{AppliedTo, EffectInstance, EffectSource};
use crate::resources::{EffectCatalog, MobDefaultEffectCatalog};

/// Install the marker component a `status` effect's `flag` stands for.
/// Returns true when `flag` maps to a plain marker. Flags that need
/// extra data (resistance, empowered, stealth) are handled by the cast
/// path itself.
pub fn install_flag_marker(world: &mut World, target: Entity, flag: &str) -> bool {
    let Ok(mut em) = world.get_entity_mut(target) else {
        return false;
    };
    match flag {
        "fly" => em.insert(crate::components::Flying),
        "bless" => em.insert(crate::components::Bless),
        "sanctuary" => em.insert(crate::components::Sanctuary),
        "detect_invisible" => em.insert(crate::components::DetectInvis),
        "haste" => em.insert(crate::components::Haste),
        _ => return false,
    };
    true
}

/// Attach the proto's `MobDefaultEffects` to a freshly spawned `mob`:
/// one permanent `EffectInstance` each, plus the marker for its flag.
/// The flag comes from the row's `modifier_data.flag`, falling back to
/// the effect's `default_params.flag`; the effect catalog resolves the
/// effect row. Rows whose effect is missing are skipped.
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
        let flag = row
            .modifier_data
            .get("flag")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                def.default_params
                    .get("flag")
                    .and_then(serde_json::Value::as_str)
            })
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        world.spawn((
            EffectInstance {
                kind: def.id,
                name: if flag.is_empty() {
                    def.name.clone()
                } else {
                    flag.clone()
                },
                strength: row.strength.max(1),
                remaining_secs: -1,
                source: EffectSource::Other("mob_default".to_string()),
                ability_id: None,
            },
            AppliedTo(mob),
        ));
        install_flag_marker(world, mob, &flag);
    }
}
