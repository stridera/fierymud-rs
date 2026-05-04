//! Every Info-category command (and Combat-adjacent verbs that
//! don't fit the cluster files) — 127 entries. Both the Command
//! records and the handler bodies live here.

#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::wildcard_imports)]

use std::collections::HashMap;

use bevy_ecs::prelude::{Entity, With, World};
use mud_db::enums::{Direction, UserRole};
use mud_world::*;

use crate::commands::*;

inventory::submit! {
    Command {
        names: &["help"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "help [command]",
            summary: "List commands or show details on a specific one.",
            long: "With no arguments, shows commands available to you grouped \
                   by category. With an argument, shows the usage and details \
                   for that command.",
        },
        run: cmd_help,
    }
}

inventory::submit! {
    Command {
        names: &["scan"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "scan",
            summary: "Peek at the adjacent rooms one step away.",
            long: "For each unblocked exit, prints the target room's \
                   name and how many mobs / players are there. Doors \
                   that are closed or locked show their state instead. \
                   Useful for spotting threats / hosts before walking \
                   in.",
        },
        run: cmd_scan,
    }
}

inventory::submit! {
    Command {
        names: &["track", "hunt"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "track <target>",
            summary: "Find the direction toward a named target.",
            long: "BFS through open exits up to 50 rooms looking for \
                   a mob or player matching the name. Reports the \
                   direction and distance. Closed or locked doors \
                   block the scan. No perception check yet — hidden \
                   targets are tracked the same as visible ones.",
        },
        run: cmd_track,
    }
}

inventory::submit! {
    Command {
        names: &["practice", "prac"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "practice [<ability>]",
            summary: "List trained abilities, or improve proficiency.",
            long: "Without an argument, renders KnownAbilities with \
                   proficiency 0-1000 and a tier label. With an \
                   ability name, raises that ability's proficiency \
                   by 5 (capped at the class's `proficiency_cap` \
                   from `ClassAbilities`). Persists across \
                   reconnect via `CharacterAbilities`.",
        },
        run: cmd_practice,
    }
}

inventory::submit! {
    Command {
        names: &["glance"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "glance <target>",
            summary: "One-line condition check on someone in your room.",
            long: "Tells you the target's name, posture, condition (e.g. \
                   `bleeding`), and whether they're fighting. Faster than \
                   `examine` for a quick teammate / enemy check.",
        },
        run: cmd_glance,
    }
}

inventory::submit! {
    Command {
        names: &["experience", "exp", "xp"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "experience",
            summary: "Show your current experience and level.",
            long: "Prints your level and total experience points. The \
                   per-level table that turns this into a `to next` \
                   readout will land with the levelling system.",
        },
        run: cmd_experience,
    }
}

inventory::submit! {
    Command {
        names: &["wealth", "gold", "money", "coins", "wallet"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "wealth",
            summary: "Show your on-hand coin in platinum/gold/silver/copper.",
            long: "Prints the current coin total split across the four \
                   denominations (1 platinum = 10 gold = 100 silver = \
                   1000 copper). Use `balance` for bank-stored coin.",
        },
        run: cmd_wealth,
    }
}

inventory::submit! {
    Command {
        names: &["bribe"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "bribe <amount> <target>",
            summary: "Hand a sum of copper to a mob — fires BRIBE triggers.",
            long: "Decrements your on-hand coin by `amount` (copper \
                   units) and pads the target mob's coin by the same. \
                   Fires the target's BRIBE-flagged Lua triggers with \
                   `actor` = you and `amount` as a Lua global so \
                   bodies can react proportionally.",
        },
        run: cmd_bribe,
    }
}

inventory::submit! {
    Command {
        names: &["deposit"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "deposit <amount>",
            summary: "Move copper into the bank.",
            long: "Refuses if you don't have that much on hand. v1 \
                   is location-agnostic; banker-mob gating arrives \
                   once `MobProfession::Banker` is hydrated.",
        },
        run: cmd_deposit,
    }
}

inventory::submit! {
    Command {
        names: &["withdraw"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "withdraw <amount>",
            summary: "Move copper from the bank to on-hand wealth.",
            long: "Refuses if your bank balance can't cover the \
                   amount. Inverse of `deposit`.",
        },
        run: cmd_withdraw,
    }
}

inventory::submit! {
    Command {
        names: &["value", "appraise"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "value <item>",
            summary: "Show an item's catalog value in coin.",
            long: "Searches your inventory and the room for the named \
                   item, then prints its base value (the schema's \
                   `Objects.cost`) split into denominations. Shops will \
                   pay some fraction of this on sell once that lands.",
        },
        run: cmd_value,
    }
}

inventory::submit! {
    Command {
        names: &["list"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "list",
            summary: "Show what the shopkeeper here is selling.",
            long: "Looks for a `Shopkeeper`-tagged mob in your room, \
                   then prints the keeper's catalog with prices and \
                   stock. `buy <#|name>` and `sell <item>` land next.",
        },
        run: cmd_list,
    }
}

inventory::submit! {
    Command {
        names: &["buy"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "buy <#|name>",
            summary: "Buy an item from the shopkeeper here.",
            long: "Argument is either the catalog index from `list` or \
                   a substring of the item's name. Coin is deducted \
                   from your `wealth`; the item lands in your \
                   inventory. Stock is advisory until per-shop \
                   instance state lands.",
        },
        run: cmd_buy,
    }
}

inventory::submit! {
    Command {
        names: &["sell"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "sell <item>",
            summary: "Sell a carried item to the shopkeeper here.",
            long: "Pays `proto.cost * sell_profit` rounded for any \
                   carried item with positive cost. Equipped items \
                   are refused (`remove` first). Item-type filters \
                   (`ShopAccepts`) are not enforced yet.",
        },
        run: cmd_sell,
    }
}

