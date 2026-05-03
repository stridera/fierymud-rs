//! Admin commands for world manipulation: movement (goto /
//! transfer / teleport / summon / where), state mutation
//! (freeze / slay / restore / apply / purge / force), and
//! prototype loading (load / loadobj / dumpworld). Bodies stay
//! in commands.rs for now; only Command records relocate.

use mud_db::enums::UserRole;

use crate::commands::{
    Category, Command, Help, cmd_apply, cmd_dumpworld, cmd_force, cmd_freeze, cmd_goto,
    cmd_load, cmd_loadobj, cmd_purge, cmd_restore, cmd_slay, cmd_summon, cmd_teleport,
    cmd_transfer, cmd_where,
};

inventory::submit! {
    Command {
        names: &["where"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "where [player]",
            summary: "Show where a player is, or list all online.",
            long: "Builder+. With a name argument, prints the named \
                   player's current room (zone, id, name). With no \
                   argument, lists every online player and where \
                   they are.",
        },
        run: cmd_where,
    }
}

inventory::submit! {
    Command {
        names: &["goto"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "goto <zone_id> <room_id>",
            summary: "Teleport to any room by composite ID.",
            long: "Builder+ command. Move directly to (zone_id, room_id) \
                   without checking exits or doors.",
        },
        run: cmd_goto,
    }
}

inventory::submit! {
    Command {
        names: &["transfer"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "transfer <player>",
            summary: "Pull an online player to your current room.",
            long: "Builder+ command. Looks up an online player by exact \
                   name (case-insensitive) and moves them to wherever \
                   you are.",
        },
        run: cmd_transfer,
    }
}

inventory::submit! {
    Command {
        names: &["teleport"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "teleport <player> <zone> <room>",
            summary: "Send an online player to a specific room.",
            long: "Builder+. Inverse of `transfer` (which pulls them \
                   to you) and `goto` (which moves you).",
        },
        run: cmd_teleport,
    }
}

inventory::submit! {
    Command {
        names: &["force"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "force <player> <command>",
            summary: "Make a player run a command as themselves.",
            long: "Implementor-only. Dispatches <command> with <player> \
                   as the actor — exactly as if they had typed it.",
        },
        run: cmd_force,
    }
}

inventory::submit! {
    Command {
        names: &["freeze"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "freeze <player>",
            summary: "Toggle a player's frozen state.",
            long: "Implementor-only. Frozen players can't input commands.",
        },
        run: cmd_freeze,
    }
}

inventory::submit! {
    Command {
        names: &["summon"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "summon <mob proto> [<count>]",
            summary: "Spawn one or more mob proto instances at your location.",
            long: "Builder+. Reads the (zone, id) MobProto and spawns \
                   `count` (default 1) instances Located on your room.",
        },
        run: cmd_summon,
    }
}

inventory::submit! {
    Command {
        names: &["apply"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "apply <player> <effect> [duration_secs]",
            summary: "Spawn an effect on a player.",
            long: "Implementor-only. Effect name is matched against \
                   the EffectCatalog. Default duration 60s.",
        },
        run: cmd_apply,
    }
}

inventory::submit! {
    Command {
        names: &["restore"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "restore <player>",
            summary: "Refill a player's HP and stamina to max.",
            long: "Implementor-only. No-op for offline players.",
        },
        run: cmd_restore,
    }
}

inventory::submit! {
    Command {
        names: &["slay"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "slay <player|mob>",
            summary: "Kill a target instantly, ignoring HP / armor.",
            long: "Implementor-only. Same death pipeline as combat — \
                   corpses, loot drops, triggers all fire normally.",
        },
        run: cmd_slay,
    }
}

inventory::submit! {
    Command {
        names: &["purge"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "purge [target]",
            summary: "Despawn the named target — or every non-player in the room.",
            long: "Implementor-only. With no arg, removes every mob / \
                   item in your current room. With a name, despawns \
                   that one entity.",
        },
        run: cmd_purge,
    }
}

inventory::submit! {
    Command {
        names: &["load"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "load <zone> <mob-id>",
            summary: "Spawn a mob proto into your current room.",
            long: "Builder+. Same as `summon` for count=1, kept as a \
                   separate verb for muscle-memory.",
        },
        run: cmd_load,
    }
}

inventory::submit! {
    Command {
        names: &["loadobj", "loado"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "loadobj <zone> <obj-id>",
            summary: "Spawn an object proto onto the floor.",
            long: "Builder+. Materializes one instance of (zone, id) \
                   from `ObjectPrototypes` Located on your current \
                   room.",
        },
        run: cmd_loadobj,
    }
}

inventory::submit! {
    Command {
        names: &["dumpworld"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "dumpworld [path]",
            summary: "Snapshot the entire entity store as JSON.",
            long: "Implementor-only. Useful for offline analysis. \
                   Path defaults to /tmp/world-dump-<ts>.json.",
        },
        run: cmd_dumpworld,
    }
}
