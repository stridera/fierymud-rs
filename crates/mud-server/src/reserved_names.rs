//! Words that must not become a character name (or an item keyword).
//!
//! DATA-OVER-CODE SCAFFOLDING: these lists are content a builder or admin
//! may want to edit without a recompile. They live here, in one place, as a
//! bridge; the follow-up is a `ReservedNames` DB table (Prisma schema,
//! fierylib seed, loaded into an ECS resource at boot). The mob-proto part
//! of the check already reads the DB-loaded `MobPrototypes` resource.

use bevy_ecs::prelude::World;

/// Words the targeting parser and command grammar give a meaning of
/// their own. Shared with `item_custom` (custom item keywords) so a
/// character and an item can never answer to them.
pub(crate) const TARGET_WORDS: &[&str] = &[
    "all",
    "self",
    "me",
    "everyone",
    "someone",
    "something",
    "here",
];

/// Staff / system voices a player must not impersonate.
pub(crate) const STAFF_NAMES: &[&str] = &[
    "admin",
    "administrator",
    "builder",
    "god",
    "gods",
    "immortal",
    "implementor",
    "moderator",
    "server",
    "staff",
    "system",
];

/// True when `word` (any case) is one of the targeting-grammar words.
pub(crate) fn is_target_word(word: &str) -> bool {
    TARGET_WORDS.iter().any(|w| w.eq_ignore_ascii_case(word))
}

/// True when `name` (any case) is a targeting word or a staff/system name.
pub(crate) fn is_reserved_name(name: &str) -> bool {
    is_target_word(name) || STAFF_NAMES.iter().any(|w| w.eq_ignore_ascii_case(name))
}

/// Single words that read as a generic mob and so are refused as player
/// names, whether or not a prototype of that exact name exists.
pub(crate) const GENERIC_MOB_WORDS: &[&str] = &[
    "beggar",
    "citizen",
    "cityguard",
    "corpse",
    "guard",
    "guardian",
    "guardsman",
    "merchant",
    "mob",
    "npc",
    "shopkeeper",
];

/// A mob's short name reduced for comparison: lowercase, a leading
/// article (`a` / `an` / `the` / `some`) removed. "an ebony raven" becomes
/// "ebony raven"; only a one-word result can equal a character name.
fn mob_short_name(name: &str) -> String {
    // Drop XML-Lite colour tags ("<red>wolf</>") before comparing.
    let mut plain = String::with_capacity(name.len());
    let mut in_tag = false;
    for c in name.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    let lower = plain.trim().to_ascii_lowercase();
    for article in ["a ", "an ", "the ", "some "] {
        if let Some(rest) = lower.strip_prefix(article) {
            return rest.trim_start().to_string();
        }
    }
    lower
}

/// True when `name` (any case) is a generic mob word or equals a mob
/// prototype's short name with its article stripped. Keywords are
/// deliberately not matched: they include common handles ("bob", "raven",
/// "shadow") that players are entitled to.
pub(crate) fn matches_mob_proto(world: &World, name: &str) -> bool {
    if GENERIC_MOB_WORDS
        .iter()
        .any(|w| w.eq_ignore_ascii_case(name))
    {
        return true;
    }
    let Some(protos) = world.get_resource::<mud_world::MobPrototypes>() else {
        return false;
    };
    let wanted = name.trim().to_ascii_lowercase();
    protos
        .by_key
        .values()
        .any(|p| mob_short_name(&p.name) == wanted)
}

/// Why `name` cannot be used for a new character, as a message ready for
/// the player; `Ok(())` when it is free of reserved words.
pub(crate) fn check_character_name(world: &World, name: &str) -> Result<(), String> {
    if is_reserved_name(name) || matches_mob_proto(world, name) {
        return Err(format!("The name '{name}' is reserved. Pick another."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::mob_proto;
    use mud_db::enums::MobProfession;
    use mud_world::MobPrototypes;

    #[test]
    fn target_and_staff_words_are_reserved_in_any_case() {
        for n in [
            "Someone",
            "SELF",
            "me",
            "All",
            "Here",
            "Something",
            "Admin",
            "System",
        ] {
            assert!(is_reserved_name(n), "{n}");
        }
        assert!(!is_reserved_name("Strider"));
    }

    #[test]
    fn mob_short_names_and_generic_words_are_reserved_but_keywords_are_not() {
        let mut world = World::new();
        assert!(
            check_character_name(&world, "Guard").is_err(),
            "curated word"
        );
        assert!(
            check_character_name(&world, "Strider").is_ok(),
            "no catalog yet"
        );
        let mut protos = MobPrototypes::default();
        let mut add = |id: i32, name: &str, keywords: &[&str]| {
            let mut p = mob_proto(1, id, MobProfession::Trainer);
            p.name = name.into();
            p.keywords = keywords.iter().map(|k| (*k).to_string()).collect();
            protos.by_key.insert((1, id), p);
        };
        add(1, "Bartholomew", &["bartholomew"]);
        add(2, "a wolf", &["wolf"]);
        add(3, "an ebony raven", &["ebony", "raven", "bob"]);
        add(4, "The Hermit", &["hermit"]);
        add(5, "<red>a</> <b>Cinder</>", &[]);
        world.insert_resource(protos);
        for n in ["bartholomew", "WOLF", "hermit", "cinder"] {
            assert!(check_character_name(&world, n).is_err(), "{n}");
        }
        // Keywords alone ("raven", "bob", "ebony") do not block a handle.
        for n in ["Raven", "Bob", "Ebony", "Shadow"] {
            assert!(check_character_name(&world, n).is_ok(), "{n}");
        }
    }
}
