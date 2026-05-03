//! Mail commands: `mail`, `mailbox`, `readmail`, `delmail`.
//!
//! These are async-dispatched: `try_dispatch_async` in
//! commands.rs intercepts before the sync registry runs, so the
//! `run:` field here points at `cmd_mail_stub` — a sentinel that
//! prints "subsystem error" if it ever fires (which means the
//! async path was bypassed). The real handlers
//! (`cmd_mail` / `cmd_mailbox` / `cmd_readmail` / `cmd_delmail`)
//! live in commands.rs and stay there until a separate pass
//! migrates the async dispatcher itself.

use mud_db::enums::UserRole;

use crate::commands::{Category, Command, Help, cmd_mail_stub};

inventory::submit! {
    Command {
        names: &["mail"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "mail <character>",
            summary: "Open a mail composition session to one player.",
            long: "Opens a multi-line composition session: first \
                   non-blank line is the subject, subsequent lines \
                   accumulate as body. Control verbs: `.send` \
                   ships the draft, `.abort` discards it, `.preview` \
                   shows what's queued, `.clear` wipes the draft.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["mailbox"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "mailbox",
            summary: "List inbound mail in your account inbox.",
            long: "Newest first; unread messages have a `*` prefix. \
                   Use `readmail <#>` to read by slot — that also \
                   marks the row read.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["readmail"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "readmail <#>",
            summary: "Read a single mail message by slot.",
            long: "Slot number is the `#` from `mailbox`. Marks the \
                   row read on first read; subsequent re-reads are \
                   silent.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["delmail"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "delmail <#>",
            summary: "Delete a mail message by slot.",
            long: "Hard-deletes the row. Slot number is the `#` from \
                   `mailbox`.",
        },
        run: cmd_mail_stub,
    }
}
