//! Permanent per-proto effects on spawned mobs (`MobDefaultEffects`) and
//! the effect-flag -> marker-component mapping shared with the
//! spell-effect `status` arm.

use bevy_ecs::prelude::*;

use crate::components::{AppliedTo, EffectInstance, EffectSource, Mob, Profile, WorldKey};
use crate::resources::{
    EffectCatalog, MobDefaultEffectCatalog, MobProto, MobPrototypes, RaceCatalog, RaceEffectCatalog,
};

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

/// Composition a mob renders with: its own `Mobs.composition`, or the
/// race default (`Races.default_composition`, legacy `def_composition`)
/// when the mob is left at the `FLESH` schema default. A mob explicitly
/// authored as flesh on a non-flesh race therefore reads as the race;
/// there is no separate "unset" state in the column.
#[must_use]
pub fn mob_composition(world: &World, proto: &MobProto) -> mud_db::enums::Composition {
    use mud_db::enums::Composition;
    if proto.composition != Composition::Flesh {
        return proto.composition;
    }
    world
        .get_resource::<RaceCatalog>()
        .and_then(|c| c.get(&proto.race))
        .map_or(Composition::Flesh, |r| r.default_composition)
}

/// Composition `target` has right now (legacy `GET_COMPOSITION`): a live
/// Waterform / Vaporform override, else the mob proto's own, else the
/// race default. Players carry no composition of their own.
#[must_use]
pub fn actor_composition(world: &World, target: Entity) -> mud_db::enums::Composition {
    use mud_db::enums::Composition;
    if let Some(over) = world.get::<crate::components::CompositionOverride>(target) {
        return over.0;
    }
    if world.get::<Mob>(target).is_some()
        && let Some(key) = world.get::<WorldKey>(target).copied()
        && let Some(proto) = world
            .get_resource::<MobPrototypes>()
            .and_then(|p| p.by_key.get(&(key.zone, key.id)))
    {
        return mob_composition(world, proto);
    }
    world
        .get::<Profile>(target)
        .and_then(|p| world.get_resource::<RaceCatalog>()?.get(&p.race))
        .map_or(Composition::Flesh, |r| r.default_composition)
}

/// Legacy `susceptibility(ch, dtype)`'s composition column: how much of a hit
/// of `element` `target`'s body takes, in percent (see
/// [`mud_db::enums::Composition::susceptibility`]). 100 for anything without a
/// composition.
#[must_use]
pub fn composition_susceptibility(
    world: &World,
    target: Entity,
    element: mud_db::enums::ElementType,
) -> i32 {
    actor_composition(world, target).susceptibility(element)
}

/// Legacy `RIGID(ch)`: `target`'s current body is in the solid phase.
#[must_use]
pub fn actor_is_rigid(world: &World, target: Entity) -> bool {
    actor_composition(world, target).is_rigid()
}

/// Legacy `MOB_FLAGGED(ch, MOB_ILLUSORY)`.
#[must_use]
pub fn actor_is_illusory(world: &World, target: Entity) -> bool {
    world
        .get::<crate::components::MobTraits>(target)
        .is_some_and(|t| t.has(mud_db::enums::MobTrait::Illusion))
}

/// Legacy `SOLIDCHAR(ch)`: rigid and not an illusory mob. An illusion looks
/// solid but cannot handle, or be handled by, solid things.
#[must_use]
pub fn actor_is_solid_char(world: &World, target: Entity) -> bool {
    actor_is_rigid(world, target) && !actor_is_illusory(world, target)
}

/// The actor's body in the legacy `{COMPOSITION_COLOR}{COMPOSITION_ADJECTIVE}&0`
/// form, as XML-Lite markup ("<b:blue>watery</>"), for the "in your watery
/// state" refusals.
#[must_use]
pub fn composition_state_phrase(world: &World, target: Entity) -> String {
    let comp = actor_composition(world, target);
    format!("{}{}</>", comp.color_open_tag(), comp.adjective())
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
    crate::components::effects_on(world, target)
        .into_iter()
        .any(|e| {
            world.get::<Tag>(e).is_some_and(|t| {
                matches!((*t, want), (Tag::Evil, Tag::Evil) | (Tag::Good, Tag::Good))
            })
        })
}

/// An `InvisibleSource`-tagged instance (`INVISIBLE` / `MASS_INVIS`, a
/// permanent `invisible` flag) is what keeps `Invisible` on.
fn invisible_tag_backed(world: &mut World, target: Entity) -> bool {
    crate::components::effects_on(world, target)
        .into_iter()
        .any(|e| world.get::<crate::components::InvisibleSource>(e).is_some())
}

/// Every spelling of the blindness flag: `blinded` (mob / race / item
/// rows), `blind` (the spell and eye-gouge instance name) and `blindness`.
const BLIND_FLAGS: &[&str] = &["blinded", "blind", "blindness"];

/// True for a status flag that blinds its bearer.
#[must_use]
pub fn is_blind_flag(flag: &str) -> bool {
    BLIND_FLAGS.contains(&flag)
}