inventory::submit! {
    Command {
        names: &["hire"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Mount,
        help: Help {
            usage: "hire <#|name>",
            summary: "Hire a pet from a pet-shop keeper.",
            long: "Spawns a fresh mob as your follower. Coin from \
                   `wealth`. Pet is renamed to `<you>'s <mob>` so \
                   it doesn't blend with wild mobs of the same kind.",
        },
        run: cmd_hire,
    }
}

inventory::submit! {
    Command {
        names: &["title"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "title [<new title> | clear]",
            summary: "Show or change the epithet shown after your name.",
            long: "With no argument, prints your current title. With a \
                   new title, sets it (max 60 chars). With `clear` (or \
                   `none` / `-`), removes it. Persists on disconnect.",
        },
        run: cmd_title,
    }
}

inventory::submit! {
    Command {
        names: &["description", "desc"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "description [<new prose> | clear]",
            summary: "Show or set the prose `examine` shows for you.",
            long: "With no argument, prints your current description. \
                   With new text, replaces it (max 500 chars). With \
                   `clear` / `none` / `-`, removes it. XML-Lite color \
                   tags render the same as room descriptions. Persists \
                   on disconnect.",
        },
        run: cmd_description,
    }
}

inventory::submit! {
    Command {
        names: &["examine", "exa"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "examine <target>",
            summary: "Look closely at a person or thing in the room.",
            long: "Match by name or keyword (case-insensitive substring) on \
                   anything in your current room — mobs, other players, \
                   items, or equipped gear. Shows their long description.",
        },
        run: cmd_examine,
    }
}

inventory::submit! {
    Command {
        names: &["look", "l"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "look",
            summary: "Look at your surroundings.",
            long: "Shows the current room's name, anyone or anything else \
                   present, and the available exits.",
        },
        run: cmd_look,
    }
}

inventory::submit! {
    Command {
        names: &["who"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "who [<min-level> [<max-level>]] | who clan <abbrev>",
            summary: "List players currently online.",
            long: "With no args, shows every connected player. \
                   With one numeric arg, filters to players at \
                   that level or higher. With two numeric args, \
                   filters to the inclusive level range. \
                   `who clan <abbrev>` filters to players in the \
                   named clan (case-insensitive match against the \
                   `[ABBR]` tag shown alongside each name).",
        },
        run: cmd_who,
    }
}

inventory::submit! {
    Command {
        names: &["score", "sc"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "score",
            summary: "Display your character's stats.",
            long: "Shows HP, combat stats (hit/damage roll, AC, alignment), \
                   and your current combat target if any.",
        },
        run: cmd_score,
    }
}

inventory::submit! {
    Command {
        names: &["roles"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "roles",
            summary: "Show your account role and permissions.",
            long: "Diagnostic: prints the role and any extra permissions \
                   attached to your account.",
        },
        run: cmd_roles,
    }
}

inventory::submit! {
    Command {
        names: &["inventory", "i", "inv"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "inventory [<filter>]",
            summary: "List items you are carrying.",
            long: "Shows everything in your inventory by name. \
                   Use `get` to pick items up and `drop` to set them down. \
                   With a filter arg, only items whose name contains the \
                   substring (case-insensitive) are shown — useful for \
                   pruning a packed bag down to e.g. just potions.",
        },
        run: cmd_inventory,
    }
}

inventory::submit! {
    Command {
        names: &["get", "take", "g"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "get <item> | get <item> from <container>",
            summary: "Pick up an item from the room or a container.",
            long: "Match is by case-insensitive substring on the item's \
                   keywords (or its name). With `from <container>`, \
                   pulls from a container the player is carrying or \
                   one in the room. The item moves into your \
                   inventory; everyone else in the room sees the action.",
        },
        run: cmd_get,
    }
}

inventory::submit! {
    Command {
        names: &["put"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "put <item> <container>",
            summary: "Move a carried item into a container.",
            long: "Container can be a carried item or one in the \
                   current room. Equipped items must be `remove`d \
                   first. Bystanders see the action.",
        },
        run: cmd_put,
    }
}

inventory::submit! {
    Command {
        names: &["drop"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "drop <item>",
            summary: "Drop an item from your inventory onto the floor.",
            long: "Match is by case-insensitive substring on keywords. \
                   The item is left in the current room; bystanders see \
                   you drop it.",
        },
        run: cmd_drop,
    }
}

inventory::submit! {
    Command {
        names: &["donate"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "donate <item>",
            summary: "Charitably leave an item for someone else.",
            long: "Drops the item in the current room with a giving \
                   message. A real donation-room flag lands later — \
                   for now, donated items sit on the floor.",
        },
        run: cmd_donate,
    }
}

inventory::submit! {
    Command {
        names: &["junk", "trash"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "junk <item>",
            summary: "Permanently destroy a carried item.",
            long: "The item is despawned; nothing is dropped on the \
                   floor and no coin is awarded. Refuses on equipped \
                   gear — `remove` first.",
        },
        run: cmd_junk,
    }
}

inventory::submit! {
    Command {
        names: &["give"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "give <item> <target>",
            summary: "Hand an item to another character in the room.",
            long: "Both you and the target must be in the same room. The \
                   item must be in your inventory (not equipped — `remove` \
                   first if needed).",
        },
        run: cmd_give,
    }
}

inventory::submit! {
    Command {
        names: &["wear"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "wear <item>",
            summary: "Equip a wearable item from your inventory.",
            long: "The item must have a wear-slot, and that slot must be \
                   free. Use `remove` to take something off first.",
        },
        run: cmd_wear,
    }
}

inventory::submit! {
    Command {
        names: &["wield"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "wield <item>",
            summary: "Wield a weapon (shortcut for wear into the Wield slot).",
            long: "Equivalent to wear, but only succeeds for items that go \
                   into the wield slot.",
        },
        run: cmd_wield,
    }
}

inventory::submit! {
    Command {
        names: &["light"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "light <item>",
            summary: "Light a torch or lantern.",
            long: "Sets a `Lit` marker on a Light-type item in your \
                   inventory. Refused on non-light items or items \
                   that are already lit.",
        },
        run: cmd_light,
    }
}

inventory::submit! {
    Command {
        names: &["extinguish", "douse"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "extinguish <item>",
            summary: "Put out a lit torch or lantern.",
            long: "Removes the `Lit` marker from a held or carried \
                   light source. Refused on items that aren't lit.",
        },
        run: cmd_extinguish,
    }
}

inventory::submit! {
    Command {
        names: &["mount"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Mount,
        help: Help {
            usage: "mount <mob>",
            summary: "Climb onto a mountable mob.",
            long: "Target must be `Mountable` (auto-applied to mobs \
                   whose keywords contain horse / steed / mount / \
                   donkey / mare). When you move, your mount comes \
                   with you. Refused on already-mounted you, on \
                   already-ridden mounts, or on mobs in combat.",
        },
        run: cmd_mount,
    }
}

inventory::submit! {
    Command {
        names: &["dismount"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Mount,
        help: Help {
            usage: "dismount",
            summary: "Get off your current mount.",
            long: "Clears the `Mounted` link on you and the \
                   `RiddenBy` link on the mount. No-op when not \
                   mounted.",
        },
        run: cmd_dismount,
    }
}

inventory::submit! {
    Command {
        names: &["fly"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "fly",
            summary: "Take to the air (sets the Flying marker).",
            long: "Movement charges a flat 2 stamina per move while \
                   flying — great savings over water/swamp (4-6 \
                   normally), slightly pricier on roads (1). Use \
                   `walk` or `land` to come back down.",
        },
        run: cmd_fly,
    }
}

inventory::submit! {
    Command {
        names: &["walk", "land"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: Help {
            usage: "walk",
            summary: "Stop flying and walk again.",
            long: "Clears the Flying marker. No-op when already \
                   walking.",
        },
        run: cmd_walk,
    }
}

inventory::submit! {
    Command {
        names: &["hide"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "hide",
            summary: "Slip into the shadows (sets the Stealth marker).",
            long: "Currently a marker toggle — combat formulas that \
                   reference `hidden` (e.g. BACKSTAB's bonus) read \
                   the marker. The full rogue skill check, noise \
                   gating, and look-time visibility filtering land \
                   with the skill system.",
        },
        run: cmd_hide,
    }
}

inventory::submit! {
    Command {
        names: &["visible", "vis"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "visible",
            summary: "Stop hiding (clears the Stealth marker).",
            long: "Removes the `Stealth` marker — back to normal visibility.",
        },
        run: cmd_visible,
    }
}

inventory::submit! {
    Command {
        names: &["eat"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "eat <item>",
            summary: "Consume a food item from your inventory.",
            long: "Despawns the food. Refused on non-food items. \
                   Effects (hunger, ConsumableEffects) are deferred.",
        },
        run: cmd_eat,
    }
}

inventory::submit! {
    Command {
        names: &["quaff"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "quaff <potion>",
            summary: "Drink a potion from your inventory.",
            long: "Despawns the potion. Refused on non-potion items. \
                   Effect application (ConsumableEffects) is deferred.",
        },
        run: cmd_quaff,
    }
}

inventory::submit! {
    Command {
        names: &["drink"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "drink <container>",
            summary: "Take a swig from a drink container.",
            long: "Decrements the container's `remaining` liquid by \
                   4 units. Empty containers refuse. Use `quaff` for \
                   potions, `sip` for a smaller swig (1 unit), \
                   `taste` to identify the liquid without drinking.",
        },
        run: cmd_drink,
    }
}

inventory::submit! {
    Command {
        names: &["sip"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "sip <container>",
            summary: "Sip 1 unit from a drink container.",
            long: "Lighter than `drink` (4 units). Same refusal on \
                   empty containers; same poison handling.",
        },
        run: cmd_sip,
    }
}

inventory::submit! {
    Command {
        names: &["taste"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "taste <container>",
            summary: "Identify a liquid without drinking it.",
            long: "Reveals the liquid name; on poisoned containers, \
                   adds an off-taste hint. No consumption.",
        },
        run: cmd_taste,
    }
}

inventory::submit! {
    Command {
        names: &["pour"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "pour <container> [target]",
            summary: "Transfer liquid between containers, or empty.",
            long: "With no target, dumps the source's liquid on the \
                   ground. With a target container, transfers as much \
                   as the target can accept; refuses on liquid-type \
                   mismatch unless the target is empty (in which case \
                   the target adopts the source's liquid + poison \
                   flag).",
        },
        run: cmd_pour,
    }
}

inventory::submit! {
    Command {
        names: &["fill"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "fill <container> <source>",
            summary: "Top up a container from another container.",
            long: "Inverse-arg `pour`: same liquid-match rules, \
                   transfers up to the destination's remaining \
                   capacity.",
        },
        run: cmd_fill,
    }
}

inventory::submit! {
    Command {
        names: &["recite"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "recite <scroll> [<target>]",
            summary: "Read a magic scroll, casting its inscribed spells.",
            long: "Looks up the bound abilities via ObjectAbilities and \
                   dispatches each through the cast pipeline. Despawns the \
                   scroll regardless of outcome (single-use).",
        },
        run: cmd_recite,
    }
}

inventory::submit! {
    Command {
        names: &["wave"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "wave <wand> [<target>]",
            summary: "Wave a wand to cast its bound spell.",
            long: "Decrements the wand's Charges component on each use; \
                   wand crumbles when charges hit 0. Refused on depleted \
                   wands. Item type must be WAND.",
        },
        run: cmd_wave,
    }
}

inventory::submit! {
    Command {
        names: &["tap"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "tap <staff> [<target>]",
            summary: "Tap a staff to invoke its bound abilities.",
            long: "Same charge mechanic as wave but for STAFF items. \
                   Useful for buff staves and AOE-style staff effects.",
        },
        run: cmd_tap,
    }
}

inventory::submit! {
    Command {
        names: &["hold", "grab"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "hold <item>",
            summary: "Hold a non-weapon item in your offhand.",
            long: "Equips an item in the Hold slot (lights, instruments, \
                   wands). Refused on items that don't go in Hold.",
        },
        run: cmd_hold,
    }
}

inventory::submit! {
    Command {
        names: &["remove", "rem"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "remove <item>",
            summary: "Unequip an item, returning it to your inventory.",
            long: "The item must currently be equipped on you.",
        },
        run: cmd_remove,
    }
}

inventory::submit! {
    Command {
        names: &["equipment", "eq"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "equipment",
            summary: "List items you are wearing/wielding.",
            long: "Shows each occupied slot and the item filling it.",
        },
        run: cmd_equipment,
    }
}

inventory::submit! {
    Command {
        names: &["exits", "ex"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "exits",
            summary: "List the exits from your current room.",
            long: "Shows each direction with the destination room's name. \
                   Exits whose target room isn't loaded show as '(beyond)'.",
        },
        run: cmd_exits,
    }
}

inventory::submit! {
    Command {
        names: &["commands"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "commands [<category>]",
            summary: "Flat alphabetical list of every command you can use.",
            long: "Shows just the names you have access to, without the \
                   per-category framing `help` uses. Aliases share their \
                   primary name's slot. With a category arg (info / \
                   movement / communication / combat / admin) the list \
                   is filtered to that category, useful when the full \
                   200+ command roster is overwhelming.",
        },
        run: cmd_commands,
    }
}

inventory::submit! {
    Command {
        names: &["open"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "open <direction>",
            summary: "Open a closed door in the given direction.",
            long: "Refused if the exit is already open or locked. \
                   Locked doors need a key (use `unlock`).",
        },
        run: cmd_open,
    }
}

inventory::submit! {
    Command {
        names: &["unlock"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "unlock <direction>",
            summary: "Unlock a locked door using a key in your inventory.",
            long: "Searches your carried items for a keyword that \
                   matches the exit's required key. On match, the \
                   door is unlocked (state Closed); use `open` to \
                   then walk through. Refused if the exit isn't \
                   locked or you have no matching key.",
        },
        run: cmd_unlock,
    }
}

inventory::submit! {
    Command {
        names: &["close"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "close <direction>",
            summary: "Close an open door in the given direction.",
            long: "Refused if the exit has no door, is already \
                   closed, or doesn't exist.",
        },
        run: cmd_close,
    }
}

inventory::submit! {
    Command {
        names: &["lock"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "lock <direction>",
            summary: "Lock a closed door using a key in your inventory.",
            long: "Mirror of `unlock`: requires the exit to be closed \
                   (not already locked, not open) and to have a key \
                   requirement, and that you carry that key. On match, \
                   the door is locked.",
        },
        run: cmd_lock,
    }
}

inventory::submit! {
    Command {
        names: &["read"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "read <item>",
            summary: "Read the text on an item (book, sign, scroll).",
            long: "Finds an item by keyword on you or in the room and \
                   prints its description text. Refuses on mobs and \
                   players — use `examine` for those.",
        },
        run: cmd_read,
    }
}

inventory::submit! {
    Command {
        names: &["compare"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "compare <item-a> [<item-b>]",
            summary: "Compare two carried/worn items by weight, level, and weapon damage.",
            long: "Each item is matched by keyword the same way `wear` \
                   matches. Both items must be on you (inventory or \
                   equipped). With a single arg, the second item is \
                   inferred from whatever you're currently wearing in \
                   that item's wearable slot — handy for the \
                   \"should I switch?\" decision. Prints weight + \
                   level deltas, plus a weapon-damage row when both \
                   sides have non-zero average damage.",
        },
        run: cmd_compare,
    }
}

inventory::submit! {
    Command {
        names: &["motd"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "motd",
            summary: "Show the message-of-the-day.",
            long: "Static welcome text for now — once a GameConfig \
                   `motd` row lands, this will read from there.",
        },
        run: cmd_motd,
    }
}

inventory::submit! {
    Command {
        names: &["news"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "news",
            summary: "Recent server news.",
            long: "Static for now; will read from a future news table.",
        },
        run: cmd_news,
    }
}

inventory::submit! {
    Command {
        names: &["credits"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "credits",
            summary: "Show contributors and credits.",
            long: "Acknowledges the project's antecedents.",
        },
        run: cmd_credits,
    }
}

inventory::submit! {
    Command {
        names: &["policies", "rules", "policy"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "policies",
            summary: "Server rules and code of conduct.",
            long: "Static for now. Aliases: `rules`, `policy`.",
        },
        run: cmd_policies,
    }
}

inventory::submit! {
    Command {
        names: &["account"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "account",
            summary: "Show your account email, role, and character roster.",
            long: "Read-only summary of who you're logged in as and \
                   which character is currently active. Snapshot taken \
                   at login — characters created mid-session won't \
                   appear until you reconnect.",
        },
        run: cmd_account,
    }
}

inventory::submit! {
    Command {
        names: &["richtest"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "richtest",
            summary: "Render a color-tag sampler.",
            long: "Prints a sampler of every color and modifier the \
                   XML-Lite renderer supports — handy for verifying \
                   your client's color depth and for debugging \
                   color-tag rendering.",
        },
        run: cmd_richtest,
    }
}

inventory::submit! {
    Command {
        names: &["clientinfo"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "clientinfo",
            summary: "Show this session's connection info.",
            long: "Reports your active character, role, session \
                   uptime (since login), and idle time (since the \
                   last command typed). Terminal capabilities — \
                   color depth, dimensions, MCCP — aren't tracked \
                   in the runtime today; the full RFC-1408 telnet \
                   negotiation would lift them off the wire.",
        },
        run: cmd_clientinfo,
    }
}

inventory::submit! {
    Command {
        names: &["world", "users", "stats"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "world",
            summary: "Live entity counts: zones, rooms, mobs, items, players.",
            long: "Snapshot of the world state right now: how many zones \
                   and rooms loaded, how many live mobs and items \
                   spawned, how many players online, server tick + \
                   uptime, and active effect-instance count. Aliases \
                   `users` and `stats` mirror `world` since the player \
                   count and per-system load are the most-asked pieces.",
        },
        run: cmd_world,
    }
}

inventory::submit! {
    Command {
        names: &["time", "date", "uptime"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "time",
            summary: "Show server uptime and tick count.",
            long: "Real-world time, how long the server has been running, \
                   and the current world tick.",
        },
        run: cmd_time,
    }
}

inventory::submit! {
    Command {
        names: &["weather"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "weather",
            summary: "Atmospheric flavor for your zone's climate.",
            long: "Renders a single line based on your current zone's \
                   `Climate` and the in-game time of day. Rule-of-thumb \
                   only — no per-tick weather simulation yet, so the \
                   same input produces the same output.",
        },
        run: cmd_weather,
    }
}

inventory::submit! {
    Command {
        names: &["version"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "version",
            summary: "Show server build identity.",
            long: "Crate name, version, and the rustc/profile combo the \
                   binary was built with. For ops/debug; players will \
                   rarely care.",
        },
        run: cmd_version,
    }
}

inventory::submit! {
    Command {
        names: &["idle"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "idle [<min-minutes>]",
            summary: "Show online players sorted by idle time, longest first.",
            long: "Same population as `who`, but ordered by how long since \
                   each player last typed something. Players who just \
                   connected and haven't typed yet show as `fresh`; anyone \
                   under a minute shows as `active`. \
                   With a numeric arg, filters to players idle that many \
                   minutes or more — handy for spotting AFK staff or stuck \
                   sessions.",
        },
        run: cmd_idle,
    }
}

inventory::submit! {
    Command {
        names: &["spells", "abilities", "abil"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "spells [all] [filter]",
            summary: "List the abilities you know (or the full catalog).",
            long: "By default shows only abilities you've actually \
                   learned, grouped by kind (Spells / Chants / Songs / \
                   Skills). Add `all` to dump the full catalog (handy \
                   for builders / curiosity). The filter substring \
                   matches both the ability name AND its sphere — \
                   `spells fire` lands Fireball (name) plus every \
                   fire-sphere spell. Try also: `spells all healing` \
                   for the full healing-sphere catalog.",
        },
        run: cmd_spells,
    }
}

inventory::submit! {
    Command {
        names: &["level"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "level",
            summary: "XP-curve readout: current level, XP, distance to next.",
            long: "Reads `Profile.level` and `Profile.experience` and \
                   shows the cumulative XP for this level, the next \
                   level's threshold, and how far you have to go. \
                   Capped levels (max in `LevelDefinition`) print a \
                   max-level note instead.",
        },
        run: cmd_level,
    }
}

inventory::submit! {
    Command {
        names: &["slots"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "slots",
            summary: "Show your spell-slot allotment per circle.",
            long: "Read-only readout of how many slots per circle \
                   your class+level grants you. Format `used / max`. \
                   Refill-on-rest tick not yet implemented; memorize \
                   only consumes slots, forget releases them.",
        },
        run: cmd_slots,
    }
}

inventory::submit! {
    Command {
        names: &["skills"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "skills [all] [filter]",
            summary: "List skills you know (or the full catalog).",
            long: "Like `spells` but filtered to kind=Skill. Default \
                   shows only what you've learned; `skills all` dumps \
                   the full catalog. Optional substring filter applies \
                   to either scope.",
        },
        run: cmd_skills,
    }
}

inventory::submit! {
    Command {
        names: &["songs"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "songs [all] [filter]",
            summary: "List bardic songs you know (or the full catalog).",
            long: "Like `spells` but filtered to kind=Song. Default \
                   shows only songs you've learned; `songs all` dumps \
                   the catalog. Use `perform <song>` to invoke.",
        },
        run: cmd_songs,
    }
}

inventory::submit! {
    Command {
        names: &["chants"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "chants [all] [filter]",
            summary: "List chants you know (or the full catalog).",
            long: "Like `spells` but filtered to kind=Chant. Default \
                   shows only chants you've learned; `chants all` \
                   dumps the catalog. Use `chant <name>` to invoke.",
        },
        run: cmd_chants,
    }
}

inventory::submit! {
    Command {
        names: &["prompt", "display"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "prompt [template]",
            summary: "Show or change your status prompt template.",
            long: "With no argument, prints your current template. With a \
                   template, replaces it. Variables: %h current HP, %H max \
                   HP, %v current stamina, %V max stamina, %n your name, \
                   %r current room, %g on-hand wealth (copper), %t in-game \
                   hour (zero-padded), %s season, %d day/night, %% literal \
                   percent. Examples: \
                     prompt <%h/%H hp %v/%V mv> \
                     prompt [%t %h/%H %d] ",
        },
        run: cmd_prompt,
    }
}

inventory::submit! {
    Command {
        names: &["style"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "style [fancy | standard | minimal]",
            summary: "Choose a UI style for info commands like score.",
            long: "Three tiers: fancy (ASCII-art borders), standard (the \
                   default — clean indented lines), and minimal (a single \
                   dense line, useful in narrow viewports). With no \
                   argument, shows the current style. Currently only score \
                   honors this; more commands will follow.",
        },
        run: cmd_style,
    }
}

inventory::submit! {
    Command {
        names: &["toggle"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "toggle <flag>",
            summary: "Flip a player flag on or off.",
            long: "Examples: `toggle afk`, `toggle deaf`, `toggle notell`. \
                   `flags` lists all currently-set flags. Recognised names \
                   include AFK, DEAF, NO_TELL/NOTELL, BRIEF, COMPACT, \
                   AUTO_LOOT, AUTO_GOLD, AUTO_EXIT, WIMPY, QUEST, PK, MSP, \
                   MXP, HOLY_LIGHT, COLOR_BLIND, SHOW_DICE_ROLLS, SHOW_IDS, \
                   NO_SUMMON, CONSENT, NO_REPEAT.",
        },
        run: cmd_toggle,
    }
}

inventory::submit! {
    Command {
        names: &["flags"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "flags",
            summary: "List your active player flags.",
            long: "Shows every flag currently set on you. Use `toggle <flag>` \
                   to flip one on or off.",
        },
        run: cmd_flags,
    }
}

inventory::submit! {
    Command {
        names: &["afk"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "afk",
            summary: "Flip your away-from-keyboard flag.",
            long: "Marks you AFK so others see the indicator on `who` and on \
                   incoming tells. Run again to come back.",
        },
        run: cmd_afk,
    }
}

inventory::submit! {
    Command {
        names: &["alias"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "alias [<name> [<command>]]",
            summary: "Define a command shortcut.",
            long: "With no args, lists every alias you've defined. With \
                   `alias <name>`, shows that alias's expansion. With \
                   `alias <name> <command>`, sets the alias — typing \
                   `<name> [args]` will be rewritten to \
                   `<command> [args]` before dispatch. Aliases persist \
                   across sessions. v1 expands once (no $1/$* yet) and \
                   the first token is replaced wholesale.",
        },
        run: cmd_alias,
    }
}

inventory::submit! {
    Command {
        names: &["unalias"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "unalias <name>",
            summary: "Remove a defined alias.",
            long: "Drops the named alias from your list. No-op if no \
                   alias by that name exists.",
        },
        run: cmd_unalias,
    }
}

inventory::submit! {
    Command {
        names: &["notell"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "notell",
            summary: "Refuse incoming `tell` messages.",
            long: "When set, other players' `tell` to you is blocked with a \
                   message. Run again to allow tells.",
        },
        run: cmd_notell,
    }
}

inventory::submit! {
    Command {
        names: &["deaf"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "deaf",
            summary: "Stop hearing room-wide channels (gossip, shout).",
            long: "When set, you no longer receive `gossip` or `shout` from \
                   other players. Run again to hear them.",
        },
        run: cmd_deaf,
    }
}

inventory::submit! {
    Command {
        names: &["color", "colour"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "color",
            summary: "Toggle ANSI color rendering for your output.",
            long: "When colors are off, XML-Lite color tags are stripped \
                   instead of rendered to ANSI. Persists for the session.",
        },
        run: cmd_color,
    }
}

inventory::submit! {
    Command {
        names: &["wimpy", "wi"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "wimpy [pct|off]",
            summary: "Set the HP percentage at which combat auto-flees you.",
            long: "`wimpy 30` enables wimpy mode and panics you out of \
                   combat when your HP drops below 30% of max. `wimpy off` \
                   (or `wimpy 0`) clears it. With no argument, prints the \
                   current setting. Default threshold when no number was \
                   set is 25%.",
        },
        run: cmd_wimpy,
    }
}

inventory::submit! {
    Command {
        names: &["autoexit"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "autoexit",
            summary: "Toggle automatic exit listing on `look`.",
            long: "When set, the room description is followed by the same \
                   line `exits` would print. Already consumed by the look \
                   path.",
        },
        run: cmd_autoexit,
    }
}

inventory::submit! {
    Command {
        names: &["autoloot"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "autoloot",
            summary: "Toggle the auto-loot flag (no behavior wired yet).",
            long: "Sets AUTO_LOOT. Once corpse loot lands, this controls \
                   whether items on slain mobs jump to your inventory \
                   automatically.",
        },
        run: cmd_autoloot,
    }
}

inventory::submit! {
    Command {
        names: &["autogold"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "autogold",
            summary: "Toggle the auto-gold flag (no behavior wired yet).",
            long: "Sets AUTO_GOLD. Once economy lands, this controls \
                   whether coins from kills jump straight to your purse.",
        },
        run: cmd_autogold,
    }
}

inventory::submit! {
    Command {
        names: &["autoassist"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "autoassist",
            summary: "Toggle the auto-assist flag (no behavior wired yet).",
            long: "Sets AUTO_ASSIST. Once group combat lands, this \
                   controls whether you automatically engage anyone \
                   attacking your group leader.",
        },
        run: cmd_autoassist,
    }
}

inventory::submit! {
    Command {
        names: &["autosplit"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "autosplit",
            summary: "Toggle the auto-split flag (no behavior wired yet).",
            long: "Sets AUTO_SPLIT. Once the group system lands, this \
                   controls whether kill rewards split automatically \
                   between group members.",
        },
        run: cmd_autosplit,
    }
}

inventory::submit! {
    Command {
        names: &["brief"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "brief",
            summary: "Toggle terse room descriptions.",
            long: "When on, `look` skips the full room description \
                   and shows just the title + exits + occupants.",
        },
        run: cmd_brief,
    }
}

inventory::submit! {
    Command {
        names: &["compact"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "compact",
            summary: "Toggle compact output (suppresses leading blank lines).",
            long: "Sets COMPACT. Renderers that respect it tighten \
                   their leading whitespace.",
        },
        run: cmd_compact,
    }
}

inventory::submit! {
    Command {
        names: &["norepeat"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "norepeat",
            summary: "Suppress consecutive duplicate output lines.",
            long: "Sets NO_REPEAT. Renderers that respect it collapse \
                   identical back-to-back lines into one.",
        },
        run: cmd_norepeat,
    }
}

inventory::submit! {
    Command {
        names: &["nosummon"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "nosummon",
            summary: "Refuse incoming summon spells.",
            long: "Sets NO_SUMMON. Once summon-class spells land, this \
                   blocks remote teleport effects targeting you.",
        },
        run: cmd_nosummon,
    }
}

inventory::submit! {
    Command {
        names: &["dice", "showdicerolls"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "dice",
            summary: "Toggle showing per-swing dice rolls in combat.",
            long: "Sets SHOW_DICE_ROLLS. Combat output surfaces hit/dmg \
                   rolls when the flag is set.",
        },
        run: cmd_dicerolls,
    }
}

inventory::submit! {
    Command {
        names: &["pk"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "pk",
            summary: "Toggle player-kill participation.",
            long: "Sets PK_ENABLED. Once the PK gate lands, this is the \
                   self-elected switch for inter-player combat.",
        },
        run: cmd_pk,
    }
}

inventory::submit! {
    Command {
        names: &["quest"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Quest,
        help: Help {
            usage: "quest",
            summary: "Toggle quest mode (placeholder; gates quest zones).",
            long: "Sets QUEST. The quest system is unimplemented; this \
                   ensures the flag persists for content that gates on it.",
        },
        run: cmd_quest_flag,
    }
}

inventory::submit! {
    Command {
        names: &["consent"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "consent",
            summary: "Toggle consent for group/share interactions.",
            long: "Sets CONSENT. Group invites and certain shared-effect \
                   spells will check this flag once those systems land.",
        },
        run: cmd_consent,
    }
}

inventory::submit! {
    Command {
        names: &["holylight"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "holylight",
            summary: "Toggle holy-light vision (admin/builder).",
            long: "Sets HOLY_LIGHT. Renderer plumbing for invisibility \
                   and darkness is pending; the flag persists so it's \
                   live the moment those land.",
        },
        run: cmd_holylight,
    }
}

inventory::submit! {
    Command {
        names: &["showids"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "showids",
            summary: "Toggle showing (zone, id) on entities you can see.",
            long: "Sets SHOW_IDS. Look/inventory renderers that want to \
                   surface coordinates check the flag.",
        },
        run: cmd_showids,
    }
}

inventory::submit! {
    Command {
        names: &["effects", "affects", "aff"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "effects",
            summary: "List active effects on yourself.",
            long: "Each line shows an effect name and its remaining \
                   duration in seconds (or 'permanent' if it has no \
                   timer).",
        },
        run: cmd_effects,
    }
}

inventory::submit! {
    Command {
        names: &["cooldowns", "cd"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "cooldowns",
            summary: "List abilities currently on cooldown.",
            long: "Each line shows an ability name and its remaining \
                   cooldown in seconds, sorted longest-first. Empty \
                   when nothing is on cooldown.",
        },
        run: cmd_cooldowns,
    }
}

inventory::submit! {
    Command {
        names: &["quit", "qu"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "quit",
            summary: "Disconnect from the game.",
            long: "Sends a goodbye message; close your client to fully \
                   disconnect.",
        },
        run: cmd_quit,
    }
}

inventory::submit! {
    Command {
        names: &["achievements", "achieve"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "achievements [<category>]",
            summary: "List your unlocked achievements (and visible-but-locked ones).",
            long: "Filter by category: combat, exploration, social, \
                   crafting, misc. Unlocked entries show with a tick; \
                   locked-but-visible ones with a placeholder. Hidden \
                   achievements stay invisible until unlocked — those \
                   are the spoiler ones.",
        },
        run: cmd_achievements,
    }
}

inventory::submit! {
    Command {
        names: &["house"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "house [info|enter|guests|rooms]",
            summary: "Inspect or enter your player house.",
            long: "Players who own a house can inspect its layout, \
                   guest list, room contents, or step inside via \
                   `house enter`. `house` (no arg) defaults to the \
                   info subcommand. Players without a house get a \
                   polite refusal — house creation isn't yet wired \
                   through the runtime.",
        },
        run: cmd_house,
    }
}

inventory::submit! {
    Command {
        names: &["train"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "train [<stat>]",
            summary: "Spend a practice point to bump a CoreStat by 1.",
            long: "With no arg, lists your current six stats and \
                   available practice points. With `train <stat>`, \
                   spends 1 point from `SkillPoints` to raise the \
                   named stat (str/dex/con/int/wis/cha) by 1. Refuses \
                   on stats already at 18 (the trainable cap), and on \
                   no points available. Persists across reconnect.",
        },
        run: cmd_train,
    }
}

inventory::submit! {
    Command {
        names: &["stand"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "stand",
            summary: "Get to your feet.",
            long: "Changes your posture to standing.",
        },
        run: cmd_stand,
    }
}

inventory::submit! {
    Command {
        names: &["sit"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "sit",
            summary: "Sit down.",
            long: "Changes your posture to sitting.",
        },
        run: cmd_sit,
    }
}

inventory::submit! {
    Command {
        names: &["kneel"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "kneel",
            summary: "Kneel — lower-profile but still alert.",
            long: "Changes your posture to kneeling. Ranks the same as \
                   sitting for ability gating, but regenerates like \
                   standing — useful for guards, prayer, or showing \
                   respect.",
        },
        run: cmd_kneel,
    }
}

inventory::submit! {
    Command {
        names: &["rest", "recline"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "rest",
            summary: "Rest in place.",
            long: "Changes your posture to resting. Slightly more relaxed \
                   than sitting; future hp/stamina regen will scale with \
                   posture.",
        },
        run: cmd_rest,
    }
}

inventory::submit! {
    Command {
        names: &["sleep"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "sleep",
            summary: "Lie down and sleep.",
            long: "Changes your posture to sleeping. Wake with `wake`, \
                   `stand`, `sit`, or `rest`.",
        },
        run: cmd_sleep,
    }
}

inventory::submit! {
    Command {
        names: &["wake"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "wake [target]",
            summary: "Wake yourself, or rouse a sleeping companion.",
            long: "With no argument, brings you out of sleep to standing. \
                   With a target, finds a sleeping player or mob in the \
                   room and stands them up; everyone in the room sees it.",
        },
        run: cmd_wake,
    }
}

inventory::submit! {
    Command {
        names: &["follow", "shadow"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "follow <name>",
            summary: "Trail another character automatically.",
            long: "When the target moves, you move with them through the \
                   same exit. `follow self` (or `unfollow`) stops \
                   following. Cycles are silently broken — you can't \
                   follow someone who is already following you.",
        },
        run: cmd_follow,
    }
}

inventory::submit! {
    Command {
        names: &["unfollow"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "unfollow",
            summary: "Stop following whoever you were following.",
            long: "No-op if you weren't following anyone.",
        },
        run: cmd_unfollow,
    }
}

inventory::submit! {
    Command {
        names: &["group"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "group",
            summary: "List your current group (follow chain).",
            long: "Shows the chain leader and every member, with HP \
                   and same-room indicator. Group membership today is \
                   informally derived from `follow` chains; an \
                   explicit invite/consent system can land later.",
        },
        run: cmd_group,
    }
}

inventory::submit! {
    Command {
        names: &["order"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "order <follower|all> <command>",
            summary: "Issue a command to one or all of your followers.",
            long: "Forwards `<command>` to a named mob follower (must \
                   be in the same room and have `Follower(you)` set), \
                   or `all` for every same-room follower at once. The \
                   mob runs the command through the normal dispatcher \
                   under its own identity, so target lookups, costs, \
                   and triggers fire as if it had typed the line. \
                   Admin commands are off-limits to mobs.",
        },
        run: cmd_order,
    }
}

inventory::submit! {
    Command {
        names: &["dismiss"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "dismiss <player>",
            summary: "Drop a single direct follower from your group.",
            long: "Equivalent to `group dismiss <player>` — removes the \
                   target's `Follower` link to you (must be following \
                   you directly, not transitively). `disband` clears \
                   everyone at once.",
        },
        run: cmd_dismiss,
    }
}

inventory::submit! {
    Command {
        names: &["split"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "split <amount>",
            summary: "Divide coin evenly among same-room group members.",
            long: "Pulls `<amount>` from your wealth and splits it \
                   evenly across every group member in your room \
                   (including you). Remainder stays with the splitter. \
                   Refuses if you're not grouped, the only group \
                   member here, or carrying less than `amount`. Coin \
                   amount is in copper; use `wealth` to check yours.",
        },
        run: cmd_split,
    }
}

inventory::submit! {
    Command {
        names: &["disband"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "disband",
            summary: "Dismiss everyone directly following you.",
            long: "Breaks the group apart at your level. Followers' \
                   own followers stay attached unless they too \
                   `disband` or `unfollow`.",
        },
        run: cmd_disband,
    }
}

inventory::submit! {
    Command {
        names: &["invite"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "invite <player>",
            summary: "Send a group invite to another player.",
            long: "Target gets a pending `GroupInvite`; they can \
                   `accept` or `decline`. Invites expire after \
                   5 minutes. Players already following you (in your \
                   group) can't be re-invited.",
        },
        run: cmd_invite,
    }
}

inventory::submit! {
    Command {
        names: &["accept"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "accept",
            summary: "Accept a pending group invite.",
            long: "Installs Follower(inviter) on you, joining their \
                   group. No-op if you have no pending invite.",
        },
        run: cmd_accept,
    }
}

inventory::submit! {
    Command {
        names: &["decline"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "decline",
            summary: "Decline a pending group invite.",
            long: "Clears the pending invite without joining. \
                   No-op if you have no pending invite.",
        },
        run: cmd_decline,
    }
}

inventory::submit! {
    Command {
        names: &["identify", "id"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "identify <item>",
            summary: "Magical analysis of a carried item.",
            long: "Reveals the item's type, weight, base value, \
                   wear slots, weapon dice (when applicable), bound \
                   abilities (scrolls/wands/staves), liquid state \
                   (drink containers), remaining charges, and any \
                   active effects on the item.",
        },
        run: cmd_identify,
    }
}


pub(crate) fn cmd_help(world: &mut World, player: Entity, args: &str) {
    let (role, perms) = world
        .get::<Account>(player)
        .map_or((UserRole::Player, Vec::new()), |a| (a.role, a.perms.clone()));

    let topic = args.trim().to_ascii_lowercase();
    if topic.is_empty() {
        let mut by_cat: HashMap<Category, Vec<&Command>> = HashMap::new();
        for cmd in all_commands() {
            if !visible(cmd, role, &perms) {
                continue;
            }
            by_cat.entry(cmd.category).or_default().push(cmd);
        }
        // Help index reads as a colored TOC: bold-cyan title, each
        // category as a bold-yellow header (matches `who`'s class
        // band hue) and command names in plain cyan so the eye
        // scans by category first, then alphabetically within.
        let mut out = String::from("\r\n<b:cyan>Available commands:</>\r\n");
        for cat in Category::ORDER {
            if let Some(cmds) = by_cat.get(cat) {
                out.push_str(&format!("\r\n  <b:yellow>{}</>\r\n", cat.label()));
                let mut names: Vec<&str> = cmds.iter().map(|c| c.names[0]).collect();
                names.sort_unstable();
                let colored: Vec<String> =
                    names.iter().map(|n| format!("<cyan>{n}</>")).collect();
                out.push_str(&format!("    {}\r\n", colored.join(", ")));
            }
        }
        out.push_str("\r\n<dim>Type `help <command>` for details.</>\r\n");
        send_to(world, player, out);
        return;
    }

    if let Some(cmd) = REGISTRY.get(topic.as_str()) {
        if !visible(cmd, role, &perms) {
            send_to(world, player, format!("<dim>No help on '{topic}'.</>\r\n"));
            return;
        }
        // Per-command page: bold-cyan command name as the title,
        // cyan section labels (Usage / Aliases / Category) so the
        // body text reads as the focal content. Usage syntax stays
        // default-color so it remains the most legible part of the
        // page; aliases / category gloss are dimmed since they're
        // reference metadata, not the example a player will copy.
        let mut out = format!("\r\n<b:cyan>{}</>\r\n", cmd.names[0]);
        out.push_str(&format!("\r\n  {}\r\n", cmd.help.summary));
        out.push_str(&format!("\r\n  <cyan>Usage:</> {}\r\n", cmd.help.usage));
        if !cmd.help.long.is_empty() {
            out.push_str(&format!("\r\n  {}\r\n", cmd.help.long));
        }
        if cmd.names.len() > 1 {
            out.push_str(&format!(
                "\r\n  <cyan>Aliases:</> <dim>{}</>\r\n",
                cmd.names[1..].join(", ")
            ));
        }
        out.push_str(&format!(
            "  <cyan>Category:</> <dim>{}</>\r\n",
            cmd.category.label()
        ));
        send_to(world, player, out);
    } else {
        // No exact match — surface visible commands whose primary
        // or alias name starts with the typed prefix. Players who
        // type "sl" usually want one of slay / sleep / slots; the
        // suggestion list saves them a second `help` round-trip.
        let mut suggestions: Vec<&'static str> = all_commands()
            .filter(|cmd| visible(cmd, role, &perms))
            .filter(|cmd| {
                cmd.names
                    .iter()
                    .any(|n: &&'static str| n.starts_with(topic.as_str()))
            })
            .map(|cmd| cmd.names[0])
            .collect();
        suggestions.sort_unstable();
        suggestions.dedup();
        if suggestions.is_empty() {
            send_to(world, player, format!("<dim>No help on '{topic}'.</>\r\n"));
        } else {
            const MAX_SUGGESTIONS: usize = 8;
            let shown: Vec<String> = suggestions
                .iter()
                .take(MAX_SUGGESTIONS)
                .map(|n| format!("<cyan>{n}</>"))
                .collect();
            let trailer = if suggestions.len() > MAX_SUGGESTIONS {
                format!(
                    " <dim>({} more)</>",
                    suggestions.len() - MAX_SUGGESTIONS
                )
            } else {
                String::new()
            };
            send_to(
                world,
                player,
                format!(
                    "<dim>No exact help for '{topic}'.</> Did you mean: {}{}?\r\n",
                    shown.join(", "),
                    trailer,
                ),
            );
        }
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_examine(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Examine whom or what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let needle = target_word.to_ascii_lowercase();

    // Dark-room gate (matches cmd_look). Self-target is allowed —
    // you can always introspect yourself even in pitch black.
    // Anything else fails until there's a light source in the room.
    if needle != "me"
        && needle != "self"
        && room_is_dark(world, room)
        && !room_has_light(world, room)
    {
        send_to(
            world,
            player,
            "It is too dark to make anything out.\r\n",
        );
        return;
    }

    // Self-target. Surfaces the same state lines as examining
    // another player would — Stealth (only visible to self anyway),
    // Flying, Mounted — so a player can confirm their state without
    // running multiple commands.
    if needle == "me" || needle == "self" {
        let name = name_of(world, player);
        let mut out = format!("\r\nYou look at yourself: {name}.\r\n");
        // Self-Description: lets the player confirm what other
        // players would see when examining them. Set with the
        // `description` command. Empty / unset → skipped silently
        // so the line doesn't add noise for players who haven't
        // bothered.
        if let Some(d) = world.get::<Description>(player)
            && !d.0.trim().is_empty()
        {
            let mode = color_mode_for(world, player);
            out.push_str(&format!(
                "{}\r\n",
                render_color_tags(d.0.trim_end(), mode),
            ));
        }
        if world.get::<mud_world::Flying>(player).is_some() {
            out.push_str("You're hovering in mid-air.\r\n");
        }
        if world.get::<Stealth>(player).is_some() {
            out.push_str("You are hidden.\r\n");
        }
        if let Some(mud_world::Mounted(mount)) = world.get::<mud_world::Mounted>(player).copied() {
            let mount_name = name_or(world, mount, "(unknown)");
            out.push_str(&format!("You're riding {mount_name}.\r\n"));
        }
        let hunger = world.get::<mud_world::Hunger>(player).map_or(0, |h| h.0);
        let thirst = world.get::<mud_world::Thirst>(player).map_or(0, |t| t.0);
        // Same active-effect query the score sheet uses, so
        // examine-self surfaces nourished/refreshed alongside
        // hungry/thirsty when condition_summary picks them up.
        let self_effects: Vec<String> = {
            let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
            q.iter(world)
                .filter(|(_, a)| a.0 == player)
                .map(|(inst, _)| inst.name.clone())
                .collect()
        };
        if let Some(c) = condition_summary(hunger, thirst, &self_effects) {
            out.push_str(&format!("You feel {c}.\r\n"));
        }
        send_to(world, player, out);
        return;
    }

    // Search the room — mobs and players are equally examinable; items too,
    // both on the ground and on the player's person.
    let target = {
        let mut q = world.query::<(Entity, &Located, &Named, Option<&Keywords>)>();
        q.iter(world)
            .find(|(e, l, n, kw)| {
                *e != player && (l.0 == room || l.0 == player) && matches(&needle, n, *kw)
            })
            .map(|(e, _, _, _)| e)
    };
    let Some(target) = target else {
        // Fall through to the room's RoomExtraDescriptions
        // before reporting "you don't see that here". Keyword
        // match is case-insensitive substring against any
        // entry's keyword list. The first match wins; builders
        // ordering things in the schema controls precedence.
        let needle_lc = needle.to_ascii_lowercase();
        if let Some(extras) = world.get::<mud_world::RoomExtras>(room) {
            let hit = extras.entries.iter().find(|(keywords, _)| {
                keywords
                    .iter()
                    .any(|kw| kw.to_ascii_lowercase().contains(&needle_lc))
            });
            if let Some((_, description)) = hit {
                let mode = color_mode_for(world, player);
                let body = render_color_tags(description.trim_end(), mode);
                send_to(world, player, format!("\r\n{body}\r\n"));
                return;
            }
        }
        // Then ObjectExtras on items in the room or in inventory.
        // Item must be visible to the player to count — same
        // located-in-room-or-on-player gate the entity search uses.
        let extras_hit: Option<String> = {
            let mut q = world
                .query_filtered::<(&Located, &WorldKey), With<Item>>();
            let protos = world.resource::<mud_world::ObjectPrototypes>();
            q.iter(world)
                .filter(|(l, _)| l.0 == room || l.0 == player)
                .find_map(|(_, key)| {
                    let proto = protos.by_key.get(&(key.zone, key.id))?;
                    proto
                        .extras
                        .iter()
                        .find(|(keywords, _)| {
                            keywords
                                .iter()
                                .any(|kw| kw.to_ascii_lowercase().contains(&needle_lc))
                        })
                        .map(|(_, body)| body.clone())
                })
        };
        if let Some(body) = extras_hit {
            let mode = color_mode_for(world, player);
            let rendered = render_color_tags(body.trim_end(), mode);
            send_to(world, player, format!("\r\n{rendered}\r\n"));
            return;
        }
        send_rendered(world, player, &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };

    let name = name_of(world, target);
    // Prefer the long-form ExamineText (mob `examine_description`)
    // over the short room-list Description. Falls back to
    // Description so mobs / objects without a separate examine
    // body still render something instead of going silent.
    let description = world
        .get::<mud_world::ExamineText>(target)
        .map(|t| t.0.clone())
        .or_else(|| world.get::<Description>(target).map(|d| d.0.clone()))
        .unwrap_or_default();
    let posture = world.get::<Posture>(target).map(|p| p.0);

    let mode = color_mode_for(world, player);
    // `name` may itself carry color tags (object names in particular).
    // The status lines that follow embed the rendered name verbatim, so
    // any trailing reset from render_color_tags terminates cleanly before
    // the literal " is sleeping here." / " is bleeding." text.
    let name_rendered = render_color_tags(&name, mode);
    let mut out = format!("\r\nYou look at {name_rendered}.\r\n");
    if !description.trim().is_empty() {
        out.push_str(&format!(
            "{}\r\n",
            render_color_tags(description.trim_end(), mode)
        ));
    }
    if let Some(p) = posture
        && p != PostureKind::Standing
    {
        out.push_str(&format!("{name_rendered} is {} here.\r\n", p.label()));
    }
    // Identity line: level + race + class for actors. Helps a
    // player gauge mob difficulty before engaging and pick the
    // right ally for a group. Pulled from Profile (players + any
    // mob with one) or the mob proto for level-only mobs.
    if let Some(prof) = world.get::<Profile>(target) {
        let class_name = prof.class_id.and_then(|cid| {
            world
                .resource::<ClassCatalog>()
                .by_id
                .get(&cid)
                .map(|c| c.plain_name.clone())
        });
        let race_label = capitalize(&prof.race);
        let class_label = class_name
            .as_deref()
            .map_or_else(String::new, |c| format!(" {c}"));
        out.push_str(&format!(
            "{name_rendered} is a level {} {race_label}{class_label}.\r\n",
            prof.level,
        ));
    } else if world.get::<Mob>(target).is_some()
        && let Some(key) = world.get::<WorldKey>(target).copied()
        && let Some(proto) = world
            .get_resource::<MobPrototypes>()
            .and_then(|p| p.by_key.get(&(key.zone, key.id)))
        && proto.level > 0
    {
        out.push_str(&format!("{name_rendered} is level {}.\r\n", proto.level));
    }
    if let Some(hp) = world.get::<Health>(target).copied() {
        out.push_str(&format!(
            "{name_rendered} {condition}.\r\n",
            condition = condition_label(hp)
        ));
    }
    if world.get::<Shopkeeper>(target).is_some() {
        out.push_str(&format!(
            "{name_rendered} is a merchant — try `list` to see their wares.\r\n"
        ));
    }
    // Surface non-shopkeeper professions on examine so players
    // know whom to talk to. Looks up the proto via WorldKey.
    if world.get::<Mob>(target).is_some()
        && let Some(key) = world.get::<WorldKey>(target).copied()
        && let Some(proto) = world
            .get_resource::<MobPrototypes>()
            .and_then(|p| p.by_key.get(&(key.zone, key.id)))
    {
        for prof in &proto.professions {
            let line = match prof {
                mud_db::enums::MobProfession::Banker => {
                    Some("a banker — try `deposit` / `withdraw`.")
                }
                mud_db::enums::MobProfession::Trainer => {
                    Some("a trainer — try `train` / `practice <ability>`.")
                }
                mud_db::enums::MobProfession::Postmaster => {
                    Some("a postmaster — try `mail <name>`.")
                }
                mud_db::enums::MobProfession::Receptionist => {
                    Some("a receptionist — they handle lodging.")
                }
                mud_db::enums::MobProfession::Guildmaster => {
                    Some("a guildmaster — manages guild services.")
                }
                // Shopkeeper already announced via the marker above.
                mud_db::enums::MobProfession::Shopkeeper => None,
            };
            if let Some(line) = line {
                out.push_str(&format!("{name_rendered} is {line}\r\n"));
            }
        }
    }
    if world.get::<mud_world::Flying>(target).is_some() {
        out.push_str(&format!("{name_rendered} hovers in mid-air.\r\n"));
    }
    if let Some(mud_world::Mounted(mount)) = world.get::<mud_world::Mounted>(target).copied() {
        let mount_name = name_or(world, mount, "(unknown)");
        out.push_str(&format!("{name_rendered} is riding {mount_name}.\r\n"));
    }
    if let Some(mud_world::RiddenBy(rider)) = world.get::<mud_world::RiddenBy>(target).copied() {
        let rider_name = name_or(world, rider, "(unknown)");
        out.push_str(&format!("{rider_name} is riding {name_rendered}.\r\n"));
    }
    if world.get::<Stealth>(target).is_some() && target == player {
        // Self-only — others shouldn't see your stealth marker.
        out.push_str("You are hidden.\r\n");
    }
    if let Some(BoardLink(board_id)) = world.get::<BoardLink>(target).copied()
        && let Some(summary) = world
            .get_resource::<BoardCatalog>()
            .and_then(|c| c.by_id.get(&board_id))
            .cloned()
    {
        let lock = if summary.locked { " (locked)" } else { "" };
        // Many board titles already end in "Board"; avoid the awkward
        // "Mortal Board board".
        let title_lc = summary.title.to_ascii_lowercase();
        let suffix = if title_lc.ends_with(" board") || title_lc.ends_with("boards") {
            ""
        } else {
            " board"
        };
        out.push_str(&format!(
            "It's the {}{}{}; type `board {}` to read it.\r\n",
            summary.title, suffix, lock, summary.alias,
        ));
    }
    // Freshness cue for corpses — same thresholds the decay tick uses
    // for its in-room atmospheric broadcasts. Players walking in late
    // need a way to read the corpse's state without waiting for the
    // next milestone tick.
    if let Some(decay) = world.get::<mud_world::CorpseDecay>(target).copied() {
        let line = match decay.remaining_secs {
            i32::MIN..=30 => "It is on the verge of dissolution.",
            31..=120 => "It reeks; flies and grubs are everywhere.",
            121..=300 => "Flies have gathered; it is no longer fresh.",
            _ => "It is still warm.",
        };
        out.push_str(&format!("{line}\r\n"));
    }
    // Active effects on actors (Player or Mob). Quick "is this mob
    // blessed / bleeding?" read without needing your own `effects`
    // command (which is self-only). Items skip this — their effects
    // are bound differently.
    if world.get::<Item>(target).is_none() {
        let names: Vec<String> = {
            let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
            q.iter(world)
                .filter(|(_, a)| a.0 == target)
                .map(|(inst, _)| inst.name.clone())
                .collect()
        };
        if !names.is_empty() {
            out.push_str(&format!(
                "{name_rendered} is affected by: {}.\r\n",
                names.join(", "),
            ));
        }
    }
    // If the target is an Item-typed container (corpse, bag, chest, ...),
    // list anything Located on it. Mirrors the legacy "you peek inside"
    // behavior — looters don't have to guess what to `get`.
    if world.get::<Item>(target).is_some() {
        // Surface item weight so a player can judge whether to pick
        // it up before bumping into the encumbrance gate. Skipped
        // for synthetic items that have no proto weight.
        let weight = item_weight(world, target);
        if weight > 0.0 {
            out.push_str(&format!("It weighs about {weight:.1} lbs.\r\n"));
        }
        // Wearable slot. Tells a player "this fits on the head" /
        // "this is wielded" without making them try `wear it` and
        // see where it lands. Skipped silently for non-wearable
        // items (most consumables, decorations).
        if let Some(slot) = world.get::<WearableIn>(target).map(|w| w.0) {
            let verb = match slot {
                Slot::Wield => "wielded",
                Slot::Hold => "held",
                _ => "worn",
            };
            out.push_str(&format!("It is {verb} on the {}.\r\n", slot.label()));
        }
        let contents: Vec<String> = {
            let mut q = world.query_filtered::<(&Located, &Named), With<Item>>();
            q.iter(world)
                .filter(|(l, _)| l.0 == target)
                .map(|(_, n)| n.name.clone())
                .collect()
        };
        if !contents.is_empty() {
            out.push_str(&format!("\r\n{name_rendered} contains:\r\n"));
            for item_name in contents {
                let rendered = render_color_tags(&item_name, mode);
                out.push_str(&format!("  {rendered}\r\n"));
            }
        }
    }
    send_to(world, player, out);
}

pub(crate) fn cmd_title(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        let cur = world.get::<Title>(player).map(|t| t.0.clone());
        let line = match cur {
            Some(t) => format!("Your title: {t}\r\n"),
            None => "You have no title set. Use `title <new>` to add one.\r\n".to_string(),
        };
        send_to(world, player, line);
        return;
    }
    if matches!(arg.to_ascii_lowercase().as_str(), "clear" | "none" | "-") {
        if let Ok(mut e) = world.get_entity_mut(player) {
            e.remove::<Title>();
        }
        send_to(world, player, "Title cleared.\r\n");
        return;
    }
    if arg.len() > MAX_TITLE_LEN {
        send_to(
            world,
            player,
            format!("Title too long (max {MAX_TITLE_LEN} chars).\r\n"),
        );
        return;
    }
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(Title(arg.to_string()));
    }
    send_to(world, player, format!("Title set to: {arg}\r\n"));
}

pub(crate) fn cmd_description(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        let cur = world.get::<Description>(player).map(|d| d.0.clone());
        let line = match cur {
            Some(d) if !d.trim().is_empty() => format!("Your description:\r\n{d}\r\n"),
            _ => "You have no description set. Use `description <prose>`.\r\n".to_string(),
        };
        send_to(world, player, line);
        return;
    }
    if matches!(arg.to_ascii_lowercase().as_str(), "clear" | "none" | "-") {
        if let Ok(mut e) = world.get_entity_mut(player) {
            e.remove::<Description>();
        }
        send_to(world, player, "Description cleared.\r\n");
        return;
    }
    if arg.len() > MAX_DESCRIPTION_LEN {
        send_to(
            world,
            player,
            format!("Description too long (max {MAX_DESCRIPTION_LEN} chars).\r\n"),
        );
        return;
    }
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(Description(arg.to_string()));
    }
    send_to(world, player, "Description set.\r\n");
}

/// `experience` / `exp` / `xp`: print level and total XP from Profile.
/// Standalone readout for the same numbers `score` already shows; the
/// loose level→required-XP table will join later.
pub(crate) fn cmd_experience(world: &mut World, player: Entity, _args: &str) {
    let Some(p) = world.get::<Profile>(player).cloned() else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    send_to(
        world,
        player,
        format!("\r\nLevel {}    Experience: {}\r\n", p.level, p.experience),
    );
}

/// `wealth` / `gold` / `money`: split the on-hand copper total into
/// platinum/gold/silver/copper denominations. The schema's per-race
/// `copperFactor` is reserved for shop/trade math; raw display uses
/// the standard 100/10/1 ratio (1 platinum = 10 gold = 100 silver =
/// 1000 copper) so the four-coin breakdown matches `FieryMUD`'s score
/// sheet. Zero-value coins are skipped; "no coin" prints when broke.
pub(crate) fn cmd_wealth(world: &mut World, player: Entity, _args: &str) {
    let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);
    let bank = world.get::<mud_world::BankWealth>(player).map_or(0, |b| b.0);
    let main_line = if let Some(parts) = format_wealth(on_hand) {
        format!("You have {parts}.")
    } else {
        "You have no coin to your name.".to_string()
    };
    // Surface the bank balance as a footnote so a player who
    // cashed out earlier can see their saved stash without
    // typing `balance`. Suppressed when the bank is empty so
    // the readout stays a one-liner for unbanked players.
    let mut out = format!("\r\n{main_line}\r\n");
    if bank > 0
        && let Some(parts) = format_wealth(bank)
    {
        out.push_str(&format!("(Bank: {parts}.)\r\n"));
    }
    send_to(world, player, out);
}

/// `bribe <amount> <target>`: transfer copper to a mob and fire
/// its BRIBE triggers. Refuses on insufficient funds, missing
/// target, or self-target.
pub(crate) fn cmd_bribe(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[1].trim().is_empty() {
        send_to(world, player, "Usage: bribe <amount> <target>\r\n");
        return;
    }
    let Ok(amount) = parts[0].trim().parse::<i64>() else {
        send_to(world, player, "Amount must be a positive integer.\r\n");
        return;
    };
    if amount <= 0 {
        send_to(world, player, "Amount must be positive.\r\n");
        return;
    }
    let target_word = parts[1].trim();
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let Some(target) = find_actor_in_room(world, target_word, located.0, player) else {
        send_rendered(
            world,
            player,
            &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    let funds = world.get::<Wealth>(player).map_or(0, |w| w.0);
    if funds < amount {
        send_to(world, player, "You don't have that much coin.\r\n");
        return;
    }
    if let Some(mut w) = world.get_mut::<Wealth>(player) {
        w.0 -= amount;
    }
    if let Some(mut w) = world.get_mut::<Wealth>(target) {
        w.0 += amount;
    } else {
        world.entity_mut(target).insert(Wealth(amount));
    }
    let target_name = name_of(world, target);
    send_rendered(
        world,
        player,
        &format!("You hand {amount} copper to {target_name}.\r\n"),
    );
    send_rendered(
        world,
        target,
        &format!(
            "{} bribes you with {amount} copper.\r\n",
            name_of(world, player)
        ),
    );
    // Fire BRIBE on target with the amount as a Lua extras global.
    let amount_str = amount.to_string();
    let to_fire: Vec<(i32, i32, String, String)> = {
        if let Some(at) = world.get::<AttachedTriggers>(target) {
            let keys = at.0.clone();
            let catalog = world.resource::<TriggerCatalog>();
            keys.into_iter()
                .filter_map(|(z, i)| {
                    let def = catalog.by_key.get(&(z, i))?;
                    if def.flags.contains(&mud_world::TriggerEvent::Bribe) {
                        Some((z, i, def.name.clone(), def.commands.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            Vec::new()
        }
    };
    for (zone, id, _name, body) in to_fire {
        let _ = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
            host.exec_for_listener_with_extras(
                world,
                target,
                player,
                &body,
                &[("amount", &amount_str)],
            )
        });
        crate::commands::drain_lua_outbox(world);
        let _ = (zone, id); // referenced for the closure capture only
    }
}

/// `list`: find a shopkeeper in the player's room and dump the catalog
/// as `# | item | price | stock`. Stock `unlimited` for `-1`. Price
/// falls back to the proto's base `cost * buy_profit` when the row's
/// override is `0`. No-op when no shopkeeper present.
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_list(world: &mut World, player: Entity, _args: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let keeper: Option<(Entity, Shopkeeper)> = {
        let mut q = world.query_filtered::<(Entity, &Located, &Shopkeeper), With<Mob>>();
        q.iter(world)
            .find(|(_, l, _)| l.0 == located.0)
            .map(|(e, _, s)| (e, *s))
    };
    let Some((keeper_entity, keeper_marker)) = keeper else {
        send_to(world, player, "No one here is selling anything.\r\n");
        return;
    };
    let keeper_name = name_of(world, keeper_entity);
    let shop_def = world
        .resource::<ShopCatalog>()
        .by_key
        .get(&(keeper_marker.shop_zone_id, keeper_marker.shop_id))
        .cloned();
    let Some(shop) = shop_def else {
        send_to(
            world,
            player,
            format!(
                "{keeper_name} fumbles, looking for the inventory ledger... but it's blank.\r\n"
            ),
        );
        return;
    };
    let buy_profit = shop.buy_profit;
    let object_protos = world.resource::<ObjectPrototypes>().by_key.clone();
    let mob_protos = world.resource::<MobPrototypes>().by_key.clone();

    let mut out = String::new();
    if !shop.items.is_empty() {
        out.push_str(&format!("\r\n{keeper_name} offers:\r\n"));
        out.push_str(&format!(
            "  {:<3} {:<40} {:<28} {}\r\n",
            "#", "Item", "Price", "Stock"
        ));
        for (i, offer) in shop.items.iter().enumerate() {
            let proto = object_protos.get(&(offer.object_zone_id, offer.object_id));
            let item_name = proto.map_or_else(
                || format!("(missing {}/{})", offer.object_zone_id, offer.object_id),
                |p| p.name.clone(),
            );
            let base_cost = proto.map_or(0, |p| p.cost);
            let price_copper = shop_offer_price(offer, base_cost, buy_profit);
            let price_str = format_wealth(price_copper).unwrap_or_else(|| "free".to_string());
            let stock_str = if offer.amount < 0 {
                "unlimited".to_string()
            } else {
                offer.amount.to_string()
            };
            out.push_str(&format!(
                "  {:<3} {:<40} {:<28} {}\r\n",
                i + 1,
                item_name,
                price_str,
                stock_str
            ));
        }
    }
    if !shop.pets.is_empty() {
        out.push_str(&format!("\r\n{keeper_name} also has pets for hire:\r\n"));
        out.push_str(&format!(
            "  {:<3} {:<40} {:<28} {}\r\n",
            "#", "Mob", "Price", "Stock"
        ));
        for (i, offer) in shop.pets.iter().enumerate() {
            let proto = mob_protos.get(&(offer.mob_zone_id, offer.mob_id));
            let mob_name = proto.map_or_else(
                || format!("(missing {}/{})", offer.mob_zone_id, offer.mob_id),
                |p| p.name.clone(),
            );
            // Pet price: override wins; else mob.level * 100 (legacy
            // CircleMUD convention).
            let price_copper: i64 = if offer.price > 0 {
                i64::from(offer.price)
            } else {
                proto.map_or(0, |p| i64::from(p.level) * 100)
            };
            let price_str = format_wealth(price_copper).unwrap_or_else(|| "free".to_string());
            let stock_str = if offer.amount < 0 {
                "unlimited".to_string()
            } else {
                offer.amount.to_string()
            };
            out.push_str(&format!(
                "  {:<3} {:<40} {:<28} {}\r\n",
                i + 1,
                mob_name,
                price_str,
                stock_str
            ));
        }
        out.push_str("\r\nUse `hire <#|name>` to hire one as a pet.\r\n");
    }
    if shop.items.is_empty() && shop.pets.is_empty() {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} has nothing to sell right now.\r\n"),
        );
        return;
    }
    // Footer: surface the player's on-hand coin so they can
    // gauge what's in reach without round-tripping through
    // `wealth`. Skipped when the player has nothing — the
    // empty-pockets case is implied by the lack of a footer.
    let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);
    if let Some(coin) = format_wealth(on_hand) {
        out.push_str(&format!("\r\nYou have {coin}.\r\n"));
    }
    send_rendered(world, player, &out);
}

/// `buy <#|name>`: purchase an item from the shopkeeper in the room.
/// Argument is either a 1-based catalog index or a substring of the
/// item's name. Deducts coin from `Wealth`; spawns the item directly
/// into the player's inventory. Stock is advisory only — the catalog
/// resource is not mutated, so unlimited / 0 / N entries all sell.
/// (Real stock decrement waits on per-shop instance state.)
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_buy(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Buy what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let keeper: Option<(Entity, Shopkeeper)> = {
        let mut q = world.query_filtered::<(Entity, &Located, &Shopkeeper), With<Mob>>();
        q.iter(world)
            .find(|(_, l, _)| l.0 == located.0)
            .map(|(e, _, s)| (e, *s))
    };
    let Some((keeper_entity, keeper_marker)) = keeper else {
        send_to(world, player, "No one here is selling anything.\r\n");
        return;
    };
    let keeper_name = name_of(world, keeper_entity);
    let Some(shop) = world
        .resource::<ShopCatalog>()
        .by_key
        .get(&(keeper_marker.shop_zone_id, keeper_marker.shop_id))
        .cloned()
    else {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} has nothing to sell.\r\n"),
        );
        return;
    };
    let object_protos = world.resource::<ObjectPrototypes>().by_key.clone();
    // Parse: integer = 1-based index; otherwise substring match on proto name.
    let offer_idx: Option<usize> = if let Ok(n) = arg.parse::<usize>() {
        if n == 0 || n > shop.items.len() {
            None
        } else {
            Some(n - 1)
        }
    } else {
        let lc = arg.to_ascii_lowercase();
        shop.items.iter().position(|o| {
            object_protos
                .get(&(o.object_zone_id, o.object_id))
                .is_some_and(|p| p.name.to_ascii_lowercase().contains(&lc))
        })
    };
    let Some(idx) = offer_idx else {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} doesn't sell '{arg}'.\r\n"),
        );
        return;
    };
    let offer = shop.items[idx];
    if offer.amount == 0 {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} is out of those.\r\n"),
        );
        return;
    }
    let Some(proto) = object_protos.get(&(offer.object_zone_id, offer.object_id)).cloned() else {
        send_to(world, player, "That item's prototype is missing.\r\n");
        return;
    };
    let price_copper = shop_offer_price(&offer, proto.cost, shop.buy_profit);
    let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);
    if on_hand < price_copper {
        let need = price_copper - on_hand;
        let need_msg = format_wealth(need).unwrap_or_else(|| "more coin".to_string());
        send_rendered(
            world,
            player,
            &format!(
                "{keeper_name} eyes you. \"You need {need_msg} more for that.\"\r\n"
            ),
        );
        return;
    }
    // Deduct coin, decrement stock (if finite), and spawn the item.
    if let Some(mut w) = world.get_mut::<Wealth>(player) {
        w.0 = w.0.saturating_sub(price_copper);
    }
    if offer.amount > 0
        && let Some(def) = world
            .resource_mut::<ShopCatalog>()
            .by_key
            .get_mut(&(keeper_marker.shop_zone_id, keeper_marker.shop_id))
        && let Some(off) = def.items.get_mut(idx)
    {
        off.amount = (off.amount - 1).max(0);
    }
    let primary_slot = mud_world::wear_flags_primary_slot(&proto.wear_flags);
    let mut bundle = world.spawn((
        Item,
        Named { name: proto.name.clone() },
        Keywords(proto.keywords.clone()),
        WorldKey { zone: proto.zone_id, id: proto.id },
        Located(player),
    ));
    if let Some(desc) = proto.examine_description.clone() {
        bundle.insert(Description(desc));
    }
    if let Some(s) = primary_slot {
        bundle.insert(WearableIn(s));
    }
    if let Some(liq) = proto.liquid.clone() {
        bundle.insert(mud_world::LiquidContainer {
            liquid: liq.liquid,
            capacity: liq.capacity,
            remaining: liq.remaining,
            poisoned: liq.poisoned,
        });
    }
    if let Some(fuel) = proto.light_fuel {
        bundle.insert(mud_world::LightFuel {
            capacity: fuel.capacity,
            remaining: fuel.remaining,
        });
    }
    let price_str = format_wealth(price_copper).unwrap_or_else(|| "free".to_string());
    let item_name = proto.name.clone();
    send_rendered(
        world,
        player,
        &format!("You buy {item_name} for {price_str}.\r\n"),
    );
    let player_name = name_of(world, player);
    broadcast_room_except_rendered(
        world,
        located.0,
        &[player],
        &format!("{player_name} buys {item_name}.\r\n"),
    );
}

