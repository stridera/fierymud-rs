//! Message boards: `boards`, `board`, `post`, `delpost`, `editpost`.
//! Async-dispatched (see commands/mail.rs for the same pattern).

use mud_db::enums::UserRole;

use crate::commands::{Category, Command, Help, cmd_mail_stub};

inventory::submit! {
    Command {
        names: &["boards"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "boards",
            summary: "List every public message board.",
            long: "Each board has an alias (`mortal`, `god`, `quest`, \
                   etc.) and a title. Use `board <alias>` to list \
                   messages on one.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["board"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "board <alias> [#]",
            summary: "List or read messages on a board.",
            long: "With just an alias, lists messages newest first \
                   (sticky-marked entries float to the top). Append \
                   a slot number to read that message's body.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["post"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "post <board-alias>",
            summary: "Compose a new message on a board.",
            long: "Opens a multi-line composition session: first \
                   non-blank line is the subject, subsequent lines \
                   accumulate as body. `.send` ships, `.abort` \
                   cancels, `.preview` shows the draft. Locked \
                   boards refuse the open.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["delpost"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "delpost <board-alias> <#>",
            summary: "Delete a board message (yours, or any if Builder+).",
            long: "Hard-deletes the row at the given slot. Players \
                   can only delete posts they made (case-insensitive \
                   poster-name match); Builder-and-above can delete \
                   anyone's. Edit history (`BoardMessageEdit`) cascades.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["editpost"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "editpost <board-alias> <#>",
            summary: "Re-open a board post for editing.",
            long: "Pre-loads the existing subject and body into a \
                   composition session. Add lines to append, or \
                   `.clear` to wipe before writing the new body. \
                   Players can only edit their own posts; Builder+ \
                   bypasses the gate.",
        },
        run: cmd_mail_stub,
    }
}