const FLAG_MARKERS: &[FlagMarker] = &[
    FlagMarker {
        flags: &["hidden", "concealment"],
        insert: |e| {
            e.insert(crate::components::Stealth);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Stealth>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["sneak"],
        insert: |e| {
            e.insert(crate::components::Sneaking);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Sneaking>();
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
        flags: &["waterwalk"],
        insert: |e| {
            e.insert(crate::components::WaterWalk);
        },
        remove: Some(|e| {
            e.remove::<crate::components::WaterWalk>();
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
    // Legacy `EFF_FARSEE`: longer `scan`, `look <dir>` down the exit.
    FlagMarker {
        flags: &["farsee"],
        insert: |e| {
            e.insert(crate::components::Farsee);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Farsee>();
        }),
        tag_backed: None,
    },
    // Waterform / Vaporform: legacy `APPLY_COMPOSITION` (`COMP_WATER` /
    // `COMP_MIST`). The override sits over the base composition, which is
    // restored by removing it.
    FlagMarker {
        flags: &["waterform"],
        insert: |e| {
            e.insert(crate::components::CompositionOverride(
                mud_db::enums::Composition::Water,
            ));
        },
        remove: Some(|e| {
            e.remove::<crate::components::CompositionOverride>();
        }),
        tag_backed: None,
    },
    FlagMarker {
        flags: &["vaporform"],
        insert: |e| {
            e.insert(crate::components::CompositionOverride(
                mud_db::enums::Composition::Mist,
            ));
        },
        remove: Some(|e| {
            e.remove::<crate::components::CompositionOverride>();
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
    // Legacy `EFF_GLORY`: aggressive mobs that see the bearer hold back, with
    // a small chance to attack it (`mud-server` `glory`).
    FlagMarker {
        flags: &["glory"],
        insert: |e| {
            e.insert(crate::components::Glory);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Glory>();
        }),
        tag_backed: None,
    },
    // Legacy `EFF_MISDIRECTION`: `<dir> <fake-dir>` misleads the room about
    // where the bearer went, and a wandering mob does it on its own.
    FlagMarker {
        flags: &["misdirection"],
        insert: |e| {
            e.insert(crate::components::Misdirection);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Misdirection>();
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
    // Legacy `EFF_BLIND`: the blindness spell and eye gouge name their
    // instance `blind`, mob / race / worn-item rows carry the `blinded`
    // flag.
    FlagMarker {
        flags: BLIND_FLAGS,
        insert: |e| {
            e.insert(crate::components::Blinded);
        },
        remove: Some(|e| {
            e.remove::<crate::components::Blinded>();
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
/// path itself; flags with no marker component (`detect_hidden`,
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
        crate::components::any_effect_on(world, target, |eff| {
            marker
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
///   `poisoned` / `sleeping`, vision and the other detect_* flags) are skipped with a debug log and spawn nothing, so no
///   source-less poison tick or fight with the mob's position can
///   arise; sleeping mobs get that from the proto's default position.
/// * `empowered` is a consume-on-cast charge, not a permanent state:
///   skipped.
///
/// Rows whose effect is missing from the catalog are skipped.
pub fn apply_mob_default_effects(
    world: &mut World,
    mob: Entity,
    proto_key: (i32, i32),
) -> Vec<String> {
    let Some(rows) = world
        .get_resource::<MobDefaultEffectCatalog>()
        .and_then(|c| c.by_key.get(&proto_key).cloned())
    else {
        return Vec::new();
    };
    let mut granted: Vec<GrantedFlag> = Vec::new();
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
            if marker_for(&flag).is_none() && !is_name_behaviour_flag(&flag) {
                tracing::debug!(
                    zone = proto_key.0,
                    id = proto_key.1,
                    flag = %flag,
                    "MobDefaultEffects flag has no marker component; ignored"
                );
                continue;
            }
            granted.push(GrantedFlag {
                flag,
                kind: def.id,
                strength: row.strength.max(1),
            });
        }
    }
    install_markers_in_order(world, mob, granted.iter().map(|g| g.flag.as_str()));
    let mut names = Vec::with_capacity(granted.len());
    for g in granted {
        spawn_permanent_instance(
            world,
            mob,
            &g,
            EffectSource::Other("mob_default".to_string()),
        );
        names.push(g.flag);
    }
    names
}

/// One flag a spawn grants as a permanent effect.
struct GrantedFlag {
    flag: String,
    kind: i32,
    strength: i32,
}

/// Spawn the permanent (`remaining_secs = -1`) instance for `granted` on
/// `target`, tagged for its flag.
fn spawn_permanent_instance(
    world: &mut World,
    target: Entity,
    granted: &GrantedFlag,
    source: EffectSource,
) {
    let effect = world
        .spawn((
            EffectInstance {
                kind: granted.kind,
                name: granted.flag.clone(),
                strength: granted.strength,
                remaining_secs: -1,
                source,
                ability_id: None,
            },
            AppliedTo(target),
        ))
        .id();
    tag_flag_instance(world, effect, &granted.flag);
}

/// Install the markers for `flags` in marker-table order, whatever order
/// the rows named them in. Every marker is a structural move, and the
/// first move into each archetype builds it: in a fixed order mobs with
/// the same flags share their intermediate archetypes instead of each
/// row order making its own chain.
fn install_markers_in_order<'a>(
    world: &mut World,
    target: Entity,
    flags: impl Iterator<Item = &'a str>,
) {
    let mut slots: Vec<usize> = flags
        .filter_map(|f| FLAG_MARKERS.iter().position(|m| m.flags.contains(&f)))
        .collect();
    slots.sort_unstable();
    slots.dedup();
    let Ok(mut em) = world.get_entity_mut(target) else {
        return;
    };
    for slot in slots {
        (FLAG_MARKERS[slot].insert)(&mut em);
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
    apply_race_effects_inner(world, entity, race, None);
}

/// [`apply_race_effects`] for a mob that has just been spawned and holds
/// no effects but `existing_permanent` (the flags its `MobDefaultEffects`
/// granted). Knowing that, it skips the two scans over every effect
/// instance in the world that the general form makes per spawn and per
/// flag, which put a respawn wave at O(mobs x effects).
pub fn apply_race_effects_to_new_mob(
    world: &mut World,
    mob: Entity,
    race: &str,
    existing_permanent: &[String],
) {
    apply_race_effects_inner(world, mob, race, Some(existing_permanent));
}

fn apply_race_effects_inner(
    world: &mut World,
    entity: Entity,
    race: &str,
    fresh_with: Option<&[String]>,
) {
    // Drop the previous application first so calling twice never doubles.
    if fresh_with.is_none() {
        let stale: Vec<Entity> = crate::components::effects_on(world, entity)
            .into_iter()
            .filter(|&e| {
                world
                    .get::<EffectInstance>(e)
                    .is_some_and(|inst| is_race_effect(&inst.source))
            })
            .collect();
        for e in stale {
            world.despawn(e);
        }
    }
    let Some(rows) = world
        .get_resource::<RaceEffectCatalog>()
        .and_then(|c| c.get(race).cloned())
    else {
        return;
    };
    let mut known: Vec<String> = fresh_with.map(<[String]>::to_vec).unwrap_or_default();
    let mut granted: Vec<GrantedFlag> = Vec::new();
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
            let already = if fresh_with.is_some() {
                known.iter().any(|k| k.eq_ignore_ascii_case(&flag))
            } else {
                crate::components::any_effect_on(world, entity, |inst| {
                    inst.remaining_secs < 0
                        && !is_worn_item_effect(&inst.source)
                        && inst.name.eq_ignore_ascii_case(&flag)
                }) || granted.iter().any(|g| g.flag.eq_ignore_ascii_case(&flag))
            };
            if already {
                continue;
            }
            if marker_for(&flag).is_none() && !is_instance_only_flag(&flag) {
                tracing::debug!(race, flag = %flag, "RaceEffects flag has no marker component; ignored");
                continue;
            }
            known.push(flag.clone());
            granted.push(GrantedFlag {
                flag,
                kind: def.id,
                strength: row.strength.max(1),
            });
        }
    }
    install_markers_in_order(world, entity, granted.iter().map(|g| g.flag.as_str()));
    for g in granted {
        spawn_permanent_instance(
            world,
            entity,
            &g,
            EffectSource::Other(RACE_EFFECT_SOURCE.to_string()),
        );
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
            ("sneak", |w, e| w.get::<Sneaking>(e).is_some()),
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
            ("misdirection", |w, e| w.get::<Misdirection>(e).is_some()),
            ("glory", |w, e| w.get::<Glory>(e).is_some()),
            ("waterwalk", |w, e| w.get::<WaterWalk>(e).is_some()),
            ("farsee", |w, e| w.get::<Farsee>(e).is_some()),
            ("waterform", |w, e| {
                w.get::<CompositionOverride>(e).is_some()
            }),
            ("vaporform", |w, e| {
                w.get::<CompositionOverride>(e).is_some()
            }),
            ("blinded", |w, e| w.get::<Blinded>(e).is_some()),
            ("blind", |w, e| w.get::<Blinded>(e).is_some()),
            ("blindness", |w, e| w.get::<Blinded>(e).is_some()),
        ];
        for (flag, has) in mapped {
            let target = world.spawn_empty().id();
            assert!(install_flag_marker(&mut world, target, flag), "{flag}");
            assert!(install_flag_marker_known(flag), "{flag}");
            assert!(has(&world, target), "{flag} marker installed");
        }
        for flag in [
            "detect_hidden",
            "detect_magic",
            "fireshield",
            "coldshield",
            "language_fluency",
            "poisoned",
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
        assert!(
            !is_instance_only_flag("language_fluency"),
            "language_fluency has no behaviour"
        );
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