/// `hire <#|name>`: hire a pet from a pet-shop keeper. Spawns a fresh
/// mob from the keeper's `ShopMobs` offerings, ties it as a follower
/// of the player, and tags its name with the player's possessive
/// (`AdminChar's wolf`). Cost is the offer's `price` or
/// `mob.level * 100` when 0.
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_hire(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Hire what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let keeper: Option<(Entity, Shopkeeper)> = {
        let mut q = world.query_filtered::<(Entity, &Located, &Shopkeeper), With<Mob>>();
        q.iter(world)
            .find(|(_, l, _)| l.0 == located.0)
            .map(|(e, _, s)| (e, *s))
    };
    let Some((keeper_entity, keeper_marker)) = keeper else {
        send_to(world, player, "No one here is hiring out pets.\r\n");
        return;
    };
    let keeper_name = name_of(world, keeper_entity);
    let Some(shop) = world
        .resource::<ShopCatalog>()
        .by_key
        .get(&(keeper_marker.shop_zone_id, keeper_marker.shop_id))
        .cloned()
    else {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} has nothing to hire.\r\n"),
        );
        return;
    };
    if shop.pets.is_empty() {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} doesn't deal in pets.\r\n"),
        );
        return;
    }
    let mob_protos = world.resource::<MobPrototypes>().by_key.clone();
    let offer_idx: Option<usize> = if let Ok(n) = arg.parse::<usize>() {
        if n == 0 || n > shop.pets.len() {
            None
        } else {
            Some(n - 1)
        }
    } else {
        let lc = arg.to_ascii_lowercase();
        shop.pets.iter().position(|o| {
            mob_protos
                .get(&(o.mob_zone_id, o.mob_id))
                .is_some_and(|p| p.name.to_ascii_lowercase().contains(&lc))
        })
    };
    let Some(idx) = offer_idx else {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} doesn't have '{arg}' for hire.\r\n"),
        );
        return;
    };
    let offer = shop.pets[idx];
    if offer.amount == 0 {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} is out of those.\r\n"),
        );
        return;
    }
    let Some(proto) = mob_protos.get(&(offer.mob_zone_id, offer.mob_id)).cloned() else {
        send_to(world, player, "That mob's prototype is missing.\r\n");
        return;
    };
    let price_copper: i64 = if offer.price > 0 {
        i64::from(offer.price)
    } else {
        i64::from(proto.level) * 100
    };
    let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);
    if on_hand < price_copper {
        let need = price_copper - on_hand;
        let need_msg = format_wealth(need).unwrap_or_else(|| "more coin".to_string());
        send_rendered(
            world,
            player,
            &format!(
                "{keeper_name} eyes you. \"You need {need_msg} more for that.\"\r\n"
            ),
        );
        return;
    }
    if let Some(mut w) = world.get_mut::<Wealth>(player) {
        w.0 = w.0.saturating_sub(price_copper);
    }
    if offer.amount > 0
        && let Some(def) = world
            .resource_mut::<ShopCatalog>()
            .by_key
            .get_mut(&(keeper_marker.shop_zone_id, keeper_marker.shop_id))
        && let Some(off) = def.pets.get_mut(idx)
    {
        off.amount = (off.amount - 1).max(0);
    }
    // Spawn the pet as a fresh mob attached as a Follower(player).
    // Name is renamed to "<player>'s <mob_name>" so room listings
    // disambiguate from wild mobs of the same proto.
    let player_name = name_of(world, player);
    let pet_name = format!("{player_name}'s {}", proto.name);
    let hp = proto.rolled_hp();
    let dmg = proto.avg_damage();
    let pet_entity = world
        .spawn((
            Mob,
            Named { name: pet_name.clone() },
            Keywords(proto.keywords.clone()),
            Description(proto.room_description.clone()),
            WorldKey { zone: proto.zone_id, id: proto.id },
            Located(located.0),
            Health { hp, max: hp },
            CombatStats {
                hit_roll: proto.hit_roll,
                dmg_roll: dmg,
                ac: proto.armor_class,
                alignment: proto.alignment,
            },
            Posture(PostureKind::Standing),
            Follower(player),
        ))
        .id();
    if !proto.examine_description.trim().is_empty()
        && let Ok(mut em) = world.get_entity_mut(pet_entity)
    {
        em.insert(mud_world::ExamineText(proto.examine_description.clone()));
    }
    let _ = pet_entity;
    let price_str = format_wealth(price_copper).unwrap_or_else(|| "free".to_string());
    send_rendered(
        world,
        player,
        &format!("You hire {} for {price_str}.\r\n", proto.name),
    );
    broadcast_room_except_rendered(
        world,
        located.0,
        &[player],
        &format!("{player_name} hires {}.\r\n", proto.name),
    );
}

/// `sell <item>`: hand a carried item to the shopkeeper here, get coin.
/// Pays `proto.cost * sell_profit` rounded; despawns the item; adds
/// the coin to the player's `Wealth`. Refuses on equipped items
/// (`remove` first), zero-value items, rooms without a keeper, and
/// items the keeper's `ShopAccepts` rules reject.
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_sell(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Sell what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let keeper: Option<(Entity, Shopkeeper)> = {
        let mut q = world.query_filtered::<(Entity, &Located, &Shopkeeper), With<Mob>>();
        q.iter(world)
            .find(|(_, l, _)| l.0 == located.0)
            .map(|(e, _, s)| (e, *s))
    };
    let Some((keeper_entity, keeper_marker)) = keeper else {
        send_to(world, player, "No one here is buying anything.\r\n");
        return;
    };
    let keeper_name = name_of(world, keeper_entity);
    let Some(item) = find_carried_by(world, target_word, player, EquipFilter::Inventory) else {
        send_rendered(
            world,
            player,
            &format!("You aren't carrying '{target_word}'.\r\n"),
        );
        return;
    };
    let item_name = name_of(world, item);
    let Some(shop) = world
        .resource::<ShopCatalog>()
        .by_key
        .get(&(keeper_marker.shop_zone_id, keeper_marker.shop_id))
        .cloned()
    else {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} isn't running a shop.\r\n"),
        );
        return;
    };
    let item_proto = world
        .get::<WorldKey>(item)
        .and_then(|k| {
            world
                .resource::<ObjectPrototypes>()
                .by_key
                .get(&(k.zone, k.id))
                .cloned()
        });
    let base_cost = item_proto.as_ref().map_or(0, |p| p.cost);
    // Sell-side filter: if `accepts` is empty, anything goes; otherwise
    // the item must match at least one rule. Type tokens normalize to
    // upper + no-underscore on both sides so DRINK_CONTAINER matches
    // the schema's DRINKCONTAINER. Keyword filter is empty = no extra
    // gate; non-empty = at least one keyword must appear in the
    // item's `Keywords`.
    if !shop.accepts.is_empty() {
        let item_type_norm = item_proto
            .as_ref()
            .map(|p| object_type_token(p.r#type))
            .unwrap_or_default();
        let item_kws: Vec<String> = world
            .get::<Keywords>(item)
            .map(|k| {
                k.0.iter()
                    .map(|s| s.to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let accepted = shop.accepts.iter().any(|rule| {
            let rule_type = rule.object_type.replace('_', "").to_ascii_uppercase();
            if rule_type != item_type_norm {
                return false;
            }
            if rule.keywords.is_empty() {
                return true;
            }
            rule.keywords
                .iter()
                .any(|k| item_kws.iter().any(|ik| ik.contains(&k.to_ascii_lowercase())))
        });
        if !accepted {
            send_rendered(
                world,
                player,
                &format!("{keeper_name} isn't interested in {item_name}.\r\n"),
            );
            return;
        }
    }
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pay_copper: i64 = (f64::from(base_cost) * shop.sell_profit).round() as i64;
    if pay_copper <= 0 {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} chuckles. \"That's worthless to me.\"\r\n"),
        );
        return;
    }
    if let Some(mut w) = world.get_mut::<Wealth>(player) {
        w.0 = w.0.saturating_add(pay_copper);
    } else {
        try_insert(world, player, Wealth(pay_copper));
    }
    if let Ok(e) = world.get_entity_mut(item) {
        e.despawn();
    }
    let pay_str = format_wealth(pay_copper).unwrap_or_else(|| "no coin".to_string());
    send_rendered(
        world,
        player,
        &format!("You sell {item_name} for {pay_str}.\r\n"),
    );
    let player_name = name_of(world, player);
    broadcast_room_except_rendered(
        world,
        located.0,
        &[player],
        &format!("{player_name} sells {item_name}.\r\n"),
    );
}

/// `value <item>`: appraise an item against its proto's `cost`. Renders
/// the raw value in denominations. Real shop sell-price math (some
/// fraction, race-specific copperFactor, durability modifier) lands
/// with the shop system; this is the bare informational version.
pub(crate) fn cmd_value(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Value what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let lc = needle.to_ascii_lowercase();
    let target = {
        let mut q =
            world.query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
        q.iter(world)
            .find(|(_, l, n, kw)| {
                (l.0 == player || l.0 == located.0) && matches(&lc, n, *kw)
            })
            .map(|(e, _, _, _)| e)
    };
    let Some(target) = target else {
        send_to(
            world,
            player,
            format!("You can't find anything called '{needle}' to value.\r\n"),
        );
        return;
    };
    let proto = world
        .get::<WorldKey>(target)
        .and_then(|k| {
            world
                .resource::<ObjectPrototypes>()
                .by_key
                .get(&(k.zone, k.id))
                .cloned()
        });
    let item_name = name_of(world, target);
    let mut msg = if let Some(p) = &proto {
        if let Some(parts) = format_wealth(i64::from(p.cost)) {
            format!("{item_name} is worth {parts}.\r\n")
        } else {
            format!("{item_name} is worthless.\r\n")
        }
    } else {
        format!("{item_name} has no proto data; treat as worthless.\r\n")
    };
    if let Some(p) = &proto {
        // Weight + level give the player the rest of the
        // is-this-worth-carrying picture without a separate
        // examine. Skip the level line at level 0 (the proto
        // default) since "minimum level: 0" reads as noise.
        if p.weight > 0.0 {
            msg.push_str(&format!("  Weight: {:.1} lbs.\r\n", p.weight));
        }
        if p.level > 0 {
            msg.push_str(&format!("  Minimum level: {}\r\n", p.level));
        }
    }
    send_rendered(world, player, &msg);
}

/// `deposit <amount>`: move on-hand copper into the bank. Refuses
/// when the player doesn't have enough on hand. v1 is location-
/// agnostic — any room works, since banker-mob detection isn't
/// wired yet (it'll gate via `MobProfession::Banker` once that's
/// hydrated). `save_player` persists both balances.
pub(crate) fn cmd_deposit(world: &mut World, player: Entity, args: &str) {
    bank_transfer(world, player, args, "deposit");
}

/// `withdraw <amount>`: pull copper from the bank back on-hand.
/// Mirrors `deposit` with the opposite sign / refusal text.
pub(crate) fn cmd_withdraw(world: &mut World, player: Entity, args: &str) {
    bank_transfer(world, player, args, "withdraw");
}

/// `practice` / `prac`: with no arg, list `KnownAbilities` with
/// proficiency rendered as a tier label. With an ability name,
/// raise that ability's proficiency by 5 (capped at the class's
/// `proficiency_cap` from `ClassAbilities`).
pub(crate) fn cmd_practice(world: &mut World, player: Entity, args: &str) {
    let trimmed = args.trim();
    if !trimmed.is_empty() {
        // Spending a practice point requires a Trainer (matches
        // Guildmasters who tag both — guildmasters teach class
        // abilities). Listing (no-arg path) stays anywhere.
        if !require_profession_in_room(
            world,
            player,
            mud_db::enums::MobProfession::Trainer,
            "trainer",
        ) {
            return;
        }
        return practice_one(world, player, trimmed);
    }
    let known = world
        .get::<KnownAbilities>(player)
        .map(|k| k.entries.clone())
        .unwrap_or_default();
    let points = world
        .get::<mud_world::SkillPoints>(player)
        .map_or(0, |s| s.0);
    if known.is_empty() {
        send_to(
            world,
            player,
            format!("\r\nYou haven't trained any abilities yet. ({points} practice point(s) available.)\r\n"),
        );
        return;
    }
    let catalog = world.resource::<AbilityCatalog>();
    let class_id = world.get::<Profile>(player).and_then(|p| p.class_id);
    let spell_caps = world.resource::<mud_world::SpellSlotData>();
    let skill_caps = world.resource::<mud_world::ClassSkillsData>();
    let mut rows: Vec<(String, String, i32, bool, Option<i32>)> = Vec::with_capacity(known.len());
    for (id, prof, learned) in &known {
        let def = catalog.by_name.values().find(|d| d.id == *id);
        let name = def.map_or_else(|| format!("ability #{id}"), |d| d.plain_name.clone());
        let kind = def.map_or("?", |d| match d.kind {
            mud_db::abilities::AbilityKind::Skill => "skill",
            mud_db::abilities::AbilityKind::Spell => "spell",
            mud_db::abilities::AbilityKind::Song => "song",
            mud_db::abilities::AbilityKind::Chant => "chant",
        });
        // Per-class proficiency cap (where modeled). Skills go
        // through ClassSkillsData; spells/chants/songs go through
        // SpellSlotData. None when the player is classless or the
        // ability isn't on the class's sheet.
        let cap = class_id.and_then(|cid| {
            if matches!(def.map(|d| d.kind), Some(mud_db::abilities::AbilityKind::Skill)) {
                skill_caps.proficiency_cap.get(&(cid, *id)).copied()
            } else {
                spell_caps.ability_cap.get(&(cid, *id)).copied()
            }
        });
        rows.push((name, kind.to_string(), *prof, *learned, cap));
    }
    rows.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut out = format!("\r\nKnown abilities ({}):\r\n", rows.len());
    for (name, kind, prof, learned, cap) in &rows {
        // Proficiency 0-1000 in schema; render as 0-100% with a tier
        // label that legacy MUDs use.
        let pct = (*prof / 10).clamp(0, 100);
        let tier = match pct {
            0 => "untrained",
            1..=25 => "novice",
            26..=50 => "apprentice",
            51..=75 => "skilled",
            76..=99 => "expert",
            _ => "master",
        };
        let learn_mark = if *learned { " " } else { "*" };
        let cap_label = cap.map_or(String::new(), |c| format!(" / {c}"));
        out.push_str(&format!(
            "  {learn_mark}{kind:<8} {name:<24} {pct:>3}%{cap_label} ({tier})\r\n"
        ));
    }
    out.push_str("\r\n* = learning (not yet mastered).\r\n");
    out.push_str(&format!("Practice points: {points}\r\n"));
    send_to(world, player, out);
}

pub(crate) fn cmd_train(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim().to_ascii_lowercase();
    // Stat-up requires a trainer mob; reading the stat list does
    // not (so a no-arg `train` works as a self-check anywhere).
    if !arg.is_empty()
        && !require_profession_in_room(
            world,
            player,
            mud_db::enums::MobProfession::Trainer,
            "trainer",
        )
    {
        return;
    }
    let stats = world
        .get::<CoreStats>(player)
        .copied()
        .unwrap_or_default();
    let points = world
        .get::<mud_world::SkillPoints>(player)
        .map_or(0, |s| s.0);
    if arg.is_empty() {
        // Show each stat with its derived bonus so a player can see
        // what training a stat would actually do for their rolls
        // (a 13 → 14 bump still gives +2 bonus; 14 → 15 unlocks +3
        // at the next odd-step boundary). Same `(stat - 10) / 2`
        // formula the score sheet uses via `CoreStats::bonus`.
        let mut out = format!("\r\nCurrent stats (cap {TRAIN_STAT_CAP}):\r\n");
        let pair = |val: i32| format!("{val:>2}({:+})", CoreStats::bonus(val));
        out.push_str(&format!(
            "  str {}   dex {}   con {}\r\n",
            pair(stats.strength),
            pair(stats.dexterity),
            pair(stats.constitution),
        ));
        out.push_str(&format!(
            "  int {}   wis {}   cha {}\r\n",
            pair(stats.intelligence),
            pair(stats.wisdom),
            pair(stats.charisma),
        ));
        out.push_str(&format!("Practice points: {points}\r\n"));
        out.push_str("Use `train <stat>` to spend one.\r\n");
        send_to(world, player, out);
        return;
    }

    let (label, current) = match arg.as_str() {
        "str" | "strength" => ("strength", stats.strength),
        "dex" | "dexterity" => ("dexterity", stats.dexterity),
        "con" | "constitution" => ("constitution", stats.constitution),
        "int" | "intelligence" => ("intelligence", stats.intelligence),
        "wis" | "wisdom" => ("wisdom", stats.wisdom),
        "cha" | "charisma" => ("charisma", stats.charisma),
        _ => {
            send_to(
                world,
                player,
                "Train which stat? str / dex / con / int / wis / cha.\r\n",
            );
            return;
        }
    };
    if current >= TRAIN_STAT_CAP {
        send_to(
            world,
            player,
            format!(
                "Your {label} is at the trainable cap of {TRAIN_STAT_CAP}.\r\n"
            ),
        );
        return;
    }
    if points <= 0 {
        send_to(
            world,
            player,
            "You have no practice points to spend. Earn more by leveling up.\r\n",
        );
        return;
    }
    if let Some(mut s) = world.get_mut::<CoreStats>(player) {
        match arg.as_str() {
            "str" | "strength" => s.strength += 1,
            "dex" | "dexterity" => s.dexterity += 1,
            "con" | "constitution" => s.constitution += 1,
            "int" | "intelligence" => s.intelligence += 1,
            "wis" | "wisdom" => s.wisdom += 1,
            "cha" | "charisma" => s.charisma += 1,
            _ => unreachable!(),
        }
    }
    if let Some(mut sp) = world.get_mut::<mud_world::SkillPoints>(player) {
        sp.0 -= 1;
    }
    let remaining = world
        .get::<mud_world::SkillPoints>(player)
        .map_or(0, |s| s.0);
    send_to(
        world,
        player,
        format!(
            "You train your {label} — now {new}. ({remaining} practice point(s) remaining.)\r\n",
            new = current + 1,
        ),
    );
}

/// `track <target>` / `hunt <target>`: BFS through open exits up to
/// 50 rooms looking for a player or mob whose name matches.
/// Reports the first direction to head and the distance.
/// Closed/locked exits block the scan; flying / hidden mobs are
/// matched normally (no perception roll yet).
pub(crate) fn cmd_track(world: &mut World, player: Entity, args: &str) {
    use std::collections::{HashSet, VecDeque};
    const MAX_DEPTH: i32 = 50;
    let needle = args.trim().to_ascii_lowercase();
    if needle.is_empty() {
        send_to(world, player, "Track whom?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let start = located.0;

    // Collect every room->target candidate in one pass: any entity
    // with Named matching the needle (excluding the player), keyed
    // by their current room. Then BFS rooms until we hit one in the
    // candidate map.
    let candidate_rooms: HashSet<Entity> = {
        let mut q = world.query::<(Entity, &Located, &Named, Option<&Keywords>)>();
        q.iter(world)
            .filter(|(e, _, n, kw)| {
                *e != player
                    && (n.name.to_ascii_lowercase().contains(&needle)
                        || kw.is_some_and(|k| {
                            k.0.iter().any(|w| w.to_ascii_lowercase().contains(&needle))
                        }))
            })
            .map(|(_, l, _, _)| l.0)
            .collect()
    };
    if candidate_rooms.is_empty() {
        send_rendered(
            world,
            player,
            &format!("You sense no trace of '{needle}' nearby.\r\n"),
        );
        return;
    }
    if candidate_rooms.contains(&start) {
        send_rendered(
            world,
            player,
            &format!("You see '{needle}' right here.\r\n"),
        );
        return;
    }

    // BFS: queue carries (room, first_direction_taken, distance).
    // Hidden exits the player hasn't found via `search` are
    // skipped on both frontier edges — otherwise track would
    // direct the player toward a passage they can't see / walk,
    // which would expose the secret indirectly.
    let mut visited: HashSet<Entity> = HashSet::new();
    visited.insert(start);
    let mut queue: VecDeque<(Entity, Direction, i32)> = VecDeque::new();
    if let Some(exits) = world.get::<Exits>(start) {
        for (dir, ed) in &exits.0 {
            if ed.state != ExitState::Open {
                continue;
            }
            if exit_is_hidden_to(world, player, start, *dir, ed) {
                continue;
            }
            let Some(to) = ed.to else { continue };
            if visited.insert(to) {
                queue.push_back((to, *dir, 1));
            }
        }
    }

    while let Some((room, first_dir, dist)) = queue.pop_front() {
        if candidate_rooms.contains(&room) {
            send_rendered(
                world,
                player,
                &format!(
                    "You catch a trail leading {} ({} room{} away).\r\n",
                    direction_name(first_dir),
                    dist,
                    if dist == 1 { "" } else { "s" },
                ),
            );
            return;
        }
        if dist >= MAX_DEPTH {
            continue;
        }
        if let Some(exits) = world.get::<Exits>(room) {
            for (dir, ed) in &exits.0 {
                if ed.state != ExitState::Open {
                    continue;
                }
                if exit_is_hidden_to(world, player, room, *dir, ed) {
                    continue;
                }
                let Some(to) = ed.to else { continue };
                if visited.insert(to) {
                    queue.push_back((to, first_dir, dist + 1));
                }
            }
        }
    }
    send_rendered(
        world,
        player,
        &format!("'{needle}' is too far away to track.\r\n"),
    );
}

/// `scan`: walk this room's exits and print one line per direction
/// with the target room's name plus mob/player counts. Closed and
/// locked exits print state instead of contents — you can see the
/// door but not what's behind it.
pub(crate) fn cmd_scan(world: &mut World, player: Entity, _args: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(exits) = world.get::<Exits>(located.0).cloned() else {
        send_to(world, player, "No exits to scan.\r\n");
        return;
    };
    if exits.0.is_empty() {
        send_to(world, player, "No exits to scan.\r\n");
        return;
    }
    // Hidden exits stay invisible to scan as well — same reveal
    // contract as look / `exits` / movement, honoring the
    // per-player RevealedExits override.
    let mut entries: Vec<(Direction, ExitData)> = exits
        .0
        .into_iter()
        .filter(|(d, ed)| !exit_is_hidden_to(world, player, located.0, *d, ed))
        .collect();
    if entries.is_empty() {
        send_to(world, player, "No exits to scan.\r\n");
        return;
    }
    entries.sort_by_key(|(d, _)| direction_rank(*d));

    let mut out = String::from("\r\n");
    for (dir, ed) in &entries {
        let dir_label = direction_name(*dir);
        if ed.state != ExitState::Open {
            out.push_str(&format!(
                "  {dir_label:>9}: <{:?}>\r\n",
                ed.state,
            ));
            continue;
        }
        let Some(target_room) = ed.to else {
            out.push_str(&format!("  {dir_label:>9}: (dangling)\r\n"));
            continue;
        };
        let target_name = name_or(world, target_room, "(unknown)");
        let mob_count = world
            .query_filtered::<&Located, With<Mob>>()
            .iter(world)
            .filter(|l| l.0 == target_room)
            .count();
        let player_count = world
            .query_filtered::<&Located, (With<Player>, With<Online>)>()
            .iter(world)
            .filter(|l| l.0 == target_room)
            .count();
        out.push_str(&format!(
            "  {dir_label:>9}: {target_name}  ({mob_count}m {player_count}p)\r\n"
        ));
    }
    send_to(world, player, out);
}

inventory::submit! {
    Command {
        names: &["search"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "search",
            summary: "Search the area for hidden exits.",
            long: "Refuses while fighting. Inspects every exit on \
                   the current room and reveals any tagged HIDDEN \
                   that you haven't already found. Reveals are \
                   per-character and session-scoped — a fresh \
                   login starts back at zero known hidden exits, \
                   matching the legacy contract until a persistent \
                   table for revealed exits lands. Pair with \
                   `exits` afterward to confirm what surfaced.",
        },
        run: cmd_search,
    }
}

/// `search`: scan the current room for hidden exits and add any
/// found to the player's `RevealedExits` set. Today every hidden
/// exit reveals on first search — no perception roll yet — but
/// the per-(`room`, `direction`) reveal granularity is already
/// in place so a difficulty model can layer in without breaking
/// the rendering contract.
pub(crate) fn cmd_search(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You're too busy fighting to search!\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere to search.\r\n");
        return;
    };
    let room = located.0;
    let player_name = name_of(world, player);
    send_to(world, player, "You search the area carefully...\r\n");
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} searches the area.\r\n"),
    );

    let hidden_dirs: Vec<Direction> = world
        .get::<Exits>(room)
        .map(|e| {
            e.0.iter()
                .filter(|(_, ed)| ed.is_hidden)
                .map(|(d, _)| *d)
                .collect()
        })
        .unwrap_or_default();
    if hidden_dirs.is_empty() {
        send_to(world, player, "You find nothing of interest.\r\n");
        return;
    }
    let already: std::collections::HashSet<(Entity, Direction)> = world
        .get::<RevealedExits>(player)
        .map(|r| r.set.clone())
        .unwrap_or_default();
    let newly: Vec<Direction> = hidden_dirs
        .into_iter()
        .filter(|d| !already.contains(&(room, *d)))
        .collect();
    if newly.is_empty() {
        send_to(world, player, "You find nothing new.\r\n");
        return;
    }
    let mut next = already;
    for dir in &newly {
        next.insert((room, *dir));
    }
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(RevealedExits { set: next });
    }
    let mut out = String::new();
    for dir in &newly {
        out.push_str(&format!(
            "You discover a hidden exit to the {}!\r\n",
            direction_name(*dir),
        ));
    }
    send_rendered(world, player, &out);
}

inventory::submit! {
    Command {
        names: &["diagnose"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "diagnose [<target>]",
            summary: "Verbose health readout for self or a target in your room.",
            long: "With no arg, reports your own condition band (e.g. \
                   `mortally wounded`), your raw HP / Stamina, and your \
                   posture. With a target, reports their condition band \
                   and posture only — exact numbers are private. Pair with \
                   `glance` for a one-line summary or `score` for the full \
                   sheet.",
        },
        run: cmd_diagnose,
    }
}

/// `diagnose [<target>]`: descriptive HP readout. Mirrors the
/// legacy C++ `diagnose` command — uses the same six-band
/// `condition_label` `glance` does. Self target also surfaces raw
/// HP / Stamina numbers; targets stay private (other players /
/// mobs only get the descriptive band, no exact values).
pub(crate) fn cmd_diagnose(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    let (target, is_self) = if target_word.is_empty() {
        (player, true)
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere; can't diagnose.\r\n");
            return;
        };
        let Some(t) = find_actor_in_room(world, target_word, located.0, player)
        else {
            send_to(
                world,
                player,
                format!("You don't see '{target_word}' here.\r\n"),
            );
            return;
        };
        (t, t == player)
    };
    let cond = world
        .get::<Health>(target)
        .copied()
        .map_or("looks fine", condition_label);
    let posture = world
        .get::<Posture>(target)
        .map_or("standing", |p| p.0.label());
    let mut out = String::from("\r\n");
    if is_self {
        out.push_str(&format!("You diagnose yourself:\r\n  You {cond}.\r\n"));
        if let Some(hp) = world.get::<Health>(player).copied() {
            out.push_str(&format!("  HP: {} / {}\r\n", hp.hp, hp.max));
        }
        if let Some(s) = world.get::<Stamina>(player).copied() {
            out.push_str(&format!("  Stamina: {} / {}\r\n", s.current, s.max));
        }
        out.push_str(&format!("  You are currently {posture}.\r\n"));
    } else {
        let name = name_of(world, target);
        out.push_str(&format!("You diagnose {name}:\r\n  {name} {cond}.\r\n"));
        out.push_str(&format!("  {name} is currently {posture}.\r\n"));
    }
    send_rendered(world, player, &out);
}

inventory::submit! {
    Command {
        names: &["whoami"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "whoami",
            summary: "Print your character's name, level, race, and class.",
            long: "One-line answer to \"which character am I logged in \
                   as?\" — handy after a long session or when a script \
                   needs to confirm identity. Pair with `score` for the \
                   full sheet.",
        },
        run: cmd_whoami,
    }
}

/// `whoami`: one-line "I am Strider the Wanderer, level 25 Human
/// Wizard". Shows name + epithet + level + race + class. Mirrors
/// the legacy convention of letting players confirm identity
/// without scrolling through `score`.
pub(crate) fn cmd_whoami(world: &mut World, player: Entity, _args: &str) {
    let name = name_of(world, player);
    let title_suffix = world
        .get::<Title>(player)
        .map(|t| t.0.clone())
        .filter(|s| !s.is_empty())
        .map_or_else(String::new, |t| format!(" {t}"));
    let line = if let Some(prof) = world.get::<Profile>(player) {
        let race = capitalize(&prof.race);
        let class = prof
            .class_id
            .and_then(|id| {
                world
                    .get_resource::<ClassCatalog>()
                    .and_then(|c| c.by_id.get(&id))
                    .map(|d| d.plain_name.clone())
            })
            .unwrap_or_else(|| "Classless".to_string());
        format!(
            "You are {name}{title_suffix}, level {} {race} {class}.\r\n",
            prof.level,
        )
    } else {
        format!("You are {name}{title_suffix}.\r\n")
    };
    send_rendered(world, player, &line);
}

