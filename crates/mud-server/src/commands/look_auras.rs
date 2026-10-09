//! Spell-aura flavor lines shown when you `look` at another actor
//! (legacy `print_char_spells_to_char`). Replaces the old flat
//! "X is affected by: a, b" readout: each visible effect gets its own
//! sentence, and the purely magical ones (armor, bless, ...) only show to
//! viewers who can detect magic.
//!
//! The lines live in the `EffectAura` table and are loaded at boot into
//! `EffectAuraCatalog`; builders edit them without a recompile.

use bevy_ecs::prelude::{Entity, World};
use mud_world::{
    AppliedTo, CombatStats, EffectAuraCatalog, EffectInstance, MobPrototypes, Profile, WorldKey,
};

fn normalize(label: &str) -> String {
    label.replace('_', " ").to_ascii_lowercase()
}

/// `(he, his, him)` for a profile / proto gender column.
pub(super) fn pronouns(gender: &str) -> (&'static str, &'static str, &'static str) {
    match gender.to_ascii_lowercase().as_str() {
        "male" => ("he", "his", "him"),
        "female" => ("she", "her", "her"),
        _ => ("it", "its", "it"),
    }
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().collect::<String>() + chars.as_str()
    })
}

pub(super) fn target_gender(world: &World, target: Entity) -> String {
    if let Some(p) = world.get::<Profile>(target) {
        return p.gender.clone();
    }
    world
        .get::<WorldKey>(target)
        .and_then(|k| {
            world
                .get_resource::<MobPrototypes>()
                .and_then(|p| p.by_key.get(&(k.zone, k.id)))
        })
        .map(|p| p.gender.clone())
        .unwrap_or_default()
}

/// Normalized labels of every effect on `target`: the originating
/// ability's name and the effect's own flag name.
fn effect_labels(world: &mut World, target: Entity) -> Vec<String> {
    let rows: Vec<(String, Option<i32>)> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, a)| a.0 == target)
            .map(|(inst, _)| (inst.name.clone(), inst.ability_id))
            .collect()
    };
    let catalog = world.get_resource::<mud_world::AbilityCatalog>();
    let mut labels = Vec::new();
    for (name, ability_id) in rows {
        labels.push(normalize(&name));
        if let Some(id) = ability_id
            && let Some(def) = catalog.and_then(|c| c.by_name.values().find(|d| d.id == id))
        {
            labels.push(normalize(&def.plain_name));
        }
    }
    labels
}

/// Flavor lines (CRLF-terminated, XML-Lite tagged) for what `viewer` can see
/// of the effects on `target`.
pub(super) fn aura_lines(world: &mut World, viewer: Entity, target: Entity) -> String {
    let labels = effect_labels(world, target);
    if labels.is_empty() {
        return String::new();
    }
    let sees_magic = labels_of(world, viewer)
        .iter()
        .any(|l| l == "detect magic" || l == "sphere of divination")
        || crate::commands::player_can_see_in_dark(world, viewer);
    let (he, his, him) = pronouns(&target_gender(world, target));
    let alignment = world.get::<CombatStats>(target).map_or(0, |s| s.alignment);
    let Some(catalog) = world.get_resource::<EffectAuraCatalog>() else {
        return String::new();
    };
    let mut out = String::new();
    let mut shown_groups: Vec<&str> = Vec::new();
    for aura in &catalog.auras {
        if aura.needs_detect_magic && !sees_magic {
            continue;
        }
        if !aura.keys.iter().any(|k| labels.iter().any(|l| l == k)) {
            continue;
        }
        if aura.min_alignment.is_some_and(|min| alignment < min)
            || aura.max_alignment.is_some_and(|max| alignment > max)
        {
            continue;
        }
        // Only the first matching line of an exclusive group shows
        // (Dragon's health supersedes plain endurance, as in legacy).
        if let Some(group) = aura.exclusive_group.as_deref() {
            if shown_groups.contains(&group) {
                continue;
            }
            shown_groups.push(group);
        }
        out.push_str(&render(&aura.text, he, his, him));
        out.push_str("\r\n");
    }
    out
}

/// The viewer's own effect labels (for the Detect Magic check).
fn labels_of(world: &mut World, viewer: Entity) -> Vec<String> {
    effect_labels(world, viewer)
}

fn render(text: &str, he: &str, his: &str, him: &str) -> String {
    text.replace("{^S}", &capitalize_first(his))
        .replace("{^E}", &capitalize_first(he))
        .replace("{S}", his)
        .replace("{E}", he)
        .replace("{M}", him)
}
