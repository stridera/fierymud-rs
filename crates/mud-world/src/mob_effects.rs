//! Permanent per-proto effects on spawned mobs (`MobDefaultEffects`) and
//! the effect-flag -> marker-component mapping shared with the
//! spell-effect `status` arm.

use bevy_ecs::prelude::*;

use crate::components::{AppliedTo, EffectInstance, EffectSource};
use crate::resources::{EffectCatalog, MobDefaultEffectCatalog, RaceEffectCatalog};

/// One row of the effect-flag -> marker-component table. Every path that
/// grants a status flag (spell `status` arm, `MobDefaultEffects`,
/// `RaceEffects`, worn `ObjectEffects`, login restore of a saved spell)
/// installs through [`install_flag_marker`], and every teardown (expiry,
/// dispel, unequip) removes through [`teardown_flag_marker`], so the
/// table below is the one place a flag's component is named.
struct FlagMarker {
    /// Effect flags (lowercase) that all stand for this one component.
    flags: &'static [&'static str],
    insert: fn(&mut EntityWorldMut),
    /// How the component comes off once no instance backs it. `None`:
    /// the teardown has side effects of its own (the "fades back into
    /// view" broadcast) and lives with the caller.
    remove: Option<fn(&mut EntityWorldMut)>,
    /// Backing that is not an instance named after one of `flags`
    /// (a spell-granted instance carrying a companion tag).
    tag_backed: Option<fn(&mut World, Entity) -> bool>,
}

fn protect_evil_tag_backed(world: &mut World, target: Entity) -> bool {
    alignment_tag_backed(
        world,
        target,
        crate::components::AlignmentProtectionTag::Evil,
    )
}

fn protect_good_tag_backed(world: &mut World, target: Entity) -> bool {
    alignment_tag_backed(
        world,
        target,
        crate::components::AlignmentProtectionTag::Good,
    )
}

/// A `PROT_FROM_EVIL` / `PROT_FROM_GOOD` spell instance is named
/// `resistance` and tagged with the alignment it guards against.
fn alignment_tag_backed(
    world: &mut World,
    target: Entity,
    want: crate::components::AlignmentProtectionTag,
) -> bool {
    use crate::components::AlignmentProtectionTag as Tag;
    let mut q = world.query::<(&AppliedTo, &Tag)>();
    q.iter(world).any(|(a, t)| {
        a.0 == target && matches!((*t, want), (Tag::Evil, Tag::Evil) | (Tag::Good, Tag::Good))
    })
}

/// An `InvisibleSource`-tagged instance (`INVISIBLE` / `MASS_INVIS`, a
/// permanent `invisible` flag) is what keeps `Invisible` on.
fn invisible_tag_backed(world: &mut World, target: Entity) -> bool {
    let mut q = world.query_filtered::<&AppliedTo, With<crate::components::InvisibleSource>>();
    q.iter(world).any(|a| a.0 == target)
}