/// One-line snapshot: name + posture + HP condition + current target.
/// Useful for a quick teammate / enemy check without the wall of text
/// from `examine`.
pub(crate) fn cmd_glance(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Glance at whom?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; can't glance.\r\n");
        return;
    };
    let Some(target) = find_actor_in_room(world, target_word, located.0, player) else {
        send_to(world, player, format!("You don't see '{target_word}' here.\r\n"));
        return;
    };
    let name = name_of(world, target);
    let hp = world.get::<Health>(target).copied();
    let cond = hp.map_or("looks fine", condition_label);
    // Wrap the condition phrase in vital-graded color so a glance
    // at a wounded target reads red without forcing the player to
    // parse the words. None → render plain (target is fine enough).
    let cond_open = hp.and_then(|h| vital_color_tag(h.hp, h.max));
    let cond_text = cond_open.map_or_else(
        || cond.to_string(),
        |open| format!("{open}{cond}</>"),
    );
    let posture = world
        .get::<Posture>(target)
        .map_or("standing", |p| p.0.label());
    let fighting = world
        .get::<Fighting>(target)
        .map(|f| name_or(world, f.0, "(gone)"));
    // Posture parenthetical dims (it's framing, not the headline);
    // the fighting clause is the alarm — bold-red so an in-progress
    // brawl is obvious at a glance.
    let mut line = format!(
        "\r\n<b:cyan>{name}</> <dim>({posture})</> {cond_text}"
    );
    if let Some(target_name) = fighting {
        line.push_str(&format!(" <b:red>— fighting {target_name}</>"));
    }
    line.push_str(".\r\n");
    send_rendered(world, player, &line);
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_look(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if !arg.is_empty() {
        if let Some(dir) = parse_direction(arg) {
            look_direction(world, player, dir);
            return;
        }
        // `look at sky` / `look stars` / `look horizon`: roll up the
        // scattered weather/time/season readouts into one line. Done
        // before the examine fallthrough so it works even though
        // there's no `sky` entity to find.
        let lower = arg.to_ascii_lowercase();
        let stripped = lower.strip_prefix("at ").unwrap_or(&lower);
        if matches!(stripped, "sky" | "stars" | "horizon" | "heavens") {
            look_at_sky(world, player);
            return;
        }
        // Anything else: fall through to examine (look <object>).
        cmd_examine(world, player, arg);
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;

    // Dark-room gate: caves, underdark, underwater, and outdoor
    // rooms at night print only "It is pitch black..." plus exits
    // (AUTO_EXIT) — unless someone in the room carries a Lit item.
    // Players with the AUTO_LIGHT class trait could bypass later;
    // for now, a held torch / staff / luminous gem suffices.
    if room_is_dark(world, room) && !room_has_light(world, room) {
        let mut out = String::from("\r\nIt is pitch black; you can see nothing.\r\n");
        if has_flag(world, player, PlayerFlag::AutoExit) {
            // Hidden exits stay out of the auto-listing even in
            // pitch-black unless this player has already found
            // them via `search`.
            let exits: Vec<Direction> = world
                .get::<Exits>(room)
                .map(|e| {
                    e.0.iter()
                        .filter(|(d, ed)| {
                            !exit_is_hidden_to(world, player, room, **d, ed)
                        })
                        .map(|(d, _)| *d)
                        .collect()
                })
                .unwrap_or_default();
            if !exits.is_empty() {
                let mode = color_mode_for(world, player);
                let header = render_color_tags("<cyan>Exits:</>", mode);
                let names: Vec<&str> = exits.iter().map(|d| direction_name(*d)).collect();
                out.push_str(&format!("{header} {}\r\n", names.join(", ")));
            }
        }
        send_to(world, player, out);
        return;
    }

    let room_name = name_or(world, room, "(nowhere)");
    let room_desc = world
        .get::<Description>(room)
        .map(|d| d.0.clone())
        .unwrap_or_default();
    // Carry the (direction, state) pair so the auto-exit line can
    // tag closed/locked doors. Sorted later for stable output.
    // Hidden exits stay out of the listing unless this player has
    // already found them via `search`.
    let exits: Vec<(Direction, ExitState)> = world
        .get::<Exits>(room)
        .map(|e| {
            e.0.iter()
                .filter(|(d, ed)| !exit_is_hidden_to(world, player, room, **d, ed))
                .map(|(d, ed)| (*d, ed.state))
                .collect()
        })
        .unwrap_or_default();

    // Players in the room — names go in "Also here:". Non-standing players
    // get a posture annotation.
    let other_players: Vec<String> = {
        let mut q = world
            .query_filtered::<(Entity, &Located, &Named, Option<&Posture>), With<Player>>();
        q.iter(world)
            .filter(|(e, l, _, _)| *e != player && l.0 == room)
            .map(|(_, _, n, posture)| {
                let p = posture.map_or(PostureKind::Standing, |p| p.0);
                if p == PostureKind::Standing {
                    n.name.clone()
                } else {
                    format!("{} (is {} here)", n.name, p.label())
                }
            })
            .collect()
    };
    // Mobs — each gets their own line with their room_description, falling
    // back to the name if Description is missing or empty. Aggressive
    // mobs (alignment past `AGGRO_ALIGNMENT`) get a `<red>(HOSTILE)</>`
    // suffix so a careful look reveals what `consider` would and the
    // auto-engage rule will land. Non-mob entities skip it.
    // Mob lines, grouped so identical proto-instances stack into a
    // `(N) <description>` entry. The body is the rendered text we
    // would print for one instance; identical bodies are
    // collapsed. `(HOSTILE)` is part of the body, so two mobs with
    // the same proto but different alignments (rare) won't merge.
    // First-seen ordering is preserved so the room reads in the
    // same order it always did.
    let mob_lines: Vec<String> = {
        let aggro_threshold = aggro_alignment(world);
        let mut order: Vec<String> = Vec::new();
        let mut counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut q = world
            .query_filtered::<(&Located, &Named, Option<&Description>, Option<&CombatStats>), With<Mob>>();
        for (_, n, desc, stats) in q.iter(world).filter(|(l, _, _, _)| l.0 == room) {
            let body = desc
                .filter(|d| !d.0.trim().is_empty())
                .map_or_else(|| n.name.clone(), |d| d.0.trim_end().to_string());
            let line = if stats.is_some_and(|s| s.alignment <= aggro_threshold) {
                format!("{body} <red>(HOSTILE)</>")
            } else {
                body
            };
            if !counts.contains_key(&line) {
                order.push(line.clone());
            }
            *counts.entry(line).or_insert(0) += 1;
        }
        order
            .into_iter()
            .map(|line| {
                let n = counts.get(&line).copied().unwrap_or(1);
                if n > 1 {
                    format!("<dim>({n})</> {line}")
                } else {
                    line
                }
            })
            .collect()
    };
    // Items on the ground, count-grouped the same way as mob lines
    // so a pile of identical coins / arrows / corpses doesn't show
    // up as ten copies of `a copper coin` separated by commas.
    // Pattern matches `cmd_inventory`.
    let items: Vec<String> = {
        let mut order: Vec<String> = Vec::new();
        let mut counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut q = world.query_filtered::<(&Located, &Named), With<Item>>();
        for (_, n) in q.iter(world).filter(|(l, _)| l.0 == room) {
            if !counts.contains_key(&n.name) {
                order.push(n.name.clone());
            }
            *counts.entry(n.name.clone()).or_insert(0) += 1;
        }
        order
            .into_iter()
            .map(|name| {
                let n = counts.get(&name).copied().unwrap_or(1);
                if n > 1 {
                    format!("<dim>({n})</> {name}")
                } else {
                    name
                }
            })
            .collect()
    };

    let mode = color_mode_for(world, player);
    let mut out = String::new();
    // Append a `[peaceful]` tag to the room title when the room is
    // marked `Room.is_peaceful` — combat is refused here, so a
    // visible cue saves the player from typing `attack` and
    // wondering why nothing happened.
    let peaceful_tag =
        if world.get::<mud_world::PeacefulRoom>(room).is_some() {
            "  <green>[peaceful]</>"
        } else {
            ""
        };
    // Colorize plain room names in cyan as the default. Authored
    // names that carry their own XML-Lite tags (~23 rooms in the
    // imported world) keep their builder-set color.
    let titled_room_name = colorize_default(&room_name, "<b:cyan>");
    out.push_str(&format!(
        "\r\n{}{}\r\n",
        render_color_tags(&titled_room_name, mode),
        render_color_tags(peaceful_tag, mode),
    ));
    // BRIEF flag suppresses the description — name/occupants/exits only.
    // CircleMUD-standard "brief mode".
    if !has_flag(world, player, PlayerFlag::Brief) && !room_desc.trim().is_empty() {
        out.push_str(&format!(
            "{}\r\n",
            render_color_tags(room_desc.trim_end(), mode)
        ));
    }
    // Weather hint for outdoor rooms — drawn from the per-zone live
    // WeatherCatalog. Skipped for STRUCTURE / CAVE / UNDERWATER /
    // UNDERDARK / planes where the sky isn't visible. BRIEF mode
    // also suppresses to keep the terse output truly terse.
    if !has_flag(world, player, PlayerFlag::Brief)
        && let Some(sector) = world.get::<RoomSector>(room).map(|s| s.0)
        && sector_is_outdoor_for_weather(sector)
        && let Some(zone_id) = world.get::<WorldKey>(room).map(|k| k.zone)
        && let Some(state) = world
            .resource::<mud_world::WeatherCatalog>()
            .by_zone
            .get(&zone_id)
            .copied()
    {
        out.push_str(&format!("{}\r\n", crate::weather::describe(state)));
    }
    for line in &mob_lines {
        out.push_str(&format!("{}\r\n", render_color_tags(line, mode)));
    }
    if !other_players.is_empty() {
        let rendered: Vec<String> = other_players
            .iter()
            .map(|p| render_color_tags(p, mode))
            .collect();
        let header = render_color_tags("<cyan>Also here:</>", mode);
        out.push_str(&format!("{header} {}\r\n", rendered.join(", ")));
    }
    if !items.is_empty() {
        let rendered: Vec<String> = items
            .iter()
            .map(|i| render_color_tags(i, mode))
            .collect();
        let header = render_color_tags("<cyan>On the ground:</>", mode);
        out.push_str(&format!("{header} {}\r\n", rendered.join(", ")));
    }
    // Auto-exits: only render the exits line on look when the player has the
    // AUTO_EXIT flag set. Without it, the room shows clean and the player
    // types `exits` (or peeks with `look <dir>`) on demand. Classic CircleMUD
    // semantics — kept opt-in to avoid clutter. Each direction is colored by
    // door state — open=green, closed=yellow, locked=red — so the player
    // notices a barrier before they try to walk through.
    if has_flag(world, player, PlayerFlag::AutoExit) {
        let header = render_color_tags("<cyan>Exits:</>", mode);
        if exits.is_empty() {
            out.push_str(&format!("{header} none\r\n"));
        } else {
            let names: Vec<String> = exits
                .iter()
                .map(|(d, state)| {
                    let open = exit_state_color(*state);
                    let suffix = match state {
                        ExitState::Open => "",
                        ExitState::Closed => "[C]",
                        ExitState::Locked => "[L]",
                    };
                    let raw = format!("{open}{}{suffix}</>", direction_name(*d));
                    render_color_tags(&raw, mode)
                })
                .collect();
            out.push_str(&format!("{header} {}\r\n", names.join(", ")));
        }
    }
    send_to(world, player, out);
}

/// Parse `who`'s optional level-range args. `""` → None (no
/// filter). `"50"` → Some(50, i32::MAX). `"1 50"` → Some(1, 50)
/// (always in ascending order, so `who 50 1` is normalised to
/// 1..=50). Non-numeric args silently fall back to None — the
/// help text is the canonical reference for users who care.
fn parse_who_level_filter(args: &str) -> Option<(i32, i32)> {
    let mut nums = args
        .split_whitespace()
        .filter_map(|t| t.parse::<i32>().ok());
    let lo = nums.next()?;
    let hi = nums.next();
    let (lo, hi) = match hi {
        Some(h) => (lo.min(h), lo.max(h)),
        None => (lo, i32::MAX),
    };
    Some((lo, hi))
}

pub(crate) fn cmd_who(world: &mut World, player: Entity, args: &str) {
    // Width-aware columns: pad the name to NAME_COL visible chars
    // (skipping XML-Lite color tags via pad_visible) so titles and
    // flags line up across players regardless of name length or
    // colors. NAME_COL covers the canonical Characters.name limit.
    const NAME_COL: usize = 20;
    // Parse optional level filter args. `who` shows everyone;
    // `who N` shows level >= N; `who LO HI` shows the inclusive
    // [LO, HI] range. Garbage args fall through to "show all"
    // rather than printing an error — the help text is the gate
    // for users who care to read it.
    let level_filter: Option<(i32, i32)> = parse_who_level_filter(args);
    // `who clan <abbrev>` is a parallel filter mode. Captured as
    // the lowercased abbreviation when present so the row filter
    // can match against `clan_abbrev` case-insensitively. Empty
    // string after `clan ` is treated as a no-op (no filter)
    // rather than silently matching nothing — same charity rule
    // as garbage level args.
    let clan_filter: Option<String> = args
        .split_whitespace()
        .next()
        .filter(|head| head.eq_ignore_ascii_case("clan"))
        .and_then(|_| args.split_whitespace().nth(1))
        .filter(|abbrev| !abbrev.is_empty())
        .map(str::to_ascii_lowercase);
    // Two-pass: first collect rows, then resolve group roots so we
    // can mark grouped players with [G].
    let raw: Vec<WhoRow> = {
        // Snapshot the catalog once outside the query so it doesn't
        // collide with the query's borrow on World. Class is
        // looked up by Profile.class_id and rendered as plain_name
        // (no color tags — color sneaks in via the title).
        // `name` (with builder color tags) — `plain_name` was the
        // pre-color shape. Class-tag color in the who list comes
        // from this; the render path strips tags for clients that
        // don't support color so plain telnet still reads cleanly.
        let class_lookup: std::collections::HashMap<i32, String> = world
            .resource::<ClassCatalog>()
            .by_id
            .iter()
            .map(|(id, def)| (*id, def.name.clone()))
            .collect();
        let mut q = world.query_filtered::<(
            Entity,
            &Named,
            Option<&Title>,
            Option<&PlayerFlags>,
            Option<&LastInputAt>,
            Option<&Profile>,
            Option<&mud_world::ClanMembership>,
        ), (With<Player>, With<Online>)>();
        q.iter(world)
            .map(|(e, n, t, f, last, prof, clan)| WhoRow {
                entity: e,
                name: n.name.clone(),
                title: t.map(|t| t.0.clone()),
                afk: f.is_some_and(|pf| pf.has(PlayerFlag::Afk)),
                idle: last.map(|l| l.0.elapsed().as_secs()),
                level: prof.map_or(0, |p| p.level),
                clan_abbrev: clan.map(|c| c.clan_abbrev.clone()),
                class_name: prof
                    .and_then(|p| p.class_id)
                    .and_then(|cid| class_lookup.get(&cid).cloned()),
            })
            .collect()
    };
    // Per-entity group root, so grouped players can be marked. A
    // non-singleton group root means the entity has at least one
    // groupmate.
    let mut roots: std::collections::HashMap<Entity, Entity> =
        std::collections::HashMap::with_capacity(raw.len());
    for r in &raw {
        roots.insert(r.entity, group_root(world, r.entity));
    }
    let mut group_size: std::collections::HashMap<Entity, usize> = std::collections::HashMap::new();
    for root in roots.values() {
        *group_size.entry(*root).or_insert(0) += 1;
    }

    // Filter by level range when args narrowed the view.
    let total_online = raw.len();
    let raw_filtered: Vec<WhoRow> = if let Some(needle) = &clan_filter {
        // Clan abbreviations are stored as authored (typically
        // upper-case) but match case-insensitively here so a
        // player typing the lowercase form still gets results.
        raw.into_iter()
            .filter(|r| {
                r.clan_abbrev
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(needle))
            })
            .collect()
    } else if let Some((lo, hi)) = level_filter {
        raw.into_iter()
            .filter(|r| r.level >= lo && r.level <= hi)
            .collect()
    } else {
        raw
    };
    let header = if let Some(needle) = &clan_filter {
        format!(
            "\r\n<b:cyan>{} of {} online</> (clan <b:yellow>{needle}</>):\r\n",
            raw_filtered.len(),
            total_online,
        )
    } else if let Some((lo, hi)) = level_filter {
        if lo == hi {
            format!(
                "\r\n<b:cyan>{} of {} online</> (level {lo}):\r\n",
                raw_filtered.len(),
                total_online,
            )
        } else {
            format!(
                "\r\n<b:cyan>{} of {} online</> (levels {lo}-{hi}):\r\n",
                raw_filtered.len(),
                total_online,
            )
        }
    } else {
        format!("\r\n<b:cyan>{total_online} online</>:\r\n")
    };
    let mut out = header;
    // Sort by level desc so endgame players surface first; same-
    // level players sort alphabetically for stable output.
    let mut raw_sorted = raw_filtered;
    raw_sorted.sort_by(|a, b| b.level.cmp(&a.level).then_with(|| a.name.cmp(&b.name)));
    for r in &raw_sorted {
        let root = roots.get(&r.entity).copied().unwrap_or(r.entity);
        let in_group = group_size.get(&root).copied().unwrap_or(0) > 1;
        out.push_str("  ");
        if r.level > 0 {
            // Level tag colored by progression band — newbie
            // yellow / mid green / endgame cyan / staff magenta.
            // Lets the player scan the list and spot peers + staff
            // at a glance.
            let lvl_color = who_level_color(r.level);
            let lvl_label = match lvl_color {
                Some(open) => format!("{open}[L{:>3}]</>", r.level),
                None => format!("[L{:>3}]", r.level),
            };
            out.push_str(&format!("{lvl_label} "));
        } else {
            out.push_str("       ");
        }
        out.push_str(&pad_visible(&r.name, NAME_COL));
        if let Some(class) = &r.class_name {
            // class is the catalog `name` field (carries authored
            // color); render-time mapping turns the tags into ANSI.
            out.push_str(&format!(" [{class}]"));
        }
        if let Some(abbrev) = &r.clan_abbrev {
            out.push_str(&format!(" [<b:yellow>{abbrev}</>]"));
        }
        if let Some(t) = &r.title {
            out.push(' ');
            out.push_str(t);
        }
        if in_group {
            out.push_str(" [<b:green>G</>]");
        }
        if r.afk {
            out.push_str(" [<yellow>AFK</>]");
        }
        if let Some(secs) = r.idle
            && secs >= 60
        {
            let idle_label = match idle_color(secs) {
                Some(open) => format!("[{open}idle {}</>]", format_idle(secs)),
                None => format!("[idle {}]", format_idle(secs)),
            };
            out.push_str(&format!(" {idle_label}"));
        }
        out.push_str("\r\n");
    }
    // Player titles can contain XML-Lite color tags; render before
    // sending so they show as ANSI rather than literal markup.
    send_rendered(world, player, &out);
}

pub(crate) fn cmd_idle(world: &mut World, player: Entity, args: &str) {
    // Optional minimum-idle filter in minutes. Lenient parser:
    // non-numeric args fall back to "no filter" — matches `who`'s
    // arg-handling shape so the two info commands feel consistent.
    let min_idle_secs: Option<u64> = args
        .split_whitespace()
        .find_map(|t| t.parse::<u64>().ok())
        .map(|m| m.saturating_mul(60));
    let mut rows: Vec<(String, Option<u64>, Option<u64>)> = {
        let mut q = world.query_filtered::<(
            &Named,
            Option<&LastInputAt>,
            Option<&LoggedInAt>,
        ), (With<Player>, With<Online>)>();
        q.iter(world)
            .map(|(n, last, login)| {
                (
                    n.name.clone(),
                    last.map(|l| l.0.elapsed().as_secs()),
                    login.map(|l| l.0.elapsed().as_secs()),
                )
            })
            .collect()
    };
    let total_online = rows.len();
    if let Some(threshold) = min_idle_secs {
        rows.retain(|(_, idle, _)| idle.is_some_and(|s| s >= threshold));
    }
    // Highest idle first; fresh-never-typed go to the bottom.
    rows.sort_by(|a, b| match (a.1, b.1) {
        (Some(x), Some(y)) => y.cmp(&x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.0.cmp(&b.0),
    });
    let mut out = if let Some(threshold) = min_idle_secs {
        format!(
            "\r\n<b:cyan>{} of {} online idle ≥ {}</>:\r\n",
            rows.len(),
            total_online,
            format_idle(threshold),
        )
    } else {
        format!("\r\n<b:cyan>{total_online} online by idle</>:\r\n")
    };
    out.push_str("  <cyan>Name                     Idle      Online</>\r\n");
    for (name, idle, online) in &rows {
        // Idle column: plain "active" for <60s sessions, gray
        // "fresh" for never-typed, otherwise color-graded by band
        // (cyan / yellow / red as the session goes stale).
        let idle_label = match idle {
            None => "<dim>fresh</>".to_string(),
            Some(s) if *s < 60 => "active".to_string(),
            Some(s) => match idle_color(*s) {
                Some(open) => format!("{open}{}</>", format_idle(*s)),
                None => format_idle(*s),
            },
        };
        let online_label = online.map_or_else(|| "?".to_string(), format_idle);
        // pad_visible counts visible chars (skipping XML-Lite tags)
        // so columns stay aligned even when names contain `<red>...</>`.
        let padded_name = pad_visible(name, 24);
        let padded_idle = pad_visible(&idle_label, 9);
        out.push_str(&format!(
            "  {padded_name} {padded_idle} {online_label}\r\n"
        ));
    }
    send_rendered(world, player, &out);
}

pub(crate) fn cmd_score(world: &mut World, player: Entity, _args: &str) {
    let name = name_of(world, player);
    let hp = world.get::<Health>(player).copied();
    let stamina = world.get::<Stamina>(player).copied();
    let cs = world.get::<CombatStats>(player).copied();
    let fighting = world.get::<Fighting>(player).copied();
    let posture = world.get::<Posture>(player).copied();
    let fight_target_name = fighting.map(|f| name_or(world, f.0, "(gone)"));
    let style = world.get::<UiStyle>(player).copied().unwrap_or_default();
    // Profile + class catalog lookup: resolve the display name once here so
    // renderers stay pure (no &World access). Uses `plain_name` (no color
    // tags) so the fixed-width fancy box aligns correctly; once a visible-
    // width-aware writer lands, this can switch to the colored `name`.
    let profile_owned: Option<(i32, String, String, String, i32)> =
        world.get::<Profile>(player).map(|prof| {
            let class_label = prof
                .class_id
                .and_then(|id| {
                    world
                        .get_resource::<ClassCatalog>()
                        .and_then(|c| c.by_id.get(&id))
                        .map(|d| d.plain_name.clone())
                })
                .unwrap_or_else(|| String::from("Classless"));
            (
                prof.level,
                class_label,
                prof.race.clone(),
                prof.gender.clone(),
                prof.experience,
            )
        });
    let core_stats = world.get::<CoreStats>(player).copied();
    // Active effect names — compact list for the score sheet.
    // Detail (duration + source) stays in `cmd_effects`. Filter
    // by AppliedTo so we only get the player's own effects.
    let active_effects: Vec<String> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, a)| a.0 == player)
            .map(|(inst, _)| inst.name.clone())
            .collect()
    };
    // Group / follow status for the score sheet. `leader_name` is
    // populated when the player is following someone directly;
    // `group_size` is the total transitive member count from the
    // group root (`1` = solo).
    let leader_name: Option<String> = world
        .get::<Follower>(player)
        .map(|f| name_or(world, f.0, "(unknown)"));
    let group_root_e = group_root(world, player);
    let group_size = group_members(world, group_root_e).len();
    // Current room: name + composite key for the score-sheet
    // location line. Pulled from Located → Named/WorldKey so
    // builders and ops staff can see the (zone, id) at a glance
    // without typing `where` or `tellroom`.
    let location_owned: Option<(String, i32, i32)> = world
        .get::<Located>(player)
        .map(|l| l.0)
        .map(|room| {
            let raw = world
                .get::<Named>(room)
                .map_or_else(String::new, |n| n.name.clone());
            // Strip color tags so the fancy renderer's fixed-width
            // box padding matches the visible character width.
            let name = render_color_tags(&raw, ColorMode::Strip);
            let (zone, id) = world
                .get::<WorldKey>(room)
                .map_or((-1, -1), |k| (k.zone, k.id));
            (name, zone, id)
        });
    // Bound recall destination — surfaces where `recall` would
    // teleport. Same name-stripping treatment as location so the
    // fancy box's padding stays consistent. Skipped entirely when
    // the player hasn't touched a touchstone yet (the `recall`
    // command itself nudges them toward one).
    let recall_owned: Option<(String, i32, i32)> = world
        .get::<RecallPoint>(player)
        .map(|r| r.0)
        .filter(|room| world.get_entity(*room).is_ok())
        .map(|room| {
            let raw = world
                .get::<Named>(room)
                .map_or_else(String::new, |n| n.name.clone());
            let name = render_color_tags(&raw, ColorMode::Strip);
            let (zone, id) = world
                .get::<WorldKey>(room)
                .map_or((-1, -1), |k| (k.zone, k.id));
            (name, zone, id)
        });
    // Mount display name. `Mounted` carries an Entity reference;
    // resolve it through Named with color tags stripped so the
    // fancy renderer's padding matches the visible width.
    let mount_name_owned: Option<String> = world
        .get::<Mounted>(player)
        .map(|m| m.0)
        .filter(|mount| world.get_entity(*mount).is_ok())
        .map(|mount| {
            let raw = world
                .get::<Named>(mount)
                .map_or_else(String::new, |n| n.name.clone());
            render_color_tags(&raw, ColorMode::Strip)
        });
    // Guard target. `Guarding(Entity)` means combat redirects
    // swings aimed at the protected entity onto this player —
    // worth a passive reminder so a guarder who set it long ago
    // doesn't keep eating swings without realizing.
    let guarding_name_owned: Option<String> = world
        .get::<Guarding>(player)
        .map(|g| g.0)
        .filter(|target| world.get_entity(*target).is_ok())
        .map(|target| {
            let raw = world
                .get::<Named>(target)
                .map_or_else(String::new, |n| n.name.clone());
            render_color_tags(&raw, ColorMode::Strip)
        });
    // Body size from `RaceDefaults.size_by_race`. The map key is the
    // raw `Race` enum text on `Profile.race` (HUMAN / ELF / ...) and
    // values are the `Size` enum text (`MEDIUM` / `LARGE` / ...).
    // Look up here so the renderer doesn't carry a &World reference.
    let size_owned: Option<String> = profile_owned
        .as_ref()
        .and_then(|(_, _, race, ..)| {
            world
                .get_resource::<mud_world::RaceDefaults>()
                .and_then(|r| r.size_by_race.get(race).cloned())
        })
        .map(|s| {
            let mut chars = s.chars();
            match chars.next() {
                Some(c) => {
                    let head = c.to_ascii_uppercase().to_string();
                    let tail: String = chars.collect::<String>().to_ascii_lowercase();
                    head + &tail
                }
                None => String::new(),
            }
        })
        .filter(|s| !s.is_empty());

    // Mail-draft summary. Players walk away from half-composed
    // messages all the time — surfacing the recipient + line
    // count keeps the in-flight draft visible from any score
    // render.
    let mail_draft_owned: Option<(String, usize)> = world
        .get::<MailDraft>(player)
        .map(|d| (d.recipient_label.clone(), d.body.len()));
    // Same shape for in-flight board posts via `BoardDraft`.
    let board_draft_owned: Option<(String, usize)> = world
        .get::<BoardDraft>(player)
        .map(|d| (d.board_alias.clone(), d.body.len()));
    // Per-circle slot summary for spellcasters. Reads
    // SpellSlotData (level + class → slot caps) and the player's
    // MemorizedSpells (used vs ready). Empty for classless / non-
    // spellcaster characters; the score renderer skips the line.
    let slots: Vec<(i32, i32, i32)> = (|| {
        let prof = world.get::<Profile>(player)?;
        let class_id = prof.class_id?;
        let level = prof.level;
        let mem = world
            .get::<mud_world::MemorizedSpells>(player)
            .cloned()
            .unwrap_or_default();
        let caps = world
            .resource::<mud_world::SpellSlotData>()
            .slots_for(class_id, level);
        Some(
            caps.into_iter()
                .map(|(circle, max)| (circle, mem.ready_in_circle(circle), max))
                .collect(),
        )
    })()
    .unwrap_or_default();

    let wealth = world.get::<Wealth>(player).map_or(0, |w| w.0);
    let bank = world.get::<BankWealth>(player).map_or(0, |b| b.0);
    let hunger = world.get::<mud_world::Hunger>(player).map_or(0, |h| h.0);
    let thirst = world.get::<mud_world::Thirst>(player).map_or(0, |t| t.0);
    let drunkenness = world.get::<mud_world::Drunkenness>(player).map_or(0, |d| d.0);
    let kill_total = world.get::<mud_world::KillStats>(player).map_or(0, |k| k.total);
    let carry = (carried_weight(world, player), carry_capacity(world, player));
    let clan_owned: Option<(String, String, String)> = world
        .get::<mud_world::ClanMembership>(player)
        .map(|c| (c.clan_name.clone(), c.clan_abbrev.clone(), c.rank.clone()));
    // Player-set epithet. Title strings can be empty in practice
    // (login filters NULL but a blank string would still slip in)
    // so guard with `.is_empty()` before threading it into the
    // renderers to avoid an empty "Title:  " line.
    let title_owned: Option<String> = world
        .get::<Title>(player)
        .map(|t| t.0.clone())
        .filter(|s| !s.is_empty());
    // Wimpy auto-flee threshold. Surfaced when `PlayerFlag::Wimpy`
    // is set; combat reads the same state so this matches actual
    // behavior. Default 25% applies when the flag is on but no
    // explicit `WimpyThreshold` was set — same fallback combat
    // uses (combat.rs:858).
    let wimpy_pct: Option<i32> = world
        .get::<PlayerFlags>(player)
        .filter(|pf| pf.has(PlayerFlag::Wimpy))
        .map(|_| {
            world
                .get::<mud_world::WimpyThreshold>(player)
                .map_or(25, |w| w.0)
        });
    // Per-level rank title — `LevelDefinition.name` carries
    // "Avatar" / "Implementer" / etc. only for staff rows; mortal
    // levels return None and the Level line stays unchanged.
    let level_title_owned: Option<String> = profile_owned
        .as_ref()
        .and_then(|(lvl, ..)| {
            world
                .resource::<mud_world::LevelTable>()
                .title_for(*lvl)
                .map(str::to_string)
        });
    // Next-level HP / Stamina preview — reads the row for
    // `level + 1` so the player sees what they'll gain on the
    // upcoming level-up. None at the cap (no row above max) or
    // when the table hasn't loaded yet.
    let next_level_gains: Option<(i32, i32, i32)> = profile_owned
        .as_ref()
        .and_then(|(lvl, ..)| {
            let next = lvl + 1;
            world
                .resource::<mud_world::LevelTable>()
                .gains_for(next)
                .map(|(hp, st)| (next, hp, st))
        });
    let data = ScoreData {
        name: &name,
        hp,
        stamina,
        cs,
        core_stats,
        posture,
        fight_target: fight_target_name.as_deref(),
        profile: profile_owned.as_ref().map(|(lvl, cls, race, gender, xp)| {
            (
                *lvl,
                cls.as_str(),
                race.as_str(),
                gender.as_str(),
                *xp,
            )
        }),
        wealth,
        bank,
        hunger,
        thirst,
        carry,
        drunkenness,
        kill_total,
        clan: clan_owned
            .as_ref()
            .map(|(n, a, r)| (n.as_str(), a.as_str(), r.as_str())),
        slots: &slots,
        active_effects: &active_effects,
        group_status: GroupStatus {
            leader: leader_name.as_deref(),
            member_count: group_size,
        },
        // Score's level-progress reads from the live LevelTable so
        // the percent agrees with `level` command output. Falls
        // back to the legacy `level^2.5 * 1000` curve via
        // `level_progress_for` only if the table doesn't carry a
        // next-level row (e.g. brand-new boot before levels are
        // loaded). Capped at level >= 100 the same way score did
        // before the LevelTable hookup.
        level_progress: profile_owned.as_ref().and_then(|(lvl, _, _, _, xp)| {
            if !(1..100).contains(lvl) {
                return None;
            }
            let table = world.resource::<mud_world::LevelTable>();
            if let (prev, Some(next)) = (
                table.exp_for(*lvl).unwrap_or(0),
                table.exp_for(*lvl + 1),
            ) {
                let bracket = (next - prev).max(1);
                let into = (*xp - prev).max(0);
                let percent = ((i64::from(into) * 100) / i64::from(bracket)).clamp(0, 100);
                Some(LevelProgress {
                    current_xp: i64::from(*xp),
                    next_level_xp: i64::from(next),
                    percent: i32::try_from(percent).unwrap_or(0),
                })
            } else {
                level_progress_for(*lvl, *xp)
            }
        }),
        location: location_owned
            .as_ref()
            .map(|(name, zone, id)| (name.as_str(), *zone, *id)),
        practice_points: world
            .get::<mud_world::SkillPoints>(player)
            .map_or(0, |s| s.0),
        achievements: {
            let unlocked = world
                .get::<mud_world::CharacterAchievements>(player)
                .map_or(0, |a| a.unlocked.len());
            // Total counts non-hidden rows so the visible
            // denominator excludes secret challenges. A hidden
            // row only enters the numerator once it's actually
            // unlocked — at that point it stops being a secret.
            let catalog = world.resource::<mud_world::AchievementCatalog>();
            let visible_total = catalog
                .by_id
                .values()
                .filter(|d| !d.hidden)
                .count();
            let unlocked_hidden = world
                .get::<mud_world::CharacterAchievements>(player)
                .map_or(0, |a| {
                    a.unlocked
                        .iter()
                        .filter(|id| catalog.by_id.get(id).is_some_and(|d| d.hidden))
                        .count()
                });
            (unlocked, visible_total + unlocked_hidden)
        },
        title: title_owned.as_deref(),
        wimpy: wimpy_pct,
        level_title: level_title_owned.as_deref(),
        next_level_gains,
        recall: recall_owned
            .as_ref()
            .map(|(name, zone, id)| (name.as_str(), *zone, *id)),
        stealth: world.get::<Stealth>(player).is_some(),
        flying: world.get::<Flying>(player).is_some(),
        mount_name: mount_name_owned.as_deref(),
        house: world.get::<HouseSummary>(player).map(|h| {
            (h.rooms.len(), h.entrance_room.zone, h.entrance_room.id)
        }),
        cooldowns_active: world
            .get::<mud_world::Cooldowns>(player)
            .map_or(0, |c| {
                let now = std::time::Instant::now();
                c.ready_at.values().filter(|when| **when > now).count()
            }),
        guarding_name: guarding_name_owned.as_deref(),
        mail_draft: mail_draft_owned
            .as_ref()
            .map(|(to, lines)| (to.as_str(), *lines)),
        board_draft: board_draft_owned
            .as_ref()
            .map(|(alias, lines)| (alias.as_str(), *lines)),
        size: size_owned.as_deref(),
        is_ghost: world.get::<mud_world::Ghost>(player).is_some(),
        is_stunned: world.get::<mud_world::Stunned>(player).is_some(),
        is_frozen: world.get::<mud_world::Frozen>(player).is_some(),
    };
    let out = match style {
        UiStyle::Standard => render_score_standard(&data),
        UiStyle::Fancy => render_score_fancy(&data),
        UiStyle::Minimal => render_score_minimal(&data),
    };
    send_to(world, player, out);
}

