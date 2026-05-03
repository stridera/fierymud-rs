//! Admin stat / show / set / scripterror / lua / trigger commands.
//! All read-or-mutate diagnostics; bodies stay in commands.rs.

use mud_db::enums::UserRole;

use crate::commands::{
    Category, Command, Help, cmd_astat, cmd_firetrig, cmd_lua, cmd_mstat, cmd_ostat,
    cmd_rstat, cmd_scripterrors, cmd_set, cmd_setweather, cmd_show, cmd_sstat, cmd_stat,
    cmd_syslog, cmd_triggers, cmd_tstat, cmd_zstat,
};

inventory::submit! {
    Command {
        names: &["zstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "zstat [<zone_id>]",
            summary: "Dump ECS state of a zone.",
            long: "Builder+. With no arg, inspects the zone you're in. \
                   Prints zone metadata + entity / mob / item counts.",
        },
        run: cmd_zstat,
    }
}

inventory::submit! {
    Command {
        names: &["mstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "mstat <zone> <id>",
            summary: "Dump a mob prototype's metadata.",
            long: "Builder+. Reads `MobPrototypes[(zone, id)]` and \
                   prints the proto fields + linked behaviors / \
                   professions / abilities / triggers.",
        },
        run: cmd_mstat,
    }
}

inventory::submit! {
    Command {
        names: &["ostat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "ostat <zone> <id>",
            summary: "Dump an object prototype's metadata.",
            long: "Builder+. Mirrors `mstat` for objects: type, weight, \
                   wear flags, restrictions, special-values per type.",
        },
        run: cmd_ostat,
    }
}

inventory::submit! {
    Command {
        names: &["sstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "sstat <zone> <id>",
            summary: "Dump a shop's metadata.",
            long: "Builder+. Reads `ShopCatalog[(zone, id)]` for \
                   keeper, accept rules, items offered, pet roster.",
        },
        run: cmd_sstat,
    }
}

inventory::submit! {
    Command {
        names: &["tstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "tstat <zone> <id>",
            summary: "Dump a trigger's metadata.",
            long: "Builder+. Reads `TriggerCatalog[(zone, id)]` and \
                   prints flags, body length, last-fire stats.",
        },
        run: cmd_tstat,
    }
}

inventory::submit! {
    Command {
        names: &["astat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "astat <ability>",
            summary: "Dump an ability's metadata.",
            long: "Builder+. Looks up the ability by name (or id) and \
                   shows its school, cost, duration, restrictions, \
                   linked effects.",
        },
        run: cmd_astat,
    }
}

inventory::submit! {
    Command {
        names: &["rstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "rstat [<zone> <id>]",
            summary: "Dump a room's ECS state.",
            long: "Builder+. With no arg, inspects your current room. \
                   Otherwise looks up `WorldKeyIndex.rooms[(zone, id)]`.",
        },
        run: cmd_rstat,
    }
}

inventory::submit! {
    Command {
        names: &["stat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "stat <player>",
            summary: "Dump a player entity's component state.",
            long: "Builder+. Reads every component on the named \
                   player and prints a structured dump.",
        },
        run: cmd_stat,
    }
}

inventory::submit! {
    Command {
        names: &["setweather"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "setweather <zone> <kind>",
            summary: "Override a zone's weather.",
            long: "Builder+. Forces a precip kind in the zone's \
                   `WeatherCatalog` entry until the next natural \
                   weather tick rolls a new value.",
        },
        run: cmd_setweather,
    }
}

inventory::submit! {
    Command {
        names: &["set"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "set <player> <field> <value>",
            summary: "Mutate a player field directly.",
            long: "Implementor-only. Field names match the writable \
                   columns on `Characters` (level, alignment, etc).",
        },
        run: cmd_set,
    }
}

inventory::submit! {
    Command {
        names: &["show"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "show <category>",
            summary: "Dump runtime catalogs / counts.",
            long: "Builder+. Categories include `audit`, `effects`, \
                   `weather`, `triggers`, `tickrate`. See in-source \
                   for the full list.",
        },
        run: cmd_show,
    }
}

inventory::submit! {
    Command {
        names: &["scripterrors", "scripterr"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "scripterrors [<n>]",
            summary: "Show recent trigger fire failures.",
            long: "Builder+. Prints the in-memory `ScriptErrorLog` \
                   ring (most-recent first). Default `n=20`.",
        },
        run: cmd_scripterrors,
    }
}

inventory::submit! {
    Command {
        names: &["syslog"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "syslog [<n>]",
            summary: "Show recent server log lines.",
            long: "Builder+. In-memory tail of the tracing log.",
        },
        run: cmd_syslog,
    }
}

inventory::submit! {
    Command {
        names: &["lua"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "lua <code>",
            summary: "Run a snippet of Lua code.",
            long: "Runs `code` with `actor` bound to your character. \
                   Same Lua API surface as triggers.",
        },
        run: cmd_lua,
    }
}

inventory::submit! {
    Command {
        names: &["triggers", "trigs"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "triggers [<zone> <id>]",
            summary: "List loaded triggers / inspect one.",
            long: "Builder+. With no args, lists every (zone, id) in \
                   the trigger catalog. With an id, prints body + \
                   flags + fire stats.",
        },
        run: cmd_triggers,
    }
}

inventory::submit! {
    Command {
        names: &["firetrig"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "firetrig <zone> <id> [<actor>]",
            summary: "Manually fire a trigger by id.",
            long: "Builder+. Useful for testing trigger bodies. The \
                   `actor` defaults to the caster.",
        },
        run: cmd_firetrig,
    }
}
