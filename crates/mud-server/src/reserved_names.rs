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

/// True when `name` (any case) equals a mob prototype's name or one of its
/// keywords, so a player can never be confused with, or target-collide
/// with, a mob.
pub(crate) fn matches_mob_proto(world: &World, name: &str) -> bool {
    let Some(protos) = world.get_resource::<mud_world::MobPrototypes>() else {
        return false;
    };
    protos.by_key.values().any(|p| {
        p.name.eq_ignore_ascii_case(name)
            || p.keywords
                .iter()
                .flat_map(|k| k.split_whitespace())
                .any(|k| k.eq_ignore_ascii_case(name))
    })
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
    fn mob_names_and_keywords_are_reserved() {
        let mut world = World::new();
        assert!(
            check_character_name(&world, "Guard").is_ok(),
            "no catalog yet"
        );
        let mut protos = MobPrototypes::default();
        let mut p = mob_proto(1, 1, MobProfession::Trainer);
        p.name = "Bartholomew".into();
        p.keywords = vec!["goblin".into(), "big guard".into()];
        protos.by_key.insert((1, 1), p);
        world.insert_resource(protos);
        assert!(check_character_name(&world, "bartholomew").is_err());
        assert!(check_character_name(&world, "GOBLIN").is_err());
        assert!(check_character_name(&world, "Guard").is_err());
        assert!(check_character_name(&world, "Strider").is_ok());
    }
}