pub(crate) fn cmd_style(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        let cur = world.get::<UiStyle>(player).copied().unwrap_or_default();
        send_to(
            world,
            player,
            format!("UI style: {} (try: fancy / standard / minimal)\r\n", cur.label()),
        );
        return;
    }
    let Some(new) = UiStyle::from_label(arg) else {
        send_to(
            world,
            player,
            format!("Unknown style '{arg}'. Try: fancy, standard, minimal.\r\n"),
        );
        return;
    };
    try_insert(world, player, new);
    send_to(world, player, format!("UI style set to {}.\r\n", new.label()));
}

pub(crate) fn cmd_stand(world: &mut World, player: Entity, _args: &str) {
    set_posture(world, player, PostureKind::Standing);
}

pub(crate) fn cmd_sit(world: &mut World, player: Entity, _args: &str) {
    set_posture(world, player, PostureKind::Sitting);
}

pub(crate) fn cmd_kneel(world: &mut World, player: Entity, _args: &str) {
    set_posture(world, player, PostureKind::Kneeling);
}

pub(crate) fn cmd_rest(world: &mut World, player: Entity, _args: &str) {
    set_posture(world, player, PostureKind::Resting);
}

pub(crate) fn cmd_sleep(world: &mut World, player: Entity, _args: &str) {
    set_posture(world, player, PostureKind::Sleeping);
}

pub(crate) fn cmd_wake(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        if world.get::<Posture>(player).map(|p| p.0) == Some(PostureKind::Sleeping) {
            set_posture(world, player, PostureKind::Standing);
        } else {
            send_to(world, player, "You aren't asleep.\r\n");
        }
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    let target_name = name_of(world, target);
    if world.get::<Posture>(target).map(|p| p.0) != Some(PostureKind::Sleeping) {
        send_rendered(world, player, &format!("{target_name} is already awake.\r\n"));
        return;
    }
    try_insert(world, target, Posture(PostureKind::Standing));
    let player_name = name_of(world, player);
    send_rendered(world, player, &format!("You wake {target_name}.\r\n"));
    send_rendered(world, target, &format!("{player_name} wakes you up.\r\n"),
    );
    broadcast_room_except_players_rendered(
        world,
        located.0,
        &[player, target],
        &format!("{player_name} wakes {target_name} up.\r\n"),
    );
}

pub(crate) fn cmd_roles(world: &mut World, player: Entity, _args: &str) {
    let Some(account) = world.get::<Account>(player).cloned() else {
        send_to(world, player, "No account info.\r\n");
        return;
    };
    let mut out = format!("\r\nRole: {:?}\r\n", account.role);
    if account.perms.is_empty() {
        out.push_str("Permissions: none\r\n");
    } else {
        out.push_str("Permissions:\r\n");
        for p in &account.perms {
            out.push_str(&format!("  {p:?}\r\n"));
        }
    }
    send_to(world, player, out);
}

pub(crate) fn cmd_quit(world: &mut World, player: Entity, _args: &str) {
    // Save happens automatically on disconnect via the
    // ConnRouter::on_disconnect path; the autosave tick also
    // runs every 5 minutes so anything since the last save is
    // safe even on Ctrl-C. Spelling it out here so a player who
    // just types `quit` without closing the client doesn't worry
    // about losing progress.
    send_to(
        world,
        player,
        "Goodbye! Your character is auto-saved on disconnect — close your client to log out.\r\n",
    );
}

pub(crate) fn cmd_prompt(world: &mut World, player: Entity, args: &str) {
    let template = args.trim();
    if template.is_empty() {
        let current = world
            .get::<Prompt>(player)
            .map(|p| p.0.clone())
            .unwrap_or_default();
        send_to(
            world,
            player,
            format!(
                "Your prompt is: {current}\r\n\
                 \r\n\
                 Vitals:    %h current HP   %H max HP   \
                 %B 10-cell HP bar (color-graded)\r\n\
                 \x20          %v current stamina  %V max stamina  \
                 %M 10-cell stamina bar\r\n\
                 Identity:  %n character name   %r room name\r\n\
                 Wealth:    %g on-hand copper\r\n\
                 Calendar:  %t hour (00-23)   %s season   %d day/night\r\n\
                 Literal:   %% emits a single `%`.\r\n"
            ),
        );
        return;
    }
    try_insert(world, player, Prompt(template.to_string()));
    send_to(world, player, format!("Prompt set to: {template}\r\n"));
}

pub(crate) fn cmd_toggle(world: &mut World, player: Entity, args: &str) {
    let raw = args.trim();
    if raw.is_empty() {
        send_to(world, player, "Toggle which flag? Try `flags` to see what's set, or `help toggle`.\r\n");
        return;
    }
    let Some(flag) = PlayerFlag::from_label(raw) else {
        send_to(world, player, format!("Unknown flag '{raw}'.\r\n"));
        return;
    };
    // God-only flags (HOLY_LIGHT, SHOW_IDS) are gated on the
    // dedicated cmd_holylight / cmd_showids commands; the generic
    // toggle path must not become a bypass.
    if flag.is_god_only() {
        let allowed = world
            .get::<mud_world::Account>(player)
            .is_some_and(|a| a.role.at_least(mud_db::enums::UserRole::Builder));
        if !allowed {
            send_to(world, player, format!("'{raw}' is not a flag you can toggle.\r\n"));
            return;
        }
    }
    let now_on = world
        .get_mut::<PlayerFlags>(player)
        .map(|mut pf| pf.toggle(flag));
    let Some(now_on) = now_on else {
        send_to(world, player, "You have no player flags slot.\r\n");
        return;
    };
    let label = flag.label();
    if now_on {
        send_to(world, player, format!("{label} is now ON.\r\n"));
    } else {
        send_to(world, player, format!("{label} is now OFF.\r\n"));
    }
}

pub(crate) fn cmd_afk(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Afk,
        "You are now marked AFK.",
        "You're back from AFK.",
    );
}

pub(crate) fn cmd_alias(world: &mut World, player: Entity, args: &str) {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        // List
        let Some(aliases) = world.get::<mud_world::Aliases>(player) else {
            send_to(world, player, "You have no aliases defined.\r\n");
            return;
        };
        if aliases.entries.is_empty() {
            send_to(world, player, "You have no aliases defined.\r\n");
            return;
        }
        let mut out = format!("\r\n{} alias(es):\r\n", aliases.entries.len());
        for (alias, command) in &aliases.entries {
            out.push_str(&format!("  {alias:<12}  {command}\r\n"));
        }
        send_to(world, player, out);
        return;
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let expansion = parts.next().map_or("", str::trim);

    if name.is_empty() || name.contains(char::is_whitespace) {
        send_to(world, player, "Usage: alias <name> [<command>]\r\n");
        return;
    }

    if expansion.is_empty() {
        // Show single
        let Some(aliases) = world.get::<mud_world::Aliases>(player) else {
            send_to(world, player, format!("No alias '{name}'.\r\n"));
            return;
        };
        if let Some(cmd) = aliases.get(&name) {
            send_to(world, player, format!("alias {name} = {cmd}\r\n"));
        } else {
            send_to(world, player, format!("No alias '{name}'.\r\n"));
        }
        return;
    }

    if RESERVED_ALIAS_NAMES.contains(&name.as_str()) {
        send_to(
            world,
            player,
            format!("'{name}' can't be aliased — reserved.\r\n"),
        );
        return;
    }

    let Ok(mut entity_mut) = world.get_entity_mut(player) else {
        return;
    };
    let mut aliases = entity_mut.take::<mud_world::Aliases>().unwrap_or_default();
    let replaced = aliases.set(&name, expansion.to_string());
    entity_mut.insert(aliases);
    send_to(
        world,
        player,
        if replaced {
            format!("Alias '{name}' updated.\r\n")
        } else {
            format!("Alias '{name}' set.\r\n")
        },
    );
}

pub(crate) fn cmd_unalias(world: &mut World, player: Entity, args: &str) {
    let name = args.trim().to_ascii_lowercase();
    if name.is_empty() || name.contains(char::is_whitespace) {
        send_to(world, player, "Usage: unalias <name>\r\n");
        return;
    }
    let Ok(mut entity_mut) = world.get_entity_mut(player) else {
        return;
    };
    let mut aliases = entity_mut.take::<mud_world::Aliases>().unwrap_or_default();
    let removed = aliases.remove(&name);
    entity_mut.insert(aliases);
    send_to(
        world,
        player,
        if removed {
            format!("Alias '{name}' removed.\r\n")
        } else {
            format!("No alias '{name}'.\r\n")
        },
    );
}

pub(crate) fn cmd_notell(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::NoTell,
        "You will no longer receive tells.",
        "You will now receive tells.",
    );
}

pub(crate) fn cmd_deaf(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Deaf,
        "You no longer hear gossip or shouts.",
        "You can hear gossip and shouts again.",
    );
}

pub(crate) fn cmd_color(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::ColorBlind,
        "Colors are now OFF.",
        "Colors are now ON.",
    );
}

pub(crate) fn cmd_wimpy(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();

    let currently_on = world
        .get::<PlayerFlags>(player)
        .is_some_and(|pf| pf.has(PlayerFlag::Wimpy));
    let current_pct = world
        .get::<mud_world::WimpyThreshold>(player)
        .map_or(25, |w| w.0);

    if arg.is_empty() {
        let msg = if currently_on {
            format!(
                "Wimpy mode is on at {current_pct}% — you'll try to flee \
                 when your HP drops below that.\r\n"
            )
        } else {
            "Wimpy mode is off. Use `wimpy <pct>` (1-99) to enable.\r\n"
                .to_string()
        };
        send_to(world, player, msg);
        return;
    }

    if arg.eq_ignore_ascii_case("off") || arg == "0" {
        if currently_on
            && let Some(mut pf) = world.get_mut::<PlayerFlags>(player)
        {
            pf.toggle(PlayerFlag::Wimpy);
        }
        try_remove::<mud_world::WimpyThreshold>(world, player);
        send_to(
            world,
            player,
            "Okay, you'll now stand and fight to the bitter end.\r\n",
        );
        return;
    }

    let pct = match arg.parse::<i32>() {
        Ok(n) if (1..=99).contains(&n) => n,
        Ok(_) => {
            send_to(
                world,
                player,
                "Wimpy percent must be between 1 and 99 (or `off` to disable).\r\n",
            );
            return;
        }
        Err(_) => {
            send_to(
                world,
                player,
                "Usage: `wimpy <pct>` (1-99) or `wimpy off`.\r\n",
            );
            return;
        }
    };

    if !currently_on
        && let Some(mut pf) = world.get_mut::<PlayerFlags>(player)
    {
        pf.toggle(PlayerFlag::Wimpy);
    }
    try_insert(world, player, mud_world::WimpyThreshold(pct));
    send_to(
        world,
        player,
        format!(
            "You'll panic and try to flee when your HP drops below {pct}%.\r\n"
        ),
    );
}

pub(crate) fn cmd_autoexit(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::AutoExit,
        "Exits will be shown automatically with each `look`.",
        "Exits will no longer auto-list — use `exits` to see them.",
    );
}

pub(crate) fn cmd_autoloot(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::AutoLoot,
        "Auto-loot enabled.",
        "Auto-loot disabled.",
    );
}

pub(crate) fn cmd_autogold(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::AutoGold,
        "Auto-gold enabled.",
        "Auto-gold disabled.",
    );
}

pub(crate) fn cmd_autoassist(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::AutoAssist,
        "Auto-assist enabled.",
        "Auto-assist disabled.",
    );
}

pub(crate) fn cmd_autosplit(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::AutoSplit,
        "Auto-split enabled.",
        "Auto-split disabled.",
    );
}

pub(crate) fn cmd_brief(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Brief,
        "Room descriptions will now be terse on `look`.",
        "Full room descriptions restored.",
    );
}

pub(crate) fn cmd_compact(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Compact,
        "Compact mode enabled.",
        "Compact mode disabled.",
    );
}

pub(crate) fn cmd_norepeat(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::NoRepeat,
        "Suppressing duplicate consecutive lines.",
        "All output lines will be shown.",
    );
}

pub(crate) fn cmd_nosummon(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::NoSummon,
        "You can no longer be summoned by spells.",
        "You can again be summoned by spells.",
    );
}

pub(crate) fn cmd_dicerolls(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::ShowDiceRolls,
        "Showing dice rolls.",
        "Hiding dice rolls.",
    );
}

pub(crate) fn cmd_pk(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::PkEnabled,
        "PK is now enabled — you may attack and be attacked by other players.",
        "PK is now disabled.",
    );
}

pub(crate) fn cmd_quest_flag(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Quest,
        "Quest mode enabled — you'll be flagged for quest-only zones once those land.",
        "Quest mode disabled.",
    );
}

pub(crate) fn cmd_consent(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Consent,
        "You consent to group/share interactions.",
        "You revoke group/share consent.",
    );
}

pub(crate) fn cmd_holylight(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::HolyLight,
        "Holy light surrounds you — the unseen is now seen.",
        "Holy light fades.",
    );
}

pub(crate) fn cmd_showids(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::ShowIds,
        "Showing entity IDs.",
        "Hiding entity IDs.",
    );
}

pub(crate) fn cmd_flags(world: &mut World, player: Entity, _args: &str) {
    let flags: Vec<&'static str> = world
        .get::<PlayerFlags>(player)
        .map(|f| f.0.iter().map(|fl| fl.label()).collect())
        .unwrap_or_default();
    let mut out = if flags.is_empty() {
        "\r\nNo flags set.\r\n".to_string()
    } else {
        format!("\r\n{} flag(s) set:\r\n", flags.len())
    };
    for label in &flags {
        out.push_str(&format!("  {label}\r\n"));
    }
    send_to(world, player, out);
}

pub(crate) fn cmd_exits(world: &mut World, player: Entity, _args: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(exits) = world.get::<Exits>(located.0).cloned() else {
        send_to(world, player, "\r\nNo exits.\r\n");
        return;
    };
    if exits.0.is_empty() {
        send_to(world, player, "\r\nNo exits.\r\n");
        return;
    }
    // Resolve each exit's target room name + door state. Sort by
    // direction's canonical order. State trailer signals whether
    // the exit needs `open` / `unlock` before the player can pass.
    // Hidden exits stay invisible until `search` reveals them —
    // the listing here is the canonical "what can I see going
    // out" view. Movement and `look <dir>` apply the same filter
    // so the secret stays self-consistent.
    let mut rows: Vec<(mud_db::enums::Direction, String, ExitState)> = exits
        .0
        .iter()
        .filter(|(d, ed)| !exit_is_hidden_to(world, player, located.0, **d, ed))
        .map(|(dir, ed)| {
            let target_name = ed
                .to
                .and_then(|e| world.get::<Named>(e).map(|n| n.name.clone()))
                .unwrap_or_else(|| "(beyond)".to_string());
            (*dir, target_name, ed.state)
        })
        .collect();
    if rows.is_empty() {
        send_to(world, player, "\r\nNo exits.\r\n");
        return;
    }
    rows.sort_by_key(|(d, _, _)| direction_order(*d));
    let mode = color_mode_for(world, player);
    let mut out = String::from("\r\n");
    out.push_str(&render_color_tags("<cyan>Exits:</>", mode));
    out.push_str("\r\n");
    for (dir, room, state) in &rows {
        let open = exit_state_color(*state);
        let dir_label = render_color_tags(
            &format!("{open}{}</>", direction_name(*dir)),
            mode,
        );
        // Colorize plain target room names; authored ones keep
        // their builder-set color via colorize_default.
        let room_label = render_color_tags(
            &colorize_default(room, "<b:white>"),
            mode,
        );
        let state_label = match state {
            ExitState::Open => String::new(),
            ExitState::Closed => render_color_tags("  <yellow>(closed)</>", mode),
            ExitState::Locked => render_color_tags("  <red>(locked)</>", mode),
        };
        // Pad direction column visible-width-aware so color tags
        // don't break the right-justified alignment. NAME_COL = 10
        // matches the previous `:>10` formatter.
        let padded = pad_visible(&dir_label, 10);
        out.push_str(&format!("  {padded} - {room_label}{state_label}\r\n"));
    }
    send_to(world, player, out);
}

/// `unlock <direction>`: find a key item in inventory whose name or
/// keyword matches the exit's `key` and flip Locked → Closed (still
/// needs `open` afterward). Two-sided sync.
pub(crate) fn cmd_unlock(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let Some(dir) = parse_direction(arg) else {
        send_to(world, player, "Unlock which way?\r\n");
        return;
    };
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let Some((state, key_req)) = world
        .get::<Exits>(room)
        .and_then(|e| e.0.get(&dir).map(|ed| (ed.state, ed.key)))
    else {
        send_to(world, player, format!("No exit {}.\r\n", direction_name(dir)));
        return;
    };
    if state != ExitState::Locked {
        send_to(world, player, format!("It's not locked {}.\r\n", direction_name(dir)));
        return;
    }
    let Some(key_req) = key_req else {
        send_to(world, player, format!("There's no keyhole {}.\r\n", direction_name(dir)));
        return;
    };
    // Match a carried item by exact `WorldKey` against the exit's
    // (zone, id) key composite. The fallback keyword chain we used
    // for the old text-encoded vnum data isn't needed any more.
    let has_key = {
        let mut q = world.query_filtered::<(&Located, &WorldKey), With<Item>>();
        q.iter(world)
            .any(|(l, k)| l.0 == player && k.zone == key_req.0 && k.id == key_req.1)
    };
    if !has_key {
        let hint = world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&key_req)
            .and_then(|p| p.keywords.first().cloned())
            .unwrap_or_else(|| format!("({}, {})", key_req.0, key_req.1));
        send_to(world, player, format!(
            "You need '{hint}' to unlock that.\r\n",
        ));
        return;
    }
    flip_door_both_sides(world, room, dir, ExitState::Closed);
    send_to(world, player, format!("You unlock the way {}.\r\n", direction_name(dir)));
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} unlocks the door {}.\r\n", direction_name(dir)),
    );
}

/// `open <direction>`: flip a closed exit to Open. Refused on
/// locked exits and on exits that don't exist.
pub(crate) fn cmd_open(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let Some(dir) = parse_direction(arg) else {
        send_to(world, player, "Open which way?\r\n");
        return;
    };
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let cur_state = world
        .get::<Exits>(room)
        .and_then(|e| e.0.get(&dir).map(|ed| ed.state));
    let Some(state) = cur_state else {
        send_to(world, player, format!("No exit {}.\r\n", direction_name(dir)));
        return;
    };
    match state {
        ExitState::Open => {
            send_to(world, player, format!("It's already open {}.\r\n", direction_name(dir)));
            return;
        }
        ExitState::Locked => {
            send_to(world, player, format!("It's locked {}.\r\n", direction_name(dir)));
            return;
        }
        ExitState::Closed => {}
    }
    flip_door_both_sides(world, room, dir, ExitState::Open);
    send_to(world, player, format!("You open the way {}.\r\n", direction_name(dir)));
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} opens the door {}.\r\n", direction_name(dir)),
    );
}

/// `close <direction>`: flip an open exit to Closed. Refused on
/// already-closed/locked or non-existent exits.
pub(crate) fn cmd_close(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let Some(dir) = parse_direction(arg) else {
        send_to(world, player, "Close which way?\r\n");
        return;
    };
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let cur_state = world
        .get::<Exits>(room)
        .and_then(|e| e.0.get(&dir).map(|ed| ed.state));
    let Some(state) = cur_state else {
        send_to(world, player, format!("No exit {}.\r\n", direction_name(dir)));
        return;
    };
    match state {
        ExitState::Closed | ExitState::Locked => {
            send_to(world, player, format!("It's already closed {}.\r\n", direction_name(dir)));
            return;
        }
        ExitState::Open => {}
    }
    flip_door_both_sides(world, room, dir, ExitState::Closed);
    send_to(world, player, format!("You close the way {}.\r\n", direction_name(dir)));
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} closes the door {}.\r\n", direction_name(dir)),
    );
}

/// `lock <direction>`: mirror of `unlock`. Requires a Closed exit
/// with a key requirement, and that the player carries that key.
/// Already-locked / open / no-keyhole / no-key cases all refuse.
pub(crate) fn cmd_lock(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let Some(dir) = parse_direction(arg) else {
        send_to(world, player, "Lock which way?\r\n");
        return;
    };
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let Some((state, key_req)) = world
        .get::<Exits>(room)
        .and_then(|e| e.0.get(&dir).map(|ed| (ed.state, ed.key)))
    else {
        send_to(world, player, format!("No exit {}.\r\n", direction_name(dir)));
        return;
    };
    match state {
        ExitState::Open => {
            send_to(
                world,
                player,
                format!("You'll need to close it first {}.\r\n", direction_name(dir)),
            );
            return;
        }
        ExitState::Locked => {
            send_to(
                world,
                player,
                format!("It's already locked {}.\r\n", direction_name(dir)),
            );
            return;
        }
        ExitState::Closed => {}
    }
    let Some(key_req) = key_req else {
        send_to(
            world,
            player,
            format!("There's no keyhole {}.\r\n", direction_name(dir)),
        );
        return;
    };
    let has_key = {
        let mut q = world.query_filtered::<(&Located, &WorldKey), With<Item>>();
        q.iter(world)
            .any(|(l, k)| l.0 == player && k.zone == key_req.0 && k.id == key_req.1)
    };
    if !has_key {
        let hint = world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&key_req)
            .and_then(|p| p.keywords.first().cloned())
            .unwrap_or_else(|| format!("({}, {})", key_req.0, key_req.1));
        send_to(world, player, format!("You need '{hint}' to lock that.\r\n"));
        return;
    }
    flip_door_both_sides(world, room, dir, ExitState::Locked);
    send_to(
        world,
        player,
        format!("You lock the way {}.\r\n", direction_name(dir)),
    );
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!(
            "{player_name} locks the door {}.\r\n",
            direction_name(dir)
        ),
    );
}

