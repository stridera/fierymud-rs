//! Spell / skill / ability commands and the cancel-cast control
//! verbs (abort/cancel). All Combat-category in the static array;
//! moved here as a separate file from commands/combat.rs since
//! they cluster around the casting pipeline rather than the
//! attack/defend verbs.

use mud_db::enums::UserRole;

use crate::commands::{
    Category, Command, Help, cmd_abort, cmd_cancel, cmd_cast, cmd_chant, cmd_forget,
    cmd_memorize, cmd_perform, cmd_pick, cmd_skill, cmd_study,
};

inventory::submit! {
    Command {
        names: &["pick"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "pick <direction>",
            summary: "Pick a locked door open with rogue tools.",
            long: "Skill check against your `PICK_LOCK` proficiency. \
                   Refuses on unlocked exits, exits without a keyhole, \
                   and players who haven't trained pick lock. Costs \
                   5 stamina whether you succeed or fail. On success \
                   the door flips Locked → Closed (same as `unlock`); \
                   on failure you get a fumble line.",
        },
        run: cmd_pick,
    }
}

inventory::submit! {
    Command {
        names: &["study"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "study <spell>",
            summary: "Permanently learn a spell from your class list.",
            long: "Adds the spell to your `KnownAbilities` at the \
                   minimum proficiency tier (`known=true`, \
                   proficiency=1). Refuses unknown abilities, \
                   already-known spells, or off-class spells. \
                   Persists across reconnect via `CharacterAbilities`.",
        },
        run: cmd_study,
    }
}

inventory::submit! {
    Command {
        names: &["memorize", "mem", "pray"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "memorize <spell>",
            summary: "Prepare a spell into one of your circle slots.",
            long: "Looks up the spell by name in your class's circle \
                   list (via `ClassAbilities`), checks slot availability \
                   for that circle (via `SpellSlotProgression`), and \
                   appends the spell to your `MemorizedSpells` list. \
                   Refuses unknown spells, off-class spells, or full \
                   circles. Session-only — re-memorize on reconnect.",
        },
        run: cmd_memorize,
    }
}

inventory::submit! {
    Command {
        names: &["forget"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "forget <spell>",
            summary: "Drop a memorized spell from your prepared list.",
            long: "Removes the first matching memorized spell, freeing \
                   that circle slot for a new memorize. No-op if the \
                   spell isn't currently memorized.",
        },
        run: cmd_forget,
    }
}

inventory::submit! {
    Command {
        names: &["cast", "c"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "cast <spell> [target]",
            summary: "Cast a spell from the loaded catalog.",
            long: "Looks up a SPELL by case-insensitive name (partial \
                   match accepted). For now this is a stub: prints the \
                   ability's metadata so you can see what's in the \
                   catalog. Real effect application — slot consumption, \
                   restriction checks, damage/heal/buff resolution — \
                   lands when CharacterAbilities and the effect \
                   pipeline are wired. Only matches abilityType = \
                   SPELL; for chants and songs use `chant` / `perform`.",
        },
        run: cmd_cast,
    }
}

inventory::submit! {
    Command {
        names: &["chant"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "chant <chant> [target]",
            summary: "Invoke a chant from the catalog (cleric-side spells).",
            long: "Same shape as `cast` but filters to abilityType = \
                   CHANT. Stub: prints metadata and gates on \
                   KnownAbilities, no effect application yet.",
        },
        run: cmd_chant,
    }
}

inventory::submit! {
    Command {
        names: &["perform"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "perform <song> [target]",
            summary: "Perform a song from the catalog (bard).",
            long: "Same shape as `cast` but filters to abilityType = \
                   SONG. Stub: prints metadata and gates on \
                   KnownAbilities, no effect application yet.",
        },
        run: cmd_perform,
    }
}

inventory::submit! {
    Command {
        names: &["skill", "use"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "skill <name> [target]",
            summary: "Invoke a SKILL-type ability from the catalog.",
            long: "Sibling to cast/chant/perform: looks up a SKILL \
                   row by name and runs it through the same effect \
                   application pipeline. New combat skills should be \
                   added as Muditor `Ability` rows (kind=SKILL) with \
                   `AbilityEffect` mappings — no Rust change needed. \
                   Hardcoded skills (bandage, gouge, etc.) coexist \
                   for now; they'll migrate as Phase B effect-type \
                   consumers land.",
        },
        run: cmd_skill,
    }
}

inventory::submit! {
    Command {
        names: &["abort"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "abort",
            summary: "Cancel an in-progress cast or queued spell.",
            long: "FieryMUD legacy: aborts the spell you're currently \
                   casting and clears any spell queued behind it. \
                   Today's runtime resolves casts immediately and \
                   has no queue, so abort has nothing to do — kept \
                   as a registered command name for muscle memory \
                   and to provide a clear message instead of \
                   'Unknown command'. Use `cancel` to drop a \
                   non-permanent buff already on you.",
        },
        run: cmd_abort,
    }
}

inventory::submit! {
    Command {
        names: &["cancel"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "cancel [<effect>]",
            summary: "Drop a non-permanent buff from yourself.",
            long: "With no arg, lists effects you can cancel \
                   (anything not flagged permanent). With an effect \
                   name, finds the matching `EffectInstance` on you \
                   and despawns it. Permanent effects (e.g. innate \
                   resistances) refuse to cancel.",
        },
        run: cmd_cancel,
    }
}