const FLAG_MARKERS: &[FlagMarker] = &[
    FlagMarker {
        flags: &["hidden", "sneak", "concealment"],
        insert: |e| {
            e.insert(crate::components::Stealth);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Stealth>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["fly"],
        insert: |e| {
            e.insert(crate::components::Flying);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Flying>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["bless"],
        insert: |e| {
            e.insert(crate::components::Bless);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Bless>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["sanctuary"],
        insert: |e| {
            e.insert(crate::components::Sanctuary);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Sanctuary>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["detect_invisible"],
        insert: |e| {
            e.insert(crate::components::DetectInvis);
        },
        remove: Some(|e| {
            e.remove::<crate::components::DetectInvis>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["infravision"],
        insert: |e| {
            e.insert(crate::components::Infravision);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Infravision>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["detect_life"],
        insert: |e| {
            e.insert(crate::components::SenseLife);
        },
        remove: Some(|e| {
            e.remove::<crate::components::SenseLife>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["detect_align"],
        insert: |e| {
            e.insert(crate::components::DetectAlign);
        },
        remove: Some(|e| {
            e.remove::<crate::components::DetectAlign>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["blur"],
        insert: |e| {
            e.insert(crate::components::Blur);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Blur>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["familiarity"],
        insert: |e| {
            e.insert(crate::components::Familiar);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Familiar>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["haste"],
        insert: |e| {
            e.insert(crate::components::Haste);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Haste>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["protect_evil"],
        insert: |e| {
            e.insert(crate::components::ProtectFromEvil);
        },
        remove: Some(|e| {
            e.remove::<crate::components::ProtectFromEvil>();
        }),
        tag_backed: Some(protect_evil_tag_backed),
    },
    FlagMarker {
        flags: &["protect_good"],
        insert: |e| {
            e.insert(crate::components::ProtectFromGood);
        },
        remove: Some(|e| {
            e.remove::<crate::components::ProtectFromGood>();
        }),
        tag_backed: Some(protect_good_tag_backed),
    },
    // `Invisible` is torn down by the caller (`invisibility_faded`), and
    // the instance must be tagged `InvisibleSource` ([`tag_flag_instance`])
    // so `break_invisibility` strips it when the bearer attacks.
    FlagMarker {
        flags: &["invisible"],
        insert: |e| {
            e.insert(crate::components::Invisible);
        },
        remove: None,
        tag_backed: Some(invisible_tag_backed),
    },
];

/// Flags with no marker component whose behaviour reads the
/// `EffectInstance` itself by name: `detect_magic` (look auras that need
/// detect magic, the duration colours in `effects`) and
/// `fireshield` / `coldshield` (their `EffectAura` flavour lines). The
/// spell path spawns the instance anyway; for mobs, races and worn items
/// the instance is the whole effect.
const NAME_BEHAVIOUR_FLAGS: &[&str] = &["detect_magic", "fireshield", "coldshield"];

/// True for a flag whose only runtime behaviour is its named
/// `EffectInstance` (see [`NAME_BEHAVIOUR_FLAGS`]).
#[must_use]
pub fn is_name_behaviour_flag(flag: &str) -> bool {
    NAME_BEHAVIOUR_FLAGS.contains(&flag)
}

fn marker_for(flag: &str) -> Option<&'static FlagMarker> {
    FLAG_MARKERS.iter().find(|m| m.flags.contains(&flag))
}

/// Install the marker component a `status` effect's `flag` stands for.
/// Returns true when `flag` maps to a plain marker. Flags that need
/// extra data (resistance, empowered, globe) are handled by the cast
/// path itself; flags with no marker component (waterwalk, `detect_hidden`,
/// language, debuffs, ...) return false.
pub fn install_flag_marker(world: &mut World, target: Entity, flag: &str) -> bool {
    let Some(marker) = marker_for(flag) else {
        return false;
    };
    let Ok(mut em) = world.get_entity_mut(target) else {
        return false;
    };
    (marker.insert)(&mut em);
    true
}

/// Tag the `EffectInstance` `effect` just spawned for `flag` with any
/// companion component the flag's behaviour needs. Call it right after
/// spawning an instance (alongside [`install_flag_marker`]); a no-op for
/// flags that need none. `invisible` gets `InvisibleSource`: the tag
/// keeps `Invisible` alive past other effects' expiry and lets
/// `break_invisibility` strip it when the bearer attacks.
pub fn tag_flag_instance(world: &mut World, effect: Entity, flag: &str) {
    if flag == "invisible"
        && let Ok(mut em) = world.get_entity_mut(effect)
    {
        em.insert(crate::components::InvisibleSource);
    }
}

/// True when [`install_flag_marker`] has a marker component for `flag`
/// (kept in step with the table by construction).
#[must_use]
pub fn install_flag_marker_known(flag: &str) -> bool {
    marker_for(flag).is_some()
}

/// Drop the marker a just-removed instance named `name` backed, unless
/// another instance (any name sharing the marker, or a tagged spell
/// instance) still backs it. Shared by expiry, dispel / cleanse and
/// unequip. Returns true when the component was removed.
pub fn teardown_flag_marker(world: &mut World, target: Entity, name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let Some(marker) = marker_for(&name) else {
        return false;
    };
    let Some(remove) = marker.remove else {
        return false;
    };
    let named_backing = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world).any(|(eff, applied)| {
            applied.0 == target
                && marker
                    .flags
                    .iter()
                    .any(|f| eff.name.eq_ignore_ascii_case(f))
        })
    };
    if named_backing || marker.tag_backed.is_some_and(|b| b(world, target)) {
        return false;
    }
    let Ok(mut em) = world.get_entity_mut(target) else {
        return false;
    };
    remove(&mut em);
    true
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
///   `InvisibleSource` ([`tag_flag_instance`]), so `break_invisibility`
///   strips it when the mob attacks. Legacy `aggro_lose_spells` removes
///   `EFF_INVISIBLE` from mobs too, innate or not, so this matches
///   legacy: the mob stays visible until it respawns.
/// * Flags with neither a marker component nor an instance-read
///   behaviour ([`is_name_behaviour_flag`]) (permanent debuffs such as
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
            let installed = install_flag_marker(world, mob, &flag) || is_name_behaviour_flag(&flag);
            if !installed {
                tracing::debug!(
                    zone = proto_key.0,
                    id = proto_key.1,
                    flag = %flag,
                    "MobDefaultEffects flag has no marker component; ignored"
                );
                continue;
            }
            let effect = world
                .spawn((
                    EffectInstance {
                        kind: def.id,
                        name: flag.clone(),
                        strength: row.strength.max(1),
                        remaining_secs: -1,
                        source: EffectSource::Other("mob_default".to_string()),
                        ability_id: None,
                    },
                    AppliedTo(mob),
                ))
                .id();
            tag_flag_instance(world, effect, &flag);
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

/// True when `flag` has no marker component but is still carried as a
/// permanent `EffectInstance` by races and worn items: the passive
/// perception flags shown for display, and the flags whose behaviour
/// reads the instance by name ([`is_name_behaviour_flag`]).
#[must_use]
pub fn is_instance_only_flag(flag: &str) -> bool {
    DISPLAY_ONLY_FLAGS.contains(&flag) || is_name_behaviour_flag(flag)
}

/// Passive perception flags with no marker component and no behaviour
/// yet: a race (or worn item) still carries them as a permanent,
/// display-only `EffectInstance` so `effects` / `score` show the innate
/// ("ultravision (permanent)"). Anything else without a marker
/// (permanent debuffs such as `poisoned`) is skipped, as for mobs.
/// `detect_hidden` stays here: nothing in the runtime hides an actor
/// from a room listing, so there is nothing for it to detect.
const DISPLAY_ONLY_FLAGS: &[&str] = &["ultravision", "detect_poison", "detect_hidden"];

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
            if !marked && !is_instance_only_flag(&flag) {
                tracing::debug!(race, flag = %flag, "RaceEffects flag has no marker component; ignored");
                continue;
            }
            let effect = world
                .spawn((
                    EffectInstance {
                        kind: def.id,
                        name: flag.clone(),
                        strength: row.strength.max(1),
                        remaining_secs: -1,
                        source: EffectSource::Other(RACE_EFFECT_SOURCE.to_string()),
                        ability_id: None,
                    },
                    AppliedTo(entity),
                ))
                .id();
            tag_flag_instance(world, effect, &flag);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every flag the table maps installs its component, and every flag
    /// it leaves out (no component and no behaviour anywhere) stays
    /// unmapped.
    type Has = fn(&World, Entity) -> bool;

    #[test]
    fn marker_table_installs_exactly_the_mapped_flags() {
        use crate::components::*;
        let mut world = World::new();
        let mapped: &[(&str, Has)] = &[
            ("hidden", |w, e| w.get::<Stealth>(e).is_some()),
            ("sneak", |w, e| w.get::<Stealth>(e).is_some()),
            ("concealment", |w, e| w.get::<Stealth>(e).is_some()),
            ("fly", |w, e| w.get::<Flying>(e).is_some()),
            ("bless", |w, e| w.get::<Bless>(e).is_some()),
            ("sanctuary", |w, e| w.get::<Sanctuary>(e).is_some()),
            ("detect_invisible", |w, e| w.get::<DetectInvis>(e).is_some()),
            ("haste", |w, e| w.get::<Haste>(e).is_some()),
            ("protect_evil", |w, e| w.get::<ProtectFromEvil>(e).is_some()),
            ("protect_good", |w, e| w.get::<ProtectFromGood>(e).is_some()),
            ("invisible", |w, e| w.get::<Invisible>(e).is_some()),
            ("infravision", |w, e| w.get::<Infravision>(e).is_some()),
            ("detect_life", |w, e| w.get::<SenseLife>(e).is_some()),
            ("detect_align", |w, e| w.get::<DetectAlign>(e).is_some()),
            ("blur", |w, e| w.get::<Blur>(e).is_some()),
            ("familiarity", |w, e| w.get::<Familiar>(e).is_some()),
        ];
        for (flag, has) in mapped {
            let target = world.spawn_empty().id();
            assert!(install_flag_marker(&mut world, target, flag), "{flag}");
            assert!(install_flag_marker_known(flag), "{flag}");
            assert!(has(&world, target), "{flag} marker installed");
        }
        for flag in [
            "waterwalk",
            "detect_hidden",
            "detect_magic",
            "fireshield",
            "coldshield",
            "language_fluency",
            "poisoned",
            "blinded",
            "",
        ] {
            let target = world.spawn_empty().id();
            assert!(!install_flag_marker(&mut world, target, flag), "{flag:?}");
            assert!(!install_flag_marker_known(flag), "{flag:?}");
        }
    }

    #[test]
    fn name_behaviour_flags_are_instance_only() {
        for flag in ["detect_magic", "fireshield", "coldshield"] {
            assert!(is_name_behaviour_flag(flag) && is_instance_only_flag(flag));
            assert!(!install_flag_marker_known(flag));
        }
        for flag in ["waterwalk", "language_fluency"] {
            assert!(!is_instance_only_flag(flag), "{flag} has no behaviour");
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