/// `read <item>`: find an item by keyword on the player or in their
/// room and print its Description. Refuses on mobs/players (use
/// `examine`). The Description component is the same one
/// `ObjectPrototypes.examine_description` feeds at load time, so books
/// / signs / scrolls all surface their text via this path.
pub(crate) fn cmd_read(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Read what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let lc = needle.to_ascii_lowercase();
    // Match items only — mobs/players go to `examine`. Search the
    // player's inventory + the current room.
    let target = {
        let mut q = world
            .query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
        q.iter(world)
            .find(|(_, l, n, kw)| {
                (l.0 == player || l.0 == located.0) && matches(&lc, n, *kw)
            })
            .map(|(e, _, _, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("You can't find anything to read called '{needle}'.\r\n"));
        return;
    };
    let name = name_of(world, target);
    let mode = color_mode_for(world, player);
    let name_rendered = render_color_tags(&name, mode);
    let mut out = format!("\r\nYou read {name_rendered}:\r\n");
    if let Some(desc) = world.get::<Description>(target) {
        let body = desc.0.trim();
        if body.is_empty() {
            out.push_str("It's blank.\r\n");
        } else {
            out.push_str(&format!("{}\r\n", render_color_tags(body, mode)));
        }
    } else {
        out.push_str("It's blank.\r\n");
    }
    send_to(world, player, out);
}

/// `compare <a> [<b>]`: side-by-side weight + level + type comparison.
/// With one arg, compares against whatever's equipped in the same
/// slot (the "should I switch?" workflow). With two args, compares
/// the two named items directly. Splits the args at the first run
/// of whitespace; multi-word keywords aren't supported (a quoted-
/// arg parser would be more general but no other command needs
/// one yet).
pub(crate) fn cmd_compare(world: &mut World, player: Entity, args: &str) {
    let mut parts = args.trim().splitn(2, char::is_whitespace);
    let Some(a_word) = parts.next().filter(|s| !s.is_empty()) else {
        send_to(world, player, "Compare what to what?\r\n");
        return;
    };
    let b_word: Option<&str> = parts.next().map(str::trim).filter(|s| !s.is_empty());

    let Some(a) = find_carried_by(world, a_word, player, EquipFilter::Anywhere) else {
        send_to(world, player, format!("You don't have '{a_word}'.\r\n"));
        return;
    };
    // Resolve B. With an explicit second arg, look it up the same way
    // as A. With no second arg, find the item the player currently
    // wears in A's wearable slot — saves a manual `equipment` lookup
    // for the "is this new sword better?" question.
    let target_b: Entity = if let Some(word) = b_word {
        let Some(found) = find_carried_by(world, word, player, EquipFilter::Anywhere)
        else {
            send_to(world, player, format!("You don't have '{word}'.\r\n"));
            return;
        };
        found
    } else {
        let Some(target_slot) = world.get::<WearableIn>(a).map(|w| w.0) else {
            send_to(
                world,
                player,
                "That item isn't wearable, so there's nothing in a matching slot to compare.\r\n",
            );
            return;
        };
        let equipped = {
            let mut q = world.query_filtered::<
                (Entity, &Located, &EquippedSlot),
                With<Item>,
            >();
            q.iter(world)
                .find(|(_, l, eq)| l.0 == player && eq.0 == target_slot)
                .map(|(ent, _, _)| ent)
        };
        let Some(found) = equipped else {
            send_to(
                world,
                player,
                format!(
                    "You're not wearing anything in the {} slot to compare against.\r\n",
                    target_slot.label(),
                ),
            );
            return;
        };
        found
    };
    let b = target_b;
    if a == b {
        send_to(world, player, "That's the same item.\r\n");
        return;
    }

    let a_name = name_of(world, a);
    let b_name = name_of(world, b);
    let a_proto = world
        .get::<WorldKey>(a)
        .and_then(|k| world.resource::<ObjectPrototypes>().by_key.get(&(k.zone, k.id)).cloned());
    let b_proto = world
        .get::<WorldKey>(b)
        .and_then(|k| world.resource::<ObjectPrototypes>().by_key.get(&(k.zone, k.id)).cloned());
    let Some(ap) = a_proto else {
        send_to(world, player, format!("No prototype data for {a_name}.\r\n"));
        return;
    };
    let Some(bp) = b_proto else {
        send_to(world, player, format!("No prototype data for {b_name}.\r\n"));
        return;
    };

    let mode = color_mode_for(world, player);
    // Compare table: bold-cyan A/B side tags so the eye locks onto
    // which row is which; weight/level/type are reference fields,
    // dimmed. Item names render through the normal color pipeline
    // so authored tags survive. Render through send_rendered keeps
    // the color tags inside the dim wrapper from re-entering the
    // pipeline.
    let mut out = String::from("\r\n<b:cyan>Compare</>\r\n");
    out.push_str(&format!(
        "  <b:cyan>A:</> {}    <dim>weight: {:.1}   level: {}   ({:?})</>\r\n",
        render_color_tags(&a_name, mode),
        ap.weight,
        ap.level,
        ap.r#type,
    ));
    out.push_str(&format!(
        "  <b:cyan>B:</> {}    <dim>weight: {:.1}   level: {}   ({:?})</>\r\n",
        render_color_tags(&b_name, mode),
        bp.weight,
        bp.level,
        bp.r#type,
    ));
    let weight_delta = ap.weight - bp.weight;
    let level_delta = ap.level - bp.level;
    // Delta lines: equality reads dim (no signal); non-equal lines
    // colored neutral cyan since "A heavier" isn't inherently good
    // or bad — the player decides whether weight is a feature or
    // a tax for their build.
    let weight_line = if weight_delta.abs() < f64::EPSILON {
        "<dim>Same weight.</>".to_string()
    } else if weight_delta > 0.0 {
        format!("<cyan>A heavier by {weight_delta:.1}.</>")
    } else {
        format!("<cyan>B heavier by {:.1}.</>", -weight_delta)
    };
    let level_line = match level_delta.cmp(&0) {
        std::cmp::Ordering::Equal => "<dim>Same level.</>".to_string(),
        std::cmp::Ordering::Greater => {
            format!("<cyan>A higher level by {level_delta}.</>")
        }
        std::cmp::Ordering::Less => {
            format!("<cyan>B higher level by {}.</>", -level_delta)
        }
    };
    out.push_str(&format!("  {weight_line}  {level_line}\r\n"));
    // Weapon-vs-weapon damage comparison. Skipped silently when
    // either side has zero average damage (i.e. it's not a weapon
    // proto) so non-weapon comparisons stay terse. Surfacing avg
    // damage and the dice pretty-print together gives players
    // both the at-a-glance comparison ("A higher by 3.5 avg") and
    // the underlying NdM+B that produced it.
    let a_avg = ap.avg_damage();
    let b_avg = bp.avg_damage();
    if a_avg > 0 && b_avg > 0 {
        let dice_line = |p: &mud_world::ObjectProto| -> String {
            let bonus = match p.weapon_dice_bonus.cmp(&0) {
                std::cmp::Ordering::Equal => String::new(),
                std::cmp::Ordering::Greater => format!("+{}", p.weapon_dice_bonus),
                std::cmp::Ordering::Less => format!("{}", p.weapon_dice_bonus),
            };
            format!("{}d{}{bonus}", p.weapon_dice_num, p.weapon_dice_size)
        };
        let damage_delta = a_avg - b_avg;
        // Damage delta picks up `damage_color_tag` so a meaningful
        // difference reads with the same warmth used on combat
        // swings. Equal damage dims as "no signal".
        let damage_line = match damage_delta.cmp(&0) {
            std::cmp::Ordering::Equal => {
                "<dim>Same average damage.</>".to_string()
            }
            std::cmp::Ordering::Greater => {
                let open = damage_color_tag(damage_delta).unwrap_or("<cyan>");
                format!("{open}A higher avg damage by {damage_delta}.</>")
            }
            std::cmp::Ordering::Less => {
                let open = damage_color_tag(-damage_delta).unwrap_or("<cyan>");
                format!("{open}B higher avg damage by {}.</>", -damage_delta)
            }
        };
        out.push_str(&format!(
            "  <b:cyan>A:</> <yellow>{}</> <dim>(avg {a_avg})</>    <b:cyan>B:</> <yellow>{}</> <dim>(avg {b_avg})</>    {damage_line}\r\n",
            dice_line(&ap),
            dice_line(&bp),
        ));
    }
    send_to(world, player, out);
}

pub(crate) fn cmd_motd(world: &mut World, player: Entity, _args: &str) {
    send_to(world, player, MOTD_TEXT.to_string());
}

pub(crate) fn cmd_news(world: &mut World, player: Entity, _args: &str) {
    send_to(world, player, NEWS_TEXT.to_string());
}

pub(crate) fn cmd_credits(world: &mut World, player: Entity, _args: &str) {
    send_to(world, player, CREDITS_TEXT.to_string());
}

pub(crate) fn cmd_policies(world: &mut World, player: Entity, _args: &str) {
    send_to(world, player, POLICIES_TEXT.to_string());
}

/// `account`: read-only summary from the `AccountSummary` component
/// inserted at login. Active character is the one with the same name
/// as the entity's Named component (which is unique per player).
/// `richtest`: sampler that exercises every named color and
/// modifier the XML-Lite renderer supports. Useful for verifying
/// terminal color rendering and for debugging tag handling — the
/// output goes through `send_rendered` (same path as room
/// descriptions) so what you see is what every other render path
/// produces.
pub(crate) fn cmd_richtest(world: &mut World, player: Entity, _args: &str) {
    let body = "\r\nXML-Lite color sampler:\r\n\
                <red>red</> <green>green</> <yellow>yellow</> <blue>blue</> \
                <magenta>magenta</> <cyan>cyan</> <white>white</>\r\n\
                <b:red>bright red</> <b:green>bright green</> \
                <b:yellow>bright yellow</> <b:blue>bright blue</> \
                <b:magenta>bright magenta</> <b:cyan>bright cyan</> \
                <b:white>bright white</>\r\n\
                Nested: <red>red <yellow>yellow inside</> back to red</>\r\n\
                Anonymous: <red>red until close</> done\r\n\
                Tag form: <name> opens a layer, </> closes the most \
                recent. Use `b:` prefix for bright (e.g. <b:cyan>like \
                this</>).\r\n";
    send_rendered(world, player, body);
}

/// `clientinfo`: per-session connection summary. Quick check that
/// surfaces what the runtime tracks today (idle, uptime, role) — the
/// proper terminal-capability split (color depth, dimensions, MCCP)
/// needs the telnet negotiation parsing that mud-net hasn't grown
/// yet.
pub(crate) fn cmd_clientinfo(world: &mut World, player: Entity, _args: &str) {
    let now = std::time::Instant::now();
    let uptime_secs = world
        .get::<LoggedInAt>(player)
        .map_or(0, |l| now.duration_since(l.0).as_secs());
    let idle_secs = world
        .get::<LastInputAt>(player)
        .map_or(0, |l| now.duration_since(l.0).as_secs());
    let role = world
        .get::<Account>(player)
        .map_or(UserRole::Player, |a| a.role);
    let char_name = name_of(world, player);
    let format_dur = |secs: u64| -> String {
        if secs < 60 {
            format!("{secs}s")
        } else if secs < 3600 {
            format!("{}m {}s", secs / 60, secs % 60)
        } else {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        }
    };
    // Session readout: cyan labels frame each line; character
    // name + role colored, uptime bold-cyan as the headline,
    // idle picks up `idle_color`'s warmth grade so a stale
    // session reads at a glance (matches `who` / `idle`).
    let role_open = role_color_tag(role);
    let role_text = role_open.map_or_else(
        || role.label().to_string(),
        |open| format!("{open}{}</>", role.label()),
    );
    let idle_open = idle_color(idle_secs);
    let idle_str = format_dur(idle_secs);
    let idle_text = idle_open.map_or(idle_str.clone(), |open| {
        format!("{open}{idle_str}</>")
    });
    // Lifetime time played: persisted seconds + this session's
    // elapsed. Pulled here (not on score) since it's session-meta,
    // not combat-relevant. Suppressed when both are zero.
    let played_secs: u64 = {
        let persisted: u64 = world
            .get::<mud_world::TimePlayed>(player)
            .map_or(0, |t| u64::try_from(t.0.max(0)).unwrap_or(0));
        persisted.saturating_add(uptime_secs)
    };
    // "Last login" = seconds since the player's previous session
    // started. Captured at this session's spawn so the value
    // doesn't drift across the session.
    let last_login_secs_ago: Option<i64> = world
        .get::<mud_world::PreviousLogin>(player)
        .map(|p| p.0)
        .and_then(|prev_ts| {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            let now_i64 = i64::try_from(now_secs).ok()?;
            Some(now_i64.saturating_sub(prev_ts))
        });

    let mut out = String::from("\r\n<b:cyan>Session</>\r\n");
    out.push_str(&format!(
        "  <cyan>Character:</> <b:cyan>{char_name}</>\r\n"
    ));
    out.push_str(&format!("  <cyan>Role:</>      {role_text}\r\n"));
    out.push_str(&format!(
        "  <cyan>Uptime:</>    <b:cyan>{}</> <dim>(since login)</>\r\n",
        format_dur(uptime_secs)
    ));
    out.push_str(&format!(
        "  <cyan>Idle:</>      {idle_text} <dim>(since last input)</>\r\n",
    ));
    if played_secs > 0 {
        out.push_str(&format!(
            "  <cyan>Played:</>    <dim>{}</> <dim>(lifetime)</>\r\n",
            format_play_time(played_secs)
        ));
    }
    if let Some(secs) = last_login_secs_ago {
        out.push_str(&format!(
            "  <cyan>Last login:</> <dim>{}</>\r\n",
            format_time_ago(secs)
        ));
    }
    send_to(world, player, out);
}

pub(crate) fn cmd_account(world: &mut World, player: Entity, _args: &str) {
    let Some(summary) = world.get::<AccountSummary>(player).cloned() else {
        send_to(world, player, "No account info available.\r\n");
        return;
    };
    let active_name = name_of(world, player);
    let role = world
        .get::<Account>(player)
        .map_or(UserRole::Player, |a| a.role);

    // Account readout: email + display name read as reference
    // metadata (dim values), role picks up `role_color_tag`'s
    // staff hue so an Implementor stands out, character list
    // marks the active character with bold-yellow `*` + cyan
    // name, level dimmed since it's a callout for the active
    // line not the headline.
    let role_open = role_color_tag(role);
    let role_text = role_open.map_or_else(
        || role.label().to_string(),
        |open| format!("{open}{}</>", role.label()),
    );
    let mut out = String::from("\r\n<b:cyan>Account</>\r\n");
    out.push_str(&format!(
        "  <cyan>Email:</>        <dim>{}</>\r\n",
        summary.email
    ));
    out.push_str(&format!(
        "  <cyan>Display name:</> <dim>{}</>\r\n",
        summary.display_name
    ));
    out.push_str(&format!("  <cyan>Role:</>         {role_text}\r\n"));
    out.push_str(&format!(
        "  <cyan>Characters</>   <dim>({}):</>\r\n",
        summary.characters.len()
    ));
    for (name, level) in &summary.characters {
        if name == &active_name {
            out.push_str(&format!(
                "    <b:yellow>*</> <cyan>{name}</> <dim>(level {level})</>\r\n"
            ));
        } else {
            out.push_str(&format!(
                "      <cyan>{name}</> <dim>(level {level})</>\r\n"
            ));
        }
    }
    out.push_str("\r\n  <dim>* = currently playing</>\r\n");
    send_to(world, player, out);
}

pub(crate) fn cmd_commands(world: &mut World, player: Entity, args: &str) {
    let (role, perms) = world
        .get::<Account>(player)
        .map_or((UserRole::Player, Vec::new()), |a| (a.role, a.perms.clone()));
    // Optional category filter. Match case-insensitively against
    // each Category's `label()`. Unknown / typo args fall through
    // to "no filter" — matches the lenient style of other info
    // commands (`who`, `idle`).
    let category_arg = args.trim().to_ascii_lowercase();
    let want_category: Option<Category> = if category_arg.is_empty() {
        None
    } else {
        Category::ORDER
            .iter()
            .copied()
            .find(|c| c.label().eq_ignore_ascii_case(&category_arg))
    };
    let mut names: Vec<&'static str> = all_commands()
        .filter(|c| visible(c, role, &perms))
        .filter(|c| want_category.is_none_or(|w| c.category == w))
        .map(|c| c.names[0])
        .collect();
    names.sort_unstable();

    // Header reads bold-cyan as the count + category title; the
    // category label inside it picks up a bold-yellow accent to
    // match the help-index category framing. Names render in cyan,
    // padded inside the color tags so the column grid stays
    // visually aligned in clients that strip color (mode = Strip
    // collapses tags before width calc downstream is unaffected).
    let header = if let Some(cat) = want_category {
        format!(
            "\r\n<b:cyan>{} <b:yellow>{}</> commands available:</>\r\n",
            names.len(),
            cat.label()
        )
    } else {
        format!(
            "\r\n<b:cyan>{} commands available:</>\r\n",
            names.len()
        )
    };
    let mut out = header;
    for chunk in names.chunks(COMMANDS_LIST_COLS) {
        out.push_str("  ");
        for name in chunk {
            out.push_str(&format!(
                "<cyan>{name:<COMMANDS_LIST_COL_WIDTH$}</>"
            ));
        }
        out.push_str("\r\n");
    }
    out.push_str("\r\n<dim>Use `help <command>` for details.</>\r\n");
    send_to(world, player, out);
}

pub(crate) fn cmd_world(world: &mut World, player: Entity, _args: &str) {
    let zones = world.query_filtered::<Entity, With<mud_world::Zone>>().iter(world).count();
    let rooms = world.query_filtered::<Entity, With<mud_world::Room>>().iter(world).count();
    let mobs = world.query_filtered::<Entity, With<Mob>>().iter(world).count();
    let items = world.query_filtered::<Entity, With<Item>>().iter(world).count();
    let players_online = world
        .query_filtered::<Entity, (With<Player>, With<Online>)>()
        .iter(world)
        .count();
    let effects = world.query::<&EffectInstance>().iter(world).count();
    let tick = world.resource::<TickCount>().0;
    let uptime_secs = world.resource::<ServerStart>().0.elapsed().as_secs();
    let h = uptime_secs / 3600;
    let m = (uptime_secs % 3600) / 60;
    let s = uptime_secs % 60;

    // Status readout: cyan labels frame each line; counts stay
    // default-color so the eye lands on the value. Uptime gets
    // bold-cyan because "how long has the server been up" is the
    // single most-checked field and reads as a header for the rest.
    let mut out = String::from("\r\n<b:cyan>World status</>\r\n");
    out.push_str(&format!("  <cyan>Zones loaded:</>    {zones}\r\n"));
    out.push_str(&format!("  <cyan>Rooms loaded:</>    {rooms}\r\n"));
    out.push_str(&format!("  <cyan>Mobs spawned:</>    {mobs}\r\n"));
    out.push_str(&format!("  <cyan>Items spawned:</>   {items}\r\n"));
    out.push_str(&format!("  <cyan>Players online:</>  {players_online}\r\n"));
    out.push_str(&format!("  <cyan>Active effects:</>  {effects}\r\n"));
    out.push_str(&format!("  <cyan>Server tick:</>     <dim>{tick}</>\r\n"));
    out.push_str(&format!(
        "  <cyan>Uptime:</>          <b:cyan>{h}h {m}m {s}s</>\r\n"
    ));
    send_to(world, player, out);
}

pub(crate) fn cmd_time(world: &mut World, player: Entity, _args: &str) {
    let tick = world.resource::<TickCount>().0;
    let started = world.resource::<ServerStart>().0;
    let uptime = started.elapsed();
    let now = chrono::Utc::now();

    let secs = uptime.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;

    // Read from MudClock — the canonical in-game-time source. It
    // starts at hour 12 day 1 month 1 year 2025 (per MudClock's
    // Default impl), so a tick-derived calculation would be off by
    // 12 hours from what Lua triggers / day-night gates / weather
    // see via time.hour. Single source of truth wins.
    let clock = world.resource::<mud_world::MudClock>();
    let mud_hour = i64::from(clock.hour);
    let mud_day = i64::from(clock.day);
    let mud_year = i64::from(clock.year);
    let month_name = clock.month_name();
    let season = clock.season().label();
    let period = match mud_hour {
        0..=4 => "deep night",
        5..=7 => "early morning",
        8..=11 => "morning",
        12..=13 => "midday",
        14..=17 => "afternoon",
        18..=20 => "evening",
        _ => "night",
    };
    let day_suffix = ordinal_suffix(mud_day);

    // Time readout: cyan section labels match the rest of the
    // static-info family. Real-world server time is reference info
    // (dimmed); world tick is debug-flavor (dimmed). Game time is
    // the focal line — bold-yellow on the calendar text plays into
    // the fantasy-flavor framing and matches the season hue used
    // in `weather`.
    let mut out = String::from("\r\n");
    out.push_str(&format!(
        "  <cyan>Server time:</> <dim>{}</>\r\n",
        now.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    out.push_str(&format!(
        "  <cyan>Uptime:</>      <b:cyan>{h}h {m}m {s}s</>\r\n"
    ));
    out.push_str(&format!("  <cyan>World tick:</>  <dim>{tick}</>\r\n"));
    out.push_str(&format!(
        "  <cyan>Game time:</>   <b:yellow>The {mud_day}{day_suffix} day of {month_name}, Year {mud_year}.</>\r\n",
    ));
    out.push_str(&format!(
        "               It is <b:yellow>{mud_hour:02}:00</> <dim>({period})</>; the season is <yellow>{season}</>.\r\n",
    ));
    send_to(world, player, out);
}

/// `weather`: render an atmospheric flavor line based on the player's
/// current zone's `Climate` and the in-game time of day. The
/// underlying weather model is rule-of-thumb only — there's no
/// per-tick simulation; same input gives the same output. Players
/// pull this when they want to feel the world's character; admins
/// could also use it as a quick climate-tag readout.
pub(crate) fn cmd_weather(world: &mut World, player: Entity, _args: &str) {
    use mud_db::enums::Climate;
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; the sky is blank.\r\n");
        return;
    };
    let room = located.0;
    let zone_id = world.get::<WorldKey>(room).map(|k| k.zone);
    let zone = zone_id.and_then(|z| world.resource::<WorldKeyIndex>().zones.get(&z).copied());
    let climate = zone.and_then(|z| world.get::<ZoneClimate>(z).map(|c| c.0));
    // Live state line from the per-zone catalog (drifts via
    // weather_tick). Falls back to climate-default if the catalog
    // hasn't been populated for some reason.
    let live_line = zone_id
        .and_then(|zid| {
            world
                .resource::<mud_world::WeatherCatalog>()
                .by_zone
                .get(&zid)
                .copied()
        })
        .map(crate::weather::describe);
    let mud_hour = world.resource::<mud_world::MudClock>().hour;
    let day = match mud_hour {
        0..=4 | 21..=23 => "night",
        5..=8 => "dawn",
        9..=11 | 14..=17 => "day",
        12..=13 => "midday",
        _ => "evening",
    };
    let line = match (climate, day) {
        (Some(Climate::Arid), "day" | "midday") => "Heat shimmers off the parched ground; the air is dry as bone.",
        (Some(Climate::Arid), _) => "The desert chill cuts through cloaks; stars wheel sharply overhead.",
        (Some(Climate::Semiarid), "day" | "midday") => "Dust dances on a warm wind; the sun bears down without mercy.",
        (Some(Climate::Semiarid), _) => "The scrub cools rapidly; far-off coyotes call.",
        (Some(Climate::Tropical), "day" | "midday") => "Humid air clings to your skin; a green-tinted sun hangs heavy.",
        (Some(Climate::Tropical), _) => "Frogs and night-birds compete in the dripping dark.",
        (Some(Climate::Subtropical), "day" | "midday") => "Warm gusts carry distant rain; cumulus clouds tower in lazy stacks.",
        (Some(Climate::Subtropical), _) => "Crickets thicken the air; warm mist drifts through the night.",
        (Some(Climate::Temperate), "dawn") => "A cool breeze rustles the leaves; dew glistens on every blade.",
        (Some(Climate::Temperate), "day" | "midday") => "Mild sun and a clean breeze; pleasant traveling weather.",
        (Some(Climate::Temperate), _) => "Stars glitter through clear, cool air.",
        (Some(Climate::Oceanic), "day" | "midday") => "Salt spray rides a steady wind; gulls wheel and complain.",
        (Some(Climate::Oceanic), _) => "Distant surf and sea-mist mute the night.",
        (Some(Climate::Subarctic), "day" | "midday") => "Pale sun glints off stubborn frost; your breath fogs the air.",
        (Some(Climate::Subarctic), _) => "Bitter cold seeps through every seam; aurora flickers overhead.",
        (Some(Climate::Arctic), "day" | "midday") => "Wind-driven snow blurs the horizon; the sun is a pale disc.",
        (Some(Climate::Arctic), _) => "The cold is absolute; ice creaks in the dark.",
        (Some(Climate::Alpine), "day" | "midday") => "Thin, sharp air bites at your lungs; the sun is fierce off the snow.",
        (Some(Climate::Alpine), _) => "The wind howls down the slopes; stars are knife-bright at altitude.",
        (Some(Climate::None) | None, _) => "The air is still and unremarkable.",
    };
    // Season-flavored closer. Climate-agnostic so adding a new
    // climate doesn't require updating four more lines — the climate
    // arm above does the heavy lifting; this just lands the calendar
    // on the readout. Hidden for Climate::None / unmapped rooms
    // (caves, planes) where seasons don't apply.
    let season_line: Option<&str> = match (climate, world.resource::<mud_world::MudClock>().season()) {
        (Some(Climate::None) | None, _) => None,
        (_, mud_world::Season::Winter) => Some("It is the depths of winter."),
        (_, mud_world::Season::Spring) => Some("Spring stirs the world toward new growth."),
        (_, mud_world::Season::Summer) => Some("The long days of summer hold sway."),
        (_, mud_world::Season::Autumn) => Some("Autumn paints the air with change."),
    };
    // Weather is atmospheric flavor — keep the body default-color
    // so authored climate lines read cleanly. The live drift line
    // (current state from WeatherCatalog) gets a cyan accent so
    // players can tell "this is what the sky is doing right now"
    // apart from the "this is what this climate feels like"
    // baseline. Season closer dims; it's a calendar gloss, not the
    // headline.
    let mut out = String::from("\r\n");
    if let Some(live) = live_line {
        out.push_str(&format!("<cyan>{live}</>\r\n"));
    }
    out.push_str(&format!("{line}\r\n"));
    if let Some(s) = season_line {
        out.push_str(&format!("<dim>{s}</>\r\n"));
    }
    send_to(world, player, out);
}

pub(crate) fn cmd_version(world: &mut World, player: Entity, _args: &str) {
    // Headline line: bold-cyan crate name + bold-yellow version so
    // the "what am I running" answer pops at the top of the
    // readout. Profile / tick fields are reference info — cyan
    // labels, dimmed values.
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let mut out = String::from("\r\n");
    out.push_str(&format!(
        "  <b:cyan>{}</> <b:yellow>{}</>\r\n",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
    ));
    out.push_str(&format!("  <cyan>Profile:</>   <dim>{profile}</>\r\n"));
    out.push_str(&format!(
        "  <cyan>Tick rate:</> <dim>{} Hz</>\r\n",
        crate::TICK_HZ
    ));
    send_to(world, player, out);
}

pub(crate) fn cmd_inventory(world: &mut World, player: Entity, args: &str) {
    // Optional substring filter so a packed bag is searchable —
    // `inv potion` collapses to just the potion-shaped items.
    // Match is case-insensitive against the rendered (color-
    // stripped) item name so "vial" works even when the proto's
    // name carries XML-Lite color tags.
    let filter = args.trim().to_ascii_lowercase();
    // Snapshot in two passes so we can group identical names into a
    // single "3x <name>" line. Order is preserved by tracking the
    // first-seen position so duplicates fold without scrambling.
    let items: Vec<String> = {
        let mut q = world
            .query_filtered::<(&Located, &Named, Option<&EquippedSlot>), With<Item>>();
        q.iter(world)
            .filter(|(l, _, eq)| l.0 == player && eq.is_none())
            .map(|(_, n, _)| n.name.clone())
            .filter(|name| {
                filter.is_empty()
                    || render_color_tags(name, ColorMode::Strip)
                        .to_ascii_lowercase()
                        .contains(&filter)
            })
            .collect()
    };
    let mut order: Vec<String> = Vec::new();
    let mut counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for name in &items {
        if !counts.contains_key(name) {
            order.push(name.clone());
        }
        *counts.entry(name.clone()).or_insert(0) += 1;
    }
    let weight = carried_weight(world, player);
    let mut raw = if items.is_empty() {
        if filter.is_empty() {
            "\r\nYou are carrying nothing.\r\n".to_string()
        } else {
            format!("\r\nYou aren't carrying anything matching '{filter}'.\r\n")
        }
    } else if filter.is_empty() {
        format!(
            "\r\n<b:cyan>You are carrying {} item(s):</>\r\n",
            items.len()
        )
    } else {
        format!(
            "\r\n<b:cyan>{} item(s) match '{filter}':</>\r\n",
            items.len(),
        )
    };
    for name in &order {
        let n = counts.get(name).copied().unwrap_or(1);
        if n > 1 {
            // Stack count dimmed so the eye lands on the item name.
            raw.push_str(&format!("  <dim>({n})</> {name}\r\n"));
        } else {
            raw.push_str(&format!("      {name}\r\n"));
        }
    }
    // Always show total carried weight when the player has any —
    // even when filtering — so the encumbrance picture stays
    // accurate regardless of which subset they're inspecting.
    if weight > 0.0 {
        let cap = carry_capacity(world, player);
        let band = encumbrance_band(weight, cap);
        // Same encumbrance gradient the score sheet uses (red ≥90%,
        // yellow ≥70%, plain otherwise) so a player checking
        // inventory immediately sees whether they're bumping their
        // move-stamina penalty bracket.
        let (open, close) = encumbrance_color_tag(weight, cap)
            .map_or((String::new(), String::new()), |t| {
                (t.to_string(), "</>".to_string())
            });
        raw.push_str(&format!(
            "\r\nTotal weight carried: {open}{weight:.1}{close} / {cap:.0} lbs.  ({open}{band}{close})\r\n",
        ));
    }
    send_rendered(world, player, &raw);
}

#[allow(clippy::too_many_lines)]
/// Pull a `CoinPile` off `container`, add the amount to `player`'s
/// `Wealth`, and remove the component. Returns `Some(amount)` when
/// a pile was drained; `None` when the container had no coin. Used
/// by the `get all from <container>` path so corpse-loot picks up
/// both items and coin in one command.
fn drain_coin_pile(world: &mut World, container: Entity, player: Entity) -> Option<i64> {
    let amount = world.get::<CoinPile>(container).map(|p| p.0)?;
    if amount <= 0 {
        try_remove::<CoinPile>(world, container);
        return None;
    }
    if let Some(mut w) = world.get_mut::<Wealth>(player) {
        w.0 = w.0.saturating_add(amount);
    } else {
        try_insert(world, player, Wealth(amount));
    }
    try_remove::<CoinPile>(world, container);
    Some(amount)
}

pub(crate) fn cmd_get(world: &mut World, player: Entity, args: &str) {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        send_to(world, player, "Get what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;

    // `get <item> from <container>` — pull from a container the
    // player is carrying or which sits in the room. `all` as the
    // item word loots everything inside.
    if let Some((needle, container_word)) = split_from_keyword(trimmed) {
        let container = find_in_room(world, container_word, room)
            .or_else(|| find_carried_by(world, container_word, player, EquipFilter::Anywhere));
        let Some(container) = container else {
            send_to(
                world,
                player,
                format!("You don't see '{container_word}' here.\r\n"),
            );
            return;
        };
        let container_name = name_of(world, container);
        let player_name = name_of(world, player);

        // Loot-claim gate: corpses with an active LootClaim refuse
        // anyone other than the owner until the window expires.
        // Past the deadline the component still exists; we just
        // don't enforce it. The despawn-on-decay path cleans it up.
        if let Some(claim) = world.get::<mud_world::LootClaim>(container).copied()
            && claim.expires_at > std::time::Instant::now()
            && claim.owner != player
        {
            let owner_name = name_or(world, claim.owner, "another");
            send_to(
                world,
                player,
                format!(
                    "{container_name} is claimed by {owner_name}; \
                     you cannot loot it yet.\r\n"
                ),
            );
            return;
        }

        // `get all from <container>`: snapshot every item inside,
        // re-Located to the player, broadcast a single line with the
        // count. Also drains a `CoinPile` if the container carries
        // one (e.g. a corpse from a non-AutoGold kill). Empty
        // containers report the obvious "nothing in there" rather
        // than failing the keyword lookup.
        if needle.eq_ignore_ascii_case("all") {
            let items: Vec<(Entity, String)> = {
                let mut q = world.query_filtered::<(Entity, &Located, &Named), With<Item>>();
                q.iter(world)
                    .filter(|(_, l, _)| l.0 == container)
                    .map(|(e, _, n)| (e, n.name.clone()))
                    .collect()
            };
            // Drain CoinPile first — independent of items so a
            // corpse that holds *only* coin (low-tier mob with no
            // gear) still completes meaningfully instead of
            // reporting "nothing in there".
            let coin_drained = drain_coin_pile(world, container, player);
            if let Some(amount) = coin_drained {
                let msg = render_color_tags(
                    &crate::commands::format_wealth(amount)
                        .unwrap_or_else(|| "no coin".to_string()),
                    color_mode_for(world, player),
                );
                send_rendered(
                    world,
                    player,
                    &format!("You collect {msg} from {container_name}.\r\n"),
                );
            }
            if items.is_empty() {
                if coin_drained.is_none() {
                    send_rendered(
                        world,
                        player,
                        &format!("There's nothing in {container_name}.\r\n"),
                    );
                }
                return;
            }
            let cap = carry_capacity(world, player);
            let mut running = carried_weight(world, player);
            let mut moved = 0usize;
            let mut skipped = 0usize;
            for (item, item_name) in &items {
                let w = item_weight(world, *item);
                if running + w > cap {
                    skipped += 1;
                    continue;
                }
                running += w;
                if let Some(mut l) = world.get_mut::<Located>(*item) {
                    l.0 = player;
                }
                send_rendered(
                    world,
                    player,
                    &format!("You take {item_name} from {container_name}.\r\n"),
                );
                crate::triggers::fire_item_event(
                    world,
                    *item,
                    player,
                    mud_world::TriggerEvent::Get,
                );
                if let Some(key) = world.get::<WorldKey>(*item).copied() {
                    bump_collect_quest_progress(world, player, key.zone, key.id);
                }
                moved += 1;
            }
            if moved > 0 {
                broadcast_room_except_rendered(
                    world,
                    room,
                    &[player],
                    &format!("{player_name} loots {moved} item(s) from {container_name}.\r\n"),
                );
            }
            if skipped > 0 {
                send_to(
                    world,
                    player,
                    format!("You're too encumbered to carry {skipped} more item(s).\r\n"),
                );
            }
            return;
        }

        let item = find_in_container(world, needle, container);
        let Some(item) = item else {
            send_rendered(world, player, &format!("There's no '{needle}' in {container_name}.\r\n"));
            return;
        };
        let item_name = name_of(world, item);
        if carried_weight(world, player) + item_weight(world, item)
            > carry_capacity(world, player)
        {
            send_rendered(
                world,
                player,
                &format!("{item_name} is too heavy — you'd be encumbered.\r\n"),
            );
            return;
        }
        if let Some(mut l) = world.get_mut::<Located>(item) {
            l.0 = player;
        }
        send_rendered(
            world,
            player,
            &format!("You take {item_name} from {container_name}.\r\n"),
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} takes {item_name} from {container_name}.\r\n"),
        );
        crate::triggers::fire_item_event(world, item, player, mud_world::TriggerEvent::Get);
        if let Some(key) = world.get::<WorldKey>(item).copied() {
            bump_collect_quest_progress(world, player, key.zone, key.id);
        }
        return;
    }

    // Plain `get <item>` from the floor.
    let item = find_in_room(world, trimmed, room);
    let Some(item) = item else {
        send_to(world, player, format!("You don't see '{trimmed}' here.\r\n"));
        return;
    };

    let item_name = name_of(world, item);
    let player_name = name_of(world, player);

    if carried_weight(world, player) + item_weight(world, item)
        > carry_capacity(world, player)
    {
        send_rendered(
            world,
            player,
            &format!("{item_name} is too heavy — you'd be encumbered.\r\n"),
        );
        return;
    }

    if let Some(mut l) = world.get_mut::<Located>(item) {
        l.0 = player;
    }

    send_rendered(world, player, &format!("You pick up {item_name}.\r\n"));
    broadcast_room_except_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} picks up {item_name}.\r\n"),
    );
    crate::triggers::fire_item_event(world, item, player, mud_world::TriggerEvent::Get);
    if let Some(key) = world.get::<WorldKey>(item).copied() {
        bump_collect_quest_progress(world, player, key.zone, key.id);
    }
}

/// `put <item> <container>`: move a carried item into a container
/// the player is carrying or which sits in the room.
pub(crate) fn cmd_put(world: &mut World, player: Entity, args: &str) {
    // Support both `put <item> <container>` and `put <item> in <container>`.
    // The "in" keyword form is natural and matches how players type it.
    let trimmed = args.trim();
    let (item_word, container_word) = if let Some(pair) = split_in_keyword(trimmed) {
        pair
    } else {
        let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
        if parts.len() != 2 || parts[1].trim().is_empty() {
            send_to(
                world,
                player,
                "Usage: put <item> in <container>\r\n",
            );
            return;
        }
        (parts[0].trim(), parts[1].trim())
    };

    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let player_name = name_of(world, player);

    let container = find_carried_by(world, container_word, player, EquipFilter::Anywhere)
        .or_else(|| find_in_room(world, container_word, room));
    let Some(container) = container else {
        send_rendered(
            world,
            player,
            &format!("You don't see '{container_word}' here.\r\n"),
        );
        return;
    };
    let container_name = name_of(world, container);

    // `put all in <container>` — store every carried (non-equipped)
    // item in the target. Skips the container itself.
    if item_word.eq_ignore_ascii_case("all") {
        let items: Vec<(Entity, String)> = {
            let mut q = world
                .query_filtered::<(Entity, &Located, &Named, Option<&EquippedSlot>), With<Item>>();
            q.iter(world)
                .filter(|(e, l, _, eq)| {
                    l.0 == player && eq.is_none() && *e != container
                })
                .map(|(e, _, n, _)| (e, n.name.clone()))
                .collect()
        };
        if items.is_empty() {
            send_to(world, player, "You aren't carrying anything to put away.\r\n");
            return;
        }
        let count = items.len();
        for (item, item_name) in &items {
            if let Some(mut l) = world.get_mut::<Located>(*item) {
                l.0 = container;
            }
            send_rendered(
                world,
                player,
                &format!("You put {item_name} in {container_name}.\r\n"),
            );
        }
        broadcast_room_except_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} puts {count} item(s) in {container_name}.\r\n"),
        );
        return;
    }

    let item = find_carried_by(world, item_word, player, EquipFilter::Inventory);
    let Some(item) = item else {
        send_rendered(
            world,
            player,
            &format!("You aren't carrying '{item_word}'.\r\n"),
        );
        return;
    };
    if container == item {
        send_to(world, player, "You can't put something inside itself.\r\n");
        return;
    }
    let item_name = name_of(world, item);
    if let Some(mut l) = world.get_mut::<Located>(item) {
        l.0 = container;
    }
    send_rendered(
        world,
        player,
        &format!("You put {item_name} in {container_name}.\r\n"),
    );
    broadcast_room_except_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} puts {item_name} in {container_name}.\r\n"),
    );
}

/// `junk <item>` / `trash <item>`: destroy a carried item. Equipped
/// items are refused — `remove` first. No coin is awarded; if the
/// player is throwing it away, they're throwing it away.
pub(crate) fn cmd_junk(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Junk what?\r\n");
        return;
    }
    let Some(item) = find_carried_by(world, target_word, player, EquipFilter::Inventory) else {
        send_rendered(
            world,
            player,
            &format!("You aren't carrying '{target_word}'.\r\n"),
        );
        return;
    };
    let item_name = name_of(world, item);
    let player_name = name_of(world, player);
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    if let Ok(e) = world.get_entity_mut(item) {
        e.despawn();
    }
    send_rendered(world, player, &format!("You destroy {item_name}.\r\n"));
    broadcast_room_except_rendered(
        world,
        located.0,
        &[player],
        &format!("{player_name} destroys {item_name}.\r\n"),
    );
}

/// `donate <item>`: drop an item with a charitable flavor. Without a
/// dedicated donation-room flag, donated items just land at the
/// player's feet — but the message reads as a giving gesture rather
/// than a discard, so admins / quest-givers can wire pickup
/// behavior on top later.
pub(crate) fn cmd_donate(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Donate what?\r\n");
        return;
    }
    let Some(item) = find_carried_by(world, target_word, player, EquipFilter::Inventory) else {
        send_rendered(
            world,
            player,
            &format!("You aren't carrying '{target_word}'.\r\n"),
        );
        return;
    };
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let item_name = name_of(world, item);
    let player_name = name_of(world, player);
    if let Some(mut l) = world.get_mut::<Located>(item) {
        l.0 = room;
    }
    send_rendered(
        world,
        player,
        &format!("You leave {item_name} for whoever might need it.\r\n"),
    );
    broadcast_room_except_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} donates {item_name}.\r\n"),
    );
}

pub(crate) fn cmd_drop(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Drop what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let player_name = name_of(world, player);

    // `drop all` — drop every carried (non-equipped) item.
    if target_word.eq_ignore_ascii_case("all") {
        let items: Vec<(Entity, String)> = {
            let mut q = world
                .query_filtered::<(Entity, &Located, &Named, Option<&EquippedSlot>), With<Item>>();
            q.iter(world)
                .filter(|(_, l, _, eq)| l.0 == player && eq.is_none())
                .map(|(e, _, n, _)| (e, n.name.clone()))
                .collect()
        };
        if items.is_empty() {
            send_to(world, player, "You aren't carrying anything to drop.\r\n");
            return;
        }
        let count = items.len();
        for (item, item_name) in &items {
            if let Some(mut l) = world.get_mut::<Located>(*item) {
                l.0 = room;
            }
            send_rendered(world, player, &format!("You drop {item_name}.\r\n"));
            crate::triggers::fire_item_event(world, *item, player, mud_world::TriggerEvent::Drop);
        }
        broadcast_room_except_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} drops {count} item(s).\r\n"),
        );
        return;
    }

    let item = find_carried_by(world, target_word, player, EquipFilter::Inventory);
    let Some(item) = item else {
        send_rendered(world, player, &format!("You aren't carrying '{target_word}'.\r\n"),
        );
        return;
    };

    let item_name = name_of(world, item);

    if let Some(mut l) = world.get_mut::<Located>(item) {
        l.0 = room;
    }

    send_rendered(world, player, &format!("You drop {item_name}.\r\n"));
    broadcast_room_except_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} drops {item_name}.\r\n"),
    );
    crate::triggers::fire_item_event(world, item, player, mud_world::TriggerEvent::Drop);
}

