//! Spell-aura flavor lines shown when you `look` at another actor
//! (legacy `print_char_spells_to_char`). Replaces the old flat
//! "X is affected by: a, b" readout: each visible effect gets its own
//! sentence, and the purely magical ones (armor, bless, ...) only show to
//! viewers who can detect magic.
//!
//! The table is temporary scaffolding: it is content a builder might one day
//! edit, so it should migrate to the DB (an `EffectDef` look-line column)
//! once the Muditor editor for it exists.

use bevy_ecs::prelude::{Entity, World};
use mud_world::{AppliedTo, CombatStats, EffectInstance, MobPrototypes, Profile, WorldKey};

/// One flavor line. `keys` are normalized effect labels (lowercase, spaces):
/// the originating ability's name or the effect's flag name. The text uses
/// `{S}` his/her/its, `{M}` him/her/it, `{E}` he/she/it, and `{^S}` / `{^E}`
/// for the capitalized forms at the start of a sentence.
struct Aura {
    keys: &'static [&'static str],
    needs_detect_magic: bool,
    text: &'static str,
}

const AURAS: &[Aura] = &[
    // Seen only with Detect Magic (or holylight).
    Aura {
        keys: &["armor", "group armor"],
        needs_detect_magic: true,
        text: "A <b:white>translucent shimmering aura</> surrounds {M}.",
    },
    Aura {
        keys: &["bless"],
        needs_detect_magic: true,
        text: "The shimmering telltales of a <b:yellow>magical blessing</> flutter about {S} head.",
    },
    Aura {
        keys: &["demonic aspect"],
        needs_detect_magic: true,
        text: "A <red>demonic tinge</> circulates in {S} <red>blood</>.",
    },
    Aura {
        keys: &["demonic mutation"],
        needs_detect_magic: true,
        text: "Two <red>large red horns</> sprout from {S} head.",
    },
    Aura {
        keys: &["dark presence"],
        needs_detect_magic: true,
        text: "You sense a <dim>dark presence</> within {M}.",
    },
    Aura {
        keys: &["dragons health", "dragon's health"],
        needs_detect_magic: true,
        text: "The power of <magenta>dragon's blood</> fills {M}!",
    },
    Aura {
        keys: &[
            "lesser endurance",
            "endurance",
            "greater endurance",
            "vitality",
            "greater vitality",
        ],
        needs_detect_magic: true,
        text: "{^S} health appears to be bolstered by magical power.",
    },
    Aura {
        keys: &["chill touch"],
        needs_detect_magic: true,
        text: "A <cyan>weakening chill</> circulates in {S} veins.",
    },
    Aura {
        keys: &["clarity"],
        needs_detect_magic: true,
        text: "A <b:yellow>clarity</> of mind surrounds {M}.",
    },
    Aura {
        keys: &["minor globe"],
        needs_detect_magic: true,
        text: "<red>{^S} body is encased in a shimmering globe!</>",
    },
    // Visible to everyone.
    Aura {
        keys: &["stone skin", "stoneskin"],
        needs_detect_magic: false,
        text: "<dim>{^S} body seems to be made of stone!</>",
    },
    Aura {
        keys: &["barkskin"],
        needs_detect_magic: false,
        text: "<yellow>{^S} skin is thick, brown, and wrinkly.</>",
    },
    Aura {
        keys: &["bone armor"],
        needs_detect_magic: false,
        text: "<white>Heavy bony plates cover {S} body.</>",
    },
    Aura {
        keys: &["demonskin"],
        needs_detect_magic: false,
        text: "<red>{^S} skin is shiny, smooth, and <b:red>very red</>.</>",
    },
    Aura {
        keys: &["gaias cloak", "gaia's cloak"],
        needs_detect_magic: false,
        text: "<green>A whirlwind of leaves and <yellow>sticks</> whips around {S} body.</>",
    },
    Aura {
        keys: &["ice armor"],
        needs_detect_magic: false,
        text: "A layer of <blue>solid ice</> covers {M} entirely.",
    },
    Aura {
        keys: &["mirage"],
        needs_detect_magic: false,
        text: "<white>{^S} image <red>wavers</> and <dim>shimmers</> and is somewhat indistinct.</>",
    },
    Aura {
        keys: &["blind", "blindness"],
        needs_detect_magic: false,
        text: "{^S} <dim>dull</> eyes suggest {E} is blind!",
    },
    Aura {
        keys: &["fireshield"],
        needs_detect_magic: false,
        text: "<b:red>{^S} body is encased in fire!</>",
    },
    Aura {
        keys: &["coldshield"],
        needs_detect_magic: false,
        text: "<b:blue>{^S} body is encased in jagged ice!</>",
    },
    Aura {
        keys: &["major globe"],
        needs_detect_magic: false,
        text: "<b:red>{^S} body is encased in shimmering globe of force!</>",
    },
    Aura {
        keys: &["entangle"],
        needs_detect_magic: false,
        text: "<green>{^E} is entwined by a tangled mass of vines.</>",
    },
    Aura {
        keys: &["paralyzed", "minor paralysis", "major paralysis"],
        needs_detect_magic: false,
        text: "<cyan>{^E} is completely still, and shows no awareness of {S} surroundings.</>",
    },
    Aura {
        keys: &["web"],
        needs_detect_magic: false,
        text: "<green>{^E} is tangled in glowing <b:yellow>webs</>!</>",
    },
    Aura {
        keys: &["wings of hell"],
        needs_detect_magic: false,
        text: "<b:red>Huge leathery <dim>bat-like</> wings sprout from {S} back.</>",
    },
    Aura {
        keys: &["wings of heaven"],
        needs_detect_magic: false,
        text: "<b:white>{^E} has a pair of beautiful bright white wings.</>",
    },
    Aura {
        keys: &["magic torch"],
        needs_detect_magic: false,
        text: "{^E} is being followed by a <red>bright glowing light</>.",
    },
    Aura {
        keys: &["circle of light"],
        needs_detect_magic: false,
        text: "<b:white>A circle of light floats over {S} head.</>",
    },
    Aura {
        keys: &["on fire", "burning"],
        needs_detect_magic: false,
        text: "<b:red>{^E} is on FIRE!</>",
    },
];

fn normalize(label: &str) -> String {
    label.replace('_', " ").to_ascii_lowercase()
}

/// `(he, his, him)` for a profile / proto gender column.
fn pronouns(gender: &str) -> (&'static str, &'static str, &'static str) {
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

fn target_gender(world: &World, target: Entity) -> String {
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
    let mut out = String::new();
    let mut dragon = false;
    for aura in AURAS {
        if aura.needs_detect_magic && !sees_magic {
            continue;
        }
        if !aura.keys.iter().any(|k| labels.iter().any(|l| l == k)) {
            continue;
        }
        // Dragon's health supersedes the plain endurance line (legacy else-if).
        if aura.keys.contains(&"dragons health") {
            dragon = true;
        } else if aura.keys.contains(&"endurance") && dragon {
            continue;
        }
        out.push_str(&render(aura.text, he, his, him));
        out.push_str("\r\n");
    }
    // Sanctuary's aura depends on the bearer's alignment.
    if labels.iter().any(|l| l == "sanctuary") {
        let alignment = world.get::<CombatStats>(target).map_or(0, |s| s.alignment);
        let line = if alignment <= -350 {
            "<dim>{^S} body is surrounded by a black aura!</>"
        } else if alignment >= 350 {
            "<b:white>{^S} body is surrounded by a white aura!</>"
        } else {
            "<b:blue>{^S} body is surrounded by a blue aura!</>"
        };
        out.push_str(&render(line, he, his, him));
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
