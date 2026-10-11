//! Words that must not become a character name (or an item keyword).
//!
//! Two layers. The command-grammar words ([`TARGET_WORDS`]) are a runtime
//! invariant tied to the targeting parser and stay in code. Everything
//! curated (staff / system voices, generic mob words such as "guard") is
//! content: it lives in the `ReservedNames` table, which builders edit in
//! Muditor and the boot loader reads into the `ReservedNames` resource; a
//! missing table boots with an empty list (logged at ERROR). The mob-proto
//! part of the check reads the DB-loaded `MobPrototypes` resource.

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

/// True when `word` (any case) is one of the targeting-grammar words.
pub(crate) fn is_target_word(word: &str) -> bool {
    TARGET_WORDS.iter().any(|w| w.eq_ignore_ascii_case(word))
}

/// True when `name` (any case) is a targeting word or a curated reserved
/// name from the `ReservedNames` table (none when the resource is absent).
pub(crate) fn is_reserved_name(world: &World, name: &str) -> bool {
    is_target_word(name)
        || world
            .get_resource::<mud_world::ReservedNames>()
            .is_some_and(|r| r.contains(name))
}

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

/// True when `name` (any case) equals a mob prototype's short name with its
/// article stripped. Generic mob words ("guard") are curated rows in
/// `ReservedNames`, checked by [`is_reserved_name`]. Keywords are
/// deliberately not matched: they include common handles ("bob", "raven",
/// "shadow") that players are entitled to.
pub(crate) fn matches_mob_proto(world: &World, name: &str) -> bool {
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
    if is_reserved_name(world, name) || matches_mob_proto(world, name) {
        return Err(format!("The name '{name}' is reserved. Pick another."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::mob_proto;
    use mud_db::enums::MobProfession;
    use mud_world::{MobPrototypes, ReservedNames};

    /// A world whose `ReservedNames` resource holds `names`.
    fn world_reserving(names: &[&str]) -> World {
        let mut world = World::new();
        world.insert_resource(ReservedNames {
            names: names.iter().map(|n| (*n).to_string()).collect(),
        });
        world
    }

    #[test]
    fn target_words_are_reserved_in_any_case_without_a_table() {
        let world = World::new();
        for n in ["Someone", "SELF", "me", "All", "Here", "Something"] {
            assert!(is_reserved_name(&world, n), "{n}");
        }
        assert!(!is_reserved_name(&world, "Strider"));
        // The curated words come from the table: absent resource, nothing reserved.
        assert!(!is_reserved_name(&world, "Admin"));
    }

    #[test]
    fn curated_names_come_from_the_resource_in_any_case() {
        let world = world_reserving(&["admin", "guard"]);
        for n in ["Admin", "GUARD", " guard "] {
            assert!(is_reserved_name(&world, n), "{n}");
        }
        assert!(!is_reserved_name(&world, "Strider"));
        // Removing a row (an empty list) frees the word, but not the grammar words.
        let world = world_reserving(&[]);
        assert!(!is_reserved_name(&world, "Admin"));
        assert!(is_reserved_name(&world, "Everyone"));
    }

    #[test]
    fn mob_short_names_and_curated_words_are_reserved_but_keywords_are_not() {
        let mut world = world_reserving(&["guard"]);
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