pub(crate) fn cmd_give(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[1].trim().is_empty() {
        send_to(world, player, "Usage: give <item> <target>\r\n");
        return;
    }
    let item_word = parts[0].trim();
    let target_word = parts[1].trim();

    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;

    let item = find_carried_by(world, item_word, player, EquipFilter::Inventory);
    let Some(item) = item else {
        send_rendered(world, player, &format!("You aren't carrying '{item_word}'.\r\n"),
        );
        return;
    };
    let target = find_actor_in_room(world, target_word, room, player);
    let Some(target) = target else {
        send_rendered(world, player, &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };

    let item_name = name_of(world, item);
    let target_name = name_of(world, target);
    let player_name = name_of(world, player);

    // Encumbrance gate on the recipient. Mobs (no Profile + no
    // capacity check) skip — they're carrying gear, not balancing
    // a budget — and a quest-turn-in mob would balk at otherwise
    // valid gifts. Player-to-player gifts respect the same load
    // cap a player would hit picking the item up off the floor.
    if world.get::<Player>(target).is_some()
        && carried_weight(world, target) + item_weight(world, item)
            > carry_capacity(world, target)
    {
        send_rendered(
            world,
            player,
            &format!("{target_name} is too laden to take {item_name}.\r\n"),
        );
        return;
    }

    if let Some(mut l) = world.get_mut::<Located>(item) {
        l.0 = target;
    }

    send_to(
        world,
        player,
        format!("You give {item_name} to {target_name}.\r\n"),
    );
    send_to(
        world,
        target,
        format!("{player_name} gives you {item_name}.\r\n"),
    );
    broadcast_room_except_rendered(
        world,
        room,
        &[player, target],
        &format!("{player_name} gives {item_name} to {target_name}.\r\n"),
    );

    // Fire RECEIVE triggers on the recipient. Bodies typically gate
    // on `object.id` to handle quest item turn-ins.
    crate::triggers::fire_receive(world, target, player, item);
    // DELIVER_ITEM objective progression. Only when the recipient
    // is a mob with a known prototype (player-to-player gifts
    // don't satisfy quest deliveries).
    if world.get::<Mob>(target).is_some()
        && let Some(item_key) = world.get::<WorldKey>(item).copied()
        && let Some(mob_key) = world.get::<WorldKey>(target).copied()
    {
        bump_deliver_quest_progress(
            world,
            player,
            item_key.zone,
            item_key.id,
            mob_key.zone,
            mob_key.id,
        );
    }
}

pub(crate) fn cmd_wear(world: &mut World, player: Entity, args: &str) {
    let trimmed = args.trim();
    // `wear all` — try to equip every carried wearable. Items whose
    // primary slot is already filled get skipped silently (a single
    // collective "couldn't wear N" line summarizes failures).
    if trimmed.eq_ignore_ascii_case("all") {
        let items: Vec<Entity> = {
            let mut q = world
                .query_filtered::<(Entity, &Located, Option<&EquippedSlot>, Option<&WearableIn>), With<Item>>();
            q.iter(world)
                .filter(|(_, l, eq, wi)| l.0 == player && eq.is_none() && wi.is_some())
                .map(|(e, _, _, _)| e)
                .collect()
        };
        if items.is_empty() {
            send_to(world, player, "You have nothing wearable in your inventory.\r\n");
            return;
        }
        // wear_into handles its own per-item messaging including
        // refusal lines for slot conflicts; we just feed it a name.
        for item in items {
            let name = name_of(world, item);
            wear_into(world, player, &name, None);
        }
        return;
    }
    wear_into(world, player, trimmed, None);
}

pub(crate) fn cmd_wield(world: &mut World, player: Entity, args: &str) {
    wear_into(world, player, args.trim(), Some(Slot::Wield));
}

pub(crate) fn cmd_hold(world: &mut World, player: Entity, args: &str) {
    wear_into(world, player, args.trim(), Some(Slot::Hold));
}

/// `light <item>`: mark a Light-type carried item as lit. Refused
/// on non-Light items or already-lit ones.
pub(crate) fn cmd_light(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Light what?\r\n");
        return;
    }
    let Some(item) = find_carried_by(world, target_word, player, EquipFilter::Anywhere) else {
        send_to(world, player, format!("You aren't carrying '{target_word}'.\r\n"));
        return;
    };
    let item_name = name_of(world, item);
    let kind = world
        .get::<WorldKey>(item)
        .and_then(|k| world.resource::<ObjectPrototypes>().by_key.get(&(k.zone, k.id)).map(|p| p.r#type));
    if kind != Some(mud_db::enums::ObjectType::Light) {
        send_to(world, player, format!("{item_name} isn't a light source.\r\n"));
        return;
    }
    if world.get::<mud_world::Lit>(item).is_some() {
        send_to(world, player, format!("{item_name} is already lit.\r\n"));
        return;
    }
    if let Ok(mut e) = world.get_entity_mut(item) {
        e.insert(mud_world::Lit);
    }
    send_rendered(world, player, &format!("You light {item_name}.\r\n"));
}

/// `extinguish <item>`: clear the Lit marker.
pub(crate) fn cmd_extinguish(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Extinguish what?\r\n");
        return;
    }
    let Some(item) = find_carried_by(world, target_word, player, EquipFilter::Anywhere) else {
        send_to(world, player, format!("You aren't carrying '{target_word}'.\r\n"));
        return;
    };
    let item_name = name_of(world, item);
    if world.get::<mud_world::Lit>(item).is_none() {
        send_to(world, player, format!("{item_name} isn't lit.\r\n"));
        return;
    }
    if let Ok(mut e) = world.get_entity_mut(item) {
        e.remove::<mud_world::Lit>();
    }
    send_rendered(world, player, &format!("You extinguish {item_name}.\r\n"));
}

/// `mount <mob>`: climb onto a mountable mob in the room. Installs
/// `Mounted(mob)` on the rider and `RiddenBy(rider)` on the mount;
/// movement (when the rider walks) carries the mount along. Refused
/// on non-mountable mobs, on already-ridden mounts, when the rider
/// is already mounted, or when the mob is in combat.
pub(crate) fn cmd_mount(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Mount what?\r\n");
        return;
    }
    if world.get::<mud_world::Mounted>(player).is_some() {
        send_to(world, player, "You're already mounted.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if world.get::<mud_world::Mountable>(target).is_none() {
        let n = name_of(world, target);
        send_rendered(world, player, &format!("You can't ride {n}.\r\n"));
        return;
    }
    if world.get::<mud_world::RiddenBy>(target).is_some() {
        let n = name_of(world, target);
        send_rendered(world, player, &format!("{n} is already being ridden.\r\n"));
        return;
    }
    if world.get::<Fighting>(target).is_some() {
        send_to(world, player, "It's struggling too much to mount.\r\n");
        return;
    }
    try_insert(world, player, mud_world::Mounted(target));
    try_insert(world, target, mud_world::RiddenBy(player));
    let mover = name_of(world, player);
    let mount_name = name_of(world, target);
    send_rendered(world, player, &format!("You mount {mount_name}.\r\n"));
    broadcast_room_except_players_rendered(
        world,
        located.0,
        &[player],
        &format!("{mover} mounts {mount_name}.\r\n"),
    );
}

/// `dismount`: get off your current mount. Clears `Mounted` /
/// `RiddenBy` on both sides. No-op when not mounted.
pub(crate) fn cmd_dismount(world: &mut World, player: Entity, _args: &str) {
    let Some(mud_world::Mounted(mount)) = world.get::<mud_world::Mounted>(player).copied() else {
        send_to(world, player, "You aren't riding anything.\r\n");
        return;
    };
    try_remove::<mud_world::Mounted>(world, player);
    try_remove::<mud_world::RiddenBy>(world, mount);
    let mount_name = name_of(world, mount);
    let mover = name_of(world, player);
    send_rendered(world, player, &format!("You dismount from {mount_name}.\r\n"));
    if let Some(located) = world.get::<Located>(player).copied() {
        broadcast_room_except_players_rendered(
            world,
            located.0,
            &[player],
            &format!("{mover} dismounts from {mount_name}.\r\n"),
        );
    }
}

/// `fly`: take to the air. Inserts the `Flying` marker. While flying,
/// movement charges a flat 2 stamina per move (sector-cost flattens
/// to 1, plus a +1 wing-flap) — great over water/swamp, slightly
/// pricier on roads. `walk` / `land` clears the marker.
pub(crate) fn cmd_fly(world: &mut World, player: Entity, _args: &str) {
    if world.get::<mud_world::Flying>(player).is_some() {
        send_to(world, player, "You're already flying.\r\n");
        return;
    }
    try_insert(world, player, mud_world::Flying);
    let mover_name = name_of(world, player);
    send_to(world, player, "You spread your wings and take to the air.\r\n");
    if let Some(located) = world.get::<Located>(player).copied() {
        broadcast_room_except_players_rendered(
            world,
            located.0,
            &[player],
            &format!("{mover_name} takes to the air.\r\n"),
        );
    }
}

/// `walk` / `land`: clear the `Flying` marker.
pub(crate) fn cmd_walk(world: &mut World, player: Entity, _args: &str) {
    if world.get::<mud_world::Flying>(player).is_none() {
        send_to(world, player, "You're already on the ground.\r\n");
        return;
    }
    try_remove::<mud_world::Flying>(world, player);
    let mover_name = name_of(world, player);
    send_to(world, player, "You touch down and start walking again.\r\n");
    if let Some(located) = world.get::<Located>(player).copied() {
        broadcast_room_except_players_rendered(
            world,
            located.0,
            &[player],
            &format!("{mover_name} lands and starts walking.\r\n"),
        );
    }
}

/// `hide`: set the `Stealth` marker on the player. Today this just
/// flips the `hidden` symbol in damage formulas (BACKSTAB's bonus
/// reads it) — there's no auto-fail on noisy actions, no skill check,
/// and no visibility filtering in `look` yet. Those land with the
/// rogue skill tree. The verb works so muscle memory is preserved.
pub(crate) fn cmd_hide(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Stealth>(player).is_some() {
        send_to(world, player, "You're already hidden.\r\n");
        return;
    }
    try_insert(world, player, Stealth);
    send_to(
        world,
        player,
        "You attempt to slip into the shadows.\r\n",
    );
}

/// `visible` / `vis`: clear the `Stealth` marker. Always succeeds.
pub(crate) fn cmd_visible(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Stealth>(player).is_none() {
        send_to(world, player, "You're already visible.\r\n");
        return;
    }
    try_remove::<Stealth>(world, player);
    send_to(world, player, "You stop hiding.\r\n");
}

pub(crate) fn cmd_eat(world: &mut World, player: Entity, args: &str) {
    if consume_item(world, player, args, mud_db::enums::ObjectType::Food, "eat")
        && let Some(mut h) = world.get_mut::<mud_world::Hunger>(player)
    {
        // v1: any Food fully sates. Legacy CircleMUD's per-food
        // `fill` attribute can refine in a follow-up.
        h.0 = 0;
    }
}

pub(crate) fn cmd_quaff(world: &mut World, player: Entity, args: &str) {
    consume_item(world, player, args, mud_db::enums::ObjectType::Potion, "quaff");
}

pub(crate) fn cmd_drink(world: &mut World, player: Entity, args: &str) {
    drink_amount(world, player, args, 4, "drink");
}

pub(crate) fn cmd_sip(world: &mut World, player: Entity, args: &str) {
    drink_amount(world, player, args, 1, "sip");
}

/// `pour <container> [target]`: transfer liquid from a held
/// container. With no target, empties to the floor. With a target
/// container, transfers as much as the target can accept (limited
/// by capacity − remaining). Liquid types must match — pouring
/// water into wine refuses.
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_pour(world: &mut World, player: Entity, args: &str) {
    let mut parts = args.split_whitespace();
    let Some(src_word) = parts.next() else {
        send_to(world, player, "Usage: pour <container> [target]\r\n");
        return;
    };
    let target_word = parts.next();
    let Some(src) = find_carried_by(world, src_word, player, EquipFilter::Anywhere) else {
        send_to(world, player, format!("You aren't carrying '{src_word}'.\r\n"));
        return;
    };
    let src_name = name_of(world, src);
    let Some(src_state) = world.get::<mud_world::LiquidContainer>(src).cloned() else {
        send_rendered(
            world,
            player,
            &format!("{src_name} isn't a drink container.\r\n"),
        );
        return;
    };
    if src_state.remaining <= 0 {
        send_rendered(world, player, &format!("{src_name} is already empty.\r\n"));
        return;
    }
    // No target: empty the source onto the floor.
    let Some(target_word) = target_word else {
        if let Some(mut lc) = world.get_mut::<mud_world::LiquidContainer>(src) {
            lc.remaining = 0;
        }
        let liquid_lc = src_state.liquid.to_ascii_lowercase();
        send_rendered(
            world,
            player,
            &format!("You pour the {liquid_lc} from {src_name} onto the ground.\r\n"),
        );
        return;
    };
    let Some(dest) = find_carried_by(world, target_word, player, EquipFilter::Anywhere) else {
        send_to(world, player, format!("You aren't carrying '{target_word}'.\r\n"));
        return;
    };
    if dest == src {
        send_to(world, player, "You can't pour something into itself.\r\n");
        return;
    }
    let dest_name = name_of(world, dest);
    let Some(dest_state) = world.get::<mud_world::LiquidContainer>(dest).cloned() else {
        send_rendered(
            world,
            player,
            &format!("{dest_name} isn't a drink container.\r\n"),
        );
        return;
    };
    let dest_room = dest_state.capacity - dest_state.remaining;
    if dest_room <= 0 {
        send_rendered(world, player, &format!("{dest_name} is full.\r\n"));
        return;
    }
    // Empty destination: takes the source's liquid type and any
    // poison flag. Non-empty: must match liquid type, refuse on
    // mismatch.
    let same_liquid = dest_state.remaining == 0
        || dest_state.liquid.eq_ignore_ascii_case(&src_state.liquid);
    if !same_liquid {
        send_rendered(
            world,
            player,
            &format!("{dest_name} already holds something else.\r\n"),
        );
        return;
    }
    let amount = dest_room.min(src_state.remaining);
    if let Some(mut s) = world.get_mut::<mud_world::LiquidContainer>(src) {
        s.remaining -= amount;
    }
    if let Some(mut d) = world.get_mut::<mud_world::LiquidContainer>(dest) {
        if d.remaining == 0 {
            d.liquid.clone_from(&src_state.liquid);
            d.poisoned = src_state.poisoned;
        } else if src_state.poisoned {
            // Poisoning spreads when topping up a non-poisoned with
            // poisoned: any bad liquid contaminates the lot.
            d.poisoned = true;
        }
        d.remaining += amount;
    }
    let liquid_lc = src_state.liquid.to_ascii_lowercase();
    send_rendered(
        world,
        player,
        &format!("You pour {amount} units of {liquid_lc} from {src_name} into {dest_name}.\r\n"),
    );
}

/// `fill <container> <source>`: top up the destination from the
/// source. Inverse of `pour`. Same liquid-match rules apply.
pub(crate) fn cmd_fill(world: &mut World, player: Entity, args: &str) {
    let mut parts = args.split_whitespace();
    let Some(dest_word) = parts.next() else {
        send_to(world, player, "Usage: fill <container> <source>\r\n");
        return;
    };
    let Some(src_word) = parts.next() else {
        send_to(world, player, "Fill from what?\r\n");
        return;
    };
    // Reuse pour's exact logic (source first arg) by swapping order.
    cmd_pour(world, player, &format!("{src_word} {dest_word}"));
}

/// `taste <container>`: identify the liquid without drinking. No
/// state mutation, no consumption. On poisoned containers, gives a
/// "tastes off" hint.
pub(crate) fn cmd_taste(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Taste what?\r\n");
        return;
    }
    let Some(item) = find_carried_by(world, target_word, player, EquipFilter::Anywhere) else {
        send_to(world, player, format!("You aren't carrying '{target_word}'.\r\n"));
        return;
    };
    let item_name = name_of(world, item);
    let Some(state) = world.get::<mud_world::LiquidContainer>(item).cloned() else {
        send_rendered(
            world,
            player,
            &format!("{item_name} isn't a drink container.\r\n"),
        );
        return;
    };
    let liquid_lc = state.liquid.to_ascii_lowercase();
    if state.remaining <= 0 {
        send_rendered(
            world,
            player,
            &format!("{item_name} is empty — nothing to taste.\r\n"),
        );
        return;
    }
    send_rendered(
        world,
        player,
        &format!("It tastes like {liquid_lc}.\r\n"),
    );
    if state.poisoned {
        send_to(
            world,
            player,
            "...with an unpleasant aftertaste. Probably poisoned.\r\n",
        );
    }
}

pub(crate) fn cmd_recite(world: &mut World, player: Entity, args: &str) {
    invoke_object_abilities(
        world,
        player,
        args,
        mud_db::enums::ObjectType::Scroll,
        "recite",
        "You read aloud from",
        true,
    );
}

pub(crate) fn cmd_wave(world: &mut World, player: Entity, args: &str) {
    invoke_object_abilities(
        world,
        player,
        args,
        mud_db::enums::ObjectType::Wand,
        "wave",
        "You wave",
        false,
    );
}

pub(crate) fn cmd_tap(world: &mut World, player: Entity, args: &str) {
    invoke_object_abilities(
        world,
        player,
        args,
        mud_db::enums::ObjectType::Staff,
        "tap",
        "You tap",
        false,
    );
}

pub(crate) fn cmd_remove(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Remove what?\r\n");
        return;
    }
    // `remove all` — strip every equipped item.
    if target_word.eq_ignore_ascii_case("all") {
        let items: Vec<(Entity, String)> = {
            let mut q = world
                .query_filtered::<(Entity, &Located, &Named, &EquippedSlot), With<Item>>();
            q.iter(world)
                .filter(|(_, l, _, _)| l.0 == player)
                .map(|(e, _, n, _)| (e, n.name.clone()))
                .collect()
        };
        if items.is_empty() {
            send_to(world, player, "You aren't wearing anything.\r\n");
            return;
        }
        for (item, item_name) in &items {
            try_remove::<EquippedSlot>(world, *item);
            send_rendered(world, player, &format!("You remove {item_name}.\r\n"));
            crate::triggers::fire_item_event(
                world,
                *item,
                player,
                mud_world::TriggerEvent::Remove,
            );
        }
        return;
    }
    let item = find_carried_by(world, target_word, player, EquipFilter::Equipped);
    let Some(item) = item else {
        send_rendered(world, player, &format!("You aren't wearing '{target_word}'.\r\n"),
        );
        return;
    };
    let item_name = name_of(world, item);
    try_remove::<EquippedSlot>(world, item);
    send_rendered(world, player, &format!("You remove {item_name}.\r\n"));
    crate::triggers::fire_item_event(world, item, player, mud_world::TriggerEvent::Remove);
}

pub(crate) fn cmd_equipment(world: &mut World, player: Entity, _args: &str) {
    // Snapshot (slot, name, weight) per worn item. Weight comes from
    // the proto via WorldKey; synthetic items without a proto count
    // as 0 (matches the carried_weight contract).
    let mut by_slot: Vec<(Slot, String, f64)> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &Named, &EquippedSlot),
            With<Item>,
        >();
        q.iter(world)
            .filter(|(_, l, _, _)| l.0 == player)
            .map(|(e, _, n, eq)| (eq.0, n.name.clone(), item_weight(world, e)))
            .collect()
    };
    if by_slot.is_empty() {
        send_to(world, player, "\r\nYou aren't wearing anything.\r\n");
        return;
    }
    by_slot.sort_by_key(|(s, _, _)| Slot::ORDER.iter().position(|x| x == s).unwrap_or(usize::MAX));
    let total_weight: f64 = by_slot.iter().map(|(_, _, w)| w).sum();
    let mut out = String::from("\r\n<b:cyan>Equipment:</>\r\n");
    for (slot, name, weight) in &by_slot {
        // Slot label dimmed so the eye lands on the item name.
        // Weight in parentheses also dimmed — supplemental data.
        let weight_label = if *weight > 0.0 {
            format!(" <dim>({weight:.1} lbs)</>")
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  <cyan>{:>14}</>: {}{}\r\n",
            slot.label(),
            name,
            weight_label,
        ));
    }
    if total_weight > 0.0 {
        out.push_str(&format!(
            "\r\n<dim>Total worn weight: {total_weight:.1} lbs.</>\r\n",
        ));
    }
    send_to(world, player, out);
}

/// `cooldowns` / `cd`: list active ability cooldowns for the player.
/// Reads the `Cooldowns` component (set by `invoke_ability` after a
/// successful cast). Stale entries (`ready_at` in the past) are
/// skipped — they're effectively expired even if not pruned yet.
pub(crate) fn cmd_cooldowns(world: &mut World, player: Entity, _args: &str) {
    let now = std::time::Instant::now();
    let mut active: Vec<(String, f32)> = {
        let Some(cd) = world.get::<Cooldowns>(player) else {
            send_to(world, player, "\r\nNo abilities are on cooldown.\r\n");
            return;
        };
        let catalog = world.resource::<AbilityCatalog>();
        cd.ready_at
            .iter()
            .filter(|(_, ready)| **ready > now)
            .map(|(id, ready)| {
                let name = catalog
                    .by_name
                    .values()
                    .find(|d| d.id == *id)
                    .map_or_else(|| format!("ability #{id}"), |d| d.plain_name.clone());
                let remaining = ready.saturating_duration_since(now).as_secs_f32();
                (name, remaining)
            })
            .collect()
    };
    if active.is_empty() {
        send_to(world, player, "\r\nNo abilities are on cooldown.\r\n");
        return;
    }
    // Sort by descending remaining time so the longest is on top —
    // matches what players want to see ("how long until I can do this
    // big thing again?").
    active.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = format!("\r\n{} ability/abilities on cooldown:\r\n", active.len());
    for (name, remaining) in active {
        // Sub-minute cooldowns deserve fractional seconds so the
        // player sees the precise re-arm point for tight rotations
        // (kick, backstab); longer cooldowns fall back to the
        // human-friendly "Xm" / "Xh" shape used elsewhere.
        let label = if remaining < 60.0 {
            format!("{remaining:.1}s")
        } else {
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let secs = remaining.max(0.0) as u64;
            format_idle(secs)
        };
        out.push_str(&format!("  {name:<24} {label} remaining\r\n"));
    }
    send_to(world, player, out);
}

/// `cancel [<effect>]`: drop a non-permanent effect from yourself.
/// Empty arg lists cancellable effects; named arg matches by
/// case-insensitive substring on the effect's name.
pub(crate) fn cmd_effects(world: &mut World, player: Entity, _args: &str) {
    // Snapshot effects on the player; pull the optional ModifyDelta
    // companion in the same query so the renderer can show
    // "ward (+60) (2245s)" for stat-bonus effects.
    let active: Vec<(String, i32, Option<i32>, Option<i32>)> = {
        let mut q =
            world.query::<(&EffectInstance, &AppliedTo, Option<&mud_world::ModifyDelta>)>();
        q.iter(world)
            .filter(|(_, a, _)| a.0 == player)
            .map(|(inst, _, delta)| {
                (
                    inst.name.clone(),
                    inst.remaining_secs,
                    inst.ability_id,
                    delta.map(|d| d.amount),
                )
            })
            .collect()
    };
    let mut out = if active.is_empty() {
        "\r\nYou have no active effects.\r\n".to_string()
    } else {
        format!("\r\n<b:cyan>{} active effect(s):</>\r\n", active.len())
    };
    let catalog = world.resource::<AbilityCatalog>();
    for (name, remaining, ability_id, delta_amount) in active {
        // Look up the spawning ability's plain_name when known so
        // players can see "bleed (45s) — from REND" instead of
        // just the bare effect tag.
        let from = ability_id.and_then(|id| {
            catalog
                .by_name
                .values()
                .find(|d| d.id == id)
                .map(|d| d.plain_name.clone())
        });
        // Source attribution dimmed — supplemental, not the focus.
        let suffix = from
            .as_deref()
            .map_or(String::new(), |n| format!(" <dim>— from {n}</>"));
        // Modifier delta colored by sign — green for buffs, red for
        // debuffs. A bless (+2 STR) reads green; a curse (-3 DEX)
        // reads red. Player can scan the list and immediately see
        // which way each effect is pulling them.
        let delta_label = delta_amount.map_or(String::new(), |a| {
            let (sign, color) = if a >= 0 {
                ("+", "<green>")
            } else {
                ("", "<red>")
            };
            format!(" {color}({sign}{a})</>")
        });
        if remaining < 0 {
            // Permanent effects (innate racials, divine boons, etc.)
            // get a bold-cyan tag — they're not on the clock.
            out.push_str(&format!(
                "  <b:cyan>{name}</>{delta_label} <b:cyan>(permanent)</>{suffix}\r\n"
            ));
        } else {
            // Render long durations as "37m" / "2h15m" instead of
            // raw "2245s remaining". Color-graded by `effect_duration_color`
            // so a buff about to expire reads warm and the player
            // notices in time to refresh.
            #[allow(clippy::cast_sign_loss)]
            let secs = remaining.max(0) as u64;
            let dur_open = effect_duration_color(secs);
            let dur_label = match dur_open {
                Some(open) => format!("{open}{}</>", format_idle(secs)),
                None => format_idle(secs),
            };
            out.push_str(&format!(
                "  <cyan>{name}</>{delta_label} ({dur_label} remaining){suffix}\r\n"
            ));
        }
    }
    send_to(world, player, out);
}

/// `achievements [<category>]` — list achievements grouped by
/// category. Unlocked ones show their title + description; locked
/// (and not hidden) ones show a placeholder; hidden ones are
/// suppressed until unlocked.
pub(crate) fn cmd_achievements(world: &mut World, player: Entity, args: &str) {
    use mud_db::enums::AchievementCategory;
    let filter = args.trim().to_ascii_lowercase();
    let unlocked = world
        .get::<mud_world::CharacterAchievements>(player)
        .map(|c| c.unlocked.clone())
        .unwrap_or_default();
    let catalog = world.resource::<mud_world::AchievementCatalog>();
    let mut entries: Vec<&mud_world::AchievementDef> = catalog.by_id.values().collect();
    entries.sort_by_key(|d| (d.category as i32, d.sort_order, d.id));
    let want_cat: Option<AchievementCategory> = match filter.as_str() {
        "" | "all" => None,
        "combat" => Some(AchievementCategory::Combat),
        "exploration" => Some(AchievementCategory::Exploration),
        "social" => Some(AchievementCategory::Social),
        "crafting" => Some(AchievementCategory::Crafting),
        "misc" => Some(AchievementCategory::Misc),
        other => {
            send_to(
                world,
                player,
                format!("Unknown category '{other}'. Try: all, combat, exploration, social, crafting, misc.\r\n"),
            );
            return;
        }
    };
    let mut out = String::from("\r\nAchievements:\r\n");
    let mut current_cat: Option<AchievementCategory> = None;
    let mut shown = 0;
    let total_unlocked = unlocked.len();
    for def in entries {
        if let Some(want) = want_cat
            && def.category != want
        {
            continue;
        }
        let is_unlocked = unlocked.contains(&def.id);
        if def.hidden && !is_unlocked {
            continue;
        }
        if current_cat != Some(def.category) {
            current_cat = Some(def.category);
            out.push_str(&format!("\r\n  --- {} ---\r\n", def.category.label()));
        }
        let mark = if is_unlocked { "[*]" } else { "[ ]" };
        out.push_str(&format!(
            "  {mark} {} — {}\r\n",
            def.title, def.description,
        ));
        shown += 1;
    }
    if shown == 0 {
        out.push_str("  (none visible)\r\n");
    }
    out.push_str(&format!(
        "\r\n{total_unlocked} unlocked of {} total.\r\n",
        catalog.by_id.len()
    ));
    send_to(world, player, out);
}

/// `house [subcommand]` — read-only inspection of the player's
/// house. v1: info / rooms / guests subcommands. Mutating
/// commands (place, remove, expand, name) and the `home` /
/// `visit` traversal commands land in subsequent slices.
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_house(world: &mut World, player: Entity, args: &str) {
    let sub = args.trim().to_ascii_lowercase();
    let sub = if sub.is_empty() { "info" } else { sub.as_str() };
    let house = world.get::<mud_world::HouseSummary>(player).cloned();
    let Some(house) = house else {
        send_to(
            world,
            player,
            "You don't own a house. Speak with a builder to claim one.\r\n",
        );
        return;
    };
    let mut out = String::from("\r\n");
    match sub {
        "enter" => {
            cmd_home(world, player, "");
            return;
        }
        "info" => {
            out.push_str(&format!("House #{}\r\n", house.house_id));
            out.push_str(&format!(
                "Entrance: zone {} room {}\r\n",
                house.entrance_room.zone, house.entrance_room.id
            ));
            if let Some(rr) = house.return_room {
                out.push_str(&format!(
                    "Return-on-exit: zone {} room {}\r\n",
                    rr.zone, rr.id
                ));
            }
            out.push_str(&format!("Rooms: {}\r\n", house.rooms.len()));
            out.push_str(&format!("Items placed: {}\r\n", house.items.len()));
            out.push_str(&format!("Guests: {}\r\n", house.guests.len()));
        }
        "rooms" => {
            if house.rooms.is_empty() {
                out.push_str("Your house has no rooms — that shouldn't happen.\r\n");
            } else {
                out.push_str(&format!("{} room(s):\r\n", house.rooms.len()));
                for r in &house.rooms {
                    let item_count = house.items.iter().filter(|i| i.room_id == r.id).count();
                    out.push_str(&format!(
                        "  [{:>2}] {} ({} item(s), capacity {}{})\r\n",
                        r.local_index,
                        r.name,
                        item_count,
                        r.capacity,
                        if r.is_peaceful { ", peaceful" } else { "" },
                    ));
                }
            }
        }
        "guests" => {
            if house.guests.is_empty() {
                out.push_str("No guests on your access list.\r\n");
            } else {
                out.push_str(&format!("{} guest(s):\r\n", house.guests.len()));
                for g in &house.guests {
                    out.push_str(&format!(
                        "  {} ({})\r\n",
                        g.character_id,
                        if g.can_place { "can place items" } else { "visit only" },
                    ));
                }
            }
        }
        s if s.starts_with("place ") || s == "place" => {
            let rest = args.trim().trim_start_matches("place").trim();
            cmd_house_place(world, player, &house, rest);
            return;
        }
        s if s.starts_with("take ") || s == "take" => {
            let rest = args.trim().trim_start_matches("take").trim();
            cmd_house_take(world, player, &house, rest);
            return;
        }
        s if s.starts_with("guest") => {
            let rest = args.trim().trim_start_matches("guest").trim();
            cmd_house_guest(world, player, &house, rest);
            return;
        }
        s if s.starts_with("rename ") => {
            let rest = args.trim().trim_start_matches("rename").trim();
            cmd_house_rename(world, player, &house, rest, false);
            return;
        }
        s if s.starts_with("describe ") || s.starts_with("redesc ") => {
            let rest = if s.starts_with("describe ") {
                args.trim().trim_start_matches("describe").trim()
            } else {
                args.trim().trim_start_matches("redesc").trim()
            };
            cmd_house_rename(world, player, &house, rest, true);
            return;
        }
        other => {
            out.push_str(&format!(
                "Unknown subcommand '{other}'. Try `house info`, `house rooms`, `house guests`, `house place <item>`, `house take <item>`, `house guest add <name> [place]`, `house guest remove <name>`, `house rename <#> <name>`, `house describe <#> <text>`.\r\n"
            ));
        }
    }
    send_to(world, player, out);
}

/// `level`: print level / XP / next-level delta.
pub(crate) fn cmd_level(world: &mut World, player: Entity, _args: &str) {
    use mud_world::LevelTable;
    let Some(p) = world.get::<Profile>(player) else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    let level = p.level;
    let xp = p.experience;
    let table = world.resource::<LevelTable>();
    let level_name = table.name_for(level);
    let prev_threshold = table.exp_for(level).unwrap_or(0);
    let next_threshold = table.exp_for(level + 1);
    let mut out = format!("\r\n{level_name} (level {level})\r\n");
    out.push_str(&format!("Experience: {xp}\r\n"));
    if let Some(threshold) = next_threshold {
        let to_go = (threshold - xp).max(0);
        let next_name = table.name_for(level + 1);
        // Progress bar within the current bracket. Uses the live
        // LevelTable thresholds rather than the score sheet's
        // legacy `level^2.5 * 1000` curve so the percent here
        // matches what the level table actually requires for
        // this character class. Visual format mirrors score.
        let bracket = (threshold - prev_threshold).max(1);
        let into_bracket = (xp - prev_threshold).max(0);
        let percent = ((i64::from(into_bracket) * 100) / i64::from(bracket)).clamp(0, 100);
        let percent_i32 = i32::try_from(percent).unwrap_or(0);
        out.push_str(&format!(
            "Progress: {} {percent_i32}%  ({xp} / {threshold})\r\n",
            crate::commands::progress_bar(percent_i32),
        ));
        out.push_str(&format!(
            "Next level ({next_name}, level {next_level}) at {threshold} XP — {to_go} to go.\r\n",
            next_level = level + 1
        ));
    } else {
        out.push_str("You are at the maximum level.\r\n");
    }
    send_to(world, player, out);
}

/// `slots`: display the player's per-circle slot count along with
/// how many are currently memorized. Format: `Circle N: used/max`.
pub(crate) fn cmd_slots(world: &mut World, player: Entity, _args: &str) {
    use mud_world::{MemorizedSpells, SpellSlotData};
    let Some(profile) = world.get::<Profile>(player) else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    let level = profile.level;
    let Some(class_id) = profile.class_id else {
        send_to(world, player, "You have no class — no spell slots.\r\n");
        return;
    };
    let class_name = world
        .resource::<ClassCatalog>()
        .by_id
        .get(&class_id)
        .map_or_else(|| format!("class {class_id}"), |c| c.plain_name.clone());
    let slots = world.resource::<SpellSlotData>().slots_for(class_id, level);
    if slots.is_empty() {
        send_to(
            world,
            player,
            format!("\r\nLevel {level} {class_name} — no accessible spell circles.\r\n"),
        );
        return;
    }
    let mem = world.get::<MemorizedSpells>(player).cloned().unwrap_or_default();
    let mut out = format!("\r\nLevel {level} {class_name} spell slots:\r\n");
    for (circle, max) in slots {
        let used = mem.used_in_circle(circle);
        let ready = mem.ready_in_circle(circle);
        let preparing = used - ready;
        if preparing > 0 {
            out.push_str(&format!(
                "  Circle {circle:>2}: {ready:>2} ready + {preparing} preparing / {max:>2}\r\n"
            ));
        } else {
            out.push_str(&format!(
                "  Circle {circle:>2}: {ready:>2} ready / {max:>2}\r\n"
            ));
        }
    }
    send_to(world, player, out);
}

pub(crate) fn cmd_spells(world: &mut World, player: Entity, args: &str) {
    use mud_db::abilities::AbilityKind;

    let mode = color_mode_for(world, player);
    // Argument shape:
    //   `spells`               → known abilities only
    //   `spells all`           → full catalog (builders / curiosity)
    //   `spells <substring>`   → filter known by substring
    //   `spells all <substr>`  → filter full catalog by substring
    let raw = args.trim().to_ascii_lowercase();
    let (show_all, filter) = if let Some(rest) = raw.strip_prefix("all") {
        (true, rest.trim().to_string())
    } else {
        (false, raw)
    };

    // Known set is the default scope. `spells all` overrides to the
    // full catalog regardless of what the player has trained.
    let known: Option<std::collections::HashSet<i32>> = if show_all {
        None
    } else {
        Some(
            world
                .get::<KnownAbilities>(player)
                .map_or_else(std::collections::HashSet::new, |k| {
                    k.entries.iter().map(|(id, _, _)| *id).collect()
                }),
        )
    };

    let mut buckets: std::collections::BTreeMap<&'static str, Vec<String>> =
        std::collections::BTreeMap::new();
    for def in world.resource::<AbilityCatalog>().by_name.values() {
        if let Some(set) = &known
            && !set.contains(&def.id)
        {
            continue;
        }
        // Filter matches if the substring lands on either the
        // ability name OR its sphere — so `spells fire` shows
        // both Fireball (name match) and Burning Hands (sphere
        // match), letting players cluster by elemental theme
        // without learning a separate keyword.
        if !filter.is_empty()
            && !def.plain_name.to_ascii_lowercase().contains(&filter)
            && def.sphere.as_deref().is_none_or(|s| s != filter)
        {
            continue;
        }
        let bucket = match def.kind {
            AbilityKind::Spell => "Spells",
            AbilityKind::Chant => "Chants",
            AbilityKind::Song => "Songs",
            AbilityKind::Skill => "Skills",
        };
        // Append the sphere as a dim parenthetical so the listing
        // doubles as an elemental-affinity scan. Skipped when the
        // ability has no sphere assigned (most SKILLs).
        let entry = format_ability_with_sphere(def);
        buckets.entry(bucket).or_default().push(entry);
    }
    if buckets.is_empty() {
        if show_all {
            // Catalog dump explicitly requested but produced
            // nothing — likely a substring filter that matched
            // zero rows.
            if filter.is_empty() {
                send_to(world, player, "\r\nNo abilities loaded.\r\n");
            } else {
                send_rendered(
                    world,
                    player,
                    &format!(
                        "\r\nNo abilities matching '{filter}' loaded.\r\n"
                    ),
                );
            }
        } else if filter.is_empty() {
            send_to(
                world,
                player,
                "\r\n<dim>You haven't learned any abilities yet.</> \
                 Type `spells all` to browse the catalog.\r\n",
            );
        } else {
            send_rendered(
                world,
                player,
                &format!(
                    "\r\nNo abilities matching '{filter}' in your known list. \
                     Try `spells all {filter}`.\r\n"
                ),
            );
        }
        return;
    }

    let header = if show_all {
        "<b:cyan>All loaded abilities</>"
    } else {
        "<b:cyan>Abilities you know</>"
    };
    let mut out = format!("\r\n{header}:\r\n");
    for (bucket, names) in &mut buckets {
        names.sort_unstable();
        out.push_str(&format!(
            "<b:yellow>{}</> <dim>({})</>:\r\n",
            bucket,
            names.len()
        ));
        let column_width = name_column_width(names);
        for chunk in names.chunks(3) {
            out.push_str("  ");
            for n in chunk {
                let rendered = render_color_tags(n, mode);
                let padded = pad_visible(&rendered, column_width);
                out.push_str(&padded);
            }
            out.push_str("\r\n");
        }
    }
    send_to(world, player, out);
}

/// Render an ability's display name plus a colored parenthetical
/// for its sphere when one is assigned. Lets the spells / chants
/// / songs / skills listings double as an elemental-affinity scan
/// without forcing players to run `identify` per spell. Sphere
/// hue picks from the palette in `sphere_color_tag`
/// (fire=red, water=cyan, healing=green, etc.); unmapped or
/// missing spheres fall through to dim. Abilities without a
/// sphere assignment render the name unchanged.
fn format_ability_with_sphere(def: &mud_world::AbilityDef) -> String {
    let Some(s) = def.sphere.as_deref().filter(|s| !s.is_empty()) else {
        return def.name.clone();
    };
    let open = sphere_color_tag(s).unwrap_or("<dim>");
    format!("{} {open}({s})</>", def.name)
}

/// Pick the column width for an ability-list grid. Sized to the
/// widest *visible* name in the page plus a 2-space gutter, with a
/// 22-char floor so short-name pages don't render too cramped.
fn name_column_width(names: &[String]) -> usize {
    const MIN_WIDTH: usize = 22;
    const GUTTER: usize = 2;
    let widest = names
        .iter()
        .map(|n| visible_width(n))
        .max()
        .unwrap_or(MIN_WIDTH);
    widest.saturating_add(GUTTER).max(MIN_WIDTH)
}

