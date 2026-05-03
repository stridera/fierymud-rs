//! Every Info-category command (and Combat-adjacent verbs that
//! don't fit the cluster files) — 127 entries. The single biggest
//! Step 2 file: it covers the long tail of player-facing readouts
//! (look / score / who / time / ...), inventory manipulation
//! (get / drop / give / wear / wield / ...), settings toggles
//! (afk / autoloot / brief / ...), and shop interactions.
//!
//! Bodies stay in commands.rs (promoted to pub(crate)); only the
//! Command records and help text live here. After this commit
//! the central `COMMANDS` static array is empty — everything
//! reaches dispatch via `inventory::iter`.

use mud_db::enums::UserRole;

use crate::commands::{
    Category, Command, Help,
    cmd_accept, cmd_account, cmd_achievements, cmd_afk, cmd_alias, cmd_autoassist,
    cmd_autoexit, cmd_autogold, cmd_autoloot, cmd_autosplit, cmd_bribe, cmd_brief, cmd_buy,
    cmd_chants, cmd_clientinfo, cmd_close, cmd_color, cmd_commands, cmd_compact, cmd_compare,
    cmd_consent, cmd_cooldowns, cmd_credits, cmd_deaf, cmd_decline, cmd_deposit,
    cmd_description, cmd_dicerolls, cmd_disband, cmd_dismiss, cmd_dismount, cmd_donate,
    cmd_drink, cmd_drop, cmd_eat, cmd_effects, cmd_equipment, cmd_examine, cmd_exits,
    cmd_experience, cmd_extinguish, cmd_fill, cmd_flags, cmd_fly, cmd_follow, cmd_get,
    cmd_give, cmd_glance, cmd_group, cmd_help, cmd_hide, cmd_hire, cmd_hold, cmd_holylight,
    cmd_house, cmd_identify, cmd_idle, cmd_inventory, cmd_invite, cmd_junk, cmd_kneel,
    cmd_level, cmd_light, cmd_list, cmd_lock, cmd_look, cmd_motd, cmd_mount, cmd_news,
    cmd_norepeat, cmd_nosummon, cmd_notell, cmd_open, cmd_order, cmd_pk, cmd_policies,
    cmd_pour, cmd_practice, cmd_prompt, cmd_put, cmd_quaff, cmd_quest_flag, cmd_quit,
    cmd_read, cmd_recite, cmd_remove, cmd_rest, cmd_richtest, cmd_roles, cmd_scan, cmd_score,
    cmd_sell, cmd_showids, cmd_sip, cmd_sit, cmd_skills, cmd_sleep, cmd_slots, cmd_songs,
    cmd_spells, cmd_split, cmd_stand, cmd_style, cmd_tap, cmd_taste, cmd_time, cmd_title,
    cmd_toggle, cmd_track, cmd_train, cmd_unalias, cmd_unfollow, cmd_unlock, cmd_value,
    cmd_version, cmd_visible, cmd_wake, cmd_walk, cmd_wave, cmd_wealth, cmd_wear, cmd_weather,
    cmd_who, cmd_wield, cmd_wimpy, cmd_withdraw, cmd_world,
};

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
        category: Category::Info,
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
        names: &["wealth", "gold", "money"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
            usage: "who",
            summary: "List players currently online.",
            long: "Shows the names of every connected player.",
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
        category: Category::Info,
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
        category: Category::Info,
        help: Help {
            usage: "inventory",
            summary: "List items you are carrying.",
            long: "Shows everything in your inventory by name. \
                   Use `get` to pick items up and `drop` to set them down.",
        },
        run: cmd_inventory,
    }
}

inventory::submit! {
    Command {
        names: &["get", "take", "g"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
            usage: "commands",
            summary: "Flat alphabetical list of every command you can use.",
            long: "Shows just the names you have access to, without the \
                   per-category framing `help` uses. Aliases share their \
                   primary name's slot.",
        },
        run: cmd_commands,
    }
}

inventory::submit! {
    Command {
        names: &["open"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
        help: Help {
            usage: "compare <item-a> <item-b>",
            summary: "Compare two carried/worn items by weight and level.",
            long: "Each item is matched by keyword the same way `wear` \
                   matches. Both items must be on you (inventory or \
                   equipped). Prints the deltas with arrows pointing at \
                   the lighter / lower-level side.",
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
        names: &["policies", "rules"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "policies",
            summary: "Server rules and code of conduct.",
            long: "Static for now.",
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
        category: Category::Info,
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
        category: Category::Info,
        help: Help {
            usage: "idle",
            summary: "Show online players sorted by idle time, longest first.",
            long: "Same population as `who`, but ordered by how long since \
                   each player last typed something. Players who just \
                   connected and haven't typed yet show as `fresh`; anyone \
                   under a minute shows as `active`.",
        },
        run: cmd_idle,
    }
}

inventory::submit! {
    Command {
        names: &["spells", "abilities", "abil"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "spells [filter]",
            summary: "List loaded abilities (spells/chants/songs/skills).",
            long: "Shows every ability the world has loaded, grouped by \
                   kind. Optional filter narrows by case-insensitive \
                   substring match on the name. Once a per-character \
                   ability list lands this command will show only what \
                   you know — for now it's the full catalog.",
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
        category: Category::Info,
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
        category: Category::Info,
        help: Help {
            usage: "skills [filter]",
            summary: "List skills (kind=Skill abilities).",
            long: "Like `spells` but filtered to skills only. Honors \
                   KnownAbilities — shows only what you know when set. \
                   Optional filter narrows by case-insensitive substring.",
        },
        run: cmd_skills,
    }
}

inventory::submit! {
    Command {
        names: &["songs"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "songs [filter]",
            summary: "List bardic songs (kind=Song abilities).",
            long: "Like `spells` but filtered to songs only. Use \
                   `perform <song>` to invoke them.",
        },
        run: cmd_songs,
    }
}

inventory::submit! {
    Command {
        names: &["chants"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "chants [filter]",
            summary: "List chants (kind=Chant abilities).",
            long: "Like `spells` but filtered to chants only. Use \
                   `chant <name>` to invoke them.",
        },
        run: cmd_chants,
    }
}

inventory::submit! {
    Command {
        names: &["prompt", "display"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        names: &["quit"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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
        category: Category::Info,
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

