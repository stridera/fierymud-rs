//! Quest verbs — `quests` / `abandon` / `questinfo` / `qaccept`
//! and admin counterparts `qload` / `qgive` / `qcomplete`. Plus
//! `innate` (race-ability listing) since it lives next to the
//! quest readouts in the help category and uses the same
//! async-dispatch + `cmd_mail_stub` shape.

use mud_db::enums::UserRole;

use crate::commands::{Category, Command, Help, cmd_mail_stub};

inventory::submit! {
    Command {
        names: &["quests", "qstat", "qlist"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "quests",
            summary: "List your accepted quests.",
            long: "In-progress quests appear first with their short \
                   description; completed/abandoned quests follow with \
                   their final status. Each in-progress quest also \
                   shows its phases + objective progress (▶ for \
                   current, ✓ for done, [N/M] for showProgress).",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["abandon"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "abandon <#>",
            summary: "Drop an in-progress quest.",
            long: "Argument is the slot number from the in-progress \
                   section of `quests`. Marks the row ABANDONED \
                   rather than deleting it, preserving the audit \
                   trail and the (char, zone, id) unique key.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["innate"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "innate",
            summary: "List your race's innate abilities.",
            long: "Reads the `RaceAbilities` rows for your character's \
                   race and prints each ability's name, category \
                   (PRIMARY / SECONDARY / ...), starting bonus, and \
                   proficiency cap.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["questinfo"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "questinfo <zone> <id>",
            summary: "Show details for a single quest definition.",
            long: "Reads the `Quest` row for `(zone, id)` and prints \
                   name, level range, flags (repeatable / shareable / \
                   hidden / auto-accept), short description, and full \
                   description.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["qaccept"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "qaccept <zone> <quest-id>",
            summary: "Accept a quest by composite id.",
            long: "Self-service quest acceptance. Validates level \
                   range, checks every required prerequisite, and \
                   refuses if you already have it in progress (or \
                   completed it once already on a non-repeatable \
                   quest).",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["qload"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "qload <zone> <quest-id>",
            summary: "Admin: assign a quest to your character (testing).",
            long: "Inserts a CharacterQuest row with status \
                   IN_PROGRESS for the caller's character. No-op if \
                   that quest is already assigned to you.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["qgive"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "qgive <player> <zone> <quest-id>",
            summary: "Admin: assign a quest to another online player.",
            long: "Same as `qload` but targets a named online player \
                   instead of the caller. Target must be currently \
                   online (offline-character assignment is left for a \
                   future iteration).",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["qcomplete"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "qcomplete <#>",
            summary: "Admin: force-complete an in-progress quest.",
            long: "Slot number from the `quests` in-progress section. \
                   Flips IN_PROGRESS → COMPLETED, stamps completed_at, \
                   bumps completion_count.",
        },
        run: cmd_mail_stub,
    }
}
