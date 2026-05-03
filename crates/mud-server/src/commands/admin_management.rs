//! Admin commands for player / clan / housing / staff-note
//! management. These bodies stay in commands.rs for now (they
//! reach into a lot of helpers); only the Command records and
//! help text move here.

use mud_db::enums::UserRole;

use crate::commands::{
    Category, Command, Help, cmd_ban, cmd_cclan, cmd_hgrant, cmd_hinfo, cmd_hrevoke, cmd_pnote,
};

inventory::submit! {
    Command {
        names: &["ban"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "ban <player> <reason>",
            summary: "Ban a player's account.",
            long: "Implementor-only. Looks up the player by name, \
                   resolves the owning Users row, inserts a \
                   BanRecords row. The login flow refuses any of \
                   that account's characters until `unban` lifts.",
        },
        run: cmd_ban,
    }
}

inventory::submit! {
    Command {
        names: &["cclan"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "cclan create <name> <abbrev>\n       cclan assign <player> <abbrev> [rank]\n       cclan kick <player>\n       cclan motd <abbrev> <text>",
            summary: "Manage clans (create / assign / kick / MOTD).",
            long: "Implementor-only.\n\
                   - `create` opens a new clan; name + abbrev must \
                     both be unique.\n\
                   - `assign` puts a character into a clan, defaulting \
                     to MEMBER. Rank can be LEADER / OFFICER / MEMBER \
                     / APPLICANT.\n\
                   - `kick` removes their clan_member row entirely.\n\
                   - `motd` sets a clan's message-of-the-day; empty \
                     text clears it.",
        },
        run: cmd_cclan,
    }
}

inventory::submit! {
    Command {
        names: &["pnote", "playernote"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "pnote <player> [<text> | clear]",
            summary: "Read / append / clear staff notes on a character.",
            long: "Builder+. Without args after the name, prints the \
                   current staff notes. With text, appends a new line \
                   prefixed with the timestamp and your character name. \
                   `clear` is Implementor-only and wipes the entire log.",
        },
        run: cmd_pnote,
    }
}

inventory::submit! {
    Command {
        names: &["hinfo"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "hinfo <player>",
            summary: "Inspect any player's house from anywhere.",
            long: "Builder+. Loads the named player's PlayerHouse row \
                   plus rooms / item count / guest list and prints the \
                   summary. Doesn't require the target to be online.",
        },
        run: cmd_hinfo,
    }
}

inventory::submit! {
    Command {
        names: &["hgrant"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "hgrant <player>",
            summary: "Assign a fresh house to a player.",
            long: "Builder+. Creates a `PlayerHouse` row for the named \
                   character with the entrance set to the room you're \
                   currently standing in. Seeds one foyer room \
                   (`local_index = 0`). Fails if the character already \
                   owns a house — use `hrevoke` first.",
        },
        run: cmd_hgrant,
    }
}

inventory::submit! {
    Command {
        names: &["hrevoke"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "hrevoke <player>",
            summary: "Delete a player's house and all its rooms / items.",
            long: "Implementor-only. Deletes the named character's \
                   PlayerHouse row; FK cascades remove all rooms, \
                   exits, placed items, and guest entries. The owner's \
                   in-memory `HouseSummary` component remains until \
                   they reconnect — bounce them if they're online.",
        },
        run: cmd_hrevoke,
    }
}