pub(crate) fn cmd_skills(world: &mut World, player: Entity, args: &str) {
    cmd_abilities_kind(world, player, args, mud_db::abilities::AbilityKind::Skill);
}

pub(crate) fn cmd_songs(world: &mut World, player: Entity, args: &str) {
    cmd_abilities_kind(world, player, args, mud_db::abilities::AbilityKind::Song);
}

pub(crate) fn cmd_chants(world: &mut World, player: Entity, args: &str) {
    cmd_abilities_kind(world, player, args, mud_db::abilities::AbilityKind::Chant);
}

/// `invite <player>`: send a group invite. Recipient gets a
/// `GroupInvite` component carrying the inviter's entity; their
/// `accept` will install Follower(self) for the sender.
pub(crate) fn cmd_invite(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Invite whom?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "You can't invite yourself.\r\n");
        return;
    }
    if world.get::<Player>(target).is_none() {
        send_to(world, player, "You can only invite other players.\r\n");
        return;
    }
    if world.get::<Follower>(target).is_some_and(|f| f.0 == player) {
        let n = name_of(world, target);
        send_rendered(
            world,
            player,
            &format!("{n} is already in your group.\r\n"),
        );
        return;
    }
    try_insert(
        world,
        target,
        mud_world::GroupInvite {
            from: player,
            at: std::time::Instant::now(),
        },
    );
    let inviter = name_of(world, player);
    let target_name = name_of(world, target);
    send_rendered(
        world,
        player,
        &format!("You invite {target_name} to your group.\r\n"),
    );
    send_rendered(
        world,
        target,
        &format!(
            "{inviter} invites you to a group. Type `accept` to join, \
             `decline` to refuse.\r\n"
        ),
    );
}

/// `accept`: accept the most recent group invite. Installs a
/// `Follower(inviter)` on the caller (matching the existing
/// follow-chain group model). Refused if the invite has expired
/// (older than 5 minutes), the inviter has disconnected, or no
/// invite exists.
pub(crate) fn cmd_accept(world: &mut World, player: Entity, _args: &str) {
    let Some(invite) = world.get::<mud_world::GroupInvite>(player).copied() else {
        send_to(
            world,
            player,
            "You have no pending group invites.\r\n",
        );
        return;
    };
    // 5-minute expiry on invites — keeps the marker from sticking
    // around indefinitely.
    if invite.at.elapsed() > std::time::Duration::from_secs(300) {
        try_remove::<mud_world::GroupInvite>(world, player);
        send_to(world, player, "Your invite has expired.\r\n");
        return;
    }
    if world.get_entity(invite.from).is_err() {
        try_remove::<mud_world::GroupInvite>(world, player);
        send_to(world, player, "The inviter has gone away.\r\n");
        return;
    }
    if would_create_cycle(world, invite.from, player) {
        try_remove::<mud_world::GroupInvite>(world, player);
        send_to(
            world,
            player,
            "Joining that group would create a follow cycle — refused.\r\n",
        );
        return;
    }
    try_insert(world, player, Follower(invite.from));
    try_remove::<mud_world::GroupInvite>(world, player);
    let inviter_name = name_of(world, invite.from);
    let player_name = name_of(world, player);
    send_rendered(
        world,
        player,
        &format!("You join {inviter_name}'s group.\r\n"),
    );
    send_rendered(
        world,
        invite.from,
        &format!("{player_name} joins your group.\r\n"),
    );
}

/// `decline`: discard the pending group invite without joining.
pub(crate) fn cmd_decline(world: &mut World, player: Entity, _args: &str) {
    let Some(invite) = world.get::<mud_world::GroupInvite>(player).copied() else {
        send_to(world, player, "You have no pending group invites.\r\n");
        return;
    };
    try_remove::<mud_world::GroupInvite>(world, player);
    let inviter_alive = world.get_entity(invite.from).is_ok();
    send_to(world, player, "You decline the invite.\r\n");
    if inviter_alive {
        let player_name = name_of(world, player);
        send_rendered(
            world,
            invite.from,
            &format!("{player_name} declines your group invite.\r\n"),
        );
    }
}

/// `group` (no args): list the player's current group — everyone
/// transitively connected via `Follower` chains rooted at the
/// chain's top. The leader is shown first, followed by members
/// indented. With a single entity (no followers / not following),
/// reports "you're not in a group."
///
/// `group dismiss <name>`: remove a single direct follower (the
/// surgical version of `disband`). The named player must currently
/// be following the caller.
pub(crate) fn cmd_group(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if let Some(rest) = arg.strip_prefix("dismiss") {
        let target_word = rest.trim();
        group_dismiss_one(world, player, target_word);
        return;
    }
    if !arg.is_empty() {
        send_to(
            world,
            player,
            "Usage: `group` (list) or `group dismiss <player>`.\r\n",
        );
        return;
    }
    let root = group_root(world, player);
    let members = group_members(world, root);
    if members.len() <= 1 {
        send_to(world, player, "You're not in a group.\r\n");
        return;
    }
    // Group roster: bold-cyan title with member count dimmed. Per
    // line: leader tag bold-yellow (the only one that's special),
    // member tag dimmed; cyan name; HP wrapped in vital-color so
    // a wounded party member reads red at a glance; here/elsewhere
    // colored green/dim so "who's actually with me right now"
    // jumps out without parsing every line. Column padding is done
    // on the *plain* strings before color-wrapping so the visible
    // alignment is correct after the renderer strips/emits tags.
    let mut out = format!(
        "\r\n<b:cyan>Group</> <dim>({} members)</>\r\n",
        members.len()
    );
    let my_room = world.get::<Located>(player).map(|l| l.0);
    for (i, m) in members.iter().enumerate() {
        let name = name_of(world, *m);
        let role_word = if i == 0 { "leader" } else { "member" };
        let role_open = if i == 0 { "<b:yellow>" } else { "<dim>" };
        let role_inner = format!("[{role_word:<6}]");
        let role_text = format!("{role_open}{role_inner}</>");
        let name_padded = format!("{name:<20}");
        let name_text = format!("<cyan>{name_padded}</>");
        let hp_inner = world
            .get::<Health>(*m)
            .map_or(String::new(), |h| format!("HP {}/{}", h.hp, h.max));
        let hp_padded = format!("{hp_inner:<14}");
        let hp_open = world
            .get::<Health>(*m)
            .and_then(|h| vital_color_tag(h.hp, h.max));
        let hp_text = hp_open.map_or(hp_padded.clone(), |open| {
            format!("{open}{hp_padded}</>")
        });
        let here_text = match (my_room, world.get::<Located>(*m).map(|l| l.0)) {
            (Some(mine), Some(theirs)) if mine == theirs => "<green>here</>",
            _ => "<dim>elsewhere</>",
        };
        out.push_str(&format!(
            "  {role_text} {name_text} {hp_text} ({here_text})\r\n"
        ));
    }
    send_to(world, player, out);
}

/// `order <follower|all> <command>`: forwards a command to a mob
/// follower of the caller. Resolves the named mob (must be in the
/// same room and pointing `Follower(player)` at the caller); `all`
/// reaches every same-room mob follower. The mob runs the command
/// via the normal dispatcher — admin gates still apply (mobs only
/// reach Player-level commands).
pub(crate) fn cmd_order(world: &mut World, player: Entity, args: &str) {
    let mut parts = args.trim().splitn(2, char::is_whitespace);
    let target_word = parts.next().unwrap_or("");
    let cmd_text = parts.next().unwrap_or("").trim();
    if target_word.is_empty() || cmd_text.is_empty() {
        send_to(
            world,
            player,
            "Usage: order <follower|all> <command>\r\n",
        );
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You're nowhere.\r\n");
        return;
    };
    let room = located.0;

    // Mob followers in the same room pointing Follower(player) at
    // the caller. Players following you are NOT touched; `order` is
    // a charm/pet thing.
    let followers: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located, &Follower), With<Mob>>();
        q.iter(world)
            .filter(|(_, l, f)| l.0 == room && f.0 == player)
            .map(|(e, _, _)| e)
            .collect()
    };
    if followers.is_empty() {
        send_to(world, player, "You have no followers here to order.\r\n");
        return;
    }

    let chosen: Vec<Entity> = if target_word.eq_ignore_ascii_case("all")
        || target_word.eq_ignore_ascii_case("followers")
    {
        followers
    } else {
        let needle = target_word.to_ascii_lowercase();
        let one = followers
            .into_iter()
            .find(|e| {
                world
                    .get::<Named>(*e)
                    .is_some_and(|n| n.name.to_ascii_lowercase().contains(&needle))
                    || world.get::<Keywords>(*e).is_some_and(|k| {
                        k.0.iter().any(|w| w.to_ascii_lowercase().contains(&needle))
                    })
            });
        let Some(one) = one else {
            send_to(
                world,
                player,
                format!("'{target_word}' isn't a follower of yours here.\r\n"),
            );
            return;
        };
        vec![one]
    };

    let player_name = name_of(world, player);
    for mob in &chosen {
        let mob_name = name_of(world, *mob);
        send_to(
            world,
            player,
            format!("You order {mob_name} to: {cmd_text}\r\n"),
        );
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} orders {mob_name} to: {cmd_text}\r\n"),
        );
        dispatch(world, *mob, cmd_text);
    }
}

/// `dismiss <player>`: top-level alias for `group dismiss <player>`.
pub(crate) fn cmd_dismiss(world: &mut World, player: Entity, args: &str) {
    group_dismiss_one(world, player, args.trim());
}

/// `split <amount>`: pull `<amount>` from the caller's `Wealth` and
/// distribute it evenly across every group member currently in the
/// same room (including the caller). Remainder stays with the caller.
pub(crate) fn cmd_split(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let Ok(amount) = arg.parse::<i64>() else {
        send_to(world, player, "Usage: split <amount>\r\n");
        return;
    };
    if amount <= 0 {
        send_to(world, player, "Split a positive amount.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You're nowhere.\r\n");
        return;
    };
    let wealth = world.get::<Wealth>(player).map_or(0, |w| w.0);
    if amount > wealth {
        send_to(
            world,
            player,
            format!("You only have {wealth} coppers to split.\r\n"),
        );
        return;
    }
    let root = group_root(world, player);
    let members = group_members(world, root);
    let here: Vec<Entity> = members
        .into_iter()
        .filter(|m| world.get::<Located>(*m).is_some_and(|l| l.0 == located.0))
        .collect();
    if here.len() <= 1 {
        send_to(
            world,
            player,
            "There's nobody else from your group here.\r\n",
        );
        return;
    }
    #[allow(clippy::cast_possible_wrap)]
    let count = here.len() as i64;
    let share = amount / count;
    if share <= 0 {
        send_to(
            world,
            player,
            "Splitting that few coppers across the group leaves nothing for anyone.\r\n",
        );
        return;
    }
    let mut total_given = 0_i64;
    for m in &here {
        if *m == player {
            continue;
        }
        if let Some(mut w) = world.get_mut::<Wealth>(*m) {
            w.0 = w.0.saturating_add(share);
        } else if let Ok(mut e) = world.get_entity_mut(*m) {
            e.insert(Wealth(share));
        }
        total_given += share;
    }
    if let Some(mut w) = world.get_mut::<Wealth>(player) {
        w.0 = w.0.saturating_sub(total_given);
    }
    let player_name = name_of(world, player);
    send_to(
        world,
        player,
        format!(
            "You split {amount} coppers among {} member(s); each receives {share}.\r\n",
            here.len()
        ),
    );
    for m in &here {
        if *m == player {
            continue;
        }
        send_to(
            world,
            *m,
            format!("{player_name} splits coin with the group; you gain {share} copper(s).\r\n"),
        );
    }
}

/// `disband`: clear every direct `Follower(self)` link, breaking the
/// group apart. Members deeper in the chain stay connected to each
/// other unless they too disband. Self has no Follower component to
/// touch — only entities pointing at self.
pub(crate) fn cmd_disband(world: &mut World, player: Entity, _args: &str) {
    let to_release: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Follower), With<Player>>();
        q.iter(world)
            .filter(|(_, f)| f.0 == player)
            .map(|(e, _)| e)
            .collect()
    };
    if to_release.is_empty() {
        send_to(world, player, "Nobody is following you.\r\n");
        return;
    }
    let player_name = name_of(world, player);
    for member in &to_release {
        try_remove::<Follower>(world, *member);
        let m_name = name_of(world, *member);
        send_rendered(
            world,
            *member,
            &format!("{player_name} dismisses you from the group.\r\n"),
        );
        send_rendered(
            world,
            player,
            &format!("You dismiss {m_name} from the group.\r\n"),
        );
    }
}

pub(crate) fn cmd_follow(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Follow whom?\r\n");
        return;
    }
    if target_word.eq_ignore_ascii_case("self") || target_word.eq_ignore_ascii_case("me") {
        cmd_unfollow(world, player, "");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let target = find_actor_in_room(world, target_word, located.0, player);
    let Some(target) = target else {
        send_to(world, player, format!("You don't see '{target_word}' here.\r\n"));
        return;
    };

    // Cycle guard: if target already follows us (or any chain leading back to
    // us), refuse — keeps cmd_move's BFS terminating.
    if would_create_cycle(world, target, player) {
        send_to(
            world,
            player,
            "That would create a follow cycle.\r\n",
        );
        return;
    }

    try_insert(world, player, Follower(target));
    let target_name = name_of(world, target);
    let player_name = name_of(world, player);
    send_rendered(world, player, &format!("You start following {target_name}.\r\n"));
    send_rendered(
        world,
        target,
        &format!("{player_name} starts following you.\r\n"),
    );
}

pub(crate) fn cmd_unfollow(world: &mut World, player: Entity, _args: &str) {
    let prev = world.get::<Follower>(player).copied();
    try_remove::<Follower>(world, player);
    if let Some(Follower(prev_target)) = prev {
        let target_name = name_of(world, prev_target);
        send_rendered(world, player, &format!("You stop following {target_name}.\r\n"));
        let player_name = name_of(world, player);
        send_rendered(
            world,
            prev_target,
            &format!("{player_name} stops following you.\r\n"),
        );
    } else {
        send_to(world, player, "You weren't following anyone.\r\n");
    }
}

/// `identify <item>`: dump proto + runtime state for a carried item.
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_identify(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Identify what?\r\n");
        return;
    }
    let Some(item) = find_carried_by(world, needle, player, EquipFilter::Anywhere) else {
        send_rendered(
            world,
            player,
            &format!("You aren't carrying '{needle}'.\r\n"),
        );
        return;
    };
    let item_name = name_of(world, item);
    let key = world.get::<WorldKey>(item).copied();
    let Some(key) = key else {
        send_rendered(
            world,
            player,
            &format!("{item_name} has no proto link.\r\n"),
        );
        return;
    };
    let proto = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(key.zone, key.id))
        .cloned();
    let Some(p) = proto else {
        send_rendered(
            world,
            player,
            &format!("No prototype data for {item_name}.\r\n"),
        );
        return;
    };
    let mode = color_mode_for(world, player);
    let mut out = String::from("\r\n<b:cyan>Identify</>\r\n");
    out.push_str(&format!(
        "  <cyan>Item:</>      <b:cyan>{}</>\r\n",
        render_color_tags(&p.name, mode)
    ));
    out.push_str(&format!("  <cyan>Type:</>      {:?}\r\n", p.r#type));
    out.push_str(&format!(
        "  <cyan>Weight:</>    <dim>{:.1}</>\r\n",
        p.weight
    ));
    // Level 0 is the schema default — don't pad the readout with
    // "Level: 0" when the proto carries no requirement.
    if p.level > 0 {
        out.push_str(&format!("  <cyan>Level:</>     {}\r\n", p.level));
    }
    if p.cost > 0
        && let Some(coin) = format_wealth(i64::from(p.cost))
    {
        out.push_str(&format!("  <cyan>Value:</>     {coin}\r\n"));
    }
    if !p.wear_flags.is_empty() {
        let labels: Vec<String> = p.wear_flags.iter().map(|f| format!("{f:?}")).collect();
        out.push_str(&format!(
            "  <cyan>Wear:</>      <dim>{}</>\r\n",
            labels.join(", ")
        ));
    }
    if p.weapon_dice_num > 0 {
        // Match compare's bonus formatting: positive bonus gets a
        // "+", negative shows verbatim, zero hides. Keeps the line
        // readable for "1d8" weapons that don't carry a flat bonus.
        let bonus = match p.weapon_dice_bonus.cmp(&0) {
            std::cmp::Ordering::Equal => String::new(),
            std::cmp::Ordering::Greater => format!("+{}", p.weapon_dice_bonus),
            std::cmp::Ordering::Less => format!("{}", p.weapon_dice_bonus),
        };
        out.push_str(&format!(
            "  <cyan>Damage:</>    <yellow>{}d{}{bonus}</>  <dim>(avg {})</>\r\n",
            p.weapon_dice_num,
            p.weapon_dice_size,
            p.avg_damage(),
        ));
    }
    if let Some(liq) = &p.liquid {
        let state = world.get::<mud_world::LiquidContainer>(item).cloned();
        let (remaining, capacity) =
            state.as_ref().map_or((liq.remaining, liq.capacity), |s| (s.remaining, s.capacity));
        out.push_str(&format!(
            "  <cyan>Liquid:</>    {} <dim>({}/{})</>{}\r\n",
            liq.liquid,
            remaining,
            capacity,
            if state.as_ref().is_some_and(|s| s.poisoned) {
                " <b:red>— POISONED</>"
            } else {
                ""
            }
        ));
    }

    // Bound abilities (scrolls, wands, staves).
    let bindings = world
        .resource::<mud_world::ObjectAbilityCatalog>()
        .by_key
        .get(&(key.zone, key.id))
        .cloned()
        .unwrap_or_default();
    if !bindings.is_empty() {
        out.push_str("  <cyan>Bound abilities:</>\r\n");
        let abilities = world.resource::<AbilityCatalog>();
        for b in bindings {
            // Render through the same sphere-coloring formatter as
            // the spells listing so identify reads consistently —
            // `Magic Missile (force)` with the parenthetical in
            // the sphere's hue. Falls back to "ability #N" when
            // the catalog has no row for the binding (orphan).
            let entry = abilities
                .by_name
                .values()
                .find(|d| d.id == b.ability_id)
                .map_or_else(
                    || format!("ability #{}", b.ability_id),
                    format_ability_with_sphere,
                );
            let charges = b
                .charges
                .map_or_else(|| "unlimited".to_string(), |c| format!("{c} charges"));
            out.push_str(&format!(
                "    <dim>·</> {entry} <dim>(level {}, {charges})</>\r\n",
                b.level
            ));
        }
    }
    if let Some(c) = world.get::<mud_world::Charges>(item) {
        out.push_str(&format!("  <cyan>Charges remaining:</> {}\r\n", c.0));
    }

    // Active effects on the item (rare today; surfaces if any are
    // applied via consume/quaff bindings later).
    let item_effects: Vec<String> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, a)| a.0 == item)
            .map(|(inst, _)| inst.name.clone())
            .collect()
    };
    if !item_effects.is_empty() {
        out.push_str(&format!(
            "  <cyan>Effects:</>   {}\r\n",
            item_effects.join(", ")
        ));
    }

    send_rendered(world, player, &out);
}

/// `home` — teleport to the foyer of the player's house. Lazily
/// synthesizes ECS Room entities for each `PlayerHouseRoom` on
/// first call (cached in `HousingIndex`); subsequent calls
/// just look up and move.
pub(crate) fn cmd_home(world: &mut World, player: Entity, _args: &str) {
    let summary = world.get::<mud_world::HouseSummary>(player).cloned();
    let Some(summary) = summary else {
        send_to(
            world,
            player,
            "You don't own a house. Speak with a builder to claim one.\r\n",
        );
        return;
    };
    if summary.rooms.is_empty() {
        send_to(
            world,
            player,
            "Your house has no rooms — that shouldn't happen.\r\n",
        );
        return;
    }

    // Spawn missing rooms. The HousingIndex gates so we don't
    // double-spawn on subsequent `home` calls.
    let house_id = summary.house_id;
    let already_spawned = world
        .resource::<mud_world::HousingIndex>()
        .by_key
        .contains_key(&(house_id, summary.rooms[0].local_index));
    if !already_spawned {
        synthesize_house_rooms(world, &summary);
    }

    // Look up the foyer (local_index 0; falls back to first room
    // if the foyer is missing).
    let foyer_idx = summary
        .rooms
        .iter()
        .find(|r| r.local_index == 0)
        .map_or(summary.rooms[0].local_index, |r| r.local_index);
    let foyer_entity = world
        .resource::<mud_world::HousingIndex>()
        .by_key
        .get(&(house_id, foyer_idx))
        .copied();
    let Some(foyer) = foyer_entity else {
        send_to(world, player, "Your house couldn't be reached.\r\n");
        return;
    };

    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = foyer;
    }
    send_to(world, player, "You return home.\r\n");
    cmd_look(world, player, "");
}

/// `house place <item>` — moves an item from the player's
/// inventory into the current house room (player must be standing
/// in a room of *their own* house). Persists via `PlayerHouseItem`
/// fire-and-forget; the in-memory entity gets a `HouseItem(row_id)`
/// component once the insert completes so `house take` can find
/// the FK row.
pub(crate) fn cmd_house_place(
    world: &mut World,
    player: Entity,
    house: &mud_world::HouseSummary,
    target_word: &str,
) {
    if target_word.is_empty() {
        send_to(world, player, "Place what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    // Owner-room gate: must be in a room belonging to *this* house.
    let house_room = match world.get::<mud_world::HouseRoom>(room) {
        Some(hr) if hr.house_id == house.house_id => *hr,
        Some(_) => {
            send_to(
                world,
                player,
                "You can only place items in your own house.\r\n",
            );
            return;
        }
        None => {
            send_to(
                world,
                player,
                "You can only place items inside your house. Try `house enter` first.\r\n",
            );
            return;
        }
    };
    let Some(room_row_id) = house
        .rooms
        .iter()
        .find(|r| r.local_index == house_room.local_index)
        .map(|r| r.id)
    else {
        send_to(world, player, "Couldn't resolve this house room.\r\n");
        return;
    };
    let item = find_carried_by(world, target_word, player, EquipFilter::Inventory);
    let Some(item) = item else {
        send_rendered(
            world,
            player,
            &format!("You aren't carrying '{target_word}'.\r\n"),
        );
        return;
    };
    let Some(proto_key) = world.get::<WorldKey>(item).copied() else {
        send_to(
            world,
            player,
            "That item has no prototype — it can't be placed.\r\n",
        );
        return;
    };
    let item_name = name_of(world, item);
    if let Some(mut l) = world.get_mut::<Located>(item) {
        l.0 = room;
    }
    send_rendered(
        world,
        player,
        &format!("You place {item_name} in your house.\r\n"),
    );
    // Fire-and-forget DB insert; the returned id is attached to
    // the entity so a later `house take` can DELETE the row.
    if let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) {
        let outbound_player = player;
        tokio::spawn(async move {
            match mud_db::housing::place_item(
                &pool,
                room_row_id,
                proto_key.zone,
                proto_key.id,
            )
            .await
            {
                Ok(id) => {
                    tracing::debug!(
                        ?outbound_player,
                        item_id = id,
                        "house item placed"
                    );
                    // Note: we don't flow the id back into the
                    // entity here (would need a world handle).
                    // The component is attached on next login when
                    // the row reloads via spawn_house_item.
                }
                Err(e) => {
                    tracing::warn!(error = %e, "house item place failed");
                }
            }
        });
    }
}

/// `house take <item>` — pick up a placed item back into the
/// player's inventory. Requires the item to carry a
/// `HouseItem(row_id)` component (which only fully-loaded house
/// items have — items placed *this session* won't be takeable
/// until next login, see the comment in `cmd_house_place`).
pub(crate) fn cmd_house_take(
    world: &mut World,
    player: Entity,
    house: &mud_world::HouseSummary,
    target_word: &str,
) {
    if target_word.is_empty() {
        send_to(world, player, "Take what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    match world.get::<mud_world::HouseRoom>(room) {
        Some(hr) if hr.house_id == house.house_id => {}
        _ => {
            send_to(
                world,
                player,
                "You can only take items inside your house.\r\n",
            );
            return;
        }
    }
    // Find a HouseItem in this room matching the keyword.
    let matched: Option<(Entity, i32, String)> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &Named, Option<&Keywords>, &mud_world::HouseItem),
            With<Item>,
        >();
        q.iter(world)
            .filter(|(_, l, _, _, _)| l.0 == room)
            .find(|(_, _, named, kw, _)| name_or_keyword_matches(target_word, &named.name, *kw))
            .map(|(e, _, n, _, hi)| (e, hi.0, n.name.clone()))
    };
    let Some((item, house_item_id, item_name)) = matched else {
        send_rendered(
            world,
            player,
            &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    if let Some(mut l) = world.get_mut::<Located>(item) {
        l.0 = player;
    }
    // Strip the FK so the item is now an ordinary carried item.
    if let Ok(mut e) = world.get_entity_mut(item) {
        e.remove::<mud_world::HouseItem>();
    }
    send_rendered(
        world,
        player,
        &format!("You take {item_name} from the room.\r\n"),
    );
    if let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) {
        tokio::spawn(async move {
            if let Err(e) = mud_db::housing::remove_item(&pool, house_item_id).await {
                tracing::warn!(error = %e, "house item remove failed");
            }
        });
    }
}

/// `house rename <#> <new name>` and `house describe <#> <text>`.
/// Single helper for both — `is_description=true` writes the
/// description column instead of the name. The local index `#`
/// must be a room belonging to *this* house.
pub(crate) fn cmd_house_rename(
    world: &mut World,
    player: Entity,
    house: &mud_world::HouseSummary,
    args: &str,
    is_description: bool,
) {
    let mut parts = args.splitn(2, char::is_whitespace);
    let idx_str = parts.next().unwrap_or("");
    let new_text = parts.next().unwrap_or("").trim();
    let Ok(local_idx) = idx_str.parse::<i32>() else {
        send_to(
            world,
            player,
            "Usage: house rename <local-index> <new name>\r\n       house describe <local-index> <text>\r\n",
        );
        return;
    };
    if new_text.is_empty() {
        send_to(world, player, "New text can't be empty.\r\n");
        return;
    }
    let Some(room_id) = house
        .rooms
        .iter()
        .find(|r| r.local_index == local_idx)
        .map(|r| r.id)
    else {
        send_to(
            world,
            player,
            format!("No room #{local_idx} in your house.\r\n"),
        );
        return;
    };
    // Mirror the change onto the live ECS room entity (if it's
    // currently synthesized) so the change is visible without an
    // exit-and-re-enter dance.
    let entity = world
        .get_resource::<mud_world::HousingIndex>()
        .and_then(|hi| hi.by_key.get(&(house.house_id, local_idx)).copied());
    if let Some(entity) = entity {
        if is_description {
            try_insert(world, entity, Description(new_text.to_string()));
        } else if let Some(mut named) = world.get_mut::<Named>(entity) {
            named.name = new_text.to_string();
        }
    }
    let new_text_owned = new_text.to_string();
    if let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) {
        let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
        tokio::spawn(async move {
            let res = if is_description {
                mud_db::housing::rename_room(&pool, room_id, None, Some(&new_text_owned)).await
            } else {
                mud_db::housing::rename_room(&pool, room_id, Some(&new_text_owned), None).await
            };
            if let Some(out) = outbound {
                match res {
                    Ok(_) => {
                        let label = if is_description { "description" } else { "name" };
                        let _ = out
                            .send(format!("Updated room #{local_idx} {label}.\r\n").into_bytes());
                    }
                    Err(e) => {
                        let _ = out
                            .send(format!("DB write failed: {e}\r\n").into_bytes());
                    }
                }
            }
        });
    }
}

/// `house guest add <name> [place]` and `house guest remove <name>`.
/// `add` defaults to "visit only"; trailing `place` flips the
/// `can_place` flag so the guest can drop items in your rooms too.
/// DB lookup against `Characters.name` (case-insensitive).
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_house_guest(
    world: &mut World,
    player: Entity,
    house: &mud_world::HouseSummary,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let action = parts.next().unwrap_or("");
    let name = parts.next().unwrap_or("");
    let modifier = parts.next().unwrap_or("");
    if name.is_empty() {
        send_to(
            world,
            player,
            "Usage: house guest add <name> [place]\r\n       house guest remove <name>\r\n",
        );
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        send_to(world, player, "Database unavailable.\r\n");
        return;
    };
    let house_id = house.house_id;
    let name = name.to_string();
    let can_place = modifier.eq_ignore_ascii_case("place")
        || modifier.eq_ignore_ascii_case("can_place");
    let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
    match action {
        "add" => {
            tokio::spawn(async move {
                let row = match mud_db::characters::find_by_name(&pool, &name).await {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        if let Some(out) = outbound {
                            let _ = out
                                .send(format!("No character named '{name}'.\r\n").into_bytes());
                        }
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "guest lookup failed");
                        return;
                    }
                };
                match mud_db::housing::add_guest(&pool, house_id, &row.id, can_place).await {
                    Ok(_) => {
                        if let Some(out) = outbound {
                            let suffix = if can_place {
                                " (with place permission)"
                            } else {
                                ""
                            };
                            let _ = out
                                .send(
                                    format!(
                                        "{} added to your guest list{suffix}.\r\n",
                                        row.name
                                    )
                                    .into_bytes(),
                                );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "guest add failed");
                    }
                }
            });
        }
        "remove" | "rm" | "del" => {
            tokio::spawn(async move {
                let row = match mud_db::characters::find_by_name(&pool, &name).await {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        if let Some(out) = outbound {
                            let _ = out
                                .send(format!("No character named '{name}'.\r\n").into_bytes());
                        }
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "guest lookup failed");
                        return;
                    }
                };
                match mud_db::housing::remove_guest(&pool, house_id, &row.id).await {
                    Ok(0) => {
                        if let Some(out) = outbound {
                            let _ = out
                                .send(
                                    format!(
                                        "{} wasn't on your guest list.\r\n",
                                        row.name
                                    )
                                    .into_bytes(),
                                );
                        }
                    }
                    Ok(_) => {
                        if let Some(out) = outbound {
                            let _ = out.send(
                                format!("{} removed from your guest list.\r\n", row.name)
                                    .into_bytes(),
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "guest remove failed");
                    }
                }
            });
        }
        other => {
            send_to(
                world,
                player,
                format!("Unknown guest action '{other}'. Use `add` or `remove`.\r\n"),
            );
        }
    }
}

/// Kind-filtered listing for `skills` / `songs` / `chants`. Walks
/// the ability catalog like `cmd_spells` but restricts to a single
/// `AbilityKind`. Honors `KnownAbilities` gating and the optional
/// substring filter (passed as `args`).
pub(crate) fn cmd_abilities_kind(
    world: &mut World,
    player: Entity,
    args: &str,
    kind: mud_db::abilities::AbilityKind,
) {
    let mode = color_mode_for(world, player);
    let kind_label = kind.label();
    // Argument shape mirrors cmd_spells: bare = known, `all` =
    // catalog, optional substring filter on either scope.
    let raw = args.trim().to_ascii_lowercase();
    let (show_all, filter) = if let Some(rest) = raw.strip_prefix("all") {
        (true, rest.trim().to_string())
    } else {
        (false, raw)
    };
    let known: Option<std::collections::HashSet<i32>> = if show_all {
        None
    } else {
        Some(
            world
                .get::<KnownAbilities>(player)
                .map_or_else(std::collections::HashSet::new, |k| {
                    k.entries.iter().map(|(id, _, _)| *id).collect()
                }),
        )
    };
    let mut names: Vec<String> = Vec::new();
    for def in world.resource::<AbilityCatalog>().by_name.values() {
        if def.kind != kind {
            continue;
        }
        if let Some(set) = &known
            && !set.contains(&def.id)
        {
            continue;
        }
        // Filter matches if the substring lands on either the
        // ability name OR its sphere — so `spells fire` shows
        // both Fireball (name match) and Burning Hands (sphere
        // match), letting players cluster by elemental theme
        // without learning a separate keyword.
        if !filter.is_empty()
            && !def.plain_name.to_ascii_lowercase().contains(&filter)
            && def.sphere.as_deref().is_none_or(|s| s != filter)
        {
            continue;
        }
        names.push(format_ability_with_sphere(def));
    }
    if names.is_empty() {
        if show_all {
            if filter.is_empty() {
                send_to(world, player, format!("\r\nNo {kind_label}s loaded.\r\n"));
            } else {
                send_rendered(
                    world,
                    player,
                    &format!("\r\nNo {kind_label}s matching '{filter}' loaded.\r\n"),
                );
            }
        } else if filter.is_empty() {
            send_to(
                world,
                player,
                format!(
                    "\r\n<dim>You haven't learned any {kind_label}s yet.</> \
                     Type `{kind_label}s all` to browse the catalog.\r\n"
                ),
            );
        } else {
            send_rendered(
                world,
                player,
                &format!(
                    "\r\nNo {kind_label}s matching '{filter}' in your known list. \
                     Try `{kind_label}s all {filter}`.\r\n"
                ),
            );
        }
        return;
    }
    names.sort_unstable();
    let header = if show_all {
        format!("<b:cyan>All loaded {kind_label}s</>")
    } else {
        format!("<b:cyan>{}s you know</>", capitalize(kind_label))
    };
    let mut out = format!("\r\n{header} <dim>({})</>:\r\n", names.len());
    let column_width = name_column_width(&names);
    for chunk in names.chunks(3) {
        out.push_str("  ");
        for n in chunk {
            let rendered = render_color_tags(n, mode);
            let padded = pad_visible(&rendered, column_width);
            out.push_str(&padded);
        }
        out.push_str("\r\n");
    }
    send_to(world, player, out);
}

#[cfg(test)]
mod tests {
    use super::parse_who_level_filter;

    #[test]
    fn no_args_returns_none() {
        assert_eq!(parse_who_level_filter(""), None);
        assert_eq!(parse_who_level_filter("   "), None);
    }

    #[test]
    fn single_numeric_arg_means_level_or_higher() {
        assert_eq!(parse_who_level_filter("50"), Some((50, i32::MAX)));
        assert_eq!(parse_who_level_filter("  100  "), Some((100, i32::MAX)));
    }

    #[test]
    fn two_numeric_args_form_inclusive_range() {
        assert_eq!(parse_who_level_filter("1 50"), Some((1, 50)));
        assert_eq!(parse_who_level_filter("25  75"), Some((25, 75)));
    }

    #[test]
    fn descending_range_is_normalised_to_ascending() {
        // who 100 1 → 1..=100, not 100..=1.
        assert_eq!(parse_who_level_filter("100 1"), Some((1, 100)));
    }

    #[test]
    fn extra_tokens_after_two_numbers_are_ignored() {
        // No need to refuse — drop the trailing junk.
        assert_eq!(parse_who_level_filter("1 50 garbage"), Some((1, 50)));
    }

    #[test]
    fn non_numeric_args_silently_fall_back_to_no_filter() {
        // Lenient parser: skip non-numeric tokens entirely.
        assert_eq!(parse_who_level_filter("abc"), None);
        assert_eq!(parse_who_level_filter("xyz 50"), Some((50, i32::MAX)));
    }
}
