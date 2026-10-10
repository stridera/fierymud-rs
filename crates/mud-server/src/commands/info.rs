//! Every Info-category command (and Combat-adjacent verbs that
//! don't fit the cluster files) — 127 entries. Both the Command
//! records and the handler bodies live here.

#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::wildcard_imports)]

use std::collections::HashMap;

use bevy_ecs::prelude::{Entity, Has, With, World};
use mud_db::enums::{Direction, UserRole};
use mud_world::*;

use crate::commands::*;

inventory::submit! {
    Command {
        names: &["help", "h"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "help [command]",
            summary: "List commands or show details on a specific one.",
            long: "With no arguments, shows commands available to you grouped \
                   by category. With an argument, shows the usage and details \
                   for that command. Admin commands live in 'wizhelp' (Builder+ \
                   only). Try 'help newbie' for a starter-pack of practical \
                   first goals.",
        },
        run: cmd_help,
    }
}

inventory::submit! {
    Command {
        names: &["wizhelp", "whelp"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "wizhelp [command]",
            summary: "List admin commands or show details on a specific one.",
            long: "Builder+ counterpart of 'help'. With no arguments, lists \
                   every admin command you can run. With an argument, shows \
                   the usage and details for that command — same shape as \
                   'help', but scoped to admin tools so the player help \
                   stays uncluttered.",
        },
        run: cmd_wizhelp,
    }
}

inventory::submit! {
    Command {
        names: &["newbie", "tutorial"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "newbie",
            summary: "Show a short starter-pack of first goals.",
            long: "Lists the practical next steps for a new character: get \
                   oriented, equip gear, find a trainer, fight a safe mob, \
                   and recall back. Re-run any time you forget where you \
                   were — it's not state-dependent.",
        },
        run: cmd_newbie,
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
            long: "Walks each unblocked exit and lists every mob / player \
                   you'd see in those rooms, with a distance label and \
                   posture (std/sit/fly/slp). Default range is one \
                   room; staff scan up to three. Closed or hidden \
                   doors stop the walk in that direction. Useful for \
                   spotting threats / hosts before walking in.",
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
                   by 5 (capped at the class's 'proficiency_cap' \
                   from 'ClassAbilities'). Persists across \
                   reconnect via 'CharacterAbilities'.",
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
                   'bleeding'), and whether they're fighting. Faster than \
                   'examine' for a quick teammate / enemy check.",
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
                   per-level table that turns this into a 'to next' \
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
                   1000 copper). Use 'balance' for bank-stored coin.",
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
            long: "Decrements your on-hand coin by 'amount' (copper \
                   units) and pads the target mob's coin by the same. \
                   Fires the target's BRIBE-flagged Lua triggers with \
                   'actor' = you and 'amount' as a Lua global so \
                   bodies can react proportionally.",
        },
        run: cmd_bribe,
    }
}

inventory::submit! {
    Command {
        names: &["deposit", "dep"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "deposit <amount>",
            summary: "Move copper into the bank.",
            long: "Refuses if you don't have that much on hand. v1 \
                   is location-agnostic; banker-mob gating arrives \
                   once 'MobProfession::Banker' is hydrated.",
        },
        run: cmd_deposit,
    }
}

inventory::submit! {
    Command {
        names: &["withdraw", "with"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "withdraw <amount>",
            summary: "Move copper from the bank to on-hand wealth.",
            long: "Refuses if your bank balance can't cover the \
                   amount. Inverse of 'deposit'.",
        },
        run: cmd_withdraw,
    }
}

inventory::submit! {
    Command {
        names: &["value", "appraise", "val"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "value <item>",
            summary: "Show an item's catalog value in coin.",
            long: "Searches your inventory and the room for the named \
                   item, then prints its base value (the schema's \
                   'Objects.cost') split into denominations. Shops will \
                   pay some fraction of this on sell once that lands.",
        },
        run: cmd_value,
    }
}

inventory::submit! {
    Command {
        names: &["inspect"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "inspect [<#|item|pet>]",
            summary: "Pay a shopkeeper to identify a wares item or pet.",
            long: "Legacy shop service. With no argument, lists the \
                   keeper's wares with their inspection fee (a tenth of \
                   the purchase price, minimum one copper). With an item \
                   number or name, pays the fee and shows that item's full \
                   stat block without buying it; a pet number or name \
                   shows the pet's stats. Staff inspect for free.",
        },
        run: cmd_inspect,
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
            long: "Looks for a 'Shopkeeper'-tagged mob in your room, \
                   then prints the keeper's catalog with prices and \
                   stock. 'buy <#|name>' and 'sell <item>' land next.",
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
            long: "Argument is either the catalog index from 'list' or \
                   a substring of the item's name. Coin is deducted \
                   from your 'wealth'; the item lands in your \
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
            long: "Pays 'proto.cost * sell_profit' rounded (never more \
                   than the item's cost) for a carried item the keeper \
                   trades in. Equipped items are refused ('remove' \
                   first). A keeper with no 'ShopAccepts' rules buys \
                   nothing.",
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
                   'wealth'. Pet is renamed to '<you>'s <mob>' so \
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
                   new title, sets it (max 60 chars). With 'clear' (or \
                   'none' / '-'), removes it. Persists on disconnect.",
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
            summary: "Show or set the prose 'examine' shows for you.",
            long: "With no argument, prints your current description. \
                   With new text, replaces it (max 500 chars). With \
                   'clear' / 'none' / '-', removes it. XML-Lite color \
                   tags render the same as room descriptions. Persists \
                   on disconnect.",
        },
        run: cmd_description,
    }
}

inventory::submit! {
    Command {
        names: &["examine", "exam", "exa"],
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
                   'who clan <abbrev>' filters to players in the \
                   named clan (case-insensitive match against the \
                   '[ABBR]' tag shown alongside each name).",
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
            long: "Shows HP, stamina, six core attributes, the modern \
                   combat triple (Acc / Eva / Atk / Armor%), \
                   alignment, posture, location, recall point, and \
                   your current combat target when fighting. \
                   Spell-circle slots are surfaced separately via \
                   'slots'; full effect durations via 'effects'; \
                   XP / next-level gains via 'level'.",
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
                   Use 'get' to pick items up and 'drop' to set them down. \
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
                   keywords (or its name). With 'from <container>', \
                   pulls from a container the player is carrying or \
                   one in the room. The item moves into your \
                   inventory; everyone else in the room sees the action.",
        },
        run: cmd_get,
    }
}

inventory::submit! {
    Command {
        names: &["put", "p"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "put <item> <container>",
            summary: "Move a carried item into a container.",
            long: "Container can be a carried item or one in the \
                   current room. Equipped items must be 'remove'd \
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
                   gear — 'remove' first.",
        },
        run: cmd_junk,
    }
}

inventory::submit! {
    Command {
        names: &["give", "gi"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "give <item> <target>",
            summary: "Hand an item to another character in the room.",
            long: "Both you and the target must be in the same room. The \
                   item must be in your inventory (not equipped — 'remove' \
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
                   free. Use 'remove' to take something off first.",
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
            long: "Lights a Light-type item you are carrying, holding \
                   or wearing. Wearing or holding a light does not \
                   light it. Refused on non-light items, items that \
                   are already lit, and burnt-out lights. Permanent \
                   lights are always lit.",
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
            long: "Puts out a held, worn or carried light source. \
                   Refused on items that aren't lit and on permanent \
                   lights, which cannot be put out.",
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
            long: "Target must be 'Mountable' (auto-applied to mobs \
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
            long: "Clears the 'Mounted' link on you and the \
                   'RiddenBy' link on the mount. No-op when not \
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
            long: "Needs a source of flight (the fly spell, a winged \
                   form or a worn item). Movement charges a flat 2 \
                   stamina per move while flying — great savings over \
                   water/swamp (4-6 normally), slightly pricier on \
                   roads (1). Air rooms have no floor: anyone who is \
                   not flying falls through them, and so do loose items \
                   and corpses. Use 'land' (or 'walk') to come back \
                   down.",
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
            usage: "land",
            summary: "Come down to the ground (stop flying).",
            long: "Ends flight (also available as 'walk'). On solid \
                   ground you simply touch down. In an air room you \
                   have nothing to stand on, so landing there means \
                   falling through the down exits until you hit \
                   something solid, and the landing hurts (a long drop \
                   hurts more; Feather Fall and the Safefall skill \
                   soften it). Flight also ends on its own when the \
                   fly spell or item that grants it runs out, or when \
                   you pick up more than about 95% of what you can \
                   carry. You need a source of flight (the fly spell, \
                   a winged form or a worn item) to 'fly' in the first \
                   place. No-op when you are already on foot.",
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
            summary: "Slip into the shadows.",
            long: "Rolls how well you hide from your hide skill, \
                   Dexterity and Intelligence (rogues and DEX help \
                   most). Anyone whose perception is below that \
                   number cannot see you until you act: any command \
                   but looking around, hiding, moving and the like \
                   gives you away, as does attacking or being found \
                   by 'search'. Walking wears the hiding down; \
                   'sneak' slows that. Hiding leaves you busy for \
                   a combat round (half for thieves). Your group \
                   always sees you.",
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
            summary: "Stop hiding.",
            long: "Drops your hiding: back to normal visibility.",
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
            long: "Decrements the container's 'remaining' liquid by \
                   4 units. Empty containers refuse. Use 'quaff' for \
                   potions, 'sip' for a smaller swig (1 unit), \
                   'taste' to identify the liquid without drinking.",
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
            long: "Lighter than 'drink' (4 units). Same refusal on \
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
            long: "Inverse-arg 'pour': same liquid-match rules, \
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
        names: &["use"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "use <wand|staff> [<target>]",
            summary: "Use a wand or staff you are holding.",
            long: "Wands are pointed at a target (or waved when the spell \
                   needs none); staves are tapped on the ground. Each use \
                   spends a charge only when the magic lands. The item \
                   must be held, and not too powerful for you.",
        },
        run: cmd_use,
    }
}

inventory::submit! {
    Command {
        names: &["play"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "play <instrument>",
            summary: "Play an instrument you are holding.",
            long: "Plays a held instrument; magical instruments invoke \
                   their bound abilities and spend a charge when they land.",
        },
        run: cmd_play,
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
        names: &["commands", "comm"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "commands [<category>]",
            summary: "Flat alphabetical list of every command you can use.",
            long: "Shows just the names you have access to, without the \
                   per-category framing 'help' uses. Aliases share their \
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
                   Locked doors need a key (use 'unlock').",
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
                   door is unlocked (state Closed); use 'open' to \
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
            long: "Mirror of 'unlock': requires the exit to be closed \
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
                   players — use 'examine' for those.",
        },
        run: cmd_read,
    }
}

inventory::submit! {
    Command {
        names: &["compare", "comp"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "compare <item-a> [<item-b>]",
            summary: "Compare two carried/worn items by weight, level, and weapon damage.",
            long: "Each item is matched by keyword the same way 'wear' \
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
                   'motd' row lands, this will read from there.",
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
            long: "Static for now. Aliases: 'rules', 'policy'.",
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
            summary: "Show your account email, role, character roster, and linked Discord/Google.",
            long: "Read-only summary of who you're logged in as and \
                   which character is currently active. Snapshot taken \
                   at login — characters created mid-session won't \
                   appear until you reconnect. Also reads 'discord_links' \
                   and 'google_links' for your Users row and prints \
                   whatever's bound; Discord links display as \
                   'verified' / 'unverified' so you can tell whether \
                   the bot will honor messages from the linked account. \
                   ('whoami' is a separate one-line identity command.)",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, _args| match head {
            "account" => Some(Box::pin(cmd_account(world, player, pool))),
            _ => None,
        },
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
        names: &["world", "stats"],
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
                   'users' and 'stats' mirror 'world' since the player \
                   count and per-system load are the most-asked pieces.",
        },
        run: cmd_world,
    }
}

inventory::submit! {
    Command {
        names: &["time"],
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
                   'Climate' and the in-game time of day. Rule-of-thumb \
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
        names: &["trophy"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "trophy [<player>]",
            summary: "Show your recent kills (and the XP penalty band).",
            long: "Lists your most recent ~21 kill targets and the \
                   accumulated kill count per target. Re-killing \
                   targets you've farmed scales XP down — the \
                   color band on each row hints at how steep the \
                   penalty is. With a player name (Builder+), \
                   shows their trophy instead of yours.",
        },
        run: cmd_trophy,
    }
}

inventory::submit! {
    Command {
        names: &["summonmount", "summon-mount"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Mount,
        help: Help {
            usage: "summonmount",
            summary: "Conjure a mount (Paladin / Anti-Paladin only).",
            long: "Gated on the Summon Mount skill (Paladin and \
                   Anti-Paladin from level 15 in the legacy data); \
                   refused indoors, while fighting, or when you \
                   already have a mount following you. Spawns the \
                   first matching mountable proto as a Follower in \
                   your current room.",
        },
        run: cmd_summonmount,
    }
}

inventory::submit! {
    Command {
        names: &["camp"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "camp",
            summary: "Pitch camp, then save and leave the game.",
            long: "Refuses indoors, in cities, on water, in the air, \
                   while mounted, or while in combat. Once setup \
                   completes (~35 seconds), you're saved and leave \
                   the game, to return right here. Walking away or \
                   being attacked aborts the camp.",
        },
        run: cmd_camp,
    }
}

inventory::submit! {
    Command {
        names: &["point"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "point <target>",
            summary: "Point at someone, something, or a direction.",
            long: "Resolves <target> against (in order): a direction \
                   (n/s/e/w/u/d), an actor in your room (player or \
                   mob), or an item in your room or inventory. \
                   Pointing at a hidden actor reveals them — \
                   stealth doesn't survive being singled out.",
        },
        run: cmd_point,
    }
}

inventory::submit! {
    Command {
        names: &["aggr", "aggro"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "aggr",
            summary: "Show what's hostile to you right now.",
            long: "Lists every mob anywhere in the world that has \
                   you in its hate list (most recent swing first) \
                   or its memory (mobs you've fled from that will \
                   re-engage on sight). Mobs in your current room \
                   are flagged so you know what's about to swing. \
                   Mirrored as a 'Char.Aggro' GMCP frame on each \
                   prompt for HUD-style clients.",
        },
        run: cmd_aggr,
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
            long: "Same population as 'who', but ordered by how long since \
                   each player last typed something. Players who just \
                   connected and haven't typed yet show as 'fresh'; anyone \
                   under a minute shows as 'active'. \
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
            usage: "spells [<circle>|<lo>-<hi>] [filter] | spells all [filter]",
            summary: "List spells you know, grouped by circle.",
            long: "By default shows the spells you've learned grouped \
                   by your class's spell circle (1 = lowest). Numeric \
                   args filter by circle: 'spells 3' shows circle 3, \
                   'spells 1-2' shows circles 1 through 2. A trailing \
                   word filters by name, sphere, or damage type — \
                   'spells 2-3 fire' or 'spells dam'. Add 'all' to \
                   dump the full spell catalog (handy for builders); \
                   the cross-class catalog has no circle grouping.",
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
            summary: "Your level and progress toward the next one.",
            long: "Shows your level, a progress bar and how much XP is \
                   left to the next level, scaled for your class. At the \
                   mortal cap with enough XP to reach level 100 you are \
                   marked with your class stars ('**') instead.",
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
            summary: "Show your spell-slot pool and any in-flight cooldowns.",
            long: "Pooled slot model (legacy): each circle has \
                   'class+level' slots; casting consumes a slot and \
                   starts a per-circle cooldown timer. Slots regenerate \
                   on their own under Sleeping / Resting / Sitting \
                   postures (faster while Meditating). Format \
                   'Circle N: free/max  (recovering: 12s, 30s)'. \
                   Cooldowns persist across disconnect.",
        },
        run: cmd_slots,
    }
}

inventory::submit! {
    Command {
        names: &["skills", "sk"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "skills [all] [filter]",
            summary: "List skills you know (or the full catalog).",
            long: "Like 'spells' but filtered to kind=Skill. Default \
                   shows only what you've learned; 'skills all' dumps \
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
            long: "Like 'spells' but filtered to kind=Song. Default \
                   shows only songs you've learned; 'songs all' dumps \
                   the catalog. Use 'perform <song>' to invoke.",
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
            long: "Like 'spells' but filtered to kind=Chant. Default \
                   shows only chants you've learned; 'chants all' \
                   dumps the catalog. Use 'chant <name>' to invoke.",
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
            usage: "toggle [<flag>]",
            summary: "List your toggles, or flip one on or off.",
            long: "With no argument, lists every toggle with its ON/OFF \
                   state. 'toggle <name>' flips one: names are matched \
                   like command abbreviations ('toggle auto' reaches the \
                   first AutoXxx in the list) and don't need underscores \
                   ('toggle showdicerolls', 'toggle dice', \
                   'toggle show_dice_rolls' all work). \
                   'toggle columns <n>' sets the text wrap width (see \
                   'columns').",
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
            long: "Shows every flag currently set on you. Use 'toggle <flag>' \
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
            long: "Marks you AFK so others see the indicator on 'who' and on \
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
                   'alias <name>', shows that alias's expansion. With \
                   'alias <name> <command>', sets the alias ('unalias <name>' \
                   removes it). A plain alias replaces \
                   whatever you type (extra words are dropped). Put '$*' \
                   where the rest of your line should go, '$1'..'$9' for \
                   single words, and ';' between commands to chain them: \
                   'alias gg get all corpse;put all bag' or \
                   'alias k kill $1;kill $2'. An alias never re-triggers \
                   itself, so 'alias look look $*' wraps the real command. \
                   Aliases persist across sessions.",
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
            summary: "Refuse incoming 'tell' messages.",
            long: "When set, other players' 'tell' to you is blocked with a \
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
            long: "When set, you no longer receive 'gossip' or 'shout' from \
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
            usage: "color [off|16|256|truecolor|auto]",
            summary: "Choose how much color you see.",
            long: "The server asks your client what it can display and \
                   adapts: 256-color and truecolor shades are mapped to the \
                   nearest of the 16 basic colors for clients that only \
                   do 16. 'color off' removes color entirely, 'color 16' / \
                   'color 256' / 'color truecolor' pin a depth if your client \
                   misreports itself, and 'color auto' goes back to \
                   following the client. Saved with your character. 'color' \
                   alone shows the current setting.",
        },
        run: cmd_color,
    }
}

inventory::submit! {
    Command {
        names: &["charset"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "charset [ascii|utf8|auto]",
            summary: "Choose plain ASCII or UTF-8 output.",
            long: "Clients that don't announce UTF-8 support get plain \
                   ASCII: dashes, arrows, stars and box-drawing characters \
                   are replaced by look-alikes (-- -> * + - |). If your \
                   client does display UTF-8, 'charset utf8' turns the real \
                   symbols on; 'charset ascii' forces plain text; 'charset \
                   auto' follows what your client reports. Saved with your \
                   character. 'charset' alone shows the current setting.",
        },
        run: cmd_charset,
    }
}

inventory::submit! {
    Command {
        names: &["wimpy"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "wimpy [pct|off]",
            summary: "Set the HP percentage at which combat auto-flees you.",
            long: "'wimpy 30' enables wimpy mode and panics you out of \
                   combat when your HP drops below 30% of max. 'wimpy off' \
                   (or 'wimpy 0') clears it. With no argument, prints the \
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
            summary: "Toggle automatic exit listing on 'look'.",
            long: "When set, the room description is followed by the same \
                   line 'exits' would print. Already consumed by the look \
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
            summary: "Toggle automatic looting of kills (items and coins).",
            long: "Sets AUTO_LOOT. Items and coins from mobs you kill jump \
                   straight to you. Coins respect autosplit.",
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
            summary: "Toggle automatic collection of coins from kills.",
            long: "Sets AUTO_GOLD. Coins from mobs you kill jump straight \
                   to your purse without taking the items (autoloot \
                   already collects coins too).",
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
            long: "When on, 'look' skips the full room description \
                   and shows just the title + exits + occupants.",
        },
        run: cmd_brief,
    }
}

inventory::submit! {
    Command {
        names: &["columns", "wrap"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "columns [<40-250>|auto]",
            summary: "Set the width long text is word-wrapped to.",
            long: "The server wraps room descriptions, help and other prose \
                   to your client's reported window width (80 if it \
                   doesn't report one). 'columns 100' wraps narrower than \
                   the window, which reads better on very wide windows (a \
                   value wider than your window is capped to it); \
                   'columns auto' goes back to following the client. The \
                   setting is saved with your character. 'toggle columns \
                   <n>' works too.",
        },
        run: cmd_columns,
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
            summary: "Don't echo your own say/tell/channel text back.",
            long: "Toggles NO_REPEAT. While on, say, tell, reply, whisper, \
                   ask, gsay and the global channels answer you with just \
                   \"Ok.\" instead of repeating what you typed. Others see \
                   your message as usual.",
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
            summary: "Silently auto-decline incoming summon spells.",
            long: "By default, when someone casts SUMMON on you, you get a \
                   30-second 'accept' / 'decline' prompt. Setting NO_SUMMON \
                   short-circuits that — incoming summons are silently \
                   refused without prompting you, and the caster sees \
                   \"X is not accepting summons.\" Use this when you want \
                   to AFK without summon-prompt interruptions. Run again \
                   to re-enable the prompt. PK-flagged players (see 'pk') \
                   bypass the prompt entirely — that's part of opting \
                   into PvP.",
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
        names: &["pk", "pkill"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "pk [on|off]",
            summary: "Toggle player-kill participation.",
            long: "Sets PK_ENABLED. Two players can fight each other only \
                   while BOTH have PK on. Turning it on works anywhere; \
                   you can turn it off only at your recall point, and not \
                   while fighting another player.",
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
        names: &["quit"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "quit",
            summary: "Save your character and leave the game.",
            long: "Saves your character, says goodbye and drops the \
                   connection. You can't quit while fighting (staff \
                   excepted) or while casting. The whole word is required; \
                   'q', 'qu' and 'qui' only remind you of that.",
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
                   'house enter'. 'house' (no arg) defaults to the \
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
                   available practice points. With 'train <stat>', \
                   spends 1 point from 'SkillPoints' to raise the \
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
        names: &["alert"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "alert",
            summary: "Stop resting and pay attention, without standing up.",
            long: "The opposite of 'rest': you drop the resting stance \
                   and sit at attention. Does nothing if you are already \
                   alert.",
        },
        run: cmd_alert,
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
        names: &["meditate", "med"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "meditate",
            summary: "Focus to speed up spell memorization.",
            long: "Doubles the rate at which 'memorize' slots refill. \
                   Requires resting / sitting / kneeling — standing \
                   or sleeping breaks focus, as does taking a step \
                   or being attacked. Re-running 'meditate' ends the \
                   trance.",
        },
        run: cmd_meditate,
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
            long: "Changes your posture to sleeping. Wake with 'wake', \
                   'stand', 'sit', or 'rest'.",
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
        names: &["follow", "shadow", "fol"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "follow <name>",
            summary: "Trail another character automatically.",
            long: "When the target moves, you move with them through the \
                   same exit. 'follow self' (or 'unfollow') stops \
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
            summary: "List your current group.",
            long: "Shows the group leader and every member, with HP \
                   and same-room indicator. Joining a group takes the \
                   leader's 'invite' and your 'accept'; merely \
                   following someone does not put you in their group.",
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
            long: "Forwards '<command>' to a named mob follower (must \
                   be in the same room and have 'Follower(you)' set), \
                   or 'all' for every same-room follower at once. The \
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
            long: "Equivalent to 'group dismiss <player>' — removes the \
                   target from your group and ends their follow link to \
                   you. 'disband' clears everyone at once.",
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
            long: "Pulls '<amount>' from your wealth and splits it \
                   evenly across every group member in your room \
                   (including you). Remainder stays with the splitter. \
                   Refuses if you're not grouped, the only group \
                   member here, or carrying less than 'amount'. Coin \
                   amount is in copper; use 'wealth' to check yours.",
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
            summary: "Dismiss your group and everyone following you.",
            long: "Breaks your group apart and drops everyone \
                   directly following you.",
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
            long: "Target gets a pending 'GroupInvite'; they can \
                   'accept' or 'decline'. Invites expire after \
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
            long: "Joins the inviter's group and starts you following \
                   them. No-op if you have no pending invite.",
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

pub(crate) fn cmd_newbie(world: &mut World, player: Entity, _args: &str) {
    // Static starter-pack — read on every call so a one-shot
    // `newbie` always renders. Steps roughly map to "first 30
    // minutes of play." The list is conservative and intentionally
    // class-agnostic; class-specific advice belongs in `help <class>`
    // pages once those land.
    let body = "\r\n\
        <b:cyan>Newbie starter pack:</>\r\n\
        \r\n\
        <yellow>1.</> <b:cyan>Get oriented.</> <dim>Try</> <cyan>look</> <dim>and</> <cyan>exits</> <dim>to see your room and where you can go.</>\r\n\
        <yellow>2.</> <b:cyan>Check yourself.</> <dim>Try</> <cyan>score</> <dim>(stats),</> <cyan>inventory</> <dim>(carried items), and</> <cyan>equipment</> <dim>(worn).</>\r\n\
        <yellow>3.</> <b:cyan>Equip what you have.</> <dim>Try</> <cyan>wear &lt;item&gt;</> <dim>or</> <cyan>wield &lt;weapon&gt;</> <dim>— a half-dressed character takes far more damage.</>\r\n\
        <yellow>4.</> <b:cyan>Find a trainer.</> <dim>Trainers teach skills and spells. Ask local NPCs or use</> <cyan>where &lt;name&gt;</> <dim>to track one down. Spend points with</> <cyan>practice</>.\r\n\
        <yellow>5.</> <b:cyan>Fight something safe.</> <dim>Use</> <cyan>consider [target]</> <dim>to gauge danger before</> <cyan>kill [target]</>. <dim>If a fight goes poorly,</> <cyan>flee</> <dim>or set</> <cyan>wimpy 30</> <dim>to auto-flee at low HP.</>\r\n\
        <yellow>6.</> <b:cyan>Stay alive.</> <dim>Watch hunger / thirst on</> <cyan>score</>. <dim>Eat / drink and</> <cyan>rest</> <dim>or</> <cyan>sleep</> <dim>to regen.</>\r\n\
        <yellow>7.</> <b:cyan>Set a recall.</> <dim>Touch a touchstone in a safe spot, then</> <cyan>recall</> <dim>warps you back from anywhere — invaluable when a fight goes wrong.</>\r\n\
        \r\n\
        <dim>Type</> <cyan>help &lt;command&gt;</> <dim>for any of the verbs above.</>\r\n";
    send_to(world, player, body);
}

/// Render `help <social>` as a sample of every message variant the
/// social can produce. Substitutions use the player's own name as the
/// actor and a generic placeholder as the target so the example
/// reads as a concrete sentence. Sections with no message templates
/// are skipped — many socials are no-arg-only or target-only.
fn render_social_help(world: &mut World, player: Entity, social: &SocialDef) {
    const PLACEHOLDER_TARGET: &str = "someone";
    let actor_name = name_of(world, player);

    let has_no_arg = social.char_no_arg.is_some() || social.others_no_arg.is_some();
    let has_found =
        social.char_found.is_some() || social.others_found.is_some() || social.vict_found.is_some();
    let has_auto = social.char_auto.is_some() || social.others_auto.is_some();

    let usage = match (has_no_arg, has_found) {
        (true, true) => format!("{} [target]", social.name),
        (false, true) => format!("{} <target>", social.name),
        _ => social.name.clone(),
    };

    let mut out = format!("\r\n<b:cyan>{}</> <dim>(social)</>\r\n", social.name);
    out.push_str("\r\n  An emote — describes how you're acting toward the room or a target.\r\n");
    out.push_str(&format!("\r\n  <cyan>Usage:</> {usage}\r\n"));

    if has_no_arg {
        out.push_str("\r\n  <cyan>Without a target:</>\r\n");
        if let Some(line) = social.char_no_arg.as_ref() {
            let s = substitute(line, &actor_name, None);
            out.push_str(&format!("    <dim>You see:</>    {s}\r\n"));
        }
        if let Some(line) = social.others_no_arg.as_ref() {
            let s = substitute(line, &actor_name, None);
            out.push_str(&format!("    <dim>Others see:</> {s}\r\n"));
        }
    }

    if has_found {
        out.push_str("\r\n  <cyan>With a target:</>\r\n");
        if let Some(line) = social.char_found.as_ref() {
            let s = substitute(line, &actor_name, Some(PLACEHOLDER_TARGET));
            out.push_str(&format!("    <dim>You see:</>    {s}\r\n"));
        }
        if let Some(line) = social.vict_found.as_ref() {
            let s = substitute(line, &actor_name, Some(PLACEHOLDER_TARGET));
            out.push_str(&format!("    <dim>Target sees:</> {s}\r\n"));
        }
        if let Some(line) = social.others_found.as_ref() {
            let s = substitute(line, &actor_name, Some(PLACEHOLDER_TARGET));
            out.push_str(&format!("    <dim>Others see:</> {s}\r\n"));
        }
    }

    if has_auto {
        out.push_str("\r\n  <cyan>On yourself:</>\r\n");
        if let Some(line) = social.char_auto.as_ref() {
            let s = substitute(line, &actor_name, Some(&actor_name));
            out.push_str(&format!("    <dim>You see:</>    {s}\r\n"));
        }
        if let Some(line) = social.others_auto.as_ref() {
            let s = substitute(line, &actor_name, Some(&actor_name));
            out.push_str(&format!("    <dim>Others see:</> {s}\r\n"));
        }
    }

    out.push_str("\r\n  <cyan>Category:</> <dim>Social</>\r\n");
    send_prose(world, player, out);
}

/// Render a DB-backed `HelpEntry`. Matches the per-command help page
/// shape so a player can't tell from layout whether they hit a
/// hand-authored command page or a builder-authored article — same
/// bold-cyan title, cyan section labels for metadata, body content
/// last. Usage / Sphere / Duration only render when set on the row.
fn render_help_entry(
    world: &mut World,
    player: Entity,
    entry: &mud_world::HelpEntry,
    command_usage: Option<&str>,
) {
    let mut out = format!("\r\n<b:cyan>{}</>\r\n", entry.title);
    if let Some(usage) = entry.usage.as_ref() {
        out.push_str(&format!("\r\n  <cyan>Usage:</> {usage}\r\n"));
    }
    if let Some(sphere) = entry.sphere.as_ref() {
        out.push_str(&format!("  <cyan>Sphere:</> {sphere}\r\n"));
    }
    if let Some(duration) = entry.duration.as_ref() {
        out.push_str(&format!("  <cyan>Duration:</> {duration}\r\n"));
    }
    out.push_str("\r\n");
    // Body. Trim trailing whitespace so we don't double-blank the
    // tail before the category gloss.
    out.push_str(entry.content.trim_end());
    out.push_str("\r\n");
    if let Some(category) = entry.category.as_ref() {
        out.push_str(&format!("\r\n  <cyan>Category:</> <dim>{category}</>\r\n"));
    }
    if let Some(usage) = command_usage {
        out.push_str(&format!("\r\n<dim>Command usage:</> {usage}\r\n"));
    }
    send_prose(world, player, out);
}

/// Render an `AbilityDef` as a help card. Pulled into a helper so
/// `help <spell>` (G2.6) and the eventual `spellinfo <spell>` (G2.7)
/// can share output. Shows the description text, cast time / area
/// flag, posture requirement, and AbilityMessages.successToCaster
/// so the player can see what the spell *says* when it lands without
/// casting it first.
fn render_ability_help(world: &mut World, player: Entity, def: &mud_world::AbilityDef) {
    let mode = color_mode_for(world, player);
    let mut out = format!(
        "\r\n<b:cyan>{}</> <dim>({})</>\r\n",
        render_color_tags(&def.name, mode),
        def.kind.label(),
    );
    if let Some(desc) = &def.description {
        out.push_str(&format!("\r\n{}\r\n", render_color_tags(desc.trim(), mode)));
    }
    out.push_str(&format!(
        "\r\n  <cyan>Cast time:</> {} round(s)   <cyan>Cooldown:</> {}ms\r\n",
        def.cast_time_rounds, def.cooldown_ms,
    ));
    out.push_str(&format!(
        "  <cyan>Posture:</> {}   <cyan>Area:</> {}\r\n",
        def.min_position_label,
        if def.is_area { "yes" } else { "no" },
    ));
    if let Some(sphere) = def.sphere.as_deref().filter(|s| !s.is_empty()) {
        out.push_str(&format!("  <cyan>Sphere:</> {sphere}\r\n"));
    }
    if let Some(dt) = def.damage_type.as_deref().filter(|s| !s.is_empty()) {
        out.push_str(&format!("  <cyan>Damage type:</> {dt}\r\n"));
    }
    out.push_str(&format!(
        "  <cyan>Flags:</> {}{}{}{}\r\n",
        if def.violent { "violent " } else { "" },
        if def.in_combat_only {
            "combat-only "
        } else {
            ""
        },
        if def.combat_ok {
            ""
        } else {
            "out-of-combat-only "
        },
        if def.is_magical {
            "magical"
        } else {
            "physical"
        },
    ));
    // Class access: surface every class that can use this ability
    // and at what circle / introduction level. Walk both junction
    // catalogs (SpellSlotData.ability_circle for casters,
    // ClassSkillsData.min_level for skill classes) and group by
    // class name. A class can appear on both sides for hybrid
    // abilities; we collapse to one line per class with the union
    // of qualifiers ("Cleric (C2, L10)").
    let class_lookup: std::collections::HashMap<i32, String> = world
        .resource::<mud_world::ClassCatalog>()
        .by_id
        .iter()
        .map(|(id, c)| (*id, c.plain_name.clone()))
        .collect();
    let mut by_class: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for ((cid, aid), circle) in &world.resource::<mud_world::SpellSlotData>().ability_circle {
        if *aid != def.id {
            continue;
        }
        if let Some(name) = class_lookup.get(cid) {
            by_class
                .entry(name.clone())
                .or_default()
                .push(format!("C{circle}"));
        }
    }
    for ((cid, aid), min_level) in &world.resource::<mud_world::ClassSkillsData>().min_level {
        if *aid != def.id {
            continue;
        }
        if let Some(name) = class_lookup.get(cid) {
            by_class
                .entry(name.clone())
                .or_default()
                .push(format!("L{min_level}"));
        }
    }
    if !by_class.is_empty() {
        let parts: Vec<String> = by_class
            .iter()
            .map(|(name, quals)| format!("<b:cyan>{name}</> <dim>({})</>", quals.join(", ")))
            .collect();
        out.push_str(&format!(
            "  <cyan>Available to:</> {}\r\n",
            parts.join(", "),
        ));
    }
    // Lookup the messages.successToCaster line so the player can
    // preview the cast flavor. Missing rows fall through cleanly.
    if let Some(msgs) = world
        .resource::<AbilityCatalog>()
        .messages
        .get(&def.id)
        .and_then(|m| m.success_to_caster.clone())
    {
        let line = msgs
            .replace("{target.him}", "your foe")
            .replace("{target.name}", "your foe")
            .replace("{target.her}", "your foe")
            .replace("{actor.name}", "you");
        out.push_str(&format!(
            "\r\n  <cyan>On success:</> {}\r\n",
            render_color_tags(&line, mode),
        ));
    }
    send_prose(world, player, out);
}

/// `help classes` body — overview of every class in the catalog,
/// with subclasses indented under their parent. Each line shows the
/// short identity (primary stat / HD) so a new player can scan the
/// whole roster from one screen and dive into `help <class>` for
/// the full kit. Pure ClassCatalog walk — no DB hits.
fn render_classes_overview(world: &mut World, player: Entity) {
    let mode = color_mode_for(world, player);
    let catalog = world.resource::<mud_world::ClassCatalog>();
    // Bucket subclasses under their parent. Top-level classes
    // (no parent / parent_class_id == None) anchor each tree.
    let mut children: std::collections::HashMap<i32, Vec<&mud_world::ClassDef>> =
        std::collections::HashMap::new();
    let mut roots: Vec<&mud_world::ClassDef> = Vec::new();
    for c in catalog.by_id.values() {
        if c.is_subclass
            && let Some(pid) = c.parent_class_id
        {
            children.entry(pid).or_default().push(c);
        } else {
            roots.push(c);
        }
    }
    roots.sort_by(|a, b| a.plain_name.cmp(&b.plain_name));
    let mut out = format!(
        "\r\n<b:cyan>Classes</> <dim>({} total, {} top-level)</>\r\n",
        catalog.by_id.len(),
        roots.len(),
    );
    let format_line = |c: &mud_world::ClassDef, indent: &str| -> String {
        let primary = c
            .primary_stat
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("--");
        format!(
            "  {indent}<b:cyan>{}</> <dim>(primary {primary}, HD {}, HP+{}/lvl)</>\r\n",
            render_color_tags(&c.name, mode),
            c.hit_dice,
            c.hp_per_level,
        )
    };
    for parent in roots {
        out.push_str(&format_line(parent, ""));
        if let Some(kids) = children.get(&parent.id) {
            let mut kids_sorted: Vec<&mud_world::ClassDef> = kids.clone();
            kids_sorted.sort_by(|a, b| a.plain_name.cmp(&b.plain_name));
            for k in kids_sorted {
                out.push_str(&format_line(k, "  └─ "));
            }
        }
    }
    out.push_str("\r\n  <dim>Type 'help <class>' to see a class's full toolkit.</>\r\n");
    send_to(world, player, out);
}

/// `help <class>` body — surfaces a class's prose description, the
/// stat identity line (primary stat / hit dice / per-level HP), and
/// inventories its toolkit. Spells grouped by circle, skills grouped
/// by introduction level. Lets a player picking a character read the
/// full kit in one screen instead of cross-referencing `spells` /
/// `practice` after creation.
fn render_class_help(world: &mut World, player: Entity, def: &mud_world::ClassDef) {
    let mode = color_mode_for(world, player);
    let mut out = format!(
        "\r\n<b:cyan>{}</> <dim>(class)</>\r\n",
        render_color_tags(&def.name, mode),
    );
    if let Some(d) = def.description.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("\r\n{}\r\n", render_color_tags(d.trim(), mode)));
    }
    out.push_str("\r\n  <cyan>Hit dice:</> ");
    out.push_str(&def.hit_dice);
    out.push_str(&format!("   <cyan>HP/level:</> {}", def.hp_per_level));
    if let Some(p) = def.primary_stat.as_deref().filter(|s| !s.is_empty()) {
        out.push_str(&format!("   <cyan>Primary stat:</> {p}"));
    }
    if def.is_subclass
        && let Some(pid) = def.parent_class_id
        && let Some(parent) = world.resource::<mud_world::ClassCatalog>().by_id.get(&pid)
    {
        out.push_str(&format!("\r\n  <dim>Subclass of {}</>", parent.plain_name));
    }
    out.push_str("\r\n");
    // Per-class ability inventories. Walk catalogs in one pass so a
    // class with no entries in either table skips the section header.
    let ability_circles: Vec<(i32, i32)> = world
        .resource::<mud_world::SpellSlotData>()
        .ability_circle
        .iter()
        .filter(|((cid, _), _)| *cid == def.id)
        .map(|((_, aid), c)| (*aid, *c))
        .collect();
    if !ability_circles.is_empty() {
        // Group by circle, then sort each circle's name list. Pull
        // the display name from AbilityCatalog.by_id — we walk by_name
        // here because that's the existing index keyed by plain_name.
        let ability_catalog = world.resource::<mud_world::AbilityCatalog>();
        let by_id: std::collections::HashMap<i32, &mud_world::AbilityDef> = ability_catalog
            .by_name
            .values()
            .map(|d| (d.id, d))
            .collect();
        let mut by_circle: std::collections::BTreeMap<i32, Vec<String>> =
            std::collections::BTreeMap::new();
        for (aid, circle) in ability_circles {
            if let Some(a) = by_id.get(&aid) {
                by_circle.entry(circle).or_default().push(a.name.clone());
            }
        }
        out.push_str("\r\n  <cyan>Spells</> <dim>(by circle):</>\r\n");
        for (circle, mut names) in by_circle {
            names.sort_unstable();
            out.push_str(&format!(
                "    <b:yellow>Circle {circle:>2}</> <dim>({})</>: {}\r\n",
                names.len(),
                names
                    .iter()
                    .map(|n| render_color_tags(n, mode))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
    }
    let skill_entries: Vec<(i32, i32)> = world
        .resource::<mud_world::ClassSkillsData>()
        .min_level
        .iter()
        .filter(|((cid, _), _)| *cid == def.id)
        .map(|((_, aid), lvl)| (*aid, *lvl))
        .collect();
    if !skill_entries.is_empty() {
        let ability_catalog = world.resource::<mud_world::AbilityCatalog>();
        let by_id: std::collections::HashMap<i32, &mud_world::AbilityDef> = ability_catalog
            .by_name
            .values()
            .map(|d| (d.id, d))
            .collect();
        let mut by_level: std::collections::BTreeMap<i32, Vec<String>> =
            std::collections::BTreeMap::new();
        for (aid, lvl) in skill_entries {
            if let Some(a) = by_id.get(&aid) {
                by_level.entry(lvl).or_default().push(a.name.clone());
            }
        }
        out.push_str("\r\n  <cyan>Skills</> <dim>(min level):</>\r\n");
        for (lvl, mut names) in by_level {
            names.sort_unstable();
            out.push_str(&format!(
                "    <b:yellow>L{lvl:>2}</> <dim>({})</>: {}\r\n",
                names.len(),
                names
                    .iter()
                    .map(|n| render_color_tags(n, mode))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
    }
    send_prose(world, player, out);
}

/// Player-side help: every category except Admin.
pub(crate) fn cmd_help(world: &mut World, player: Entity, args: &str) {
    run_help(world, player, args, HelpScope::Player);
}

/// Builder+ counterpart: only the Admin category. Registered with
/// `min_role: Builder` so normal players can't even invoke it.
pub(crate) fn cmd_wizhelp(world: &mut World, player: Entity, args: &str) {
    run_help(world, player, args, HelpScope::Wiz);
}

#[derive(Clone, Copy)]
enum HelpScope {
    /// Player help — excludes Admin category from the index, the
    /// per-topic lookup, and the prefix-suggestion list.
    Player,
    /// Wiz / admin help — only the Admin category is in scope.
    Wiz,
}

impl HelpScope {
    fn includes(self, cmd: &Command) -> bool {
        match self {
            Self::Player => cmd.category != Category::Admin,
            Self::Wiz => cmd.category == Category::Admin,
        }
    }
    fn index_title(self) -> &'static str {
        match self {
            Self::Player => "Available commands:",
            Self::Wiz => "Admin commands:",
        }
    }
    fn invocation(self) -> &'static str {
        match self {
            Self::Player => "help",
            Self::Wiz => "wizhelp",
        }
    }
    /// Socials only ever surface through the player-side help; an
    /// admin chasing `wizhelp` is looking for command tooling.
    fn allow_socials(self) -> bool {
        matches!(self, Self::Player)
    }
}

fn run_help(world: &mut World, player: Entity, args: &str, scope: HelpScope) {
    const HELP_INDEX_COL_WIDTH: usize = 16;
    let (role, perms) = world
        .get::<Account>(player)
        .map_or((UserRole::Player, Vec::new()), |a| {
            (a.role, a.perms.clone())
        });

    let topic = args.trim().to_ascii_lowercase();
    if topic.is_empty() {
        let mut by_cat: HashMap<Category, Vec<&Command>> = HashMap::new();
        for cmd in all_commands() {
            if !visible(cmd, role, &perms) || !scope.includes(cmd) {
                continue;
            }
            by_cat.entry(cmd.category).or_default().push(cmd);
        }
        // Help index reads as a colored TOC: bold-cyan title, each
        // category as a bold-yellow header (matches `who`'s class
        // band hue) and command names in plain cyan so the eye
        // scans by category first, then alphabetically within. Names
        // wrap to a 4-column grid so a long category (Admin, Info)
        // doesn't run off the side of an 80-column terminal — the
        // single-line `join(", ")` form was the SUGGESTIONS overflow
        // bullet.
        let mode = color_mode_for(world, player);
        let width = crate::layout::wrap_width(world, player);
        let mut out = format!("\r\n<b:cyan>{}</>\r\n", scope.index_title());
        for cat in Category::ORDER {
            if let Some(cmds) = by_cat.get(cat) {
                out.push_str(&format!("\r\n  <b:yellow>{}</>\r\n", cat.label()));
                let mut names: Vec<&str> = cmds.iter().map(|c| c.names[0]).collect();
                names.sort_unstable();
                let cells: Vec<String> = names.iter().map(|n| format!("<cyan>{n}</>")).collect();
                for row in crate::layout::format_grid(&cells, HELP_INDEX_COL_WIDTH, 4, width) {
                    out.push_str(&render_color_tags(&row, mode));
                    out.push_str("\r\n");
                }
            }
        }
        out.push_str(&format!(
            "\r\n<dim>Type '{} <command>' for details.</>\r\n",
            scope.invocation()
        ));
        // DB-backed help articles (HelpEntry) — spells / lore / mechanics
        // glosses. Only nudge when the catalog has visible rows; a fresh
        // DB with zero help entries shouldn't advertise "type help X" to
        // dead-end the player. Player scope only — wiz scope is command
        // tooling, not lore.
        if matches!(scope, HelpScope::Player) {
            let viewer_level = world.get::<Profile>(player).map_or(0, |p| p.level);
            let n = world.resource::<HelpCatalog>().visible_count(viewer_level);
            if n > 0 {
                out.push_str(&format!(
                    "<dim>{n} help articles available — type 'help <topic>' to read one.</>\r\n"
                ));
            }
        }
        // Builder+ playing through `help` get a one-line nudge to the
        // admin-side index. Skipped when they're already in `wizhelp`,
        // and skipped for plain players who can't run it anyway.
        if matches!(scope, HelpScope::Player) && role.at_least(UserRole::Builder) {
            out.push_str("<dim>Builder+: type 'wizhelp' for admin commands.</>\r\n");
        }
        send_to(world, player, out);
        return;
    }

    // Special topic: `help classes` (or `help class`) renders the
    // full class catalog with subclasses grouped under their parent.
    // Lives ahead of the command-registry lookup so the topic isn't
    // shadowed by some future builtin named "class".
    if matches!(topic.as_str(), "classes" | "class") {
        render_classes_overview(world, player);
        return;
    }

    // Authored HelpEntry wins over the auto-generated command stub when
    // it matches the topic by exact keyword and the viewer's level
    // permits. A same-named command's usage rides along as a footer so
    // nothing from the stub is lost. Title-prefix matches stay below
    // the registry (handled in the HelpEntry fallback further down).
    if matches!(scope, HelpScope::Player) {
        let viewer_level = world.get::<Profile>(player).map_or(0, |p| p.level);
        let exact = world
            .resource::<HelpCatalog>()
            .lookup_exact_keyword(topic.as_str(), viewer_level);
        if let Some(entry) = exact {
            let usage = REGISTRY
                .get(topic.as_str())
                .filter(|c| scope.includes(c) && visible(c, role, &perms))
                .map(|c| c.help.usage);
            render_help_entry(world, player, &entry, usage);
            return;
        }
    }

    if let Some(cmd) = REGISTRY.get(topic.as_str()).filter(|c| scope.includes(c)) {
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
        send_prose(world, player, out);
        return;
    }

    // Social fallback. The command registry is hand-authored; socials
    // come from the DB. A topic like `sing` has no Help entry but is
    // a valid social — render its message variants as the help page
    // so `help sing` doesn't dead-end. Wiz scope skips this — socials
    // aren't admin tooling.
    if scope.allow_socials()
        && let Some(social) = world
            .resource::<SocialRegistry>()
            .get(topic.as_str())
            .cloned()
        && !social.hide
    {
        render_social_help(world, player, &social);
        return;
    }

    // HelpEntry fallback. Builder-authored articles in the `HelpEntry`
    // table — spells ("FIREBALL"), lore ("DRAGONS"), mechanic glosses
    // ("PRACTICE"). Indexed by case-insensitive keyword; the lookup
    // falls back to title-prefix match when no keyword hits. Same
    // viewer-level gate the system_text path uses.
    let viewer_level = world.get::<Profile>(player).map_or(0, |p| p.level);
    let lookup = world
        .resource::<HelpCatalog>()
        .lookup(topic.as_str(), viewer_level);
    match lookup {
        HelpLookup::Found(entry) => {
            render_help_entry(world, player, &entry, None);
            return;
        }
        HelpLookup::AmbiguousMatches(titles) => {
            const MAX_AMBIGUOUS: usize = 20;
            let mut out = format!("<dim>Multiple matches for '{topic}':</>\r\n");
            for t in titles.iter().take(MAX_AMBIGUOUS) {
                out.push_str(&format!("  <cyan>{t}</>\r\n"));
            }
            if titles.len() > MAX_AMBIGUOUS {
                out.push_str(&format!(
                    "<dim>  ...and {} more.</>\r\n",
                    titles.len() - MAX_AMBIGUOUS
                ));
            }
            out.push_str("<dim>Type the full title for the article you want.</>\r\n");
            send_to(world, player, out);
            return;
        }
        HelpLookup::NotFound => {}
    }

    // Spell / chant / song / skill fallback (G2.6 + G2.7). The
    // HelpEntry table is sparsely authored — most ability rows have
    // no help article — so `help web` returns nothing. Render the
    // AbilityCatalog entry directly when the topic matches a
    // plain_name (case-insensitive, also tolerant of "burning_hands"
    // vs "burning hands" by underscore normalization).
    let ability_key = topic.to_ascii_lowercase().replace(' ', "_");
    let ability_def = world
        .resource::<AbilityCatalog>()
        .by_name
        .get(&ability_key)
        .cloned();
    if let Some(def) = ability_def {
        render_ability_help(world, player, &def);
        return;
    }

    // Class-name fallback. `help warrior` / `help sorcerer` etc.
    // surfaces the class's description, primary stat, hit dice, and
    // an inventory of its spells (grouped by circle) and skills
    // (grouped by introduction level). Helps new players pick a
    // class. Lookup is case-insensitive on `Class.plain_name`.
    let class_def = world
        .resource::<mud_world::ClassCatalog>()
        .by_id
        .values()
        .find(|c| c.plain_name.eq_ignore_ascii_case(topic.as_str()))
        .cloned();
    if let Some(def) = class_def {
        render_class_help(world, player, &def);
        return;
    }

    // No exact match — surface visible commands and socials whose
    // primary/alias name starts with the typed prefix. Players who
    // type "sl" usually want one of slay / sleep / slots; the
    // suggestion list saves them a second `help` round-trip.
    let mut suggestions: Vec<String> = all_commands()
        .filter(|cmd| visible(cmd, role, &perms) && scope.includes(cmd))
        .filter(|cmd| {
            cmd.names
                .iter()
                .any(|n: &&'static str| n.starts_with(topic.as_str()))
        })
        .map(|cmd| cmd.names[0].to_string())
        .collect();
    if scope.allow_socials() {
        suggestions.extend(
            world
                .resource::<SocialRegistry>()
                .by_name
                .values()
                .filter(|s| !s.hide && s.name.starts_with(topic.as_str()))
                .map(|s| s.name.clone()),
        );
    }
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
            format!(" <dim>({} more)</>", suggestions.len() - MAX_SUGGESTIONS)
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

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_examine(world: &mut World, player: Entity, args: &str) {
    if refuse_if_blind(world, player) {
        return;
    }
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Examine whom or what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    // `N.target` disambiguator — `look 2.ancient` skips the first
    // "ancient" match and surfaces the second. Used here to thread
    // through both the entity search AND the extra-description
    // fallthroughs as one unified counter, so a player can step
    // past a real item to reach an extra-description that shares
    // the same keyword.
    let (mut remaining, base_needle) = crate::commands::parse_indexed_needle(target_word);
    let needle = base_needle.to_ascii_lowercase();

    // Dark-room gate (matches cmd_look). Self-target is allowed —
    // you can always introspect yourself even in pitch black.
    // Anything else fails until there's a light source in the room.
    let own_name = name_of(world, player);
    let self_word = crate::commands::matches_self(&own_name, &needle);
    if !self_word
        && room_is_dark(world, room)
        && !room_has_light(world, room)
        && !crate::commands::player_can_see_in_dark(world, player)
    {
        send_to(world, player, "It is too dark to make anything out.\r\n");
        return;
    }
    // `me` / `self` always resolve to the looker (legacy `generic_find`);
    // the player's own name resolves through the normal search below.
    let literal_self = remaining == 1 && matches!(needle.as_str(), "me" | "self");

    // Search the room — mobs and players are equally examinable; items too,
    // both on the ground and on the player's person. Indexed-needle
    // (`2.ancient`) advances past `remaining-1` matches before
    // accepting; if entities don't fill the count, the extra-
    // description fallthroughs continue advancing the counter so a
    // player can step past a real grimoire to land on its
    // "ancient" extra-description body.
    //
    // INVISIBLE items are filtered out unless the observer pierces
    // invisibility (detect invisible, HOLY_LIGHT, Immortal+). Mob/player
    // invisibility is a separate system; this filter only fires on Item
    // entities (other entities don't carry `ObjectFlags`).
    let entity_matches: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located, &Named, Option<&Keywords>)>();
        q.iter(world)
            .filter(|(e, l, n, kw)| {
                if !(l.0 == room || l.0 == player) {
                    return false;
                }
                // Actors the looker cannot see (invisible, wizinvis)
                // cannot be examined; items never carry the markers.
                if !crate::commands::can_see_player(world, player, *e) {
                    return false;
                }
                if !crate::commands::senses::item_visible_to(world, player, *e) {
                    return false;
                }
                matches(&needle, n, *kw)
            })
            .map(|(e, _, _, _)| e)
            .collect()
    };
    // Newest arrival first inside each holder, room before inventory
    // (matches the order `look` / `inventory` list them in). Rank maps are
    // built once per holder, not once per comparison.
    let mut entity_matches = entity_matches;
    let mut ranks: std::collections::HashMap<Entity, std::collections::HashMap<Entity, usize>> =
        std::collections::HashMap::new();
    for e in &entity_matches {
        if let Some(h) = world.get::<Located>(*e).map(|l| l.0) {
            ranks.entry(h).or_insert_with(|| {
                crate::commands::newest_first(world, h)
                    .into_iter()
                    .enumerate()
                    .map(|(i, x)| (x, i))
                    .collect()
            });
        }
    }
    entity_matches.sort_by_key(|&e| {
        let holder = world.get::<Located>(e).map(|l| l.0);
        // The `RoomFirst` order (see `item_target`): room, then the pack,
        // then what is worn (in `equipment` slot order).
        let worn = holder == Some(player) && world.get::<EquippedSlot>(e).is_some();
        let in_inv = if holder != Some(player) {
            0u8
        } else if worn {
            2
        } else {
            1
        };
        let rank = if worn {
            world
                .get::<EquippedSlot>(e)
                .map_or(usize::MAX, |s| crate::commands::item_target::slot_rank(s.0))
        } else {
            holder
                .and_then(|h| ranks.get(&h))
                .and_then(|m| m.get(&e))
                .copied()
                .unwrap_or(usize::MAX)
        };
        // Mobs before players, as `look` and `find_actor_in_room` order them;
        // the looker themself only after everyone else a name matches.
        (
            in_inv,
            crate::commands::mobs_before_players_key(world, e),
            e == player,
            rank,
        )
    });
    let target = if literal_self {
        Some(player)
    } else if remaining <= entity_matches.len() {
        Some(entity_matches[remaining - 1])
    } else {
        remaining -= entity_matches.len();
        None
    };
    let Some(target) = target else {
        // Fall through to the room's RoomExtraDescriptions. Keyword
        // match is case-insensitive substring against any entry's
        // keyword list, and the indexed counter still applies — N=2
        // here means "the 2nd extra description matching".
        let needle_lc = needle.to_ascii_lowercase();
        if let Some(extras) = world.get::<mud_world::RoomExtras>(room) {
            let hits: Vec<&str> = extras
                .entries
                .iter()
                .filter(|(keywords, _)| {
                    keywords
                        .iter()
                        .any(|kw| kw.to_ascii_lowercase().contains(&needle_lc))
                })
                .map(|(_, body)| body.as_str())
                .collect();
            if remaining <= hits.len() {
                let mode = color_mode_for(world, player);
                let body = render_color_tags(hits[remaining - 1].trim_end(), mode);
                send_to(world, player, format!("\r\n{body}\r\n"));
                return;
            }
            remaining -= hits.len();
        }
        // Then ObjectExtras on items in the room or in inventory.
        // Item must be visible to the player to count — same
        // located-in-room-or-on-player gate the entity search uses.
        let extras_hits: Vec<String> = {
            let mut q = world.query_filtered::<(&Located, &WorldKey), With<Item>>();
            let protos = world.resource::<mud_world::ObjectPrototypes>();
            q.iter(world)
                .filter(|(l, _)| l.0 == room || l.0 == player)
                .flat_map(|(_, key)| {
                    let proto = protos.by_key.get(&(key.zone, key.id));
                    proto
                        .map(|p| p.extras.clone())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|(keywords, _)| {
                            keywords
                                .iter()
                                .any(|kw| kw.to_ascii_lowercase().contains(&needle_lc))
                        })
                        .map(|(_, body)| body)
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        if remaining <= extras_hits.len() {
            let mode = color_mode_for(world, player);
            let rendered = render_color_tags(extras_hits[remaining - 1].trim_end(), mode);
            send_to(world, player, format!("\r\n{rendered}\r\n"));
            return;
        }
        send_rendered(
            world,
            player,
            &format!("You don't see '{target_word}' here.\r\n"),
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
    // Bold-cyan name as the headline, matching identify's title.
    // Builder-authored color tags on the name still flow through
    // — render_color_tags handles nested layers correctly.
    let mut out = if target == player {
        format!("\r\nYou look at yourself: <b:cyan>{name_rendered}</>.\r\n")
    } else {
        format!("\r\nYou look at <b:cyan>{name_rendered}</>.\r\n")
    };
    if !description.trim().is_empty() {
        let width = crate::layout::wrap_width(world, player);
        out.push_str(&format!(
            "{}\r\n",
            render_color_tags(
                &crate::layout::wrap_lines(description.trim_end(), width),
                mode
            )
        ));
    }
    if let Some(p) = posture
        && p != PostureKind::Standing
    {
        out.push_str(&format!(
            "<b:cyan>{name_rendered}</> is <yellow>{}</> here.\r\n",
            p.label()
        ));
    }
    // Identity line: level + race + class for actors. Helps a
    // player gauge mob difficulty before engaging and pick the
    // right ally for a group. Pulled from Profile (players + any
    // mob with one) or the mob proto for level-only mobs. Level
    // picks up `who_level_color`'s band so endgame mobs read in
    // bold magenta the way endgame players do on `who`.
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
        // K3: gate exact level for mortals examining a much-stronger
        // mob. Staff (Builder+) always see the number; mortals see a
        // verbal tier when the gap is wide enough that the number
        // would just say "go away." Equal-or-lower mobs keep the
        // number so a player can gauge weak prey.
        let viewer_level = world.get::<Profile>(player).map_or(0, |p| p.level);
        let staff = crate::commands::is_staff(world, player);
        let gap = prof.level - viewer_level;
        if !staff && target != player && gap >= 5 {
            let stature = if gap >= 21 {
                "of legendary stature"
            } else if gap >= 11 {
                "of formidable power"
            } else {
                "of considerable skill"
            };
            // "a Elf" → "an Elf"; same for "an Halfling" → "a Halfling".
            // First-letter vowel check is the cheap heuristic that
            // covers every race/class label in our catalog without
            // a lookup table.
            let first = race_label
                .chars()
                .next()
                .unwrap_or('x')
                .to_ascii_lowercase();
            let article = if matches!(first, 'a' | 'e' | 'i' | 'o' | 'u') {
                "an"
            } else {
                "a"
            };
            out.push_str(&format!(
                "<b:cyan>{name_rendered}</> is {article} {race_label}{class_label} {stature}.\r\n",
            ));
        } else {
            let lvl_open = who_level_color(prof.level).unwrap_or("");
            let lvl_close = if lvl_open.is_empty() { "" } else { "</>" };
            out.push_str(&format!(
                "<b:cyan>{name_rendered}</> is a level {lvl_open}{}{lvl_close} \
                 {race_label}{class_label}.\r\n",
                prof.level,
            ));
        }
        // Lifeforce + size/composition + body metrics from `RaceCatalog`.
        // Surfaced on examine so a player can gauge a stranger's
        // physique without poking their score sheet. Size + lifeforce
        // come straight from the race row; height/weight come from
        // the per-character `BodyMetrics` component when set
        // (rolled at character creation from the race + gender
        // band; absent for legacy characters whose race / gender
        // band wasn't authored yet).
        let race_def_owned = world
            .resource::<mud_world::RaceCatalog>()
            .get(&prof.race)
            .cloned();
        if let Some(def) = race_def_owned {
            let life = capitalize(&def.default_lifeforce.to_ascii_lowercase());
            out.push_str(&format!("Lifeforce: {life}.\r\n"));
            // Legacy `print_char_appearance_to_char` runs for players as
            // well as mobs: size, plus the race's composition.
            out.push_str(&appearance_line(
                &prof.gender,
                &def.default_size,
                actor_composition(world, target),
            ));
        }
        if let Some(bm) = world.get::<mud_world::BodyMetrics>(target).copied() {
            // Render height as feet+inches so the readout matches
            // how the legacy game presented it (`5'9"`); weight in
            // pounds raw.
            let feet = bm.height / 12;
            let inches = bm.height % 12;
            out.push_str(&format!(
                "Height: {feet}'{inches}\", weight: {} lbs.\r\n",
                bm.weight,
            ));
        }
    } else if world.get::<Mob>(target).is_some()
        && let Some(key) = world.get::<WorldKey>(target).copied()
        && let Some(proto) = world
            .get_resource::<MobPrototypes>()
            .and_then(|p| p.by_key.get(&(key.zone, key.id)))
        && proto.level > 0
        && crate::commands::is_staff(world, player)
    {
        // Staff-only: legacy `look` never showed a mob's numeric level
        // to mortals (they gauge difficulty with `consider`).
        let lvl_open = who_level_color(proto.level).unwrap_or("");
        let lvl_close = if lvl_open.is_empty() { "" } else { "</>" };
        out.push_str(&format!(
            "<b:cyan>{name_rendered}</> is level {lvl_open}{}{lvl_close}.\r\n",
            proto.level
        ));
    }
    if let Some(hp) = world.get::<Health>(target).copied() {
        // Health condition phrase ("is bleeding" / "is mortally
        // wounded" / etc.) graded by the same vital_color_tag the
        // score sheet uses — wounded targets read red without the
        // player having to parse the prose.
        let cond = condition_label(hp);
        let cond_open = vital_color_tag(hp.hp, hp.max).unwrap_or("");
        let cond_close = if cond_open.is_empty() { "" } else { "</>" };
        out.push_str(&format!(
            "<b:cyan>{name_rendered}</> {cond_open}{cond}{cond_close}.\r\n",
        ));
    }
    if world.get::<Shopkeeper>(target).is_some() {
        out.push_str(&format!(
            "{name_rendered} is a merchant — try 'list' to see their wares.\r\n"
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
                    Some("a banker — try 'deposit' / 'withdraw'.")
                }
                mud_db::enums::MobProfession::Trainer => {
                    Some("a trainer — try 'train' / 'practice <ability>'.")
                }
                mud_db::enums::MobProfession::Postmaster => {
                    Some("a postmaster — try 'mail <name>'.")
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
                out.push_str(&crate::commands::cap_sentence_start(&format!(
                    "{name_rendered} is {line}\r\n"
                )));
            }
        }
    }
    if world.get::<mud_world::Flying>(target).is_some() {
        out.push_str(&crate::commands::cap_sentence_start(&format!(
            "{name_rendered} hovers in mid-air.\r\n"
        )));
    }
    // Mob latent parity (Wave 2.L) atmospheric flavor lines.
    // Legacy `print_char_appearance_to_char`: "<He> is <size> in size[,
    // and is composed of <mass>]." for every mob, so the composition
    // the reporter of #46 misses sits right under the condition line.
    if world.get::<Mob>(target).is_some()
        && let Some(key) = world.get::<WorldKey>(target).copied()
        && let Some(proto) = world
            .get_resource::<MobPrototypes>()
            .and_then(|p| p.by_key.get(&(key.zone, key.id)))
    {
        let size = world
            .get::<mud_world::Sized>(target)
            .map_or(proto.size, |s| s.0);
        out.push_str(&appearance_line(
            &proto.gender,
            size.label(),
            actor_composition(world, target),
        ));
    }
    // LifeForce: only surface non-LIFE (the default). UNDEAD gets a
    // distinctive line; the other supernatural forms share a shorter
    // "aura" cue so players can identify the mob's nature without
    // probing for `detect_undead` parity.
    if let Some(mud_world::LifeForceTag(lf)) = world.get::<mud_world::LifeForceTag>(target).copied()
    {
        let line = match lf {
            mud_db::enums::LifeForce::Life => None,
            mud_db::enums::LifeForce::Undead => Some("<dim>An aura of unlife clings to it.</>"),
            mud_db::enums::LifeForce::Magic => {
                Some("<magenta>An aura of raw magic surrounds it.</>")
            }
            mud_db::enums::LifeForce::Celestial => {
                Some("<b:white>A celestial radiance attends it.</>")
            }
            mud_db::enums::LifeForce::Demonic => Some("<red>A demonic miasma roils about it.</>"),
            mud_db::enums::LifeForce::Elemental => {
                Some("<cyan>Raw elemental force ripples across it.</>")
            }
        };
        if let Some(line) = line {
            out.push_str(&format!("{line}\r\n"));
        }
    }
    // MovementMode flavor: FLYING reads as airborne even when no
    // `Flying` marker is present (proto-set vs spell-set). The other
    // non-NORMAL modes pick up short cues.
    if let Some(mud_world::MovementModeTag(mode)) =
        world.get::<mud_world::MovementModeTag>(target).copied()
    {
        let line = match mode {
            mud_db::enums::MovementMode::Normal | mud_db::enums::MovementMode::Mounted => None,
            mud_db::enums::MovementMode::Flying => {
                // Skip when the Flying marker already fired the
                // "hovers in mid-air" line above.
                if world.get::<mud_world::Flying>(target).is_some() {
                    None
                } else {
                    Some("<cyan>It hovers in the air.</>")
                }
            }
            mud_db::enums::MovementMode::Swimming => {
                Some("<cyan>It is swimming on the surface.</>")
            }
            mud_db::enums::MovementMode::Underwater => {
                Some("<cyan>It is submerged beneath the water.</>")
            }
            mud_db::enums::MovementMode::Ethereal => {
                Some("<dim>It is partly phased out of reality.</>")
            }
        };
        if let Some(line) = line {
            out.push_str(&format!("{line}\r\n"));
        }
    }
    if let Some(mud_world::Mounted(mount)) = world.get::<mud_world::Mounted>(target).copied() {
        let mount_name = name_or(world, mount, "(unknown)");
        out.push_str(&crate::commands::cap_sentence_start(&format!(
            "{name_rendered} is riding {mount_name}.\r\n"
        )));
    }
    if let Some(mud_world::RiddenBy(rider)) = world.get::<mud_world::RiddenBy>(target).copied() {
        let rider_name = name_or(world, rider, "(unknown)");
        out.push_str(&crate::commands::cap_sentence_start(&format!(
            "{rider_name} is riding {name_rendered}.\r\n"
        )));
    }
    if crate::hiding::is_hidden(world, target) && target == player {
        // Self-only — others shouldn't see your stealth marker.
        out.push_str("You are hidden.\r\n");
    }
    if target == player {
        // Self-only — hunger / thirst, same effect query the score sheet
        // uses so nourished / refreshed read alongside hungry / thirsty.
        let hunger = world.get::<mud_world::Hunger>(player).map_or(0, |h| h.0);
        let thirst = world.get::<mud_world::Thirst>(player).map_or(0, |t| t.0);
        let self_effects: Vec<String> = {
            let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
            q.iter(world)
                .filter(|(_, a)| a.0 == player)
                .map(|(inst, _)| inst.name.clone())
                .collect()
        };
        if let Some(c) = condition_summary(hunger, thirst, &self_effects) {
            let open = condition_color_tag(hunger, thirst).unwrap_or("");
            let close = if open.is_empty() { "" } else { "</>" };
            out.push_str(&format!("You feel {open}{c}{close}.\r\n"));
        }
    }
    if let Some(BoardLink(board_id)) = world.get::<BoardLink>(target).copied()
        && let Some(summary) = world
            .get_resource::<BoardCatalog>()
            .and_then(|c| c.by_id.get(&board_id))
            .cloned()
    {
        if target == player {
            // Self-only — hunger / thirst, same effect query the score sheet
            // uses so nourished / refreshed read alongside hungry / thirsty.
            let hunger = world.get::<mud_world::Hunger>(player).map_or(0, |h| h.0);
            let thirst = world.get::<mud_world::Thirst>(player).map_or(0, |t| t.0);
            let self_effects: Vec<String> = {
                let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                q.iter(world)
                    .filter(|(_, a)| a.0 == player)
                    .map(|(inst, _)| inst.name.clone())
                    .collect()
            };
            if let Some(c) = condition_summary(hunger, thirst, &self_effects) {
                let open = condition_color_tag(hunger, thirst).unwrap_or("");
                let close = if open.is_empty() { "" } else { "</>" };
                out.push_str(&format!("You feel {open}{c}{close}.\r\n"));
            }
        }
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
            "It's the {}{}{}; type 'board {}' to read it.\r\n",
            summary.title, suffix, lock, summary.alias,
        ));
    }
    // Freshness cue for corpses — same thresholds the decay tick uses
    // for its in-room atmospheric broadcasts. Players walking in late
    // need a way to read the corpse's state without waiting for the
    // next milestone tick. Hue grades by remaining time: imminent
    // dissolution reads red, mid-stage rot yellow, fresh dim.
    if let Some(decay) = world.get::<mud_world::CorpseDecay>(target).copied() {
        let line = match decay.remaining_secs {
            i32::MIN..=30 => "<red>It is on the verge of dissolution.</>",
            31..=120 => "<yellow>It reeks; flies and grubs are everywhere.</>",
            121..=300 => "<yellow>Flies have gathered; it is no longer fresh.</>",
            _ => "<dim>It is still warm.</>",
        };
        out.push_str(&format!("{line}\r\n"));
    }
    // Active effects on actors (Player or Mob), as legacy-style flavor
    // sentences (`look_auras`); magical ones need Detect Magic. Items
    // skip this — their effects are bound differently.
    if world.get::<Item>(target).is_none() {
        out.push_str(&super::look_auras::aura_lines(world, player, target));
        // Equipped gear list. Players and mobs alike — bystanders
        // should see what someone's wielding before engaging them
        // (the warhammer vs the toothpick matters tactically).
        // Mirrors `cmd_equipment`'s ordering via Slot::ORDER so the
        // readout reads consistently regardless of equip sequence.
        let worn: Vec<(Slot, String)> = {
            let mut q = world.query_filtered::<(&Located, &Named, &EquippedSlot), With<Item>>();
            q.iter(world)
                .filter(|(l, _, _)| l.0 == target)
                .map(|(_, n, eq)| (eq.0, n.name.clone()))
                .collect()
        };
        if !worn.is_empty() {
            let mut sorted = worn;
            sorted.sort_by_key(|(s, _)| {
                Slot::ORDER
                    .iter()
                    .position(|x| x == s)
                    .unwrap_or(usize::MAX)
            });
            // One piece of gear per line (legacy `print_char_equipment_to_char`).
            out.push_str(&format!("{name_rendered} is using:\r\n"));
            for (s, n) in &sorted {
                out.push_str(&format!("  <cyan>{:>14}</>: {n}\r\n", s.label()));
            }
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
            out.push_str(&format!("It weighs about <dim>{weight:.1} lbs.</>\r\n"));
        }
        // Atmospheric notes on proto-derived flags. GLOW / MAGIC
        // surface as faint cues; DECOMPOSING is cosmetic flavor
        // (rotten-smell); SOULBOUND is a bond reminder. Other
        // flags either drive command behavior (NO_DROP, INVISIBLE)
        // and don't need narration here, or stay silent until the
        // matching system lands (PERMANENT/TEMPORARY/FLOAT/etc.).
        if has_object_flag(world, target, mud_db::enums::ObjectFlag::Glow) {
            out.push_str("<yellow>It emits a soft glow.</>\r\n");
        }
        if has_object_flag(world, target, mud_db::enums::ObjectFlag::Magic) {
            out.push_str("<magenta>It pulses faintly with magic.</>\r\n");
        }
        if has_object_flag(world, target, mud_db::enums::ObjectFlag::Decomposing) {
            out.push_str("<dim>It smells faintly of rot.</>\r\n");
        }
        if has_object_flag(world, target, mud_db::enums::ObjectFlag::Soulbound) {
            out.push_str("<cyan>It is soulbound to you.</>\r\n");
        }
        // Wearable slot. The phrasing depends on whether the
        // item is *currently equipped* (has `EquippedSlot`) or
        // just *wearable in this slot* (carries `WearableIn`
        // proto-side). Without the distinction the inventory
        // listing for a stowed badge reads "It is pinned as a
        // badge." which is a lie — the player is carrying it,
        // not wearing it.
        if let Some(slot) = world.get::<WearableIn>(target).map(|w| w.0) {
            let equipped = world.get::<EquippedSlot>(target).is_some();
            let line = if equipped {
                // Currently worn / wielded. Present-tense
                // statement; covers the slot in player-relative
                // terms ("on your left finger").
                match slot {
                    Slot::Wield => "It is <cyan>wielded</>.\r\n".to_string(),
                    Slot::Hold => "It is <cyan>held</>.\r\n".to_string(),
                    Slot::Hover => "It <cyan>hovers</> beside you.\r\n".to_string(),
                    Slot::Light => "It is <cyan>held aloft</> as a light source.\r\n".to_string(),
                    Slot::About => "It is <cyan>worn about your body</>.\r\n".to_string(),
                    Slot::LeftFinger => {
                        "It is <cyan>worn</> on your <cyan>left finger</>.\r\n".to_string()
                    }
                    Slot::RightFinger => {
                        "It is <cyan>worn</> on your <cyan>right finger</>.\r\n".to_string()
                    }
                    Slot::Badge => "It is <cyan>pinned</> as a badge.\r\n".to_string(),
                    _ => format!(
                        "It is <cyan>worn</> on your <cyan>{}</>.\r\n",
                        slot.position_label()
                    ),
                }
            } else {
                // Carried / not currently equipped. Reads as a
                // capability hint — "this can go in your <slot>"
                // — so the player knows where it will land
                // without having to try `wear it`.
                match slot {
                    Slot::Wield => "It can be <cyan>wielded</>.\r\n".to_string(),
                    Slot::Hold => "It can be <cyan>held</>.\r\n".to_string(),
                    Slot::Hover => "It can be set to <cyan>hover</> beside you.\r\n".to_string(),
                    Slot::Light => {
                        "It can be <cyan>held aloft</> as a light source.\r\n".to_string()
                    }
                    Slot::About => "It can be <cyan>worn about your body</>.\r\n".to_string(),
                    Slot::LeftFinger | Slot::RightFinger => {
                        "It can be <cyan>worn</> on a <cyan>finger</>.\r\n".to_string()
                    }
                    Slot::Badge => "It can be <cyan>pinned</> as a badge.\r\n".to_string(),
                    _ => format!(
                        "It can be <cyan>worn</> on your <cyan>{}</>.\r\n",
                        slot.position_label()
                    ),
                }
            };
            out.push_str(&line);
        }
        let contents: Vec<(String, usize)> = {
            let mut q = world.query_filtered::<(Entity, &Located, &Named), With<Item>>();
            // Stack identical contents (matching look's room dedup
            // and inventory's stacking) so a corpse holding three
            // copper coins reads as one entry with `(3)`. ExpandObjs
            // lists each on its own line.
            let mut rows: Vec<(Entity, String)> = q
                .iter(world)
                .filter(|(_, l, _)| l.0 == target)
                .map(|(e, _, n)| (e, n.name.clone()))
                .collect();
            rows.retain(|(e, _)| crate::commands::senses::item_visible_to(world, player, *e));
            crate::commands::sort_newest_first(world, target, &mut rows, |r| r.0);
            for (e, name) in &mut rows {
                *name = crate::commands::senses::with_item_tags(
                    world,
                    player,
                    *e,
                    std::mem::take(name),
                );
            }
            let names: Vec<String> = rows.into_iter().map(|(_, n)| n).collect();
            stack_entries(names, has_flag(world, player, PlayerFlag::ExpandObjs))
        };
        if !contents.is_empty() {
            out.push_str(&format!("\r\n<cyan>{name_rendered} contains:</>\r\n"));
            for (item_name, count) in contents {
                let rendered = render_color_tags(&item_name, mode);
                if count > 1 {
                    out.push_str(&format!("  <dim>({count})</> {rendered}\r\n"));
                } else {
                    out.push_str(&format!("  {rendered}\r\n"));
                }
            }
        }
        // If the player has identified this item (presence of the
        // marker component, set by the `identify` spell), splice
        // in the full stat block after the contents listing. The
        // helper renders the same body `cmd_identify` uses.
        if world.get::<mud_world::Identified>(target).is_some()
            && let Some(block) = render_identify_block(world, player, target)
        {
            out.push_str(&block);
        }
    }
    send_to(world, player, out);
}

pub(crate) fn cmd_title(world: &mut World, player: Entity, args: &str) {
    let arg = mud_net::sanitize_text(args, false);
    let arg = arg.trim();
    if arg.is_empty() {
        let cur = world.get::<Title>(player).map(|t| t.0.clone());
        let line = match cur {
            Some(t) => format!("Your title: {t}\r\n"),
            None => "You have no title set. Use 'title <new>' to add one.\r\n".to_string(),
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
    let arg = mud_net::sanitize_text(args, false);
    let arg = arg.trim();
    if arg.is_empty() {
        let cur = world.get::<Description>(player).map(|d| d.0.clone());
        let line = match cur {
            Some(d) if !d.trim().is_empty() => format!("Your description:\r\n{d}\r\n"),
            _ => "You have no description set. Use 'description <prose>'.\r\n".to_string(),
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
    let mut out = format!("\r\nLevel {}    Experience: {}\r\n", p.level, p.experience);
    if let Some(lp) = crate::commands::level_progress(world, &p) {
        let into_bracket = (lp.current_xp - lp.level_floor_xp).max(0);
        let bracket = (lp.next_level_xp - lp.level_floor_xp).max(1);
        let to_go = (bracket - into_bracket).max(0);
        out.push_str(&format!(
            "Level {} to {}:  {} / {}  {} {}%  ({} to go)\r\n",
            p.level,
            p.level + 1,
            into_bracket,
            bracket,
            crate::commands::progress_bar(lp.percent),
            lp.percent,
            to_go,
        ));
    } else if mud_db::enums::is_staff_level(p.level) {
        out.push_str("Experience has no meaning for you.\r\n");
    } else if mud_world::is_starstar(world, &p) {
        out.push_str(crate::commands::MAX_MORTAL_EXP_MESSAGE);
        out.push_str("\r\n");
    } else {
        out.push_str("(No further progress to show.)\r\n");
    }
    send_to(world, player, out);
}

/// `wealth` / `gold` / `money`: split the on-hand copper total into
/// platinum/gold/silver/copper denominations. The schema's per-race
/// `copperFactor` is reserved for shop/trade math; raw display uses
/// the standard 100/10/1 ratio (1 platinum = 10 gold = 100 silver =
/// 1000 copper) so the four-coin breakdown matches `FieryMUD`'s score
/// sheet. Zero-value coins are skipped; "no coin" prints when broke.
pub(crate) fn cmd_wealth(world: &mut World, player: Entity, _args: &str) {
    let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);
    let bank = world
        .get::<mud_world::BankWealth>(player)
        .map_or(0, |b| b.0);
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
        &format!("You hand {} to {target_name}.\r\n", format_amount(amount)),
    );
    send_rendered(
        world,
        target,
        &format!(
            "{} bribes you with {}.\r\n",
            name_of(world, player),
            format_amount(amount)
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

/// Legacy shop `is_ok_char`: a keeper refuses a customer it cannot see
/// ([`can_see_player`]: invisible, hidden or blind keeper). Sends the
/// refusal itself; returns whether the trade may go ahead.
fn shopkeeper_sees_customer(
    world: &mut World,
    player: Entity,
    keeper: Entity,
    keeper_name: &str,
) -> bool {
    if can_see_player(world, keeper, player) {
        return true;
    }
    send_rendered(
        world,
        player,
        &format!(
            "{} says, 'I don't trade with someone I can't see!'\r\n",
            cap_sentence_start(keeper_name)
        ),
    );
    false
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
    if !shopkeeper_sees_customer(world, player, keeper_entity, &keeper_name) {
        return;
    }
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
                shop.items.len() + i + 1,
                mob_name,
                price_str,
                stock_str
            ));
        }
        out.push_str("\r\nUse 'buy <#|name>' (or 'hire') to take one along.\r\n");
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

/// Composition a mob renders with: its own `Mobs.composition`, or the
/// race default (`Races.default_composition`, legacy `def_composition`)
/// when the mob is left at the `FLESH` schema default. A mob explicitly
/// authored as flesh on a non-flesh race therefore reads as the race;
/// there is no separate "unset" state in the column.
pub(super) fn mob_composition(
    world: &World,
    proto: &mud_world::MobProto,
) -> mud_db::enums::Composition {
    use mud_db::enums::Composition;
    if proto.composition != Composition::Flesh {
        return proto.composition;
    }
    world
        .get_resource::<mud_world::RaceCatalog>()
        .and_then(|c| c.get(&proto.race))
        .map_or(Composition::Flesh, |r| r.default_composition)
}

/// Composition `target` has right now (legacy `GET_COMPOSITION`): a live
/// Waterform / Vaporform override, else the mob proto's own, else the
/// race default. Players carry no composition of their own.
pub(crate) fn actor_composition(world: &World, target: Entity) -> mud_db::enums::Composition {
    use mud_db::enums::Composition;
    if let Some(over) = world.get::<mud_world::CompositionOverride>(target) {
        return over.0;
    }
    if world.get::<Mob>(target).is_some()
        && let Some(key) = world.get::<WorldKey>(target).copied()
        && let Some(proto) = world
            .get_resource::<MobPrototypes>()
            .and_then(|p| p.by_key.get(&(key.zone, key.id)))
    {
        return mob_composition(world, proto);
    }
    world
        .get::<Profile>(target)
        .and_then(|p| world.get_resource::<mud_world::RaceCatalog>()?.get(&p.race))
        .map_or(Composition::Flesh, |r| r.default_composition)
}

/// Legacy `print_char_appearance_to_char` line for an actor:
/// "He is large in size." for flesh, "... and is insubstantial." for
/// ether, "... and is composed of plant material." otherwise.
/// Ends with CRLF.
fn appearance_line(gender: &str, size: &str, composition: mud_db::enums::Composition) -> String {
    use mud_db::enums::Composition;
    let pronoun = match gender.to_ascii_lowercase().as_str() {
        "male" => "He",
        "female" => "She",
        _ => "It",
    };
    let size = size.to_ascii_lowercase();
    match composition {
        Composition::Flesh => format!("{pronoun} is <yellow>{size}</> in size.\r\n"),
        Composition::Ether => {
            format!("{pronoun} is <yellow>{size}</> in size, and is <green>insubstantial</>.\r\n")
        }
        other => format!(
            "{pronoun} is <yellow>{size}</> in size, and is composed of <green>{}</>.\r\n",
            other.mass_noun()
        ),
    }
}

/// Legacy `shopping_inspect` fee: a tenth of the purchase price, never
/// less than one copper.
fn inspect_fee(buy_price: i64) -> i64 {
    (buy_price / 10).max(1)
}

/// `inspect [<#|item|pet>]`: legacy shop service. Not the staff `stat`
/// and not the admin API's `inspect_actor`. With no argument it lists
/// the keeper's wares with the inspection fee; with an item it charges
/// the fee and renders the same stat block `identify` shows, without
/// buying or marking anything. A pet offer shows the pet's stats
/// (legacy pet-shop `inspect`).
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_inspect(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
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
    if !shopkeeper_sees_customer(world, player, keeper_entity, &keeper_name) {
        return;
    }
    let Some(shop) = world
        .resource::<ShopCatalog>()
        .by_key
        .get(&(keeper_marker.shop_zone_id, keeper_marker.shop_id))
        .cloned()
    else {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} has nothing to show you.\r\n"),
        );
        return;
    };
    let object_protos = world.resource::<ObjectPrototypes>().by_key.clone();
    let mob_protos = world.resource::<MobPrototypes>().by_key.clone();

    if arg.is_empty() {
        let mut out = String::new();
        if shop.items.is_empty() && shop.pets.is_empty() {
            send_rendered(
                world,
                player,
                &format!("{keeper_name} has nothing to inspect right now.\r\n"),
            );
            return;
        }
        out.push_str(&format!("\r\n{keeper_name} will inspect:\r\n"));
        out.push_str(&format!(
            "  {:<3} {:<4} {:<40} {}\r\n",
            "#", "Lvl", "Item", "Fee"
        ));
        for (i, offer) in shop.items.iter().enumerate() {
            let proto = object_protos.get(&(offer.object_zone_id, offer.object_id));
            let item_name = proto.map_or_else(
                || format!("(missing {}/{})", offer.object_zone_id, offer.object_id),
                |p| p.name.clone(),
            );
            let base_cost = proto.map_or(0, |p| p.cost);
            let level = proto.map_or(0, |p| p.level);
            let fee = inspect_fee(shop_offer_price(offer, base_cost, shop.buy_profit));
            out.push_str(&format!(
                "  {:<3} {:<4} {:<40} {}\r\n",
                i + 1,
                level,
                item_name,
                format_wealth(fee).unwrap_or_else(|| "free".to_string()),
            ));
        }
        for (i, offer) in shop.pets.iter().enumerate() {
            let proto = mob_protos.get(&(offer.mob_zone_id, offer.mob_id));
            let pet_name = proto.map_or_else(
                || format!("(missing {}/{})", offer.mob_zone_id, offer.mob_id),
                |p| p.name.clone(),
            );
            let level = proto.map_or(0, |p| p.level);
            out.push_str(&format!(
                "  {:<3} {:<4} {:<40} {}\r\n",
                shop.items.len() + i + 1,
                level,
                pet_name,
                format_wealth(pet_inspect_fee(level)).unwrap_or_else(|| "free".to_string()),
            ));
        }
        out.push_str("\r\nUse 'inspect <#|name>' to pay the fee and examine one.\r\n");
        send_rendered(world, player, &out);
        return;
    }

    // Unified numbering matches `list` / `buy`: items first, then pets.
    let lc = arg.to_ascii_lowercase();
    let (item_idx, pet_idx): (Option<usize>, Option<usize>) = if let Ok(n) = arg.parse::<usize>() {
        if n >= 1 && n <= shop.items.len() {
            (Some(n - 1), None)
        } else {
            (
                None,
                n.checked_sub(shop.items.len() + 1)
                    .filter(|i| *i < shop.pets.len()),
            )
        }
    } else {
        let item = shop.items.iter().position(|o| {
            object_protos
                .get(&(o.object_zone_id, o.object_id))
                .is_some_and(|p| {
                    mud_world::targeting::entity_matches(&lc, &p.name, Some(&p.keywords))
                })
        });
        let pet = if item.is_some() {
            None
        } else {
            shop.pets.iter().position(|o| {
                mob_protos.get(&(o.mob_zone_id, o.mob_id)).is_some_and(|p| {
                    mud_world::targeting::entity_matches(&lc, &p.name, Some(&p.keywords))
                })
            })
        };
        (item, pet)
    };
    let staff = is_staff(world, player);
    let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);

    if let Some(idx) = item_idx {
        let offer = shop.items[idx];
        let Some(proto) = object_protos
            .get(&(offer.object_zone_id, offer.object_id))
            .cloned()
        else {
            send_to(world, player, "That item's prototype is missing.\r\n");
            return;
        };
        let fee = inspect_fee(shop_offer_price(&offer, proto.cost, shop.buy_profit));
        if !staff && on_hand < fee {
            send_rendered(
                world,
                player,
                &format!(
                    "{keeper_name} eyes you. \"You need {} to have that inspected.\"\r\n",
                    format_wealth(fee - on_hand).unwrap_or_else(|| "more coin".to_string())
                ),
            );
            return;
        }
        // Render against a throwaway copy of the wares item: it lives
        // on the keeper (not the player, so no inventory/GMCP churn),
        // carries `Identified` so the block shows full detail, and is
        // despawned right after.
        let mut bundle = world.spawn((
            Item,
            Named {
                name: proto.name.clone(),
            },
            Keywords(proto.keywords.clone()),
            WorldKey {
                zone: proto.zone_id,
                id: proto.id,
            },
            Located(keeper_entity),
            mud_world::Identified,
        ));
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
        let temp = bundle.id();
        mud_world::attach_proto_charges(world, temp, proto.zone_id, proto.id);
        let block = render_identify_block(world, player, temp);
        world.despawn(temp);
        let Some(block) = block else {
            return;
        };
        if !staff && let Some(mut w) = world.get_mut::<Wealth>(player) {
            w.0 = w.0.saturating_sub(fee);
        }
        send_rendered(world, player, &block);
        let player_name = name_of(world, player);
        broadcast_room_except_rendered(
            world,
            located.0,
            &[player],
            &format!("{player_name} inspects {}.\r\n", proto.name),
        );
        return;
    }

    if let Some(idx) = pet_idx {
        let offer = shop.pets[idx];
        let Some(proto) = mob_protos.get(&(offer.mob_zone_id, offer.mob_id)).cloned() else {
            send_to(world, player, "That mob's prototype is missing.\r\n");
            return;
        };
        let fee = pet_inspect_fee(proto.level);
        if !staff && on_hand < fee {
            send_to(world, player, "You don't have enough money!\r\n");
            return;
        }
        if !staff && let Some(mut w) = world.get_mut::<Wealth>(player) {
            w.0 = w.0.saturating_sub(fee);
        }
        // A mob's level is misleading (issue #83): only staff see it.
        let level_label = if staff {
            format!("Level: {}, ", proto.level)
        } else {
            String::new()
        };
        let out = format!(
            "Name: {}\r\n{level_label}Hit Points: {}, Movement Points: {}\r\n\
             Damage: {}d{}+{}, Accuracy: {}, Evasion: {}\r\n{}",
            proto.name,
            proto.rolled_hp(),
            proto.move_points,
            proto.damage_dice_num,
            proto.damage_dice_size,
            proto.damage_dice_bonus,
            proto.accuracy,
            proto.evasion,
            appearance_line(
                &proto.gender,
                proto.size.label(),
                mob_composition(world, &proto),
            ),
        );
        send_rendered(world, player, &out);
        return;
    }

    send_rendered(
        world,
        player,
        &format!("{keeper_name} doesn't have '{arg}' to inspect.\r\n"),
    );
}

/// Legacy `PET_INSPECT_PRICE`: `(lvl^2 + 3*lvl) / 10` copper.
fn pet_inspect_fee(level: i32) -> i64 {
    let l = i64::from(level.max(0));
    (l * l + 3 * l) / 10
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
    if !shopkeeper_sees_customer(world, player, keeper_entity, &keeper_name) {
        return;
    }
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
    // Parse: integer = 1-based index; otherwise prefix match on proto name/keywords.
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
                .is_some_and(|p| {
                    mud_world::targeting::entity_matches(&lc, &p.name, Some(&p.keywords))
                })
        })
    };
    let Some(idx) = offer_idx else {
        // Pet and mount shops sell mobs, not items: legacy players just
        // `buy` them, so fall through to the keeper's pet offerings.
        if !shop.pets.is_empty() {
            cmd_hire(world, player, arg);
            return;
        }
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
    let Some(proto) = object_protos
        .get(&(offer.object_zone_id, offer.object_id))
        .cloned()
    else {
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
            &format!("{keeper_name} eyes you. \"You need {need_msg} more for that.\"\r\n"),
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
        Named {
            name: proto.name.clone(),
        },
        Keywords(proto.keywords.clone()),
        WorldKey {
            zone: proto.zone_id,
            id: proto.id,
        },
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
    let bought = bundle.id();
    mud_world::attach_proto_charges(world, bought, proto.zone_id, proto.id);
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
    if !shopkeeper_sees_customer(world, player, keeper_entity, &keeper_name) {
        return;
    }
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
    // Pets are numbered after the shop's items in `list`, so `#` here is
    // the same unified number `buy` accepts.
    let offer_idx: Option<usize> = if let Ok(n) = arg.parse::<usize>() {
        n.checked_sub(shop.items.len() + 1)
            .filter(|i| *i < shop.pets.len())
    } else {
        let lc = arg.to_ascii_lowercase();
        shop.pets.iter().position(|o| {
            mob_protos.get(&(o.mob_zone_id, o.mob_id)).is_some_and(|p| {
                mud_world::targeting::entity_matches(&lc, &p.name, Some(&p.keywords))
            })
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
            &format!("{keeper_name} eyes you. \"You need {need_msg} more for that.\"\r\n"),
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
    // Name is renamed to "<player>'s <mob_name>" (minus the mob name's
    // leading article) so room listings disambiguate from wild mobs of the same proto. The
    // `PersistentPet` marker tags this as a paid follower so the
    // disconnect-save snapshots it (gold spent → durable across
    // reconnect under the 1h cap).
    let player_name = name_of(world, player);
    let pet_name = crate::commands::possessive_name(&player_name, &proto.name);
    let pet_entity = mud_world::spawn_mob_from_proto(world, &proto, located.0, None);
    if let Ok(mut em) = world.get_entity_mut(pet_entity) {
        em.insert((
            Named { name: pet_name },
            Posture(PostureKind::Standing),
            Follower(player),
            mud_world::PersistentPet,
        ));
    }
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
    if !shopkeeper_sees_customer(world, player, keeper_entity, &keeper_name) {
        return;
    }
    let Some(item) = find_item(world, player, target_word, ItemClass::Inventory) else {
        send_rendered(
            world,
            player,
            &format!("You aren't carrying '{target_word}'.\r\n"),
        );
        return;
    };
    let item_name = name_of(world, item);
    // NO_SELL / SOULBOUND gates — quest items and bound gear can't
    // be liquidated for coin. Done before the shop-catalog lookup
    // so the player sees a specific reason rather than the generic
    // "shopkeeper isn't interested" fall-through.
    if has_restriction(world, item, mud_db::enums::ObjectRestriction::NoSell) {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} refuses to buy {item_name}.\r\n"),
        );
        return;
    }
    if has_object_flag(world, item, mud_db::enums::ObjectFlag::Soulbound) {
        send_rendered(
            world,
            player,
            &format!("{item_name} is soulbound — it cannot be sold.\r\n"),
        );
        return;
    }
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
    let item_proto = world.get::<WorldKey>(item).and_then(|k| {
        world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(k.zone, k.id))
            .cloned()
    });
    let base_cost = item_proto.as_ref().map_or(0, |p| p.cost);
    // Sell-side filter: the item must match at least one `accepts` rule. An
    // empty list means the keeper buys nothing (legacy `trade_with`: no
    // buytype matches). Type tokens normalize to upper + no-underscore on
    // both sides so DRINK_CONTAINER matches the schema's DRINKCONTAINER.
    // Keyword filter is empty = no extra gate; non-empty = at least one
    // keyword must appear in the item's `Keywords`.
    let item_type_norm = item_proto
        .as_ref()
        .map(|p| object_type_token(p.r#type))
        .unwrap_or_default();
    let item_kws: Vec<String> = world
        .get::<Keywords>(item)
        .map(|k| k.0.iter().map(|s| s.to_ascii_lowercase()).collect())
        .unwrap_or_default();
    let accepted = shop.accepts.iter().any(|rule| {
        let rule_type = rule.object_type.replace('_', "").to_ascii_uppercase();
        if rule_type != item_type_norm {
            return false;
        }
        if rule.keywords.is_empty() {
            return true;
        }
        rule.keywords.iter().any(|k| {
            item_kws
                .iter()
                .any(|ik| ik.contains(&k.to_ascii_lowercase()))
        })
    });
    if !accepted {
        send_rendered(
            world,
            player,
            &format!("{keeper_name} isn't interested in {item_name}.\r\n"),
        );
        return;
    }
    let item_key = world.get::<WorldKey>(item).map(|k| (k.zone, k.id));
    let pay_copper = shop_sell_payout(&shop, base_cost, item_key);
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
    crate::equip_apply::despawn_item(world, item);
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
    if world.get::<Located>(player).is_none() {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    }
    let lc = needle.to_ascii_lowercase();
    let target = find_item(world, player, &lc, ItemClass::RoomFirst);
    let Some(target) = target else {
        send_to(
            world,
            player,
            format!("You can't find anything called '{needle}' to value.\r\n"),
        );
        return;
    };
    let proto = world.get::<WorldKey>(target).and_then(|k| {
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
            format!(
                "\r\nYou haven't trained any abilities yet. ({points} practice point(s) available.)\r\n"
            ),
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
        // Use `name` (Title Case display name) rather than the raw
        // SCREAMING_SNAKE_CASE `plain_name` — "Two-Handed
        // Bludgeoning" beats "TWO_HAND_BLUDGEONING" on the practice
        // sheet. Fall through to a "#id" stub when the catalog
        // lookup somehow misses (defensive — shouldn't happen).
        let name = def.map_or_else(|| format!("ability #{id}"), |d| d.name.clone());
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
            if matches!(
                def.map(|d| d.kind),
                Some(mud_db::abilities::AbilityKind::Skill)
            ) {
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
        let pct = proficiency_percent(*prof);
        let tier = proficiency_tier_label(pct);
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
    let stats = world.get::<CoreStats>(player).copied().unwrap_or_default();
    let points = world
        .get::<mud_world::SkillPoints>(player)
        .map_or(0, |s| s.0);
    // Per-race stat caps from `RaceCatalog` (loaded from
    // `Races.max_*` columns). The trainable cap is the lower of
    // the legacy `TRAIN_STAT_CAP` (18 — kept as a soft ceiling on
    // practice-point spending) and the race max (the schema's
    // hard ceiling, typically 76+). A race whose authoring lowers
    // a specific cap below 18 will see that cap respected here.
    let race = world
        .get::<Profile>(player)
        .map(|p| p.race.clone())
        .unwrap_or_default();
    let race_catalog = world.resource::<mud_world::RaceCatalog>();
    let effective_cap = |stat: &str| -> i32 {
        let race_max = race_catalog.stat_cap(&race, stat, i32::MAX);
        TRAIN_STAT_CAP.min(race_max)
    };
    if arg.is_empty() {
        // Show each stat with its derived bonus so a player can see
        // what training a stat would actually do for their rolls
        // (a 13 → 14 bump still gives +2 bonus; 14 → 15 unlocks +3
        // at the next odd-step boundary). Same `(stat - 10) / 2`
        // formula the score sheet uses via `CoreStats::bonus`.
        let mut out = String::from("\r\nCurrent stats:\r\n");
        let pair = |val: i32, cap: i32| format!("{val:>2}({:+})/{cap}", CoreStats::bonus(val));
        out.push_str(&format!(
            "  str {}   dex {}   con {}\r\n",
            pair(stats.strength, effective_cap("strength")),
            pair(stats.dexterity, effective_cap("dexterity")),
            pair(stats.constitution, effective_cap("constitution")),
        ));
        out.push_str(&format!(
            "  int {}   wis {}   cha {}\r\n",
            pair(stats.intelligence, effective_cap("intelligence")),
            pair(stats.wisdom, effective_cap("wisdom")),
            pair(stats.charisma, effective_cap("charisma")),
        ));
        out.push_str(&format!("Practice points: {points}\r\n"));
        out.push_str("Use 'train <stat>' to spend one.\r\n");
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
    let cap = effective_cap(label);
    if current >= cap {
        send_to(
            world,
            player,
            format!("Your {label} is at the cap of {cap}.\r\n"),
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

    // NoTrackingRoom gate at the source — if the tracker stands in
    // a no-tracking room they get no trail at all. Mirrors the
    // legacy "you can't sense anything here" message before any
    // candidate scan so the wording doesn't imply the target is
    // gone.
    if world.get::<mud_world::NoTrackingRoom>(start).is_some() {
        send_rendered(
            world,
            player,
            "The trail is lost — this place defies your senses.\r\n",
        );
        return;
    }

    // Collect every room->target candidate in one pass: any entity
    // with Named matching the needle (excluding the player), keyed
    // by their current room. Then BFS rooms until we hit one in the
    // candidate map.
    let candidate_rooms: HashSet<Entity> = {
        let mut q = world.query::<(Entity, &Located, &Named, Option<&Keywords>)>();
        q.iter(world)
            .filter(|(e, _, n, kw)| {
                *e != player
                    && mud_world::targeting::entity_matches(
                        &needle,
                        &n.name,
                        kw.map(|k| k.0.as_slice()),
                    )
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
            // NoTrackingRoom is treated as a wall on both sides:
            // the trail neither enters nor passes through. Match
            // candidates inside such rooms are unreachable — the
            // overall search will fall through to "too far away".
            if world.get::<mud_world::NoTrackingRoom>(to).is_some() {
                continue;
            }
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
                if world.get::<mud_world::NoTrackingRoom>(to).is_some() {
                    continue;
                }
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
/// Distance phrase for the legacy-style mob scan. Index 0 is the
/// caster's own room ("right here"); 1+ are stepped outward, with
/// the direction word filled in by the caller.
fn scan_distance(dis: usize) -> &'static str {
    match dis {
        0 => "right",
        1 => "immediately",
        2 => "close by",
        3 => "a ways off",
        _ => "far far",
    }
}

/// Posture / flight tag for each scanned actor — three letters with
/// a sphere-style color cue so a fleeing mob's "fly" pops, while
/// "sit"/"slp" stay restful. Mirrors the legacy contract closely
/// (legacy used "fly"/"std"/"sit" only; we extend with "rst"/"slp"
/// since the new posture enum carries the extra resolution).
fn scan_posture_tag(world: &World, entity: Entity) -> &'static str {
    if world.get::<Flying>(entity).is_some() {
        return "<cyan>fly</>";
    }
    match world.get::<Posture>(entity).map(|p| p.0) {
        Some(PostureKind::Sleeping) => "<dim>slp</>",
        Some(PostureKind::Resting) => "<green>rst</>",
        Some(PostureKind::Sitting | PostureKind::Kneeling) => "<green>sit</>",
        _ => "std",
    }
}

/// Append "  <distance phrase> : <name> (<posture>)" lines for
/// every visible actor in `room` other than the scanner. Returns
/// the count appended so the caller can decide whether to print
/// the "you don't see anyone" fallback. The distance label is
/// `<scan_distance(dis)> [dir]` — at dis=0 the direction is
/// suppressed ("right here"), otherwise it follows ("immediately
/// north").
fn scan_room_actors(
    world: &mut World,
    out: &mut String,
    room: Entity,
    dis: usize,
    dir: Direction,
    scanner: Entity,
) -> usize {
    let actors: Vec<(Entity, String)> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &Named),
            Or<(With<Mob>, (With<Player>, With<Online>))>,
        >();
        q.iter(world)
            .filter(|(e, l, _)| {
                *e != scanner && l.0 == room && crate::commands::can_see_player(world, scanner, *e)
            })
            .map(|(e, _, n)| (e, n.name.clone()))
            .collect()
    };
    let label = if dis == 0 {
        format!("{} here", scan_distance(dis))
    } else {
        format!("{} {}", scan_distance(dis), direction_name(dir))
    };
    for (entity, name) in &actors {
        let posture = scan_posture_tag(world, *entity);
        out.push_str(&format!("  <cyan>{label:>22}</> : {name} ({posture})\r\n",));
    }
    actors.len()
}

/// How many rooms out `scan` reaches (legacy `do_scan` `maxdis`): staff 3,
/// everyone else 1. Farsee adds one, and a further one for each of two
/// Sphere of Divination rolls the skill beats (`roll(33, 75) <= skill`,
/// `roll(75, 150) <= skill`), so only real casters see far. `roll(lo, hi)`
/// is an inclusive random draw, injected for tests.
pub(crate) fn scan_max_distance(
    world: &World,
    player: Entity,
    roll: &mut dyn FnMut(i32, i32) -> i32,
) -> usize {
    let mut maxdis = if crate::commands::is_staff(world, player) {
        3
    } else {
        1
    };
    if world.get::<mud_world::Farsee>(player).is_some() {
        maxdis += 1;
        let divination = crate::hiding::skill_pct(world, player, "sphere_divination");
        if roll(33, 75) <= divination {
            maxdis += 1;
        }
        if roll(75, 150) <= divination {
            maxdis += 1;
        }
    }
    maxdis
}

pub(crate) fn cmd_scan(world: &mut World, player: Entity, _args: &str) {
    if refuse_if_blind(world, player) {
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let here = located.0;
    // NoScanningRoom: anti-scan wards block the caster's senses
    // outright. Mirrors the "you don't see anyone" branch instead
    // of inventing a separate message — the legacy assassin/scout
    // contract was just "scan fails here", not flavor text.
    if world.get::<mud_world::NoScanningRoom>(here).is_some() {
        send_to(
            world,
            player,
            "Your senses can't penetrate the wards in this room.\r\n",
        );
        return;
    }
    // Dark-room gate: scan is purely visual. With no light and no
    // dark-vision the scanner sees nothing — refuse outright rather
    // than printing an empty result. (Infravision / heat-source
    // detection isn't wired up yet; once it lands, this gate should
    // skip the refusal for observers carrying it and instead filter
    // the per-room actor list to heat-producing entities only.)
    if crate::commands::room_is_dark(world, here)
        && !crate::commands::room_has_light(world, here)
        && !crate::commands::player_can_see_in_dark(world, player)
    {
        send_to(world, player, "It is pitch black; you can see nothing.\r\n");
        return;
    }
    // Legacy maxdis: rogues/assassins/staff get 3, everyone else 1, plus
    // Farsee. We don't carry a class-name string on the runtime side yet
    // (class is an i32 catalog id), so for now staff get the long
    // scan and everyone else gets one room. Future: extend by
    // checking Profile.class_id against a "stealth-leaning"
    // catalog flag.
    let maxdis = scan_max_distance(world, player, &mut |lo, hi| rand::random_range(lo..=hi));

    let mut out = String::from("\r\nYou scan the area, and see:\r\n");
    // Current room first (dis=0 gets "right here").
    let mut found = scan_room_actors(world, &mut out, here, 0, Direction::North, player);

    // Walk each direction up to maxdis. The legacy scan stops at
    // any closed door, hidden exit (unless staff), or dangling
    // destination — same checks the existing `exit_is_hidden_to`
    // uses for the look path.
    for dir in [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
        Direction::Up,
        Direction::Down,
    ] {
        let mut from_room = here;
        for dis in 1..=maxdis {
            let Some(exits) = world.get::<Exits>(from_room).cloned() else {
                break;
            };
            let Some(ed) = exits
                .0
                .iter()
                .find(|(d, _)| **d == dir)
                .map(|(_, e)| e.clone())
            else {
                break;
            };
            if ed.state != ExitState::Open {
                break;
            }
            if exit_is_hidden_to(world, player, from_room, dir, &ed) {
                break;
            }
            let Some(to_room) = ed.to else {
                break;
            };
            // NoScanningRoom is a one-way curtain — the scan stops
            // at the threshold and doesn't see into or beyond it.
            // Matches the source-room gate above for symmetry.
            if world.get::<mud_world::NoScanningRoom>(to_room).is_some() {
                break;
            }
            // God zones are invisible to mortals; scanning stops at the edge.
            if !crate::room_access::room_visible_to(world, player, to_room) {
                break;
            }
            found += scan_room_actors(world, &mut out, to_room, dis, dir, player);
            from_room = to_room;
        }
    }

    if found == 0 {
        send_to(world, player, "\r\nYou don't see anyone.\r\n");
        return;
    }
    send_rendered(world, player, &out);

    // Broadcast a subtle "$n scans the area." tell so other actors
    // in the room know the player is peeking.
    let scanner_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        here,
        &[player],
        &format!(
            "{} scans the area.\r\n",
            crate::commands::cap_sentence_start(&scanner_name),
        ),
    );
}

inventory::submit! {
    Command {
        names: &["search"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "search [<keyword>]",
            summary: "Search the area for hidden objects, exits and hiding characters.",
            long: "Refuses while fighting. Hidden objects come \
                   first: 'search' checks the ones lying in the room, \
                   'search <container>' the ones inside it. Your \
                   perception cuts each one's hiding by a random \
                   amount, and whatever is left within your \
                   perception is found (staff find them all); finding \
                   something ends the search. Otherwise each hidden \
                   exit you haven't found is checked in turn: naming its \
                   keyword ('search monolith') always finds it, \
                   otherwise your Intelligence is rolled against \
                   0-200. The first exit found is revealed (staff always find one). A \
                   search leaves you busy for half a combat round. Reveals are \
                   per-character and session-scoped — a fresh \
                   login starts back at zero known hidden exits, \
                   matching the legacy contract until a persistent \
                   table for revealed exits lands. Pair with \
                   'exits' afterward to confirm what surfaced. \
                   When no exit turns up, you look for hiding \
                   characters outside your group instead: your \
                   perception cuts each one's hiding by a random \
                   amount, and whoever is left within your \
                   perception is found.",
        },
        run: cmd_search,
    }
}

/// `search [<keyword>]`: look for hidden exits and add the find to the
/// player's `RevealedExits` set. Legacy `search_for_doors`
/// (act.informative.cpp:933) walks the hidden exits in N/E/S/W/U/D
/// order and reveals the first one that either matches the keyword
/// argument or passes `INT > random(0, 200)`, then stops; the roll is
/// not made for a keyword match.
pub(crate) fn cmd_search(world: &mut World, player: Entity, args: &str) {
    search_with_roll(world, player, args, &mut |hi| rand::random_range(0..=hi));
}

/// Legacy `search_for_doors` visits directions in this order; the
/// diagonals and `in`/`out` (absent from legacy) follow.
const SEARCH_ORDER: [Direction; 12] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
    Direction::Up,
    Direction::Down,
    Direction::Northeast,
    Direction::Northwest,
    Direction::Southeast,
    Direction::Southwest,
    Direction::In,
    Direction::Out,
];

/// Legacy `isname(arg, keywords)`: the argument prefixes a keyword word.
fn exit_keyword_matches(keywords: &[String], arg: &str) -> bool {
    !arg.is_empty()
        && keywords.iter().flat_map(|k| k.split_whitespace()).any(|w| {
            w.len() >= arg.len() && w.as_bytes()[..arg.len()].eq_ignore_ascii_case(arg.as_bytes())
        })
}

/// Legacy `WAIT_STATE(ch, PULSE_VIOLENCE / 2)` after a search: half a
/// combat round. Ticks are 10 Hz, a round is 40 (`REENGAGE_LAG_TICKS`).
const SEARCH_LAG_TICKS: u64 = 20;

/// Tick before which the searcher may not search again.
#[derive(Component, Debug, Clone, Copy)]
struct SearchLag {
    until: u64,
}

/// Staff (legacy `GET_LEVEL >= LVL_IMMORT`) always find hidden exits.
pub(crate) fn is_immortal(world: &World, e: Entity) -> bool {
    world
        .get::<Account>(e)
        .is_some_and(|a| a.role.at_least(UserRole::Immortal))
}

/// `cmd_search` with the `random(0, 200)` roll injected: `roll(200)`
/// is called once per hidden exit that did not match the keyword.
pub(crate) fn search_with_roll(
    world: &mut World,
    player: Entity,
    args: &str,
    roll: &mut dyn FnMut(i32) -> i32,
) {
    // Legacy `do_search` words its own refusal (staff are exempt, which
    // `is_blind` already covers).
    if crate::commands::senses::is_blind(world, player) {
        send_to(world, player, "You're blind and can't see a thing!\r\n");
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You're too busy fighting to search!\r\n");
        return;
    }
    let now = world.get_resource::<crate::TickCount>().map_or(0, |t| t.0);
    let staff = is_immortal(world, player);
    if !staff
        && world
            .get::<SearchLag>(player)
            .is_some_and(|l| l.until > now)
    {
        send_to(
            world,
            player,
            "You are still recovering from your last search.\r\n",
        );
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere to search.\r\n");
        return;
    };
    let room = located.0;
    try_insert(
        world,
        player,
        SearchLag {
            until: now + SEARCH_LAG_TICKS,
        },
    );
    let player_name = name_of(world, player);
    send_to(world, player, "You search the area carefully...\r\n");
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} searches the area.\r\n"),
    );

    // Legacy `do_search`: hidden objects first (the ones lying here, or the
    // contents of a container named by the argument); only when none turn
    // up, doors, then hiding characters.
    let (items, in_container) = search_scope(world, player, room, args);
    if search_objects(world, player, room, &items, staff, roll) {
        return;
    }
    if in_container {
        send_to(world, player, "You find nothing of interest.\r\n");
        return;
    }
    let Some(nothing) = search_exits(world, player, room, args, &player_name, staff, roll) else {
        return;
    };
    if search_characters(world, player, room, staff, roll) {
        return;
    }
    send_to(world, player, nothing);
}

/// What `search` looks through: the contents of the container the
/// argument names (legacy `generic_find` over inventory, equipment and
/// room; `true` then means "skip doors and characters"), otherwise the
/// items lying in the room.
fn search_scope(
    world: &mut World,
    player: Entity,
    room: Entity,
    args: &str,
) -> (Vec<Entity>, bool) {
    let arg = args.split_whitespace().next().unwrap_or("");
    let container = (!arg.is_empty())
        .then(|| find_item(world, player, arg, ItemClass::RoomFirst))
        .flatten()
        .filter(|c| crate::commands::is_container_entity(world, *c));
    let holder = container.unwrap_or(room);
    let mut items: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
        q.iter(world)
            .filter(|(e, l)| l.0 == holder && *e != player)
            .map(|(e, _)| e)
            .collect()
    };
    crate::commands::sort_newest_first(world, holder, &mut items, |e| *e);
    (items, container.is_some())
}

/// The hidden-object half of `search` (legacy `do_search`,
/// act.informative.cpp:1002): every hidden item the searcher could see
/// if it were not hidden has its hiddenness cut by
/// `random(perception / 2, perception)`, and is found once what is left
/// is within the searcher's perception. The cut stays even when the item
/// is not found. Staff find everything, and keep looking after the first
/// find. `roll(hi)` draws `0..=hi`. True when something was found.
fn search_objects(
    world: &mut World,
    player: Entity,
    room: Entity,
    items: &[Entity],
    staff: bool,
    roll: &mut dyn FnMut(i32) -> i32,
) -> bool {
    use crate::hiding;
    let perception = hiding::perception_of(world, player);
    let searcher_name = name_of(world, player);
    let mut found = false;
    for &item in items {
        if found && !staff {
            break;
        }
        let orig = hiding::hiddenness(world, item);
        if orig == 0 {
            continue;
        }
        // Could the searcher see it were it not hidden?
        hiding::set_hiddenness(world, item, 0);
        let sees = crate::commands::senses::item_visible_to(world, player, item);
        hiding::set_hiddenness(world, item, orig);
        if !sees {
            continue;
        }
        let cut = perception / 2 + roll(perception - perception / 2);
        hiding::set_hiddenness(world, item, (orig - cut).max(0));
        if hiding::hiddenness(world, item) > perception && !staff {
            continue;
        }
        hiding::set_hiddenness(world, item, 0);
        let item_name = name_of(world, item);
        let (mine, theirs) = if orig <= perception {
            ("reveal", "reveals")
        } else {
            ("find", "finds")
        };
        send_rendered(world, player, &format!("You {mine} {item_name}!\r\n"));
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{searcher_name} {theirs} {item_name}!\r\n"),
        );
        found = true;
    }
    found
}

/// The hidden-exit half of `search`. `None` when an exit was found (and
/// reported); otherwise the line to show if nothing else turns up either.
fn search_exits(
    world: &mut World,
    player: Entity,
    room: Entity,
    args: &str,
    player_name: &str,
    staff: bool,
    roll: &mut dyn FnMut(i32) -> i32,
) -> Option<&'static str> {
    let arg = args.split_whitespace().next().unwrap_or("");
    let hidden: Vec<(Direction, bool)> = world
        .get::<Exits>(room)
        .map(|e| {
            SEARCH_ORDER
                .iter()
                .filter_map(|d| e.0.get(d).map(|ed| (*d, ed)))
                .filter(|(_, ed)| ed.is_hidden)
                .map(|(d, ed)| (d, exit_keyword_matches(&ed.keywords, arg)))
                .collect()
        })
        .unwrap_or_default();
    if hidden.is_empty() {
        return Some("You find nothing of interest.\r\n");
    }
    let already: std::collections::HashSet<(Entity, Direction)> = world
        .get::<RevealedExits>(player)
        .map(|r| r.set.clone())
        .unwrap_or_default();
    let candidates: Vec<(Direction, bool)> = hidden
        .into_iter()
        .filter(|(d, _)| !already.contains(&(room, *d)))
        .collect();
    if candidates.is_empty() {
        return Some("You find nothing new.\r\n");
    }
    let intelligence = world.get::<CoreStats>(player).map_or(0, |s| s.intelligence);
    let found = candidates
        .into_iter()
        .find(|(_, keyword_hit)| *keyword_hit || staff || intelligence > roll(200))
        .map(|(d, _)| d);
    let Some(found) = found else {
        return Some("You find nothing of interest.\r\n");
    };
    let newly = [found];
    let mut next = already;
    for dir in &newly {
        next.insert((room, *dir));
    }
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(RevealedExits { set: next });
    }
    // Legacy wording: "You have found a hidden monolith to the east."
    // The noun is the first word of the exit's first keyword ("door"
    // when it has none); plural nouns drop the article.
    let mut out = String::new();
    let mut others = String::new();
    for dir in &newly {
        let kw = world
            .get::<Exits>(room)
            .and_then(|e| e.0.get(dir))
            .and_then(|ed| ed.keywords.first())
            .and_then(|k| k.split_whitespace().next())
            .unwrap_or("door")
            .to_string();
        let article = if kw.ends_with('s') { "" } else { " a" };
        let place = match dir {
            Direction::Up => "in the ceiling".to_string(),
            Direction::Down => "in the floor".to_string(),
            _ => format!("to the {}", direction_name(*dir)),
        };
        out.push_str(&format!("You have found{article} hidden {kw} {place}.\r\n"));
        others.push_str(&format!(
            "{player_name} has found{article} hidden {kw} {place}.\r\n"
        ));
    }
    send_rendered(world, player, &out);
    broadcast_room_except_players_rendered(world, room, &[player], &others);
    None
}

/// The hidden-character half of `search` (legacy `do_search`,
/// act.informative.cpp:1036): every hiding non-group character the
/// searcher could see if it were not hiding has its hiddenness cut by
/// `random(perception / 2, perception)`, and is found once what is left
/// is within the searcher's perception. The cut stays even when the
/// character is not found. Staff find everyone, and keep looking after
/// the first find. `roll(hi)` draws `0..=hi`. True when someone was found.
fn search_characters(
    world: &mut World,
    player: Entity,
    room: Entity,
    staff: bool,
    roll: &mut dyn FnMut(i32) -> i32,
) -> bool {
    use crate::hiding;
    let perception = hiding::perception_of(world, player);
    let hidden: Vec<Entity> = {
        let mut q = world
            .query_filtered::<(Entity, &Located), bevy_ecs::query::Or<(With<Player>, With<Mob>)>>();
        q.iter(world)
            .filter(|(e, l)| l.0 == room && *e != player)
            .map(|(e, _)| e)
            .collect()
    };
    let searcher_name = name_of(world, player);
    let mut found = false;
    for target in hidden {
        if found && !staff {
            break;
        }
        let orig = hiding::hiddenness(world, target);
        if orig == 0 || hiding::same_group(world, player, target) {
            continue;
        }
        // Could the searcher see it were it not hiding?
        hiding::set_hiddenness(world, target, 0);
        let sees = can_see_player(world, player, target);
        hiding::set_hiddenness(world, target, orig);
        if !sees {
            continue;
        }
        let cut = perception / 2 + roll(perception - perception / 2);
        hiding::set_hiddenness(world, target, (orig - cut).max(0));
        if hiding::hiddenness(world, target) > perception && !staff {
            continue;
        }
        hiding::reveal(world, target);
        let target_name = name_of(world, target);
        let verb = if orig <= perception {
            "point out"
        } else {
            "find"
        };
        send_to(
            world,
            player,
            format!("You {verb} {target_name} lurking here!\r\n"),
        );
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player, target],
            &format!("{searcher_name} points out {target_name} lurking here!\r\n"),
        );
        if hiding::perception_of(world, target) + roll(200) > perception {
            send_to(
                world,
                target,
                format!("You think {searcher_name} has spotted you!\r\n"),
            );
        }
        found = true;
    }
    found
}

inventory::submit! {
    Command {
        names: &["diagnose", "diag"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "diagnose [<target>]",
            summary: "Verbose health readout for self or a target in your room.",
            long: "With no arg, reports your own condition band (e.g. \
                   'mortally wounded'), your raw HP / Stamina, and your \
                   posture. With a target, reports their condition band \
                   and posture only — exact numbers are private. Pair with \
                   'glance' for a one-line summary or 'score' for the full \
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
        let Some(t) = find_actor_in_room(world, target_word, located.0, player) else {
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
                   needs to confirm identity. Pair with 'score' for the \
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
        send_to(
            world,
            player,
            format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    let name = name_of(world, target);
    let hp = world.get::<Health>(target).copied();
    let cond = hp.map_or("looks fine", condition_label);
    // Wrap the condition phrase in vital-graded color so a glance
    // at a wounded target reads red without forcing the player to
    // parse the words. None → render plain (target is fine enough).
    let cond_open = hp.and_then(|h| vital_color_tag(h.hp, h.max));
    let cond_text = cond_open.map_or_else(|| cond.to_string(), |open| format!("{open}{cond}</>"));
    let posture = world
        .get::<Posture>(target)
        .map_or("standing", |p| p.0.label());
    let fighting = world
        .get::<Fighting>(target)
        .map(|f| name_or(world, f.0, "(gone)"));
    // Posture parenthetical dims (it's framing, not the headline);
    // the fighting clause is the alarm — bold-red so an in-progress
    // brawl is obvious at a glance.
    let mut line = format!("\r\n<b:cyan>{name}</> <dim>({posture})</> {cond_text}");
    if let Some(target_name) = fighting {
        line.push_str(&format!(" <b:red>— fighting {target_name}</>"));
    }
    line.push_str(".\r\n");
    send_rendered(world, player, &line);
}

/// Tell a blind `player` so and return true (legacy `YOU_ARE_BLIND`
/// guard of look, exits, read and scan). False for anyone who can see.
fn refuse_if_blind(world: &mut World, player: Entity) -> bool {
    if !crate::commands::senses::is_blind(world, player) {
        return false;
    }
    send_to(world, player, crate::commands::senses::YOU_ARE_BLIND);
    true
}

/// The look a mover gets on arriving in a new room (legacy
/// `look_at_room`): a blind mover sees "infinite darkness" instead of
/// the room, anyone else gets a normal `look`.
pub(crate) fn look_after_move(world: &mut World, player: Entity) {
    if crate::commands::senses::is_blind(world, player) {
        send_to(
            world,
            player,
            "You see nothing but infinite darkness...\r\n",
        );
        return;
    }
    cmd_look(world, player, "");
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_look(world: &mut World, player: Entity, args: &str) {
    if refuse_if_blind(world, player) {
        return;
    }
    let arg = args.trim();
    // `look at <x>` ≡ `look <x>` for every target. Only strip when
    // something follows ("at" alone stays a literal needle).
    let arg = match arg.get(..3) {
        Some(head) if head.eq_ignore_ascii_case("at ") && !arg[3..].trim().is_empty() => {
            arg[3..].trim_start()
        }
        _ => arg,
    };
    if !arg.is_empty() {
        if let Some(dir) = parse_direction(arg) {
            look_direction(world, player, dir);
            return;
        }
        // `look in <container>` — list the container's contents.
        // Carried, equipped, and in-room containers all resolve.
        // Done before the examine fallthrough so the player gets
        // the inventory listing instead of the bare description.
        let lower = arg.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("in ").map(str::trim_start)
            && !rest.is_empty()
        {
            look_in_container(world, player, rest);
            return;
        }
        // `look at sky` / `look stars` / `look horizon`: roll up the
        // scattered weather/time/season readouts into one line. Done
        // before the examine fallthrough so it works even though
        // there's no `sky` entity to find.
        if matches!(lower.as_str(), "sky" | "stars" | "horizon" | "heavens") {
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

    // GMCP Room.Info goes out BEFORE the room text (every move calls
    // `look` with no argument) so mappers see the room data first. In
    // a room the viewer can't see it is `{}`, never the room's data.
    crate::commands::gmcp::send_room_info(world, player, true);
    // Room.Mobs/Services ride along so a client that just moved always
    // gets a fresh occupant list, even if it is identical to the last.
    crate::commands::send_room_mobs(world, player, true);

    // Dark-room gate: caves, underdark, underwater, and outdoor
    // rooms at night print only "It is pitch black..." plus exits
    // (AUTO_EXIT) — unless someone in the room carries a Lit item,
    // or the player has HOLY_LIGHT (admin/staff toggle).
    if !crate::commands::gmcp::viewer_sees_room(world, player, room) {
        let mut out = String::from("\r\nIt is pitch black; you can see nothing.\r\n");
        // HUM items still carry through the dark — sound doesn't
        // need light. Single line regardless of count; the room
        // gets one ambient cue, not one per humming item.
        let any_hum_dark = world
            .query_filtered::<(&Located, &mud_world::ObjectFlags), With<Item>>()
            .iter(world)
            .any(|(l, f)| l.0 == room && f.has(mud_db::enums::ObjectFlag::Hum));
        if any_hum_dark {
            out.push_str("You hear something humming nearby.\r\n");
        }
        // Infravision shows living things as red shapes, and sense life
        // counts what it cannot see; neither reveals the room itself.
        let senses = crate::commands::senses::dark_room_lines(world, player, room);
        out.push_str(&render_color_tags(&senses, color_mode_for(world, player)));
        // In pitch black with no dark-vision the player can't see
        // where the room leads, so exits are suppressed entirely —
        // even when AUTO_EXIT is on. (A future search/feel-the-wall
        // skill would surface them; nothing today does.)
        send_to(world, player, out);
        // The who's-here panel still has to be replaced: empty (or
        // shapes only) in the dark, never the previous room's list.
        send_room_players_snapshot(world, player);
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

    // Other players in the room, one status line each (name, title, and
    // what they are doing: "Bob is sleeping here."), newest arrival first.
    let other_players: Vec<String> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        let mut rows: Vec<(Entity, ())> = q
            .iter(world)
            .filter(|(e, l)| {
                *e != player && l.0 == room && crate::commands::can_see_player(world, player, *e)
            })
            .map(|(e, _)| (e, ()))
            .collect();
        crate::commands::sort_newest_first(world, room, &mut rows, |r| r.0);
        rows.into_iter()
            .map(|(e, ())| {
                let mut line = room_line::player_line(world, player, e);
                if let Some(aura) = crate::commands::senses::alignment_aura(world, player, e) {
                    line.push(' ');
                    line.push_str(aura);
                }
                line
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
        let mut lines: Vec<String> = Vec::new();
        let mut q = world.query_filtered::<(
            Entity,
            &Located,
            Option<&Description>,
            Option<&CombatStats>,
        ), With<Mob>>();
        // Newest arrival first (legacy `char_to_room` pushes the list head).
        let mut mob_rows: Vec<(Entity, String, Option<i32>)> = q
            .iter(world)
            .filter(|(e, l, _, _)| {
                l.0 == room && crate::commands::can_see_player(world, player, *e)
            })
            .map(|(e, _, desc, stats)| {
                (
                    e,
                    desc.map_or_else(String::new, |d| d.0.clone()),
                    stats.map(|s| s.alignment),
                )
            })
            .collect();
        crate::commands::sort_newest_first(world, room, &mut mob_rows, |r| r.0);
        for (mob, desc, alignment) in mob_rows {
            // The long description while the mob is in its default state,
            // otherwise its name and what it is doing (asleep, held,
            // fighting, flying...): see `room_line`.
            let body = room_line::mob_line(world, player, mob, &desc);
            // Default yellow on plain mob descriptions so they stand
            // out from the white room description above. Builder
            // colors are preserved — `colorize_default` only adds
            // hue when the body carries no XML-Lite markup.
            let body = colorize_default(&body, "<yellow>");
            let mut line = if alignment.is_some_and(|a| a <= aggro_threshold) {
                format!("{body} <red>(HOSTILE)</>")
            } else {
                body
            };
            if let Some(aura) = crate::commands::senses::alignment_aura(world, player, mob) {
                line.push(' ');
                line.push_str(aura);
            }
            lines.push(line);
        }
        stack_entries(lines, has_flag(world, player, PlayerFlag::ExpandMobs))
            .into_iter()
            .map(|(line, n)| {
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
    //
    // INVISIBLE items vanish from the listing unless the observer
    // pierces invisibility (detect invisible, HOLY_LIGHT, Immortal+).
    let items: Vec<String> = {
        let mut names: Vec<String> = Vec::new();
        let mut q = world.query_filtered::<(Entity, &Located, &Named), With<Item>>();
        // Newest arrival first (legacy `obj_to_room` pushes the list head).
        let mut item_rows: Vec<_> = q.iter(world).filter(|(_, l, _)| l.0 == room).collect();
        crate::commands::sort_newest_first(world, room, &mut item_rows, |r| r.0);
        for (e, _l, n) in item_rows {
            if !crate::commands::senses::item_visible_to(world, player, e) {
                continue;
            }
            let line = item_room_line(world, e, &n.name);
            names.push(crate::commands::senses::with_item_tags(
                world, player, e, line,
            ));
        }
        stack_entries(names, has_flag(world, player, PlayerFlag::ExpandObjs))
            .into_iter()
            .map(|(name, n)| {
                if n > 1 {
                    format!("<dim>({n})</> {name}")
                } else {
                    name
                }
            })
            .collect()
    };
    // Auditory cue: blind / pitch-dark observers can still HEAR
    // HUM items in the room. Pre-flight check — if any HUM item
    // is on the floor we add a "you hear something humming" line
    // below regardless of light state. Cheap to compute (filter
    // pass + bool).
    let any_hum_item = world
        .query_filtered::<(&Located, &mud_world::ObjectFlags), With<Item>>()
        .iter(world)
        .any(|(l, f)| l.0 == room && f.has(mud_db::enums::ObjectFlag::Hum));

    let mode = color_mode_for(world, player);
    let mut out = String::new();
    // Append a `[peaceful]` tag to the room title when the room is
    // marked `Room.is_peaceful` — combat is refused here, so a
    // visible cue saves the player from typing `attack` and
    // wondering why nothing happened.
    let peaceful_tag = if world.get::<mud_world::PeacefulRoom>(room).is_some() {
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
        // Legacy layout: 3-space first-line indent, paragraphs
        // separated by a blank line, reflowed to the viewport.
        let width = crate::layout::wrap_width(world, player);
        out.push_str(&format!(
            "{}\r\n",
            render_color_tags(
                &crate::layout::format_room_description(&room_desc, width),
                mode
            )
        ));
    }
    // Subtle flavor lines for "exceptional" room flags. The other
    // flags (NoMagic/NoRecall/NoSummon/NoTeleport/NoTracking/
    // NoScanning/NoPortals/NoMobs/Indoor/Soundproof) stay invisible
    // — players discover them by trying. Match the legacy "feel"
    // contract: only DeathTrap, Arena, and Guildhall get a hint,
    // because a player can't recover from blind-walking into a DT
    // and the social contract for PK / class training depends on
    // the player knowing where they are.
    if !has_flag(world, player, PlayerFlag::Brief) {
        if world.get::<mud_world::DeathTrap>(room).is_some() {
            out.push_str(&render_color_tags(
                "<b:red>An aura of doom hangs heavy here.</>\r\n",
                mode,
            ));
        }
        if world.get::<mud_world::ArenaRoom>(room).is_some() {
            out.push_str(&render_color_tags(
                "<b:yellow>Combat is welcome here.</>\r\n",
                mode,
            ));
        }
        if world.get::<mud_world::GuildhallRoom>(room).is_some() {
            out.push_str(&render_color_tags(
                "<b:cyan>This is a guild hall.</>\r\n",
                mode,
            ));
        }
    }
    // Wall sentences, one per walled direction. Rendered ahead of
    // the mob block so the player sees active barriers as part of
    // the room's atmosphere rather than having to type `exits` to
    // discover them. Skipped when the room has no walls (the common
    // case).
    if let Some(walls) = world.get::<mud_world::RoomBlockedExits>(room) {
        let mut entries: Vec<(mud_db::enums::Direction, mud_world::RoomBlockedExit)> = walls
            .by_direction
            .iter()
            .map(|(d, w)| (*d, w.clone()))
            .collect();
        entries.sort_by_key(|(d, _)| direction_order(*d));
        for (dir, wall) in entries {
            let color = match wall.traversal {
                mud_world::WallTraversal::Block => "<red>",
                mud_world::WallTraversal::Slow => "<yellow>",
                mud_world::WallTraversal::Passable => "<cyan>",
            };
            let verb = match wall.traversal {
                mud_world::WallTraversal::Block => "blocks",
                mud_world::WallTraversal::Slow => "chokes",
                mud_world::WallTraversal::Passable => "shimmers across",
            };
            let line = format!(
                "{color}A {kind} {verb} the way {dir}.</>",
                kind = wall.kind_label,
                dir = direction_name(dir),
            );
            out.push_str(&format!("{}\r\n", render_color_tags(&line, mode)));
        }
    }
    // Blank line between description / weather and the occupant / item
    // block (legacy prints the description with its own trailing
    // blank), so those lines don't read as another sentence of the
    // description. Only inserted when there's something to separate.
    if !mob_lines.is_empty() || !other_players.is_empty() || !items.is_empty() {
        out.push_str("\r\n");
    }
    for line in &mob_lines {
        out.push_str(&format!("{}\r\n", render_color_tags(line, mode)));
    }
    for line in &other_players {
        out.push_str(&format!("{}\r\n", render_color_tags(line, mode)));
    }
    let sensed = crate::commands::senses::lit_room_lines(world, player, room);
    out.push_str(&render_color_tags(&sensed, mode));
    // One object per line, like the mob lines above (legacy
    // `list_obj_to_char`, SHOW_LONG_DESC): no header, no commas.
    for line in &items {
        out.push_str(&format!("{}\r\n", render_color_tags(line, mode)));
    }
    // Atmospheric cue: anything HUMs? Surfaces a single ambient
    // line, after the items list so the player connects "I see
    // these items + I hear a hum". An INVISIBLE+HUM item (e.g. a
    // magical-but-cloaked instrument) thus reveals its presence to
    // an ear that the eye misses.
    if any_hum_item {
        out.push_str("<dim>You hear something humming nearby.</>\r\n");
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
            // Wall snapshot for the autoexit line — same source
            // `cmd_exits` consults so the two surfaces stay
            // consistent. Walled directions get a `[W]` (block) /
            // `[F]` (fog) / `[I]` (illusion) suffix, taking
            // precedence over the door-state suffix because a
            // wall is the more urgent obstacle.
            let walls: std::collections::HashMap<
                mud_db::enums::Direction,
                mud_world::RoomBlockedExit,
            > = world
                .get::<mud_world::RoomBlockedExits>(room)
                .map(|b| b.by_direction.clone())
                .unwrap_or_default();
            let names: Vec<String> = exits
                .iter()
                .map(|(d, state)| {
                    let suffix = if let Some(w) = walls.get(d) {
                        match w.traversal {
                            mud_world::WallTraversal::Block => "[W]",
                            mud_world::WallTraversal::Slow => "[F]",
                            mud_world::WallTraversal::Passable => "[I]",
                        }
                    } else {
                        match state {
                            ExitState::Open => "",
                            ExitState::Closed => "[C]",
                            ExitState::Locked => "[L]",
                        }
                    };
                    let open = exit_state_color(*state);
                    let raw = format!("{open}{}{suffix}</>", direction_name(*d));
                    render_color_tags(&raw, mode)
                })
                .collect();
            out.push_str(&format!("{header} {}\r\n", names.join(", ")));
        }
    }
    send_to(world, player, out);
    // Mudlet "who's here" panel: snapshot every other player in
    // the room. Skipped silently when the viewer has no Connection
    // (mob inspection through `switch`).
    send_room_players_snapshot(world, player);
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
    // Rows are free-flowing like legacy `who`: the title follows the
    // name directly (it reads as a last name), not a padded column.
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
            .filter(|(e, _, _, _, _, _, _)| crate::commands::can_see_player(world, player, *e))
            .map(|(e, n, t, f, last, prof, clan)| WhoRow {
                entity: e,
                name: n.name.clone(),
                title: t.map(|t| t.0.clone()),
                afk: f.is_some_and(|pf| pf.has(PlayerFlag::Afk)),
                idle: last.map(|l| l.0.elapsed().as_secs()),
                level: prof.map_or(0, |p| p.level),
                stars: prof.filter(|p| mud_world::is_starstar(world, p)).map(|p| {
                    p.class_id
                        .and_then(|cid| class_lookup.get(&cid))
                        .map_or_else(|| "**".to_string(), |n| mud_world::stars_for_name(n))
                }),
                clan_abbrev: clan.map(|c| c.clan_abbrev.clone()),
                class_name: prof
                    .and_then(|p| p.class_id)
                    .and_then(|cid| class_lookup.get(&cid).cloned()),
                race: prof
                    .map(|p| crate::commands::pretty_effect_label(&p.race).replace(' ', "-"))
                    .filter(|r| !r.is_empty()),
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
    raw_sorted.sort_by(|a, b| {
        b.level
            .cmp(&a.level)
            .then_with(|| b.stars.is_some().cmp(&a.stars.is_some()))
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut last_was_staff = None;
    for r in &raw_sorted {
        let root = roots.get(&r.entity).copied().unwrap_or(r.entity);
        let in_group = group_size.get(&root).copied().unwrap_or(0) > 1;
        // Staff are listed first (level sort); a blank line keeps them
        // apart from the mortals below.
        let is_staff = r.level >= 100;
        if last_was_staff == Some(true) && !is_staff {
            out.push_str("\r\n");
        }
        last_was_staff = Some(is_staff);
        out.push_str("  ");
        if let Some(stars) = &r.stars {
            // Level-99 mortal at the XP cap: the class stars replace the
            // level number, as in legacy `who` (`[Sor **]`).
            out.push_str(&format!("[L {stars}] "));
        } else if r.level > 0 {
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
        // Name, then the title right after it: titles are written to
        // read as a surname ("Strider the Wanderer").
        out.push_str(&r.name);
        if let Some(t) = r.title.as_deref().filter(|t| !t.trim().is_empty()) {
            out.push(' ');
            out.push_str(t.trim());
        }
        if let Some(race) = &r.race {
            out.push_str(&format!(" (<cyan>{race}</>)"));
        }
        if let Some(class) = &r.class_name {
            // class is the catalog `name` field (carries authored
            // color); render-time mapping turns the tags into ANSI.
            out.push_str(&format!(" [{class}]"));
        }
        if let Some(abbrev) = &r.clan_abbrev {
            out.push_str(&format!(" [<b:yellow>{abbrev}</>]"));
        }
        if in_group {
            out.push_str(" [<b:green>G</>]");
        }
        // Honor-roll mark for endgame / staff tier. The glyph is
        // routed through the connection's charset encoding, so
        // ASCII clients see `*` instead of a character they can't
        // render.
        if is_staff {
            out.push_str(" [<b:yellow>\u{2605}</>]");
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

pub(crate) fn cmd_trophy(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    // Builder+ can inspect another player's trophy.
    let target = if arg.is_empty() {
        player
    } else if !crate::commands::is_staff(world, player) {
        send_to(world, player, "You can only view your own trophy.\r\n");
        return;
    } else {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        let Some((e, _)) = q
            .iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(arg))
        else {
            send_to(world, player, format!("'{arg}' isn't online.\r\n"));
            return;
        };
        e
    };
    let entries = world
        .get::<mud_world::Trophy>(target)
        .map(|t| t.entries.clone())
        .unwrap_or_default();
    let target_name = name_of(world, target);
    if entries.is_empty() {
        let line = if target == player {
            "<green>Your trophy list is empty.</>\r\n".to_string()
        } else {
            format!("<green>{target_name}'s trophy list is empty.</>\r\n")
        };
        send_rendered(world, player, &line);
        return;
    }
    let mut out = if target == player {
        String::from("\r\n<green>Your trophy list:</>\r\n\r\n")
    } else {
        format!("\r\n<green>{target_name}'s trophy list:</>\r\n\r\n")
    };
    out.push_str("  <b:red>Kills</>     <u>Target</>\r\n");
    // Most recent first. VecDeque is in-order push_back, so reverse.
    let rows: Vec<_> = entries.iter().rev().collect();
    for entry in rows {
        // Color band by count: low = yellow (mild penalty), mid =
        // bold-yellow, high = red (heavy penalty).
        let color = if entry.amount < 4.99 {
            "<yellow>"
        } else if entry.amount < 7.99 {
            "<b:yellow>"
        } else {
            "<red>"
        };
        out.push_str(&format!(
            "  {color}{:>6.2}</>     {}\r\n",
            entry.amount, entry.display_name,
        ));
    }
    crate::commands::send_rendered(world, player, &out);
}

pub(crate) fn cmd_summonmount(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Fighting>(player).is_some() {
        send_to(
            world,
            player,
            "You can't focus enough while you're fighting.\r\n",
        );
        return;
    }
    if world.get::<Profile>(player).is_none() {
        return;
    }
    // The Summon Mount skill's class rows (Paladin / Anti-Paladin at level
    // 15 in the legacy data) decide who may call a mount, and from when.
    let ability = world
        .get_resource::<mud_world::CoreAbilities>()
        .and_then(|c| c.summon_mount);
    match crate::commands::skill_access(world, player, ability) {
        crate::commands::SkillAccess::Allowed => {}
        crate::commands::SkillAccess::NeedsLevel(_) => {
            send_to(
                world,
                player,
                "You aren't yet deemed worthy of a mount — gain a few more levels.\r\n",
            );
            return;
        }
        crate::commands::SkillAccess::Refused => {
            send_to(
                world,
                player,
                "You have no idea what you're trying to accomplish.\r\n",
            );
            return;
        }
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let sector = world.get::<RoomSector>(room).map(|s| s.0);
    if !sector.is_some_and(crate::camp::sector_allows_camp) {
        send_to(world, player, "Try again — outdoors this time.\r\n");
        return;
    }
    // Refuse if a Mountable follower of this player is already in
    // the room.
    let already_mounted: bool = {
        let mut q = world.query_filtered::<&Follower, (With<Mob>, With<mud_world::Mountable>)>();
        q.iter(world).any(|f| f.0 == player)
    };
    if already_mounted {
        send_to(world, player, "You already have a mount.\r\n");
        return;
    }

    // Pick a proto: the first MobProto whose keywords match the
    // basic horse/steed heuristic. Future polish: scale by level
    // / alignment per the legacy mount_types table.
    let candidate: Option<(i32, i32)> = {
        let protos = world.resource::<MobPrototypes>();
        protos
            .by_key
            .iter()
            .find(|(_, p)| {
                p.keywords.iter().any(|k| {
                    let lc = k.to_ascii_lowercase();
                    lc.contains("horse") || lc.contains("steed") || lc.contains("mount")
                })
            })
            .map(|((z, id), _)| (*z, *id))
    };
    let Some((zone, id)) = candidate else {
        send_to(
            world,
            player,
            "No mount could be found in the world. Tell a god.\r\n",
        );
        return;
    };
    // Reuse the MCP/admin spawn flow indirectly: build a minimal
    // mob entity from the proto. We deliberately skip MobResets
    // bookkeeping (no FromMobReset) so the respawn tick won't
    // re-fill us, and skip Shopkeeper / triggers — a summoned
    // mount is generic.
    let proto = world
        .resource::<MobPrototypes>()
        .by_key
        .get(&(zone, id))
        .cloned();
    let Some(proto) = proto else { return };
    let player_name = name_of(world, player);
    let mount_entity = mud_world::spawn_mob_from_proto(world, &proto, room, None);
    if let Ok(mut em) = world.get_entity_mut(mount_entity) {
        em.insert((
            Posture(PostureKind::Standing),
            mud_world::Mountable,
            Follower(player),
        ));
    }
    let mount_name = name_of(world, mount_entity);
    send_rendered(
        world,
        player,
        &format!("{mount_name} answers your summons!\r\n"),
    );
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!(
            "{mount_name} walks in, seemingly from nowhere, and nuzzles {player_name}'s face.\r\n"
        ),
    );
}

pub(crate) fn cmd_camp(world: &mut World, player: Entity, args: &str) {
    use mud_world::{Camping, ObjectPrototypes, WorldKey};
    if world.get::<Camping>(player).is_some() {
        send_to(world, player, "You're already setting up camp.\r\n");
        return;
    }
    if crate::commands::in_combat(world, player) {
        send_to(world, player, "You are too busy to do this!\r\n");
        return;
    }
    if world.get::<mud_world::Mounted>(player).is_some() {
        send_to(world, player, "You'd better dismount first.\r\n");
        return;
    }
    if world.get::<mud_world::Flying>(player).is_some() {
        send_to(world, player, "You can't pitch a tent in mid-air.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    if world.get::<mud_world::IndoorRoom>(room).is_some() {
        send_to(world, player, "You always pitch a tent indoors?\r\n");
        return;
    }
    let sector = world.get::<RoomSector>(room).map(|s| s.0);
    let allows = sector.is_some_and(crate::camp::sector_allows_camp);
    if !allows {
        send_to(
            world,
            player,
            "This isn't a place to camp. Find a stretch of \
             wilderness — woods, hills, fields, beach, ruins, \
             swamp, or open road.\r\n",
        );
        return;
    }
    // Rest / repose R2: optional `camp <kit-name>` resolves to an
    // item in inventory whose proto carries `campKitTier`. The kit
    // is held by reference on the `Camping` component and consumed
    // only at completion (NOT on cancel — design doc edge-case
    // table is explicit).
    let needle = args.trim().to_ascii_lowercase();
    let (kit_entity, kit_world_key, kit_tier_bonus) = if needle.is_empty() {
        (None, None, 0)
    } else {
        // Walk inventory items for a name/keyword match, then check
        // the proto's camp_kit_tier. A name-match that isn't a kit
        // surfaces a specific refusal so the player knows the item
        // is wrong, not the keyword.
        let candidate: Option<(Entity, WorldKey)> =
            find_item(world, player, &needle, ItemClass::Carried)
                .and_then(|e| world.get::<WorldKey>(e).map(|wk| (e, *wk)));
        let Some((item, wk)) = candidate else {
            send_to(
                world,
                player,
                format!("You don't have a kit called '{needle}'.\r\n"),
            );
            return;
        };
        let tier_bonus = world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(wk.zone, wk.id))
            .and_then(|p| p.camp_kit_tier);
        let Some(tier_bonus) = tier_bonus else {
            let item_name = name_of(world, item);
            send_to(world, player, format!("{item_name} isn't a camp kit.\r\n"));
            return;
        };
        (Some(item), Some((wk.zone, wk.id)), tier_bonus)
    };
    let now_tick = world.resource::<TickCount>().0;
    try_insert(
        world,
        player,
        Camping {
            since_tick: now_tick,
            started_in: room,
            kit_entity,
            kit_world_key,
            kit_tier_bonus,
        },
    );
    let player_name = name_of(world, player);
    if kit_entity.is_some() {
        send_rendered(
            world,
            player,
            "<b:cyan>You start setting up camp with your kit.</>\r\n",
        );
    } else {
        send_rendered(world, player, "<b:cyan>You start setting up camp.</>\r\n");
    }
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} starts setting up camp.\r\n"),
    );
}

pub(crate) fn cmd_point(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Point at what? Or whom?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let player_name = name_of(world, player);

    // Direction first — so "point n" beats a mob whose keyword is "n".
    if let Some(dir) = parse_direction(arg) {
        let dir_name = direction_name(dir);
        send_rendered(world, player, &format!("You point {dir_name}.\r\n"));
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} points {dir_name}.\r\n"),
        );
        return;
    }

    // Self-target.
    if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        send_rendered(world, player, "You point at yourself.\r\n");
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} points at themself.\r\n"),
        );
        return;
    }

    // Actor in the room: player or mob.
    if let Some(target) = find_actor_in_room(world, arg, room, player) {
        let target_name = name_of(world, target);
        let was_hidden = crate::hiding::is_hidden(world, target);
        if was_hidden {
            // Reveal — pointing at someone hidden gives them away.
            crate::hiding::reveal(world, target);
            send_rendered(
                world,
                player,
                &format!("You point out {target_name}'s hiding place!\r\n"),
            );
            broadcast_room_except_players_rendered(
                world,
                room,
                &[player, target],
                &format!("{player_name} points out {target_name}, who was hiding here!\r\n"),
            );
            // The revealed actor sees the call-out personally.
            crate::commands::send_rendered(
                world,
                target,
                &format!("{player_name} points out your hiding place!\r\n"),
            );
        } else {
            send_rendered(world, player, &format!("You point at {target_name}.\r\n"));
            broadcast_room_except_players_rendered(
                world,
                room,
                &[player, target],
                &format!("{player_name} points at {target_name}.\r\n"),
            );
            crate::commands::send_rendered(
                world,
                target,
                &format!("{player_name} points at you.\r\n"),
            );
        }
        return;
    }

    // Object in the room or in inventory.
    let item = find_item(world, player, arg, ItemClass::RoomFirst);
    if let Some(item) = item {
        let item_name = name_of(world, item);
        send_rendered(world, player, &format!("You point at {item_name}.\r\n"));
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} points at {item_name}.\r\n"),
        );
        return;
    }

    send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
}

pub(crate) fn cmd_aggr(world: &mut World, player: Entity, _args: &str) {
    use crate::combat::{HateList, MobMemory};
    let here = world.get::<Located>(player).map(|l| l.0);
    // Snapshot every mob whose HateList or MobMemory contains the
    // player. Carry along the room so we can flag who's adjacent.
    let mut rows: Vec<(Entity, String, Option<Entity>, bool, bool)> = {
        let mut q = world.query_filtered::<(
            Entity,
            &Named,
            Option<&Located>,
            Option<&HateList>,
            Option<&MobMemory>,
        ), With<Mob>>();
        q.iter(world)
            .filter_map(|(e, n, l, hate, mem)| {
                let in_hate = hate.is_some_and(|h| h.0.contains(&player));
                let in_mem = mem.is_some_and(|m| m.0.contains(&player));
                if !in_hate && !in_mem {
                    return None;
                }
                Some((e, n.name.clone(), l.map(|l| l.0), in_hate, in_mem))
            })
            .collect()
    };
    if rows.is_empty() {
        send_to(
            world,
            player,
            "Nothing has you on its bad side right now.\r\n",
        );
        return;
    }
    // Sort: same room first, hate-list before memory.
    rows.sort_by_key(|(_, _, room, hate, _)| {
        let same_room_first = i32::from(here.is_none_or(|h| Some(h) != *room));
        let hate_first = i32::from(!*hate);
        (same_room_first, hate_first)
    });
    let mut out = String::from("\r\n<b:cyan>Things hostile to you:</>\r\n");
    for (_, name, room, in_hate, in_mem) in &rows {
        let here_flag = if here.is_some_and(|h| Some(h) == *room) {
            " <b:red>[here]</>"
        } else {
            ""
        };
        let mode = if *in_hate {
            "<red>actively hunting</>"
        } else if *in_mem {
            "<yellow>remembers you</>"
        } else {
            ""
        };
        let room_label = match room {
            Some(r) => crate::commands::name_or(world, *r, "(unknown)"),
            None => String::new(),
        };
        out.push_str(&format!(
            "  {name} — {mode}{here_flag} <dim>(in {room_label})</>\r\n",
        ));
    }
    crate::commands::send_rendered(world, player, &out);
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
        out.push_str(&format!("  {padded_name} {padded_idle} {online_label}\r\n"));
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
        let mut seen: Vec<String> = Vec::new();
        for (inst, applied) in q.iter(world) {
            if applied.0 != player {
                continue;
            }
            // Permanent effects (innate, worn gear, mob defaults) are
            // listed too, tagged so they read apart from timed buffs.
            let label = if inst.remaining_secs < 0 {
                format!("{} (permanent)", effect_label(world, inst))
            } else {
                effect_label(world, inst)
            };
            if !seen.contains(&label) {
                seen.push(label);
            }
        }
        seen
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
    let location_owned: Option<(String, i32, i32)> =
        world.get::<Located>(player).map(|l| l.0).map(|room| {
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
    let wealth = world.get::<Wealth>(player).map_or(0, |w| w.0);
    let bank = world.get::<BankWealth>(player).map_or(0, |b| b.0);
    let hunger = world.get::<mud_world::Hunger>(player).map_or(0, |h| h.0);
    let thirst = world.get::<mud_world::Thirst>(player).map_or(0, |t| t.0);
    let drunkenness = world
        .get::<mud_world::Drunkenness>(player)
        .map_or(0, |d| d.0);
    let kill_total = world
        .get::<mud_world::KillStats>(player)
        .map_or(0, |k| k.total);
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
    let level_title_owned: Option<String> = profile_owned.as_ref().and_then(|(lvl, ..)| {
        world
            .resource::<mud_world::LevelTable>()
            .title_for(*lvl)
            .map(str::to_string)
    });
    // Next-level HP / Stamina preview — reads the row for
    // `level + 1` so the player sees what they'll gain on the
    // upcoming level-up. None at the cap (no row above max) or
    // when the table hasn't loaded yet.
    let next_level_gains: Option<(i32, i32, i32)> = profile_owned.as_ref().and_then(|(lvl, ..)| {
        let next = lvl + 1;
        world
            .resource::<mud_world::LevelTable>()
            .gains_for(next)
            .map(|(hp, st)| (next, hp, st))
    });
    let stars_owned: Option<String> = world.get::<Profile>(player).and_then(|p| {
        mud_world::is_starstar(world, p).then(|| {
            p.class_id
                .and_then(|id| {
                    world
                        .resource::<mud_world::ClassCatalog>()
                        .by_id
                        .get(&id)
                        .map(mud_world::ClassDef::stars)
                })
                .unwrap_or_else(|| "**".to_string())
        })
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
            (*lvl, cls.as_str(), race.as_str(), gender.as_str(), *xp)
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
        active_effects: &active_effects,
        group_status: GroupStatus {
            leader: leader_name.as_deref(),
            member_count: group_size,
        },
        // Score's level-progress reads the live LevelTable scaled by the
        // character's class factor, so the percent agrees with `level`.
        level_progress: world
            .get::<Profile>(player)
            .and_then(|p| crate::commands::level_progress(world, p)),
        stars: stars_owned.as_deref(),
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
            let visible_total = catalog.by_id.values().filter(|d| !d.hidden).count();
            let unlocked_hidden = world
                .get::<mud_world::CharacterAchievements>(player)
                .map_or(0, |a| {
                    a.unlocked
                        .keys()
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
        stealth: crate::hiding::is_hidden(world, player),
        flying: world.get::<Flying>(player).is_some(),
        mount_name: mount_name_owned.as_deref(),
        house: world
            .get::<HouseSummary>(player)
            .map(|h| (h.rooms.len(), h.entrance_room.zone, h.entrance_room.id)),
        cooldowns_active: world.get::<mud_world::Cooldowns>(player).map_or(0, |c| {
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
        rest: world.get::<mud_world::RestState>(player).and_then(|r| {
            // Suppress entirely when nothing's worth saying — no
            // pending source AND no banked Repose. Avoids a noisy
            // "Rest: none / 0 XP" line for players who have neither
            // rented nor camped nor accumulated any pool.
            if matches!(r.source, mud_db::enums::RestSource::None) && r.repose == 0 {
                None
            } else {
                Some(crate::commands::RestStateDisplay {
                    source: r.source,
                    tier: r.tier,
                    repose: r.repose,
                })
            }
        }),
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
            format!(
                "UI style: {} (try: fancy / standard / minimal)\r\n",
                cur.label()
            ),
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
    send_to(
        world,
        player,
        format!("UI style set to {}.\r\n", new.label()),
    );
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

pub(crate) fn cmd_meditate(world: &mut World, player: Entity, _args: &str) {
    use mud_world::Meditating;
    if world.get::<Fighting>(player).is_some() {
        send_to(
            world,
            player,
            "You can't focus enough to meditate while fighting.\r\n",
        );
        return;
    }
    let posture = world.get::<Posture>(player).map(|p| p.0);
    let allows = matches!(
        posture,
        Some(PostureKind::Resting | PostureKind::Sitting | PostureKind::Kneeling)
    );
    if !allows {
        send_to(world, player, "Try resting or sitting first.\r\n");
        return;
    }
    let player_name = name_of(world, player);
    if world.get::<Meditating>(player).is_some() {
        try_remove::<Meditating>(world, player);
        try_remove::<crate::commands::Concentrating>(world, player);
        send_to(world, player, "You stop meditating.\r\n");
        if let Some(located) = world.get::<Located>(player).copied() {
            broadcast_room_except_players_rendered(
                world,
                located.0,
                &[player],
                &format!("{player_name} ceases their meditative trance.\r\n"),
            );
        }
        return;
    }
    try_insert(world, player, Meditating);
    send_rendered(
        world,
        player,
        "<b:cyan>You begin to meditate, slowing your breath.</>\r\n",
    );
    if let Some(located) = world.get::<Located>(player).copied() {
        broadcast_room_except_players_rendered(
            world,
            located.0,
            &[player],
            &format!("{player_name} closes their eyes and slips into meditation.\r\n"),
        );
    }
}

pub(crate) fn cmd_rest(world: &mut World, player: Entity, _args: &str) {
    set_posture(world, player, PostureKind::Resting);
}

/// Legacy `do_alert`, the opposite of `rest`: drop the resting stance
/// without getting up. Rust folds legacy position and stance into one
/// [`PostureKind`], so a resting character ends up sitting at attention
/// (legacy `POS_SITTING` + `STANCE_ALERT`); every other awake posture is
/// already alert.
pub(crate) fn cmd_alert(world: &mut World, player: Entity, _args: &str) {
    if world.get::<mud_world::Ghost>(player).is_some() {
        send_to(world, player, "Totally impossible.\r\n");
        return;
    }
    if world.get::<Stunned>(player).is_some() {
        send_to(
            world,
            player,
            "That is utterly beyond your current abilities.\r\n",
        );
        return;
    }
    let posture = world
        .get::<Posture>(player)
        .map_or(PostureKind::Standing, |p| p.0);
    if posture == PostureKind::Sleeping {
        send_to(
            world,
            player,
            "Let's try this in stages.  How about waking up first?\r\n",
        );
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(
            world,
            player,
            "Being as you're in a battle and all, you're pretty alert already!\r\n",
        );
        return;
    }
    if posture != PostureKind::Resting {
        send_to(
            world,
            player,
            "You are already about as tense as you can get.\r\n",
        );
        return;
    }
    if has_effect_named(world, player, "paralyzed") {
        send_to(
            world,
            player,
            "In your paralyzed state, you find this impossible.\r\n",
        );
        return;
    }
    try_insert(world, player, Posture(PostureKind::Sitting));
    send_to(
        world,
        player,
        "You sit up straight and start to pay attention.\r\n",
    );
    if let Some(located) = world.get::<Located>(player).copied() {
        let name = name_of(world, player);
        broadcast_room_except_players_rendered(
            world,
            located.0,
            &[player],
            &format!("{name} sits at attention.\r\n"),
        );
    }
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
        send_rendered(
            world,
            player,
            &format!(
                "{} is already awake.\r\n",
                crate::commands::cap_sentence_start(&target_name)
            ),
        );
        return;
    }
    if crate::effects::has_sleep_effect(world, target) {
        send_rendered(
            world,
            player,
            &format!("You can't wake {target_name} up!\r\n"),
        );
        return;
    }
    try_insert(world, target, Posture(PostureKind::Standing));
    let player_name = name_of(world, player);
    send_rendered(world, player, &format!("You wake {target_name}.\r\n"));
    send_rendered(world, target, &format!("{player_name} wakes you up.\r\n"));
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
    let mut out = format!("\r\nRole: {}\r\n", account.role.label());
    if account.perms.is_empty() {
        out.push_str("Permissions: none\r\n");
    } else {
        out.push_str("Permissions:\r\n");
        for p in &account.perms {
            out.push_str(&format!("  {}\r\n", p.label()));
        }
    }
    send_to(world, player, out);
}

/// Legacy `do_quit` (`act.other.cpp`): shapechanged / switched characters
/// can't quit; mortals can't quit mid-fight (staff can); otherwise say
/// goodbye and flag the session as quitting. The connection layer notices
/// the [`Quitting`] marker right after this command returns and runs the
/// canonical `on_disconnect` save + despawn path, then closes the socket
/// (`ConnRouter::finish_quit`). Everything the character carries is saved,
/// so unlike legacy nothing is dropped on the floor.
///
/// Only the whole word works: `q`, `qu`, `qui` are answered by the
/// dispatcher's safety reply (legacy hidden `qui` entry).
pub(crate) fn cmd_quit(world: &mut World, player: Entity, args: &str) {
    let first = args.split_whitespace().next().unwrap_or("");
    if !first.is_empty() && !first.eq_ignore_ascii_case("yes") {
        // Shapechange / typed-origin refusals take precedence over the
        // junk-argument reply.
        if !quit_refused(world, player) {
            send_to(world, player, "Just type 'quit' to leave the world.\r\n");
        }
        return;
    }
    begin_quit(world, player, "Goodbye, friend.  Come back soon!\r\n");
}

/// True (after telling the player why) when `player` cannot quit because
/// they are shapechanged or the line was not typed by them.
fn quit_refused(world: &mut World, player: Entity) -> bool {
    if world.get::<Player>(player).is_none() {
        send_to(world, player, "You can't quit while shapechanged!\r\n");
        return true;
    }
    // Only the connection that typed `quit` drains the `Quitting` marker, so a
    // forced or scripted quit would strand the player half-quit. Players
    // leave on their own; staff use the disconnect tools.
    if !command_is_typed() {
        send_to(
            world,
            player,
            "You can't quit on someone else's say-so.\r\n",
        );
        return true;
    }
    false
}

/// The one way a player leaves the world on their own: checks the
/// shapechange / typed-origin / mid-fight refusals, sends `farewell`, and
/// flags the session [`Quitting`] so the connection layer runs the canonical
/// save + despawn + close path. Shared by `quit` and `rent` so the two cannot
/// diverge. Returns whether the quit began.
pub(crate) fn begin_quit(world: &mut World, player: Entity, farewell: &str) -> bool {
    if quit_refused(world, player) {
        return false;
    }
    let is_staff = world
        .get::<mud_world::Account>(player)
        .is_some_and(|a| a.role.rank() > mud_db::enums::UserRole::Player.rank());
    if !is_staff && crate::commands::in_combat(world, player) {
        send_to(world, player, "No way!  You're fighting for your life!\r\n");
        return false;
    }
    send_rendered(world, player, farewell);
    crate::commands::input_queue::clear(world, player);
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(crate::commands::Quitting);
    }
    true
}

/// Built-in prompt templates a player can pick by short name, from the
/// `display.prompt_templates` `GameConfig` row: a JSON array of
/// `[name, template]` pairs. Order is the order shown by `prompt list`.
/// A missing or malformed row yields no templates (the `prompt <format>`
/// path still works).
fn prompt_templates(world: &World) -> Vec<(String, String)> {
    let raw = world
        .get_resource::<mud_world::RuntimeConfig>()
        .map_or("", |c| c.get_string("display", "prompt_templates", ""));
    if raw.is_empty() {
        return Vec::new();
    }
    serde_json::from_str(raw).unwrap_or_else(|e| {
        tracing::error!(error = %e, "GameConfig display.prompt_templates is not a JSON [[name, template], ...] array");
        Vec::new()
    })
}

pub(crate) fn cmd_prompt(world: &mut World, player: Entity, args: &str) {
    let template = mud_net::sanitize_text(args, false);
    let template = template.trim();
    let templates = prompt_templates(world);

    // `prompt list` — show the named-template menu so a player
    // doesn't have to read the format spec to find a starting
    // point. `prompt <name>` adopts a template by name.
    if template.eq_ignore_ascii_case("list") || template.eq_ignore_ascii_case("templates") {
        let mut out = String::from("\r\n<b:cyan>Built-in prompt templates:</>\r\n");
        let widest = templates.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
        for (name, body) in &templates {
            out.push_str(&format!("  <cyan>{name:<widest$}</>  {body}\r\n"));
        }
        out.push_str(
            "  <dim>Pick one with 'prompt <name>' or roll your own with the format below.</>\r\n",
        );
        send_rendered(world, player, &out);
        return;
    }
    if let Some((_, body)) = templates
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(template))
    {
        try_insert(world, player, Prompt(body.clone()));
        send_rendered(
            world,
            player,
            &format!("Prompt set to <cyan>{template}</>: {body}\r\n"),
        );
        return;
    }

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
                 Built-in templates: try 'prompt list' for a menu.\r\n\
                 \r\n\
                 Format codes (same as the legacy game):\r\n\
                 Vitals:    %h/%H hit points (current/max)   %v/%V stamina\r\n\
                 \x20          %ph %pH %pv %pV  percent of hit points / stamina\r\n\
                 \x20          %B %M  10-cell HP / stamina bar\r\n\
                 Identity:  %n name   %N real name   %k class   %# level\r\n\
                 \x20          %a %A alignment   %i %I hiddenness   %r rage\r\n\
                 Place:     %z %Z zone   %R room\r\n\
                 Progress:  %e experience bar   %E experience message\r\n\
                 Wealth:    %w coins held   %W coins banked\r\n\
                 \x20          %cp %cg %cs %cc held platinum/gold/silver/copper\r\n\
                 \x20          %cP %cG %cS %cC the same, banked\r\n\
                 Combat:    %o opponent + condition   %O opponent name\r\n\
                 \x20          %t opponent's target + condition   %T target name\r\n\
                 \x20          %g group leader + condition   %G leader name\r\n\
                 \x20          %K 10-cell opponent HP bar\r\n\
                 \x20          %dX cooldown bar (X = a-y, 1-7)   %l %L active effects\r\n\
                 Flags:     %x puts the wizi/AFK flags inline (else a line above)\r\n\
                 Calendar:  %s season   %y hour (00-23)   %Y day/night\r\n\
                 Layout:    %_ newline   %- space\r\n\
                 Literal:   %% emits a single '%'.\r\n"
            ),
        );
        return;
    }
    try_insert(world, player, Prompt(template.to_string()));
    send_to(world, player, format!("Prompt set to: {template}\r\n"));
}

/// Every player toggle in the order `toggle` lists them, with the
/// legacy-style display name (no underscores: `ShowDiceRolls`, not
/// `SHOW_DICE_ROLLS`). Legacy `do_toggle` (`prefs.cpp`) showed this table
/// for a bare `toggle`; the list order follows its `fields[]`.
const TOGGLES: &[(PlayerFlag, &str)] = &[
    (PlayerFlag::NoSummon, "NoSummon"),
    (PlayerFlag::Brief, "Brief"),
    (PlayerFlag::Compact, "Compact"),
    (PlayerFlag::NoTell, "NoTell"),
    (PlayerFlag::Afk, "AFK"),
    (PlayerFlag::Deaf, "Deaf"),
    (PlayerFlag::Quest, "Quest"),
    (PlayerFlag::NoRepeat, "NoRepeat"),
    (PlayerFlag::HolyLight, "HolyLight"),
    (PlayerFlag::AutoExit, "AutoExit"),
    (PlayerFlag::AutoSplit, "AutoSplit"),
    (PlayerFlag::AutoGold, "AutoGold"),
    (PlayerFlag::AutoLoot, "AutoLoot"),
    (PlayerFlag::ExpandObjs, "ExpandObjs"),
    (PlayerFlag::ExpandMobs, "ExpandMobs"),
    (PlayerFlag::AutoAssist, "AutoAssist"),
    (PlayerFlag::Wimpy, "Wimpy"),
    (PlayerFlag::ShowDiceRolls, "ShowDiceRolls"),
    (PlayerFlag::PkEnabled, "PK"),
    (PlayerFlag::Consent, "Consent"),
    (PlayerFlag::ColorBlind, "ColorBlind"),
    (PlayerFlag::Msp, "MSP"),
    (PlayerFlag::MxpEnabled, "MXP"),
    (PlayerFlag::ShowIds, "ShowIds"),
    (PlayerFlag::Muted, "Muted"),
];

/// Lowercase with the `_` / `-` separators removed, so `show_dice_rolls`,
/// `Show-Dice-Rolls` and `ShowDiceRolls` all compare equal.
fn toggle_key(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Whether `player` may see and flip `flag` (god-only flags need Builder+).
fn toggle_allowed(world: &World, player: Entity, flag: PlayerFlag) -> bool {
    !flag.is_god_only()
        || world
            .get::<mud_world::Account>(player)
            .is_some_and(|a| a.role.at_least(UserRole::Builder))
}

/// Resolve what the player typed to a flag the way legacy did: exact name
/// first, then the first toggle (in list order) the text abbreviates. The
/// canonical `SCREAMING_SNAKE` labels and their short aliases still work.
fn resolve_toggle(world: &World, player: Entity, raw: &str) -> Option<PlayerFlag> {
    let key = toggle_key(raw);
    if key.is_empty() {
        return None;
    }
    let visible = || {
        TOGGLES
            .iter()
            .filter(|(f, _)| toggle_allowed(world, player, *f))
    };
    visible()
        .find(|(_, n)| toggle_key(n) == key)
        .or_else(|| visible().find(|(_, n)| toggle_key(n).starts_with(&key)))
        .map(|(f, _)| *f)
        .or_else(|| PlayerFlag::from_label(raw))
}

/// The bare-`toggle` table: every toggle the player can use with its
/// ON/OFF state, three to a row.
fn render_toggle_list(world: &World, player: Entity) -> String {
    let flags = world.get::<PlayerFlags>(player);
    let mut out = String::from(
        "\r\n             FieryMUD TOGGLES!  (See HELP TOGGLE)\r\n\
         ===============================================================\r\n",
    );
    let mut column = 0;
    for (flag, name) in TOGGLES {
        if !toggle_allowed(world, player, *flag) {
            continue;
        }
        let on = flags.is_some_and(|pf| pf.has(*flag));
        let state = if on {
            "<b:white>  ON</>"
        } else {
            "<dim> OFF</>"
        };
        out.push_str(&format!(" {name:>13} {state}"));
        column += 1;
        if column == 3 {
            column = 0;
            out.push_str("\r\n");
        } else {
            out.push_str(" |");
        }
    }
    if column != 0 {
        out.push_str("\r\n");
    }
    out.push_str("===============================================================\r\n");
    out
}

pub(crate) fn cmd_toggle(world: &mut World, player: Entity, args: &str) {
    let raw = args.trim();
    if raw.is_empty() {
        let list = render_toggle_list(world, player);
        send_to(world, player, list);
        return;
    }
    // `toggle wrap` / `toggle columns <n>`: the wrap width is a numeric
    // setting, not a flag; hand it to `columns`.
    let (head, rest) = raw.split_once(char::is_whitespace).unwrap_or((raw, ""));
    if head.eq_ignore_ascii_case("wrap") || head.eq_ignore_ascii_case("columns") {
        cmd_columns(world, player, rest);
        return;
    }
    let Some(flag) = resolve_toggle(world, player, raw) else {
        send_to(
            world,
            player,
            "Toggle what!?  Type 'toggle' for the list.\r\n",
        );
        return;
    };
    // PK has its own rules (not mid-fight, only at the recall point).
    if flag == PlayerFlag::PkEnabled {
        cmd_pk(world, player, "");
        return;
    }
    // God-only flags (HOLY_LIGHT, SHOW_IDS) are gated on the
    // dedicated cmd_holylight / cmd_showids commands; the generic
    // toggle path must not become a bypass.
    if !toggle_allowed(world, player, flag) {
        send_to(
            world,
            player,
            format!("'{raw}' is not a flag you can toggle.\r\n"),
        );
        return;
    }
    let now_on = world
        .get_mut::<PlayerFlags>(player)
        .map(|mut pf| pf.toggle(flag));
    let Some(now_on) = now_on else {
        send_to(world, player, "You have no player flags slot.\r\n");
        return;
    };
    if flag == PlayerFlag::ColorBlind {
        // The colour switch also gates the connection's output encoder.
        crate::terminal::sync_output(world, player);
    }
    let label = TOGGLES
        .iter()
        .find(|(f, _)| *f == flag)
        .map_or_else(|| flag.label(), |(_, n)| *n);
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
    let trimmed = mud_net::sanitize_text(args, false);
    let trimmed = trimmed.trim();
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

    if name.len() > MAX_ALIAS_NAME_LEN {
        send_to(
            world,
            player,
            format!(
                "Alias names are limited to {MAX_ALIAS_NAME_LEN} characters.
"
            ),
        );
        return;
    }
    if expansion.chars().count() > MAX_ALIAS_DEFINITION_LEN {
        send_to(
            world,
            player,
            format!(
                "Alias definitions are limited to {MAX_ALIAS_DEFINITION_LEN} characters.
"
            ),
        );
        return;
    }
    let is_new = world
        .get::<mud_world::Aliases>(player)
        .is_none_or(|a| a.get(&name).is_none());
    if is_new
        && world
            .get::<mud_world::Aliases>(player)
            .is_some_and(|a| a.entries.len() >= MAX_ALIASES_PER_CHARACTER)
    {
        send_to(
            world,
            player,
            format!(
                "You can't have more than {MAX_ALIASES_PER_CHARACTER} aliases; unalias one first.
"
            ),
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

/// `color [off|16|256|truecolor|auto]`: show or set the colour depth.
/// "Off" is the `COLOR_BLIND` flag (so `toggle color` is the same
/// switch); the depth overrides live in `pref.color`.
pub(crate) fn cmd_color(world: &mut World, player: Entity, args: &str) {
    use crate::terminal as term;

    let arg = args.trim().to_ascii_lowercase();
    let (off, pref, msg): (bool, Option<&str>, &str) = match arg.as_str() {
        "" => {
            let depth = term::effective_color(world, player);
            let source = if term::color_override(world, player).is_some() {
                "set by you"
            } else {
                "from your client"
            };
            send_to(
                world,
                player,
                format!(
                    "Color: {} ({source}).\r\n\
                     Use 'color off', 'color 16', 'color 256', 'color truecolor' \
                     or 'color auto'.\r\n",
                    term::depth_label(depth)
                ),
            );
            return;
        }
        "off" | "none" | "no" | "0" => (true, None, "Color is now OFF."),
        "on" | "auto" | "client" | "default" => {
            (false, None, "Color now follows what your client supports.")
        }
        "16" | "ansi" => (false, Some("16"), "Color is pinned to 16 colors."),
        "256" => (false, Some("256"), "Color is pinned to 256 colors."),
        "truecolor" | "24bit" | "true" => {
            (false, Some("truecolor"), "Color is pinned to truecolor.")
        }
        _ => {
            send_to(
                world,
                player,
                "Usage: color [off|16|256|truecolor|auto]\r\n",
            );
            return;
        }
    };
    term::set_color_off(world, player, off);
    term::set_pref(world, player, mud_world::PREF_COLOR_KEY, pref);
    term::sync_output(world, player);
    send_to(world, player, format!("{msg}\r\n"));
}

/// `charset [ascii|utf8|auto]`: show or set ASCII vs UTF-8 output.
pub(crate) fn cmd_charset(world: &mut World, player: Entity, args: &str) {
    use crate::terminal as term;

    let arg = args.trim().to_ascii_lowercase();
    let (pref, msg): (Option<&str>, &str) = match arg.as_str() {
        "" => {
            let cs = term::effective_charset(world, player);
            let source = if term::pref_charset(world, player).is_some() {
                "set by you"
            } else {
                "from your client"
            };
            send_to(
                world,
                player,
                format!(
                    "Charset: {} ({source}).\r\n\
                     Use 'charset ascii', 'charset utf8' or 'charset auto'.\r\n",
                    term::charset_label(cs)
                ),
            );
            return;
        }
        "ascii" | "plain" | "us-ascii" => (Some("ascii"), "Output is now plain ASCII."),
        "utf8" | "utf-8" | "unicode" => (Some("utf8"), "Output now uses UTF-8 symbols."),
        "auto" | "client" | "default" => (None, "Charset now follows what your client supports."),
        _ => {
            send_to(world, player, "Usage: charset [ascii|utf8|auto]\r\n");
            return;
        }
    };
    term::set_pref(world, player, mud_world::PREF_CHARSET_KEY, pref);
    term::sync_output(world, player);
    send_to(world, player, format!("{msg}\r\n"));
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
            "Wimpy mode is off. Use 'wimpy <pct>' (1-99) to enable.\r\n".to_string()
        };
        send_to(world, player, msg);
        return;
    }

    if arg.eq_ignore_ascii_case("off") || arg == "0" {
        if currently_on && let Some(mut pf) = world.get_mut::<PlayerFlags>(player) {
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
                "Wimpy percent must be between 1 and 99 (or 'off' to disable).\r\n",
            );
            return;
        }
        Err(_) => {
            send_to(
                world,
                player,
                "Usage: 'wimpy <pct>' (1-99) or 'wimpy off'.\r\n",
            );
            return;
        }
    };

    if !currently_on && let Some(mut pf) = world.get_mut::<PlayerFlags>(player) {
        pf.toggle(PlayerFlag::Wimpy);
    }
    try_insert(world, player, mud_world::WimpyThreshold(pct));
    send_to(
        world,
        player,
        format!("You'll panic and try to flee when your HP drops below {pct}%.\r\n"),
    );
}

pub(crate) fn cmd_autoexit(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::AutoExit,
        "Exits will be shown automatically with each 'look'.",
        "Exits will no longer auto-list — use 'exits' to see them.",
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
        "Room descriptions will now be terse on 'look'.",
        "Full room descriptions restored.",
    );
}

/// `columns [<n>|auto]` (alias `wrap`): show or set the width prose is
/// word-wrapped to. Persisted in `ScriptVars` under
/// [`mud_world::PREF_COLUMNS_KEY`], so it saves with the character.
pub(crate) fn cmd_columns(world: &mut World, player: Entity, args: &str) {
    use crate::layout::{MAX_COLS, MIN_COLS, pref_columns, wrap_width};

    let arg = args.trim().to_ascii_lowercase();
    if arg.is_empty() {
        let width = wrap_width(world, player);
        let source = if let Some(pref) = pref_columns(world, player) {
            match crate::layout::client_columns(world, player) {
                Some(client) if pref > client => "capped to your client's window",
                _ => "set by you",
            }
        } else if crate::layout::client_columns(world, player).is_some() {
            "reported by your client"
        } else {
            "default; your client has not reported a width"
        };
        send_to(
            world,
            player,
            format!(
                "Text wraps at {width} columns ({source}).\r\n\
                 Use 'columns <{MIN_COLS}-{MAX_COLS}>' to pin a width or 'columns auto' \
                 to follow your client.\r\n"
            ),
        );
        return;
    }
    if matches!(arg.as_str(), "auto" | "off" | "default" | "client") {
        if let Some(mut vars) = world.get_mut::<mud_world::ScriptVars>(player) {
            vars.0.remove(mud_world::PREF_COLUMNS_KEY);
        }
        let width = wrap_width(world, player);
        send_to(
            world,
            player,
            format!("Text now follows your client width ({width} columns).\r\n"),
        );
        return;
    }
    let Some(n) = arg
        .parse::<usize>()
        .ok()
        .filter(|n| (MIN_COLS..=MAX_COLS).contains(n))
    else {
        send_to(
            world,
            player,
            format!("Usage: columns <{MIN_COLS}-{MAX_COLS}|auto>\r\n"),
        );
        return;
    };
    if world.get::<mud_world::ScriptVars>(player).is_none() {
        try_insert(world, player, mud_world::ScriptVars::default());
    }
    if let Some(mut vars) = world.get_mut::<mud_world::ScriptVars>(player) {
        vars.0
            .insert(mud_world::PREF_COLUMNS_KEY.to_string(), n.to_string());
    }
    send_to(world, player, format!("Text will wrap at {n} columns.\r\n"));
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
        "Your own speech will no longer be echoed back.",
        "Your own speech will be echoed back.",
    );
}

pub(crate) fn cmd_nosummon(world: &mut World, player: Entity, _args: &str) {
    // NoSummon's runtime semantic (L1-v2): incoming summon casts
    // silently auto-decline instead of prompting you. Flipping it
    // off lets you receive the standard `ACCEPT / DECLINE`
    // prompt, so you can opt in per-cast.
    toggle_player_flag(
        world,
        player,
        PlayerFlag::NoSummon,
        "Incoming summons will now silently auto-decline (no interruption).",
        "Incoming summons will now prompt you ('accept' or 'decline' within 30s).",
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

/// `pk [on|off]`: opt in or out of player killing. Two players can fight
/// only while both have `PkEnabled` (see `attack_ok`), so switching it off
/// mid-duel is refused: it would be an escape hatch.
pub(crate) fn cmd_pk(world: &mut World, player: Entity, args: &str) {
    let currently_on = world
        .get::<PlayerFlags>(player)
        .is_some_and(|f| f.has(PlayerFlag::PkEnabled));
    let want_on = match args.trim().to_ascii_lowercase().as_str() {
        "" => !currently_on,
        "on" | "yes" | "enable" => true,
        "off" | "no" | "disable" => false,
        _ => {
            send_to(world, player, "Usage: pk [on|off]\r\n");
            return;
        }
    };
    if want_on == currently_on {
        send_to(
            world,
            player,
            format!(
                "Player killing is already {}.\r\n",
                if currently_on { "ON" } else { "OFF" }
            ),
        );
        return;
    }
    if !want_on {
        if fighting_another_player(world, player) {
            send_to(world, player, "Not while you're fighting!\r\n");
            return;
        }
        // Opting out is only possible at home; staff are exempt.
        let staff = world.get::<Profile>(player).is_some_and(|p| p.level >= 100)
            || crate::room_access::is_immortal(world, player);
        let here = world.get::<Located>(player).map(|l| l.0);
        let home = crate::commands::recall::recall_room(world, player);
        if !staff
            && let Some(home_room) = home
            && here != home
        {
            let home_name = world.get::<Named>(home_room).map(|n| {
                crate::commands::render_color_tags(&n.name, crate::commands::ColorMode::Strip)
            });
            let suffix = home_name.map_or_else(String::new, |n| format!(" ({n})"));
            send_to(
                world,
                player,
                format!("You can only turn off player killing at your recall point{suffix}.\r\n"),
            );
            return;
        }
    }
    toggle_player_flag(
        world,
        player,
        PlayerFlag::PkEnabled,
        "Player killing is now ON. Players who also have PK on may attack you, and you them. \
         You can only turn it off again at your recall point.",
        "Player killing is now OFF.",
    );
}

/// Is `player` (or one of their pets) in a fight with another player or
/// another player's pet, in either direction?
fn fighting_another_player(world: &mut World, player: Entity) -> bool {
    let mut mine = vec![player];
    {
        let mut q = world.query_filtered::<Entity, With<Mob>>();
        let mobs: Vec<Entity> = q.iter(world).collect();
        for m in mobs {
            if super::attack_ok::pet_owner(world, m) == Some(player) {
                mine.push(m);
            }
        }
    }
    let theirs = |world: &World, e: Entity| {
        e != player
            && !mine.contains(&e)
            && (world.get::<Player>(e).is_some() || super::attack_ok::pet_owner(world, e).is_some())
    };
    let mut q = world.query::<(Entity, &Fighting)>();
    let pairs: Vec<(Entity, Entity)> = q.iter(world).map(|(e, f)| (e, f.0)).collect();
    pairs.into_iter().any(|(x, y)| {
        (mine.contains(&x) && theirs(world, y)) || (mine.contains(&y) && theirs(world, x))
    })
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
    if refuse_if_blind(world, player) {
        return;
    }
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
            // Rooms in a god zone read as "(beyond)" to mortals.
            let target_name = ed
                .to
                .filter(|e| crate::room_access::room_visible_to(world, player, *e))
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
    // Snapshot any walled directions ahead of the row loop. Each
    // wall is annotated in-line so the exits listing reflects the
    // same barriers `cmd_move` will refuse / drag on / dispel.
    // Empty when no wall is in effect — the conditional read keeps
    // the no-wall case to a single resource fetch.
    let walls: std::collections::HashMap<mud_db::enums::Direction, mud_world::RoomBlockedExit> =
        world
            .get::<mud_world::RoomBlockedExits>(located.0)
            .map(|b| b.by_direction.clone())
            .unwrap_or_default();
    for (dir, room, state) in &rows {
        let open = exit_state_color(*state);
        // Pad in XML-Lite space (visible_width sees `<tag>`),
        // THEN render. Same shape as cmd_spells's grid (2bb9a1a
        // / fix in this commit's sibling site): rendering first
        // would leave ANSI escapes that visible_width counts as
        // visible chars, undercounting the padding.
        let dir_xml = format!("{open}{}</>", direction_name(*dir));
        let dir_label = render_color_tags(&pad_visible(&dir_xml, 10), mode);
        // Colorize plain target room names; authored ones keep
        // their builder-set color via colorize_default.
        let room_label = render_color_tags(&colorize_default(room, "<b:white>"), mode);
        let state_label = match state {
            ExitState::Open => String::new(),
            ExitState::Closed => render_color_tags("  <yellow>(closed)</>", mode),
            ExitState::Locked => render_color_tags("  <red>(locked)</>", mode),
        };
        // Wall trailer (after door state so a door+wall direction
        // reads "north - X (closed) (wall of stone)"). Color cue
        // mirrors traversal severity: red for block, yellow for
        // slow, cyan for illusion.
        let wall_label = walls.get(dir).map_or(String::new(), |w| {
            let color = match w.traversal {
                mud_world::WallTraversal::Block => "<red>",
                mud_world::WallTraversal::Slow => "<yellow>",
                mud_world::WallTraversal::Passable => "<cyan>",
            };
            render_color_tags(&format!("  {color}({})</>", w.kind_label), mode)
        });
        out.push_str(&format!(
            "  {dir_label} - {room_label}{state_label}{wall_label}\r\n"
        ));
    }
    send_to(world, player, out);
}

/// `unlock <direction>`: find a key item in inventory whose name or
/// keyword matches the exit's `key` and flip Locked → Closed (still
/// needs `open` afterward). Two-sided sync.
pub(crate) fn cmd_unlock(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let Some(dir) = resolve_exit_arg(world, player, arg) else {
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
        send_to(
            world,
            player,
            format!("No exit {}.\r\n", direction_name(dir)),
        );
        return;
    };
    if state != ExitState::Locked {
        send_to(
            world,
            player,
            format!("It's not locked {}.\r\n", direction_name(dir)),
        );
        return;
    }
    // Legacy rooms.cpp unlock_door: only characters below LVL_IMMORT need
    // the key (or even a keyhole); staff unlock anything by hand.
    if !crate::room_access::can_unlock_without_key(world, player) {
        let Some(key_req) = key_req else {
            send_to(
                world,
                player,
                format!("There's no keyhole {}.\r\n", direction_name(dir)),
            );
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
            // Don't reveal which key fits — some doors are puzzles and
            // naming the key short-circuits the hunt. Players see only
            // that the door is locked, picking up the exit's keyword so
            // a curtain reads as a curtain.
            let noun = world
                .get::<Exits>(room)
                .and_then(|e| e.0.get(&dir).map(exit_noun_phrase))
                .unwrap_or_else(|| "The way".to_string());
            send_to(world, player, format!("{noun} is locked.\r\n"));
            return;
        }
    }
    flip_door_both_sides(world, room, dir, ExitState::Closed);
    send_to(
        world,
        player,
        format!("You unlock the way {}.\r\n", direction_name(dir)),
    );
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!(
            "{player_name} unlocks the door {}.\r\n",
            direction_name(dir)
        ),
    );
}

/// `open <direction>`: flip a closed exit to Open. Refused on
/// locked exits and on exits that don't exist.
pub(crate) fn cmd_open(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let Some(dir) = resolve_exit_arg(world, player, arg) else {
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
        send_to(
            world,
            player,
            format!("No exit {}.\r\n", direction_name(dir)),
        );
        return;
    };
    match state {
        ExitState::Open => {
            send_to(
                world,
                player,
                format!("It's already open {}.\r\n", direction_name(dir)),
            );
            return;
        }
        ExitState::Locked => {
            send_to(
                world,
                player,
                format!("It's locked {}.\r\n", direction_name(dir)),
            );
            return;
        }
        ExitState::Closed => {}
    }
    flip_door_both_sides(world, room, dir, ExitState::Open);
    send_to(
        world,
        player,
        format!("You open the way {}.\r\n", direction_name(dir)),
    );
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
    let Some(dir) = resolve_exit_arg(world, player, arg) else {
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
        send_to(
            world,
            player,
            format!("No exit {}.\r\n", direction_name(dir)),
        );
        return;
    };
    match state {
        ExitState::Closed | ExitState::Locked => {
            send_to(
                world,
                player,
                format!("It's already closed {}.\r\n", direction_name(dir)),
            );
            return;
        }
        ExitState::Open => {}
    }
    flip_door_both_sides(world, room, dir, ExitState::Closed);
    send_to(
        world,
        player,
        format!("You close the way {}.\r\n", direction_name(dir)),
    );
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
    let Some(dir) = resolve_exit_arg(world, player, arg) else {
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
        send_to(
            world,
            player,
            format!("No exit {}.\r\n", direction_name(dir)),
        );
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
        send_to(
            world,
            player,
            format!("You need '{hint}' to lock that.\r\n"),
        );
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
        &format!("{player_name} locks the door {}.\r\n", direction_name(dir)),
    );
}

/// `read <item>`: find an item by keyword on the player or in their
/// room and print its Description. Refuses on mobs/players (use
/// `examine`). The Description component is the same one
/// `ObjectPrototypes.examine_description` feeds at load time, so books
/// / signs / scrolls all surface their text via this path.
pub(crate) fn cmd_read(world: &mut World, player: Entity, args: &str) {
    if refuse_if_blind(world, player) {
        return;
    }
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Read what?\r\n");
        return;
    }
    if world.get::<Located>(player).is_none() {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    }
    let lc = needle.to_ascii_lowercase();
    // Match items only — mobs/players go to `examine`. Search the
    // player's inventory + the current room.
    let target = find_item(world, player, &lc, ItemClass::RoomFirst);
    let Some(target) = target else {
        send_to(
            world,
            player,
            format!("You can't find anything to read called '{needle}'.\r\n"),
        );
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

    let Some(a) = find_item(world, player, a_word, ItemClass::Carried) else {
        send_to(world, player, format!("You don't have '{a_word}'.\r\n"));
        return;
    };
    // Resolve B. With an explicit second arg, look it up the same way
    // as A. With no second arg, find the item the player currently
    // wears in A's wearable slot — saves a manual `equipment` lookup
    // for the "is this new sword better?" question.
    let target_b: Entity = if let Some(word) = b_word {
        let Some(found) = find_item(world, player, word, ItemClass::Carried) else {
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
            let mut q = world.query_filtered::<(Entity, &Located, &EquippedSlot), With<Item>>();
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
    let a_proto = world.get::<WorldKey>(a).and_then(|k| {
        world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(k.zone, k.id))
            .cloned()
    });
    let b_proto = world.get::<WorldKey>(b).and_then(|k| {
        world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(k.zone, k.id))
            .cloned()
    });
    let Some(ap) = a_proto else {
        send_to(
            world,
            player,
            format!("No prototype data for {a_name}.\r\n"),
        );
        return;
    };
    let Some(bp) = b_proto else {
        send_to(
            world,
            player,
            format!("No prototype data for {b_name}.\r\n"),
        );
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
        "  <b:cyan>A:</> {}    <dim>weight: {:.1}   level: {}   ({})</>\r\n",
        render_color_tags(&a_name, mode),
        ap.weight,
        ap.level,
        ap.r#type.label(),
    ));
    out.push_str(&format!(
        "  <b:cyan>B:</> {}    <dim>weight: {:.1}   level: {}   ({})</>\r\n",
        render_color_tags(&b_name, mode),
        bp.weight,
        bp.level,
        bp.r#type.label(),
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
        // Dice + damage-type as a single weapon-attack signature.
        // Mismatched damage types (slash vs crush) are a real
        // tactical decision when one resists physical-vs-elemental
        // hits, so surfacing both numbers and family in one line
        // lets the player pick on more than just average damage.
        let dice_line = |p: &mud_world::ObjectProto| -> String {
            let bonus = match p.weapon_dice_bonus.cmp(&0) {
                std::cmp::Ordering::Equal => String::new(),
                std::cmp::Ordering::Greater => format!("+{}", p.weapon_dice_bonus),
                std::cmp::Ordering::Less => format!("{}", p.weapon_dice_bonus),
            };
            let dice = format!("{}d{}{bonus}", p.weapon_dice_num, p.weapon_dice_size);
            // Append the damage-type family (slash/crush/pierce/...)
            // when present. The outer format string wraps this whole
            // result in <yellow>...</> so we don't add color here —
            // the parenthetical inherits the yellow.
            match p.weapon_damage_type.as_deref() {
                Some(t) if !t.is_empty() => format!("{dice} ({t})"),
                _ => dice,
            }
        };
        let damage_delta = a_avg - b_avg;
        // Damage delta picks up `damage_color_tag` so a meaningful
        // difference reads with the same warmth used on combat
        // swings. Equal damage dims as "no signal".
        let damage_line = match damage_delta.cmp(&0) {
            std::cmp::Ordering::Equal => "<dim>Same average damage.</>".to_string(),
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
        // Type-mismatch hint. When A and B carry different damage
        // families (slash vs crush, pierce vs bludgeon, ...) and
        // both are populated, surface the delta as its own line so
        // a player picking between two roughly-equal weapons can
        // spot the tactical fork without parsing the dice line.
        match (
            ap.weapon_damage_type.as_deref(),
            bp.weapon_damage_type.as_deref(),
        ) {
            (Some(a), Some(b)) if a != b && !a.is_empty() && !b.is_empty() => {
                out.push_str(&format!(
                    "  <dim>Different damage types — A: <yellow>{a}</>, \
                     B: <yellow>{b}</></>\r\n"
                ));
            }
            _ => {}
        }
    }
    send_to(world, player, out);
}

pub(crate) fn cmd_motd(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        system_text_with_fallback(world, player, "motd", MOTD_TEXT),
    );
}

pub(crate) fn cmd_news(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        system_text_with_fallback(world, player, "news", NEWS_TEXT),
    );
}

pub(crate) fn cmd_credits(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        system_text_with_fallback(world, player, "credits", CREDITS_TEXT),
    );
}

pub(crate) fn cmd_policies(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        system_text_with_fallback(world, player, "policies", POLICIES_TEXT),
    );
}

/// Resolve a `SystemText` row by key, gated by the viewer's level,
/// falling back to a hardcoded constant when the row is missing or
/// the viewer is under-leveled. Returns an owned `String` because
/// `send_to` consumes its input — the resource borrow can't outlive
/// the call. Keeping the fallback in code (not on disk) means a
/// fresh DB still produces a working `motd` / `news` / `credits` /
/// `policies` for muscle-memory players, and the only filesystem
/// dependency is the database connection string itself.
fn system_text_with_fallback(world: &World, player: Entity, key: &str, fallback: &str) -> String {
    let viewer_level = world.get::<Profile>(player).map_or(0, |p| p.level);
    world
        .get_resource::<SystemTexts>()
        .and_then(|t| t.content(key, viewer_level))
        .unwrap_or(fallback)
        .to_string()
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
                recent. Use 'b:' prefix for bright (e.g. <b:cyan>like \
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
    let idle_text = idle_open.map_or(idle_str.clone(), |open| format!("{open}{idle_str}</>"));
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
    out.push_str(&format!("  <cyan>Character:</> <b:cyan>{char_name}</>\r\n"));
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

/// `account` (alias `whoami`): unified identity readout. Renders the
/// in-memory `AccountSummary` snapshot (email + role + character
/// roster) plus a live read of `discord_links` and `google_links` for
/// the player's `Users.id`. Discord links surface `verified` /
/// `unverified` so the player can confirm the bot side has confirmed
/// the binding; missing rows render as `not linked`.
///
/// Async because the federated-identity lookups hit the DB. The
/// AccountSummary half stays in memory (a snapshot taken at login),
/// matching pre-Wave-6 behavior.
pub(crate) async fn cmd_account(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    let Some(summary) = world.get::<AccountSummary>(player).cloned() else {
        send_to(world, player, "No account info available.\r\n");
        return;
    };
    let active_name = name_of(world, player);
    let account = world.get::<Account>(player).cloned();
    let role = account.as_ref().map_or(UserRole::Player, |a| a.role);

    // Federated-identity lookups. Either may legitimately be absent
    // (player hasn't linked) or fail (transient DB hiccup); both
    // collapse to `not linked` in the render so the command never
    // hard-errors on a side-table read.
    let linked = account.as_ref().is_some_and(|a| !a.user_id.is_empty());
    let (discord, google) = if let Some(acct) = account.as_ref().filter(|_| linked) {
        let discord = mud_db::discord_links::for_user(pool, &acct.user_id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "discord_links lookup failed");
                None
            });
        let google = mud_db::google_links::for_user(pool, &acct.user_id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "google_links lookup failed");
                None
            });
        (discord, google)
    } else {
        (None, None)
    };

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
        if linked {
            summary.email.as_str()
        } else {
            "not linked"
        }
    ));
    out.push_str(&format!(
        "  <cyan>Display name:</> <dim>{}</>\r\n",
        if linked {
            summary.display_name.clone()
        } else {
            active_name.clone()
        }
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

    // Linked accounts block — pulled from the DB above. Verified
    // state surfaces on Discord so the player can tell whether the
    // bot has confirmed the binding; Google links don't carry a
    // verification flag (OAuth handshake is implicitly the proof).
    if !linked {
        out.push_str(
            "\r\n<dim>This character is not linked to a website account. \
             Link it on the website to use the account chest/bank, mail \
             and Discord linking.</>\r\n",
        );
        send_to(world, player, out);
        return;
    }
    out.push_str("\r\n<b:cyan>Linked accounts</>\r\n");
    out.push_str(&format!(
        "  <cyan>Discord:</>      {}\r\n",
        match &discord {
            Some(d) if d.verified => format!("<dim>{} (verified)</>", d.discord_name),
            Some(d) => format!("<dim>{} (unverified)</>", d.discord_name),
            None => "<dim>not linked</>".to_string(),
        }
    ));
    out.push_str(&format!(
        "  <cyan>Google:</>       {}\r\n",
        match &google {
            Some(g) => {
                let label = g.google_name.as_deref().unwrap_or(g.google_email.as_str());
                format!("<dim>signed in as {} &lt;{}&gt;</>", label, g.google_email)
            }
            None => "<dim>not linked</>".to_string(),
        }
    ));
    send_to(world, player, out);
}

pub(crate) fn cmd_commands(world: &mut World, player: Entity, args: &str) {
    let (role, perms) = world
        .get::<Account>(player)
        .map_or((UserRole::Player, Vec::new()), |a| {
            (a.role, a.perms.clone())
        });
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
        format!("\r\n<b:cyan>{} commands available:</>\r\n", names.len())
    };
    let mut out = header;
    for chunk in names.chunks(COMMANDS_LIST_COLS) {
        out.push_str("  ");
        for name in chunk {
            out.push_str(&format!("<cyan>{name:<COMMANDS_LIST_COL_WIDTH$}</>"));
        }
        out.push_str("\r\n");
    }
    out.push_str("\r\n<dim>Use 'help <command>' for details.</>\r\n");
    send_to(world, player, out);
}

pub(crate) fn cmd_world(world: &mut World, player: Entity, _args: &str) {
    let zones = world
        .query_filtered::<Entity, With<mud_world::Zone>>()
        .iter(world)
        .filter(|z| {
            world.get::<mud_world::GodZone>(*z).is_none()
                || crate::room_access::is_immortal(world, player)
        })
        .count();
    let rooms = world
        .query_filtered::<Entity, With<mud_world::Room>>()
        .iter(world)
        .count();
    let mobs = world
        .query_filtered::<Entity, With<Mob>>()
        .iter(world)
        .count();
    let items = world
        .query_filtered::<Entity, With<Item>>()
        .iter(world)
        .count();
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
    let mud_minute = i64::from(clock.minute);
    let mud_day = i64::from(clock.day);
    let mud_year = i64::from(clock.year);
    let month_name = mud_world::month_name(world, clock.month);
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
        "               It is <b:yellow>{mud_hour:02}:{mud_minute:02}</> <dim>({period})</>; the season is <yellow>{season}</>.\r\n",
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
    // Indoor / underground rooms muffle the weather report — players
    // shouldn't get "gulls wheel and complain" while standing in a
    // sealed forest temple. Show a single muted line instead of
    // the full climate + live-state + season triplet.
    let sector = world
        .get::<mud_world::RoomSector>(room)
        .map_or(mud_db::enums::Sector::Field, |s| s.0);
    if !crate::commands::sector_is_outdoor_for_weather(sector) {
        send_to(
            world,
            player,
            "\r\n<dim>You can't tell what the weather is doing from in here.</>\r\n",
        );
        return;
    }
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
        (Some(Climate::Arid), "day" | "midday") => {
            "Heat shimmers off the parched ground; the air is dry as bone."
        }
        (Some(Climate::Arid), _) => {
            "The desert chill cuts through cloaks; stars wheel sharply overhead."
        }
        (Some(Climate::Semiarid), "day" | "midday") => {
            "Dust dances on a warm wind; the sun bears down without mercy."
        }
        (Some(Climate::Semiarid), _) => "The scrub cools rapidly; far-off coyotes call.",
        (Some(Climate::Tropical), "day" | "midday") => {
            "Humid air clings to your skin; a green-tinted sun hangs heavy."
        }
        (Some(Climate::Tropical), _) => "Frogs and night-birds compete in the dripping dark.",
        (Some(Climate::Subtropical), "day" | "midday") => {
            "Warm gusts carry distant rain; cumulus clouds tower in lazy stacks."
        }
        (Some(Climate::Subtropical), _) => {
            "Crickets thicken the air; warm mist drifts through the night."
        }
        (Some(Climate::Temperate), "dawn") => {
            "A cool breeze rustles the leaves; dew glistens on every blade."
        }
        (Some(Climate::Temperate), "day" | "midday") => {
            "Mild sun and a clean breeze; pleasant traveling weather."
        }
        (Some(Climate::Temperate), _) => "Stars glitter through clear, cool air.",
        (Some(Climate::Oceanic), "day" | "midday") => {
            "Salt spray rides a steady wind; gulls wheel and complain."
        }
        (Some(Climate::Oceanic), _) => "Distant surf and sea-mist mute the night.",
        (Some(Climate::Subarctic), "day" | "midday") => {
            "Pale sun glints off stubborn frost; your breath fogs the air."
        }
        (Some(Climate::Subarctic), _) => {
            "Bitter cold seeps through every seam; aurora flickers overhead."
        }
        (Some(Climate::Arctic), "day" | "midday") => {
            "Wind-driven snow blurs the horizon; the sun is a pale disc."
        }
        (Some(Climate::Arctic), _) => "The cold is absolute; ice creaks in the dark.",
        (Some(Climate::Alpine), "day" | "midday") => {
            "Thin, sharp air bites at your lungs; the sun is fierce off the snow."
        }
        (Some(Climate::Alpine), _) => {
            "The wind howls down the slopes; stars are knife-bright at altitude."
        }
        (Some(Climate::None) | None, _) => "The air is still and unremarkable.",
    };
    // Season-flavored closer. Climate-agnostic so adding a new
    // climate doesn't require updating four more lines — the climate
    // arm above does the heavy lifting; this just lands the calendar
    // on the readout. Hidden for Climate::None / unmapped rooms
    // (caves, planes) where seasons don't apply.
    let season_line: Option<&str> =
        match (climate, world.resource::<mud_world::MudClock>().season()) {
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
    // Item entities collected alongside names so we can emit a
    // matching `Char.Items.List` GMCP frame after rendering the
    // text — the entity ids stable within the session let the
    // client correlate Add/Remove diffs.
    let item_entities: Vec<Entity> = {
        let mut q =
            world.query_filtered::<(Entity, &Located, &Named, Option<&EquippedSlot>), With<Item>>();
        q.iter(world)
            .filter(|(_, l, _, eq)| l.0 == player && eq.is_none())
            .filter(|(_, _, n, _)| {
                filter.is_empty()
                    || mud_world::targeting::names_match(
                        &filter,
                        std::iter::once(render_color_tags(&n.name, ColorMode::Strip).as_str()),
                    )
            })
            .map(|(e, _, _, _)| e)
            .filter(|e| crate::commands::senses::item_visible_to(world, player, *e))
            .collect()
    };
    let item_entities: Vec<Entity> = {
        let mut v = item_entities;
        crate::commands::sort_newest_first(world, player, &mut v, |e| *e);
        v
    };
    let items: Vec<String> = item_entities
        .iter()
        .map(|&e| {
            let name = world
                .get::<Named>(e)
                .map(|n| n.name.clone())
                .unwrap_or_default();
            crate::commands::senses::with_item_tags(world, player, e, name)
        })
        .collect();
    let stacked = stack_entries(
        items.iter().cloned(),
        has_flag(world, player, PlayerFlag::ExpandObjs),
    );
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
    for (name, n) in &stacked {
        if *n > 1 {
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
    // Mudlet items panel: push the matching list frame so the GUI
    // populates without waiting for a Char.Items.Inv request. Sent
    // after the text so a client race renders the prose first.
    send_char_items_list(world, player, "inv", &item_entities);
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
    crate::corpses::note_coin_take(world, player, container, amount);
    Some(amount)
}

/// If `item` is a loose pile of coins (an `Item` carrying a
/// `CoinPile`, e.g. what a decayed corpse leaves on the floor), add
/// it to `player`'s `Wealth`, remove the pile, and return the amount.
pub(crate) fn take_loose_coins(world: &mut World, player: Entity, item: Entity) -> Option<i64> {
    world.get::<Item>(item)?;
    let amount = world.get::<CoinPile>(item)?.0;
    if amount > 0 {
        if let Some(mut w) = world.get_mut::<Wealth>(player) {
            w.0 = w.0.saturating_add(amount);
        } else {
            try_insert(world, player, Wealth(amount));
        }
    }
    crate::equip_apply::despawn_item(world, item);
    (amount > 0).then_some(amount)
}

/// True for the words that name a container's coin pile.
fn is_coin_word(word: &str) -> bool {
    ["coin", "coins", "gold", "money"]
        .iter()
        .any(|w| word.trim().eq_ignore_ascii_case(w))
}

/// Drain `container`'s `CoinPile` into `player`'s wealth and tell
/// them. Returns the amount taken, or `None` when there was no pile.
fn collect_coin_pile(
    world: &mut World,
    container: Entity,
    player: Entity,
    container_name: &str,
) -> Option<i64> {
    let amount = drain_coin_pile(world, container, player)?;
    let msg = format_amount(amount);
    send_rendered(
        world,
        player,
        &format!("You collect {msg} from {container_name}.\r\n"),
    );
    Some(amount)
}

/// Resolve the prepositionless `get <needle> <container>` form: when
/// nothing on the floor matches the whole string and the LAST word
/// names a container (carried or in the room), returns
/// `(needle, container)`.
fn resolve_implicit_container<'a>(
    world: &mut World,
    player: Entity,
    trimmed: &'a str,
) -> Option<(&'a str, Entity)> {
    let (needle, last_word) = trimmed.rsplit_once(char::is_whitespace)?;
    if needle.trim().is_empty() || find_item(world, player, trimmed, ItemClass::Room).is_some() {
        return None;
    }
    let container = find_item(world, player, last_word, ItemClass::RoomFirst)?;
    is_container_entity(world, container).then_some((needle.trim(), container))
}

/// `get [N] <item> [from] [container]`. A bare leading count takes up
/// to N matching items (`get 2 sword`); `N.item` is the index syntax
/// and is handled by the lookup helpers, not here.
pub(crate) fn cmd_get(world: &mut World, player: Entity, args: &str) {
    let (count, rest) = parse_count_prefix(args.trim());
    let Some(n) = count else {
        get_plain(world, player, rest);
        return;
    };
    // `get 2 all` / `get 2 all.x`: count is meaningless, sweep as usual.
    let from_pair = split_preposition(rest, &["from"]);
    let item_part = from_pair.map_or(rest, |(i, _)| i);
    if parse_all_filter(item_part).is_some() {
        get_plain(world, player, rest);
        return;
    }
    let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
        return;
    };
    let target = if let Some((needle, container_word)) = from_pair {
        find_item(world, player, container_word, ItemClass::RoomFirst).map(|c| (needle, Some(c)))
    } else if let Some((needle, c)) = resolve_implicit_container(world, player, rest) {
        Some((needle, Some(c)))
    } else {
        Some((rest, None))
    };
    let Some((needle, container)) = target else {
        // Unresolvable container: the plain path reports it.
        get_plain(world, player, rest);
        return;
    };
    for i in 0..n {
        let before = match container {
            Some(c) => find_in_container(world, needle, c),
            None => find_item(world, player, needle, ItemClass::Room),
        };
        let Some(before) = before else {
            // Nothing (more) to take. Surface the usual message
            // only when not a single item was available.
            if i == 0 {
                get_plain(world, player, rest);
            }
            return;
        };
        match container {
            Some(c) => get_from_container(world, player, room, needle, c),
            None => get_plain(world, player, needle),
        }
        // Item refused (NO_TAKE, too heavy, claimed corpse): stop
        // rather than repeating the same rejection N times.
        let still_there = world
            .get::<Located>(before)
            .is_some_and(|l| l.0 == container.unwrap_or(room));
        if still_there {
            return;
        }
    }
}

/// `drop [N] <item>`: with a bare leading count, drops up to N
/// matching carried items through the single-item path.
pub(crate) fn cmd_drop(world: &mut World, player: Entity, args: &str) {
    let (count, rest) = parse_count_prefix(args.trim());
    let Some(n) = count else {
        drop_plain(world, player, rest);
        return;
    };
    if parse_all_filter(rest).is_some() {
        drop_plain(world, player, rest);
        return;
    }
    repeat_carried(world, player, rest, n, |w, p| drop_plain(w, p, rest));
}

/// `put [N] <item> [in|into|on] <container>`.
pub(crate) fn cmd_put(world: &mut World, player: Entity, args: &str) {
    let (count, rest) = parse_count_prefix(args.trim());
    let Some(n) = count else {
        put_plain(world, player, rest);
        return;
    };
    let (item_word, _) = split_put_args(rest).unwrap_or((rest, ""));
    if parse_all_filter(item_word).is_some() {
        put_plain(world, player, rest);
        return;
    }
    repeat_carried(world, player, item_word, n, |w, p| put_plain(w, p, rest));
}

/// `give [N] <item> [to] <target>`.
pub(crate) fn cmd_give(world: &mut World, player: Entity, args: &str) {
    let (count, rest) = parse_count_prefix(args.trim());
    let Some(n) = count else {
        give_plain(world, player, rest);
        return;
    };
    let (item_word, _) = split_give_args(rest).unwrap_or((rest, ""));
    repeat_carried(world, player, item_word, n, |w, p| give_plain(w, p, rest));
}

/// Split `put` arguments into `(item, container)`.
fn split_put_args(args: &str) -> Option<(&str, &str)> {
    if let Some(pair) = split_preposition(args, &["in", "into", "on"]) {
        return Some(pair);
    }
    let (item, container) = args.split_once(char::is_whitespace)?;
    (!container.trim().is_empty()).then_some((item.trim(), container.trim()))
}

/// Split `give` arguments into `(item, target)`.
fn split_give_args(args: &str) -> Option<(&str, &str)> {
    if let Some(pair) = split_preposition(args, &["to"]) {
        return Some(pair);
    }
    let (item, target) = args.split_once(char::is_whitespace)?;
    (!target.trim().is_empty()).then_some((item.trim(), target.trim()))
}

/// Run `action` up to `n` times against successive matches of
/// `item_word` in the player's inventory. Stops as soon as there is
/// nothing left to act on or the item did not leave the player's
/// hands (a gate refused it), so rejections are not repeated.
fn repeat_carried(
    world: &mut World,
    player: Entity,
    item_word: &str,
    n: usize,
    action: impl Fn(&mut World, Entity),
) {
    for i in 0..n {
        let Some(before) = find_item(world, player, item_word, ItemClass::Inventory) else {
            if i == 0 {
                action(world, player);
            }
            return;
        };
        action(world, player);
        if world.get::<Located>(before).is_some_and(|l| l.0 == player) {
            return;
        }
    }
}

fn get_plain(world: &mut World, player: Entity, args: &str) {
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
    // player is carrying or which sits in the room. `all` /
    // `all.<filter>` as the item word loots everything (matching)
    // inside.
    if let Some((needle, container_word)) = split_preposition(trimmed, &["from"]) {
        let container = find_item(world, player, container_word, ItemClass::RoomFirst);
        let Some(container) = container else {
            send_to(
                world,
                player,
                format!("You don't see '{container_word}' here.\r\n"),
            );
            return;
        };
        get_from_container(world, player, room, needle, container);
        return;
    }

    // Legacy phrasing without the preposition: `get all corpse`,
    // `get sword corpse`. When nothing on the floor matches the
    // whole string and the LAST word resolves to a container, treat
    // it as `get <rest> from <last>`. Anything else falls through
    // to the plain floor path below, unchanged.
    if let Some((needle, container)) = resolve_implicit_container(world, player, trimmed) {
        get_from_container(world, player, room, needle, container);
        return;
    }

    // `get all` — sweep every item off the floor.
    // `get all.<name>` — pick up every floor item whose name or
    // keywords contain <name>. Both forms run the same code path
    // with an optional substring filter; the filter is empty for
    // bare `all`.
    let lower = trimmed.to_ascii_lowercase();
    let all_filter: Option<&str> = if lower == "all" {
        Some("")
    } else if let Some(rest) = lower.strip_prefix("all.")
        && !rest.is_empty()
    {
        Some(rest)
    } else {
        None
    };
    if let Some(filter) = all_filter {
        get_all_from_floor(world, player, room, filter);
        return;
    }

    // Plain `get <item>` from the floor. Something the player cannot see
    // (hidden until `search` turns it up) is not there to take.
    let item = find_item(world, player, trimmed, ItemClass::Room)
        .filter(|i| crate::commands::senses::item_visible_to(world, player, *i));
    let Some(item) = item else {
        send_to(
            world,
            player,
            format!("You don't see '{trimmed}' here.\r\n"),
        );
        return;
    };

    let item_name = name_of(world, item);
    let player_name = name_of(world, player);

    // Mobs aren't allowed to pick up corpses. The user's body stays
    // intact for them to loot on release; otherwise a Lua trigger
    // (or scripted `actor:command("get corpse")`) could whisk a
    // freshly-killed player's body away and the body decays inside
    // the mob's inventory before the player can retrieve it. Players
    // and staff aren't gated.
    if world.get::<Mob>(player).is_some() && world.get::<Corpse>(item).is_some() {
        return;
    }

    // A player's corpse can be dragged but never carried (legacy: it has
    // no TAKE flag). Carried corpses couldn't be saved with their owner's
    // gear, so nobody, staff included, picks one up.
    if world.get::<mud_world::PlayerCorpse>(item).is_some() {
        send_rendered(
            world,
            player,
            &format!("{item_name}: you can't take that!\r\n"),
        );
        return;
    }

    // A loose pile of coins (a decayed corpse's money) goes straight
    // into the purse rather than the inventory.
    if let Some(amount) = take_loose_coins(world, player, item) {
        let coin = crate::commands::format_wealth(amount).unwrap_or_else(|| "no coin".to_string());
        send_rendered(world, player, &format!("You pick up {coin}.\r\n"));
        broadcast_room_except_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} picks up {item_name}.\r\n"),
        );
        return;
    }

    // NO_TAKE — the item is bolted to the room (fixture, prop,
    // scenery in container shape). Skip staff: builders sometimes
    // need to relocate fixtures via the loadobj-and-get path.
    if !crate::commands::is_staff(world, player)
        && has_restriction(world, item, mud_db::enums::ObjectRestriction::NoTake)
    {
        send_rendered(
            world,
            player,
            &format!("{item_name} is fixed in place.\r\n"),
        );
        return;
    }

    if !crate::commands::is_staff(world, player)
        && carried_weight(world, player) + item_weight(world, item) > carry_capacity(world, player)
    {
        send_rendered(
            world,
            player,
            &format!(
                "{item_name} is too heavy — you'd be encumbered. \
                 Drop something with 'drop <item>', or train Strength.\r\n"
            ),
        );
        return;
    }

    if world.get::<Located>(item).is_some() {
        world.entity_mut(item).insert(Located(player));
        crate::hiding::set_hiddenness(world, item, 0);
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

/// Take `needle` out of `container` (a carried or in-room container /
/// corpse). `needle` may be a keyword, `N.keyword`, bare `all`, or
/// `all.<filter>` (substring against name / keywords). Applies the
/// loot-claim and player-corpse consent gates before anything moves.
fn get_from_container(
    world: &mut World,
    player: Entity,
    room: Entity,
    needle: &str,
    container: Entity,
) {
    // A player corpse lives in the database; when a take actually emptied
    // part of it, the looter's own save (item rows re-homed, coins debited
    // in the same transaction) is the only write. Never the corpse first.
    let is_pc = world.get::<mud_world::PlayerCorpse>(container).is_some();
    let before = is_pc.then(|| crate::corpses::contents_fingerprint(world, container));
    get_from_container_inner(world, player, room, needle, container);
    if let Some(before) = before
        && world.get_entity(container).is_ok()
        && crate::corpses::contents_fingerprint(world, container) != before
    {
        // The corpse's rows can't be deleted (decay, retirement, purge)
        // until this player's save has committed the take.
        crate::corpses::note_loot(world, player, container);
        crate::quest_progress::save_player_soon(world, player);
    }
}

fn get_from_container_inner(
    world: &mut World,
    player: Entity,
    room: Entity,
    needle: &str,
    container: Entity,
) {
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

    // Player-corpse consent gate. Mirrors legacy CONSENT: only
    // the dead player themselves (matched by name extracted
    // from "the corpse of X") or staff can loot a player
    // corpse. Stops opportunistic gear theft when a player
    // dies; uncollected items still drop to the room on decay.
    let is_pc = world.get::<mud_world::PlayerCorpse>(container).is_some();
    if is_pc && !crate::commands::is_staff(world, player) {
        let owner_name = container_name
            .strip_prefix("the corpse of ")
            .unwrap_or("")
            .trim();
        if !owner_name.eq_ignore_ascii_case(player_name.as_str()) {
            send_to(
                world,
                player,
                format!(
                    "{container_name} isn't yours to loot — \
                     disturbing another adventurer's remains \
                     requires their consent.\r\n"
                ),
            );
            return;
        }
    }

    // A corpse whose death write hasn't committed holds items the
    // database doesn't know about yet; looting them now could persist the
    // looter's copy while the corpse's rows survive a crash.
    if is_pc
        && (crate::corpses::is_unsettled(world, container)
            || world
                .get::<crate::corpses::DecayDeleting>(container)
                .is_some())
    {
        send_to(
            world,
            player,
            format!("{container_name} is still settling; try again in a moment.\r\n"),
        );
        return;
    }

    // `get all from <container>`: snapshot every item inside,
    // re-Located to the player, broadcast a single line with the
    // count. Also drains a `CoinPile` if the container carries
    // one (e.g. a corpse from a non-AutoGold kill). Empty
    // containers report the obvious "nothing in there" rather
    // than failing the keyword lookup. `all.<filter>` restricts
    // the sweep to items whose name / keywords contain the filter
    // (the coin pile is drained for bare `all` and for coin words
    // such as `all.coin`).
    if let Some(filter) = parse_all_filter(needle) {
        let items: Vec<(Entity, String)> = {
            let mut q =
                world.query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
            q.iter(world)
                .filter(|(_, l, n, kw)| {
                    l.0 == container && (filter.is_empty() || matches(&filter, n, *kw))
                })
                .map(|(e, _, n, _)| (e, n.name.clone()))
                .collect()
        };
        let items = {
            let mut v = items;
            v.retain(|(e, _)| crate::commands::senses::item_visible_to(world, player, *e));
            crate::commands::sort_newest_first(world, container, &mut v, |r| r.0);
            v
        };
        // Drain CoinPile first — independent of items so a
        // corpse that holds *only* coin (low-tier mob with no
        // gear) still completes meaningfully instead of
        // reporting "nothing in there".
        let coin_drained = if filter.is_empty() || is_coin_word(&filter) {
            collect_coin_pile(world, container, player, &container_name)
        } else {
            None
        };
        if items.is_empty() {
            if coin_drained.is_none() {
                if filter.is_empty() {
                    send_rendered(
                        world,
                        player,
                        &format!("There's nothing in {container_name}.\r\n"),
                    );
                } else {
                    send_rendered(
                        world,
                        player,
                        &format!("There's nothing matching '{filter}' in {container_name}.\r\n"),
                    );
                }
            }
            return;
        }
        let cap = carry_capacity(world, player);
        let mut running = carried_weight(world, player);
        let mut moved = 0usize;
        let mut skipped = 0usize;
        // Staff bypass — gods don't get encumbered. The cap +
        // running tally still update so the score sheet stays
        // honest, but the gate doesn't reject.
        let bypass_encumbrance = crate::commands::is_staff(world, player);
        for (item, item_name) in &items {
            // NO_TAKE items inside containers also stay put.
            // Plausible: a fixed lectern inside a shrine, a
            // welded gear inside a clockwork chassis. Staff
            // bypass mirrors the floor sweep.
            if !bypass_encumbrance
                && has_restriction(world, *item, mud_db::enums::ObjectRestriction::NoTake)
            {
                continue;
            }
            let w = net_weight_gain(world, player, *item);
            if !bypass_encumbrance && running + w > cap {
                skipped += 1;
                continue;
            }
            running += w;
            if world.get::<Located>(*item).is_some() {
                world.entity_mut(*item).insert(Located(player));
                crate::hiding::set_hiddenness(world, *item, 0);
            }
            send_rendered(
                world,
                player,
                &format!("You take {item_name} from {container_name}.\r\n"),
            );
            crate::triggers::fire_item_event(world, *item, player, mud_world::TriggerEvent::Get);
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

    // Coin words (`coin`, `coins`, `gold`, `money`) drain the pile;
    // any item that also matches the word is taken afterwards.
    let coin_drained = if is_coin_word(needle) {
        collect_coin_pile(world, container, player, &container_name)
    } else {
        None
    };
    let item = find_in_container(world, needle, container)
        .filter(|i| crate::commands::senses::item_visible_to(world, player, *i));
    let Some(item) = item else {
        if coin_drained.is_none() {
            send_rendered(
                world,
                player,
                &format!("There's no '{needle}' in {container_name}.\r\n"),
            );
        }
        return;
    };
    let item_name = name_of(world, item);
    // NO_TAKE — same fixture gate as the floor path.
    if !crate::commands::is_staff(world, player)
        && has_restriction(world, item, mud_db::enums::ObjectRestriction::NoTake)
    {
        send_rendered(
            world,
            player,
            &format!("{item_name} is fixed in place.\r\n"),
        );
        return;
    }
    if !crate::commands::is_staff(world, player)
        && carried_weight(world, player) + net_weight_gain(world, player, item)
            > carry_capacity(world, player)
    {
        send_rendered(
            world,
            player,
            &format!(
                "{item_name} is too heavy — you'd be encumbered. \
             Drop something with 'drop <item>', or train Strength.\r\n"
            ),
        );
        return;
    }
    if world.get::<Located>(item).is_some() {
        world.entity_mut(item).insert(Located(player));
        crate::hiding::set_hiddenness(world, item, 0);
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
}

/// Sweep every floor item in `room` into the player's inventory,
/// optionally filtered by a substring against the item's name or
/// keywords. `filter == ""` means "every item." Mirrors the
/// `get all from <container>` path: encumbrance is honored unless
/// the picker is staff (`is_staff` bypass), per-item Get triggers
/// fire, and a single broadcast summarizes the count to onlookers.
fn get_all_from_floor(world: &mut World, player: Entity, room: Entity, filter: &str) {
    let needle = filter.to_ascii_lowercase();
    // Mobs running `get all` (typically via a Lua trigger) don't
    // sweep up corpses — same rule as the single-item path. Player
    // floor sweeps still grab corpses if the player wants them.
    let actor_is_mob = world.get::<Mob>(player).is_some();
    let items: Vec<(Entity, String)> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &Named, Option<&Keywords>, Option<&Corpse>),
            With<Item>,
        >();
        q.iter(world)
            .filter(|(_, l, n, kw, corpse)| {
                if l.0 != room {
                    return false;
                }
                if actor_is_mob && corpse.is_some() {
                    return false;
                }
                if needle.is_empty() {
                    return true;
                }
                matches(&needle, n, *kw)
            })
            .map(|(e, _, n, _, _)| (e, n.name.clone()))
            .collect()
    };
    let items = {
        let mut v = items;
        v.retain(|(e, _)| crate::commands::senses::item_visible_to(world, player, *e));
        crate::commands::sort_newest_first(world, room, &mut v, |r| r.0);
        v
    };
    if items.is_empty() {
        if needle.is_empty() {
            send_to(world, player, "There's nothing here to pick up.\r\n");
        } else {
            send_to(
                world,
                player,
                format!("There's nothing matching '{filter}' here.\r\n"),
            );
        }
        return;
    }
    let player_name = name_of(world, player);
    let cap = carry_capacity(world, player);
    let mut running = carried_weight(world, player);
    let mut moved = 0usize;
    let mut skipped = 0usize;
    let bypass_encumbrance = crate::commands::is_staff(world, player);
    for (item, item_name) in &items {
        if let Some(amount) = take_loose_coins(world, player, *item) {
            let coin =
                crate::commands::format_wealth(amount).unwrap_or_else(|| "no coin".to_string());
            send_rendered(world, player, &format!("You pick up {coin}.\r\n"));
            moved += 1;
            continue;
        }
        if world.get::<mud_world::PlayerCorpse>(*item).is_some() {
            send_rendered(
                world,
                player,
                &format!("{item_name}: you can't take that!\r\n"),
            );
            continue;
        }
        // NO_TAKE fixtures skip silently in the bulk path — the
        // singular `get <item>` path surfaces the message. Staff
        // bypass mirrors the singular path: builders sometimes
        // need to relocate fixtures via a `loadobj`-and-`get`
        // chain. (No "blocked" counter line; sweeps are common
        // and a tail-count would spam.)
        if !bypass_encumbrance
            && has_restriction(world, *item, mud_db::enums::ObjectRestriction::NoTake)
        {
            continue;
        }
        let w = item_weight(world, *item);
        if !bypass_encumbrance && running + w > cap {
            skipped += 1;
            continue;
        }
        running += w;
        if world.get::<Located>(*item).is_some() {
            world.entity_mut(*item).insert(Located(player));
            crate::hiding::set_hiddenness(world, *item, 0);
        }
        send_rendered(world, player, &format!("You pick up {item_name}.\r\n"));
        crate::triggers::fire_item_event(world, *item, player, mud_world::TriggerEvent::Get);
        if let Some(key) = world.get::<WorldKey>(*item).copied() {
            bump_collect_quest_progress(world, player, key.zone, key.id);
        }
        moved += 1;
    }
    if moved > 0 {
        let suffix = if moved == 1 { "" } else { "s" };
        broadcast_room_except_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} picks up {moved} item{suffix} from the ground.\r\n"),
        );
    }
    if skipped > 0 {
        send_to(
            world,
            player,
            format!("You're too encumbered to carry {skipped} more item(s).\r\n"),
        );
    }
    if moved > 0 {
        refresh_player_items_gmcp(world, player);
    }
}

/// `put <item> <container>`: move a carried item into a container
/// the player is carrying or which sits in the room.
fn put_plain(world: &mut World, player: Entity, args: &str) {
    // Support `put <item> <container>` and `put <item> in|into|on
    // <container>`. The preposition forms are natural and match how
    // players type it.
    let trimmed = args.trim();
    let (item_word, container_word) =
        if let Some(pair) = split_preposition(trimmed, &["in", "into", "on"]) {
            pair
        } else {
            let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
            if parts.len() != 2 || parts[1].trim().is_empty() {
                send_to(world, player, "Usage: put <item> in <container>\r\n");
                return;
            }
            (parts[0].trim(), parts[1].trim())
        };

    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let player_name = name_of(world, player);

    let container = find_item(world, player, container_word, ItemClass::CarriedFirst);
    let Some(container) = container else {
        send_rendered(
            world,
            player,
            &format!("You don't see '{container_word}' here.\r\n"),
        );
        return;
    };
    let container_name = name_of(world, container);

    // Legacy corpses have zero capacity. A player corpse's contents are
    // exactly what is in the database; anything dropped in would be lost
    // from the giver's save and never reach the corpse's rows.
    if world.get::<mud_world::PlayerCorpse>(container).is_some() {
        send_rendered(
            world,
            player,
            &format!("{container_name} can't hold anything more.\r\n"),
        );
        return;
    }

    // `put all in <container>` — store every carried (non-equipped)
    // item in the target. Skips the container itself.
    // `put all.<filter> in <container>` restricts to items whose
    // name or keywords contain the filter.
    if let Some(filter) = parse_all_filter(item_word) {
        let items: Vec<(Entity, String)> = {
            let mut q = world.query_filtered::<(
                Entity,
                &Located,
                &Named,
                Option<&Keywords>,
                Option<&EquippedSlot>,
            ), With<Item>>();
            q.iter(world)
                .filter(|(e, l, n, kw, eq)| {
                    l.0 == player
                        && eq.is_none()
                        && *e != container
                        && (filter.is_empty() || matches(&filter, n, *kw))
                })
                .map(|(e, _, n, _, _)| (e, n.name.clone()))
                .collect()
        };
        let items = {
            let mut v = items;
            crate::commands::sort_newest_first(world, player, &mut v, |r| r.0);
            v
        };
        if items.is_empty() {
            if filter.is_empty() {
                send_to(
                    world,
                    player,
                    "You aren't carrying anything to put away.\r\n",
                );
            } else {
                send_to(
                    world,
                    player,
                    format!("You aren't carrying anything matching '{filter}'.\r\n"),
                );
            }
            return;
        }
        let count = items.len();
        for (item, item_name) in &items {
            if world.get::<Located>(*item).is_some() {
                world.entity_mut(*item).insert(Located(container));
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

    let item = find_item(world, player, item_word, ItemClass::Inventory);
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
    if world.get::<Located>(item).is_some() {
        world.entity_mut(item).insert(Located(container));
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
    let Some(item) = find_item(world, player, target_word, ItemClass::Inventory) else {
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
    crate::equip_apply::despawn_item(world, item);
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
    let Some(item) = find_item(world, player, target_word, ItemClass::Inventory) else {
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
    if world.get::<Located>(item).is_some() {
        world.entity_mut(item).insert(Located(room));
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

/// True when `item` carries the given proto-derived attribute flag
/// (GLOW / HUM / INVISIBLE / SOULBOUND / …). Returns false when the
/// `ObjectFlags` component is missing — the loader only attaches it
/// when the proto's flag list is non-empty.
pub(crate) fn has_object_flag(
    world: &World,
    item: Entity,
    flag: mud_db::enums::ObjectFlag,
) -> bool {
    world
        .get::<mud_world::ObjectFlags>(item)
        .is_some_and(|f| f.has(flag))
}

/// True when `item` carries the given restriction (NO_DROP / NO_TAKE
/// / NO_SELL / …). Same component-absence semantics as
/// `has_object_flag`.
pub(crate) fn has_restriction(
    world: &World,
    item: Entity,
    restriction: mud_db::enums::ObjectRestriction,
) -> bool {
    world
        .get::<mud_world::ObjectRestrictions>(item)
        .is_some_and(|r| r.has(restriction))
}

/// True when dropping this item should be refused — either it
/// carries NO_DROP (curse / quest lock) or SOULBOUND (bound on
/// equip / pickup). Used by the bulk `drop all` path to skip
/// without spamming one rejection line per item; the singular
/// `drop <item>` path surfaces the specific reason.
#[must_use]
pub(crate) fn item_drop_blocked(world: &World, item: Entity) -> bool {
    has_restriction(world, item, mud_db::enums::ObjectRestriction::NoDrop)
        || has_object_flag(world, item, mud_db::enums::ObjectFlag::Soulbound)
}

fn drop_plain(world: &mut World, player: Entity, args: &str) {
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
    // `drop all.<filter>` — same, restricted to items whose name or
    // keywords contain the filter.
    if let Some(filter) = parse_all_filter(target_word) {
        let items: Vec<(Entity, String)> = {
            let mut q = world.query_filtered::<(
                Entity,
                &Located,
                &Named,
                Option<&Keywords>,
                Option<&EquippedSlot>,
            ), With<Item>>();
            q.iter(world)
                .filter(|(_, l, n, kw, eq)| {
                    l.0 == player && eq.is_none() && (filter.is_empty() || matches(&filter, n, *kw))
                })
                .map(|(e, _, n, _, _)| (e, n.name.clone()))
                .collect()
        };
        let items = {
            let mut v = items;
            crate::commands::sort_newest_first(world, player, &mut v, |r| r.0);
            v
        };
        if items.is_empty() {
            if filter.is_empty() {
                send_to(world, player, "You aren't carrying anything to drop.\r\n");
            } else {
                send_to(
                    world,
                    player,
                    format!("You aren't carrying anything matching '{filter}'.\r\n"),
                );
            }
            return;
        }
        let mut dropped = 0usize;
        let mut blocked = 0usize;
        for (item, item_name) in &items {
            // NO_DROP / SOULBOUND skip silently in the bulk path —
            // the singular path below surfaces the message. Mass-
            // dropping shouldn't spam one rejection line per
            // protected item; a tail-count line summarizes instead.
            if item_drop_blocked(world, *item) {
                blocked += 1;
                continue;
            }
            if world.get::<Located>(*item).is_some() {
                world.entity_mut(*item).insert(Located(room));
            }
            // Identification is broken when the item leaves the
            // player's possession to the ground. Whoever picks it
            // up next will see only its surface description until
            // they re-identify.
            if let Ok(mut e) = world.get_entity_mut(*item) {
                e.remove::<mud_world::Identified>();
            }
            send_rendered(world, player, &format!("You drop {item_name}.\r\n"));
            crate::triggers::fire_item_event(world, *item, player, mud_world::TriggerEvent::Drop);
            dropped += 1;
        }
        if dropped > 0 {
            broadcast_room_except_rendered(
                world,
                room,
                &[player],
                &format!("{player_name} drops {dropped} item(s).\r\n"),
            );
        }
        if blocked > 0 {
            send_to(
                world,
                player,
                format!("{blocked} item(s) refused to leave your grasp.\r\n"),
            );
        }
        refresh_player_items_gmcp(world, player);
        return;
    }

    let item = find_item(world, player, target_word, ItemClass::Inventory);
    let Some(item) = item else {
        send_rendered(
            world,
            player,
            &format!("You aren't carrying '{target_word}'.\r\n"),
        );
        return;
    };

    let item_name = name_of(world, item);

    // NO_DROP / SOULBOUND gates. Two separate messages so the player
    // can tell which kind of stickiness they're dealing with (curse
    // vs. permanent bond) — useful when planning around either.
    if has_restriction(world, item, mud_db::enums::ObjectRestriction::NoDrop) {
        send_rendered(
            world,
            player,
            &format!("You can't seem to let go of {item_name}.\r\n"),
        );
        return;
    }
    if has_object_flag(world, item, mud_db::enums::ObjectFlag::Soulbound) {
        send_rendered(
            world,
            player,
            &format!("{item_name} is soulbound — it stays with you.\r\n"),
        );
        return;
    }

    if world.get::<Located>(item).is_some() {
        world.entity_mut(item).insert(Located(room));
    }
    // Drop breaks identification — see `Identified` doc comment.
    // Cleared after the Located mutation so the bookkeeping
    // stays sequential with the move.
    if let Ok(mut e) = world.get_entity_mut(item) {
        e.remove::<mud_world::Identified>();
    }

    send_rendered(world, player, &format!("You drop {item_name}.\r\n"));
    broadcast_room_except_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} drops {item_name}.\r\n"),
    );
    crate::triggers::fire_item_event(world, item, player, mud_world::TriggerEvent::Drop);
    refresh_player_items_gmcp(world, player);
}

fn give_plain(world: &mut World, player: Entity, args: &str) {
    // `give <item> [to] <target>`. The `to` form is tried first so
    // multi-word items (`give rusty sword to bob`) work; otherwise
    // fall back to the legacy two-token split.
    let args = args.trim();
    let (item_word, target_word) = if let Some(pair) = split_preposition(args, &["to"]) {
        pair
    } else {
        let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
        if parts.len() != 2 || parts[1].trim().is_empty() {
            send_to(world, player, "Usage: give <item> [to] <target>\r\n");
            return;
        }
        (parts[0].trim(), parts[1].trim())
    };

    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;

    let item = find_item(world, player, item_word, ItemClass::Inventory);
    let Some(item) = item else {
        send_rendered(
            world,
            player,
            &format!("You aren't carrying '{item_word}'.\r\n"),
        );
        return;
    };
    // SOULBOUND blocks trade. Bound gear cannot change hands; the
    // bond is on the person, not the room. NO_DROP also implies
    // "won't leave your grasp" so block trade too — consistent
    // with the legacy MUD semantics where an !NODROP gate covered
    // both drop and give.
    let item_name_pre = name_of(world, item);
    if has_object_flag(world, item, mud_db::enums::ObjectFlag::Soulbound) {
        send_rendered(
            world,
            player,
            &format!("{item_name_pre} is soulbound — it cannot change hands.\r\n"),
        );
        return;
    }
    if has_restriction(world, item, mud_db::enums::ObjectRestriction::NoDrop) {
        send_rendered(
            world,
            player,
            &format!("You can't seem to let go of {item_name_pre}.\r\n"),
        );
        return;
    }
    let target = find_actor_in_room(world, target_word, room, player);
    let Some(target) = target else {
        send_rendered(
            world,
            player,
            &format!("You don't see '{target_word}' here.\r\n"),
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
        && carried_weight(world, target) + item_weight(world, item) > carry_capacity(world, target)
    {
        send_rendered(
            world,
            player,
            &format!(
                "{} is too laden to take {item_name}.\r\n",
                crate::commands::cap_sentence_start(&target_name)
            ),
        );
        return;
    }

    if world.get::<Located>(item).is_some() {
        world.entity_mut(item).insert(Located(target));
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
    // A flying recipient may now be too heavy to stay up.
    crate::flight::check_overweight(world, target);

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

/// Try to equip `item` for `wear all`: its highest-priority position
/// first, then its other legal positions (a ring-and-badge item whose
/// finger slots are full still goes on as a badge). Refusals stay silent
/// (legacy `perform_wear` with `collective`). Returns whether it was worn.
fn wear_item_collective(world: &mut World, player: Entity, item: Entity) -> bool {
    let positions = crate::commands::item_wear_positions(world, item);
    positions.iter().rev().any(|&pos| {
        crate::commands::wear_item(
            world,
            player,
            item,
            crate::commands::WearWhere::Position(pos),
            true,
        )
    })
}

/// `wear <item> [<where>]`, `wear all`, `wear all.<name>` (legacy `do_wear`).
/// A trailing body keyword (`wear nexus badge`, `wear bracelet wrist`)
/// names the position to wear the item at; a second ring/ear/wrist/neck
/// item falls through to the other side on its own.
pub(crate) fn cmd_wear(world: &mut World, player: Entity, args: &str) {
    let words: Vec<&str> = args.split_whitespace().collect();
    let Some(&first) = words.first() else {
        send_to(world, player, "Wear what?\r\n");
        return;
    };
    let all_dot = first
        .to_ascii_lowercase()
        .strip_prefix("all.")
        .map(str::to_string);
    let is_all = first.eq_ignore_ascii_case("all");
    if (is_all || all_dot.is_some()) && words.len() > 1 {
        send_to(
            world,
            player,
            "You can't specify the same body location for more than one item!\r\n",
        );
        return;
    }

    if is_all {
        // Every carried item that fits somewhere, in inventory order.
        let mut items = carried_unequipped_items(world, player);
        items.retain(|&e| !crate::commands::item_wear_positions(world, e).is_empty());
        let worn = items
            .into_iter()
            .filter(|&e| wear_item_collective(world, player, e))
            .count();
        if worn == 0 {
            send_to(world, player, "You don't have anything you can wear.\r\n");
        }
    } else if let Some(filter) = all_dot {
        if filter.is_empty() {
            send_to(world, player, "Wear all of what?\r\n");
            return;
        }
        let mut items = carried_unequipped_items(world, player);
        items.retain(|&e| {
            let named = world.get::<Named>(e).cloned();
            let kw = world.get::<Keywords>(e).cloned();
            named.is_some_and(|n| crate::commands::matches(&filter, &n, kw.as_ref()))
        });
        if items.is_empty() {
            let plural = if filter.ends_with('s') { "" } else { "s" };
            send_to(
                world,
                player,
                format!("You don't seem to have any {filter}{plural}.\r\n"),
            );
        } else {
            let worn = items
                .into_iter()
                .filter(|&e| wear_item_collective(world, player, e))
                .count();
            if worn == 0 {
                send_to(
                    world,
                    player,
                    "You don't have anything wearable like that.\r\n",
                );
            }
        }
    } else if words.len() > 1
        && let Some(&last) = words.last()
        && let Some(position) = mud_world::wear_keyword_slot(last)
    {
        // The last word is a body position; everything before it names
        // the item.
        let name = words[..words.len() - 1].join(" ");
        let Some(item) = find_item(world, player, &name, ItemClass::Inventory) else {
            send_to(world, player, format!("You aren't carrying '{name}'.\r\n"));
            return;
        };
        crate::commands::wear_item(
            world,
            player,
            item,
            crate::commands::WearWhere::Position(position),
            false,
        );
    } else {
        wear_into(world, player, &words.join(" "), None);
    }
    refresh_player_items_gmcp(world, player);
}

/// Items the player carries but has not equipped, in inventory listing
/// order (newest first, as legacy's `ch->carrying` list).
fn carried_unequipped_items(world: &mut World, player: Entity) -> Vec<Entity> {
    let mut items: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located, Option<&EquippedSlot>), With<Item>>();
        q.iter(world)
            .filter(|(_, l, eq)| l.0 == player && eq.is_none())
            .map(|(e, _, _)| e)
            .collect()
    };
    crate::commands::sort_newest_first(world, player, &mut items, |e| *e);
    items
}

pub(crate) fn cmd_wield(world: &mut World, player: Entity, args: &str) {
    wear_into(world, player, args.trim(), Some(Slot::Wield));
    refresh_player_items_gmcp(world, player);
}

pub(crate) fn cmd_hold(world: &mut World, player: Entity, args: &str) {
    wear_into(world, player, args.trim(), Some(Slot::Hold));
    refresh_player_items_gmcp(world, player);
}

/// `light <item>`: mark a Light-type carried item as lit. Refused
/// on non-Light items or already-lit ones.
pub(crate) fn cmd_light(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Light what?\r\n");
        return;
    }
    let Some(item) = find_item(world, player, target_word, ItemClass::Carried) else {
        send_to(
            world,
            player,
            format!("You aren't carrying '{target_word}'.\r\n"),
        );
        return;
    };
    let item_name = name_of(world, item);
    let kind = world.get::<WorldKey>(item).and_then(|k| {
        world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(k.zone, k.id))
            .map(|p| p.r#type)
    });
    if kind != Some(mud_db::enums::ObjectType::Light) {
        send_to(
            world,
            player,
            format!("{item_name} isn't a light source.\r\n"),
        );
        return;
    }
    if mud_world::is_lit(world, item) {
        send_to(world, player, format!("{item_name} is already lit.\r\n"));
        return;
    }
    // Burnt out (or no fuel data at all, which is never assumed infinite).
    if world
        .get::<mud_world::LightFuel>(item)
        .is_none_or(|f| f.remaining == 0)
    {
        send_to(
            world,
            player,
            format!("Sorry, there's no more power left in {item_name}.\r\n"),
        );
        return;
    }
    if let Ok(mut e) = world.get_entity_mut(item) {
        e.insert(mud_world::Lit);
    }
    send_rendered(world, player, &format!("You light {item_name}.\r\n"));
    if let Some(located) = world.get::<Located>(player).copied() {
        let actor_name = name_of(world, player);
        broadcast_room_visual(
            world,
            located.0,
            player,
            &[player],
            &cap_sentence_start(&format!("{actor_name} lights {item_name}.\r\n")),
        );
    }
}

/// `extinguish <item>`: clear the Lit marker.
pub(crate) fn cmd_extinguish(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Extinguish what?\r\n");
        return;
    }
    let Some(item) = find_item(world, player, target_word, ItemClass::Carried) else {
        send_to(
            world,
            player,
            format!("You aren't carrying '{target_word}'.\r\n"),
        );
        return;
    };
    let item_name = name_of(world, item);
    // A permanent light is always lit and can't be put out.
    if world
        .get::<mud_world::LightFuel>(item)
        .is_some_and(mud_world::LightFuel::is_permanent)
    {
        send_to(world, player, format!("You can't put out {item_name}.\r\n"));
        return;
    }
    if world.get::<mud_world::Lit>(item).is_none() {
        send_to(world, player, format!("{item_name} isn't lit.\r\n"));
        return;
    }
    if let Ok(mut e) = world.get_entity_mut(item) {
        e.remove::<mud_world::Lit>();
    }
    send_rendered(world, player, &format!("You extinguish {item_name}.\r\n"));
    if let Some(located) = world.get::<Located>(player).copied() {
        let actor_name = name_of(world, player);
        broadcast_room_visual(
            world,
            located.0,
            player,
            &[player],
            &cap_sentence_start(&format!("{actor_name} extinguishes {item_name}.\r\n")),
        );
    }
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
    send_rendered(
        world,
        player,
        &format!("You dismount from {mount_name}.\r\n"),
    );
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
    // Legacy `do_fly`: only something that grants flight lets you rise (gods
    // always can), and a load past the flight limit pins you down.
    if !has_effect_named(world, player, "fly") && effective_level(world, player) < 100 {
        send_to(world, player, "You do not have the means to fly.\r\n");
        return;
    }
    if crate::flight::too_heavy_to_fly(world, player) {
        send_to(
            world,
            player,
            "You try to rise up, but you can't get off the ground!\r\n",
        );
        if let Some(located) = world.get::<Located>(player).copied() {
            let n = crate::commands::cap_sentence_start(&name_of(world, player));
            let his = crate::flight::possessive(world, player);
            broadcast_room_except_players_rendered(
                world,
                located.0,
                &[player],
                &format!("{n} rises up on {his} toes, as if trying to fly.\r\n"),
            );
        }
        return;
    }
    try_insert(world, player, mud_world::Flying);
    let mover_name = name_of(world, player);
    send_to(
        world,
        player,
        "You spread your wings and take to the air.\r\n",
    );
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
    let over_air = world
        .get::<Located>(player)
        .and_then(|l| world.get::<mud_world::RoomSector>(l.0))
        .is_some_and(|s| s.0 == mud_db::enums::Sector::Air);
    if over_air {
        send_to(
            world,
            player,
            "You stop flying with nothing below you but thin air.\r\n",
        );
    } else {
        send_to(world, player, "You touch down and start walking again.\r\n");
    }
    // Legacy `do_stand` -> `falling_check`: landing over thin air is a fall.
    mud_world::movement::begin_fall_if_unsupported(world, player);
    if let Some(located) = world.get::<Located>(player).copied() {
        broadcast_room_except_players_rendered(
            world,
            located.0,
            &[player],
            &format!("{mover_name} lands and starts walking.\r\n"),
        );
    }
}

/// Tick before which the hider may not hide again (legacy `WAIT_STATE`
/// after `do_hide`; only `hide` itself honours it).
#[derive(Component, Debug, Clone, Copy)]
struct HideLag {
    until: u64,
}

/// `hide`: roll a [`Hiddenness`](mud_world::Hiddenness) from the `hide`
/// skill, DEX and INT (legacy `do_hide`, act.other.cpp:1105). Anyone
/// with a lower perception then fails to see you until you act, are
/// found by `search`, or walk it off. The roll's constants come from the
/// `HIDE` ability row.
pub(crate) fn cmd_hide(world: &mut World, player: Entity, _args: &str) {
    hide_with_roll(world, player, &mut |lo, hi| rand::random_range(lo..=hi));
}

/// `cmd_hide` with the `random(lower, upper)` roll injected.
pub(crate) fn hide_with_roll(
    world: &mut World,
    player: Entity,
    roll: &mut dyn FnMut(i32, i32) -> i32,
) {
    use crate::hiding;
    let staff = crate::room_access::is_immortal(world, player);
    // Staff have every skill (legacy gods carry the full set).
    let skill = if staff {
        100
    } else {
        hiding::skill_pct(world, player, "hide")
    };
    if skill == 0 {
        send_to(
            world,
            player,
            "You'd better leave that art to the rogues.\r\n",
        );
        return;
    }
    if world.get::<mud_world::Mounted>(player).is_some() {
        send_to(world, player, "While mounted? I don't think so...\r\n");
        return;
    }
    let now = world.get_resource::<crate::TickCount>().map_or(0, |t| t.0);
    if !staff && world.get::<HideLag>(player).is_some_and(|l| l.until > now) {
        send_to(
            world,
            player,
            "You are still recovering from your last attempt to hide.\r\n",
        );
        return;
    }
    if hiding::is_hidden(world, player) {
        send_to(world, player, "You try to find a better hiding spot.\r\n");
    } else {
        send_to(world, player, "You attempt to hide yourself.\r\n");
    }
    let params = hiding::hide_params(world);
    let stats = world
        .get::<mud_world::CoreStats>(player)
        .copied()
        .unwrap_or_default();
    let level = mud_world::effective_level(world, player);
    let mut bonus = hiding::rogue_skill_bonus(stats.dexterity);
    // A halfling hiding with its group makes the DEX bonus count for more.
    if hiding::is_halfling(world, player) && hiding_group_size(world, player) > 1 {
        bonus = bonus.saturating_mul(level / params.halfling_group_level_divisor + 1);
    }
    let rolled = hiding::roll_hiddenness(
        &params,
        skill,
        stats.dexterity,
        stats.intelligence,
        bonus,
        roll,
    );
    hiding::set_hiddenness(world, player, rolled);
    let wait = if hiding::is_thief(world, player) {
        params.thief_wait_ticks
    } else {
        params.wait_ticks
    };
    try_insert(world, player, HideLag { until: now + wait });
}

/// Members of `player`'s group standing in the same room, `player`
/// included (legacy `group_size(ch, true)`).
fn hiding_group_size(world: &mut World, player: Entity) -> usize {
    let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
        return 1;
    };
    let leader = crate::commands::group_root(world, player);
    crate::commands::group_members(world, leader)
        .into_iter()
        .filter(|m| world.get::<Located>(*m).is_some_and(|l| l.0 == room))
        .count()
        .max(1)
}

/// `visible` / `vis` (legacy `do_visible` -> `appear`): stop hiding and
/// drop every source of invisibility. Spell instances (`INVISIBLE`,
/// `MASS_INVIS`) are removed through [`remove_effect_instance`] so the
/// marker, the evasion bonus and the room's view all unwind; a worn item
/// that grants invisibility cannot be shrugged off, so the player is
/// told to take it off.
pub(crate) fn cmd_visible(world: &mut World, player: Entity, _args: &str) {
    let hidden = crate::hiding::is_hidden(world, player);
    let invisible = world.get::<mud_world::Invisible>(player).is_some();
    if !hidden && !invisible {
        send_to(world, player, "You are already visible.\r\n");
        return;
    }
    let mut item_backed = false;
    if invisible {
        let sources: Vec<(Entity, bool)> = {
            let mut q = world.query_filtered::<(
                Entity,
                &AppliedTo,
                &EffectInstance,
            ), With<mud_world::InvisibleSource>>();
            q.iter(world)
                .filter(|(_, a, _)| a.0 == player)
                .map(|(e, _, inst)| (e, mud_world::mob_effects::is_innate_effect(&inst.source)))
                .collect()
        };
        for (src, innate) in sources {
            if innate {
                // Worn gear, race and mob-default grants outlive the
                // command: only the owner (item / race) can end them.
                item_backed = true;
            } else if world.get::<EffectInstance>(src).is_some() {
                crate::effects::remove_effect_instance(world, player, src);
            }
        }
    }
    let was_hidden = hidden;
    crate::hiding::reveal(world, player);
    if was_hidden {
        send_to(world, player, "You step out of the shadows.\r\n");
        if let Some(room) = world.get::<Located>(player).map(|l| l.0) {
            let name = cap_sentence_start(&name_of(world, player));
            broadcast_room_except_rendered(
                world,
                room,
                &[player],
                &format!("{name} steps out of the shadows.\r\n"),
            );
            refresh_room_players(world, room);
        }
    }
    if world.get::<mud_world::Invisible>(player).is_some() {
        if item_backed {
            send_to(
                world,
                player,
                "You can't drop your invisibility while something you are wearing grants it.\r\n",
            );
        } else {
            // A marker nothing backs any more (stale): clear it.
            invisibility_faded(world, player);
        }
    }
}

/// Despawn `item` and everything nested inside it. `Located` has no
/// linked spawn, so despawning a container alone would strand its
/// contents as orphan entities; collect the tree first. Nothing else
/// is needed for persistence: the save diff only sees items still
/// reachable from the player, so the `CharacterItems` rows of the
/// whole tree are dropped on the next save.
pub(crate) fn despawn_item_tree(world: &mut World, item: Entity) {
    let mut tree = vec![item];
    let mut i = 0;
    while i < tree.len() {
        if let Some(contents) = world.get::<Contents>(tree[i]) {
            let kids: Vec<Entity> = contents
                .iter()
                .filter(|e| world.get::<Item>(*e).is_some())
                .collect();
            tree.extend(kids);
        }
        i += 1;
    }
    for e in tree {
        // A worn item's bonuses and effects go with it.
        crate::equip_apply::release_gear(world, e);
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
}

/// Gods (staff level, see `is_staff_level`) can `eat` any carried item
/// that is not food: it is destroyed, with no nutrition or effects.
/// Returns true when the command was handled here.
fn god_eats_item(world: &mut World, player: Entity, args: &str) -> bool {
    let is_god = world
        .get::<Profile>(player)
        .is_some_and(|p| mud_db::enums::is_staff_level(p.level));
    let target_word = args.trim();
    if !is_god || target_word.is_empty() {
        return false;
    }
    let Some(item) = find_item(world, player, target_word, ItemClass::Inventory) else {
        return false;
    };
    let is_food = world
        .get::<WorldKey>(item)
        .and_then(|k| {
            world
                .resource::<ObjectPrototypes>()
                .by_key
                .get(&(k.zone, k.id))
                .map(|p| p.r#type)
        })
        .is_some_and(|t| t == mud_db::enums::ObjectType::Food);
    if is_food {
        return false;
    }
    let item_name = name_of(world, item);
    send_rendered(world, player, &format!("You eat {item_name}.\r\n"));
    if let Some(located) = world.get::<Located>(player).copied() {
        let actor_name = name_of(world, player);
        broadcast_room_visual(
            world,
            located.0,
            player,
            &[player],
            &cap_sentence_start(&format!("{actor_name} eats {item_name}.\r\n")),
        );
    }
    despawn_item_tree(world, item);
    true
}

pub(crate) fn cmd_eat(world: &mut World, player: Entity, args: &str) {
    if god_eats_item(world, player, args) {
        return;
    }
    if consume_item(world, player, args, mud_db::enums::ObjectType::Food, "eat")
        && let Some(mut h) = world.get_mut::<mud_world::Hunger>(player)
    {
        // v1: any Food fully sates. Legacy CircleMUD's per-food
        // `fill` attribute can refine in a follow-up.
        h.0 = 0;
    }
}

pub(crate) fn cmd_quaff(world: &mut World, player: Entity, args: &str) {
    // Route potions through the same item-cast path scrolls/wands use:
    // the potion's bound abilities fire as full casts (with proper
    // effect names, durations, override params) and the item despawns
    // after. Falls back to the legacy consume_item path only if the
    // potion has zero ObjectAbilities bindings (older content that
    // hasn't been wired to the spell catalog yet).
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    let item_word = parts.first().map(|s| s.trim()).filter(|s| !s.is_empty());
    let Some(item_word) = item_word else {
        send_to(world, player, "Quaff what?\r\n");
        return;
    };
    let item = find_item(world, player, item_word, ItemClass::Inventory);
    let has_bindings = item.is_some_and(|e| {
        world
            .get::<mud_world::WorldKey>(e)
            .map(|k| (k.zone, k.id))
            .and_then(|key| {
                world
                    .resource::<mud_world::ObjectAbilityCatalog>()
                    .by_key
                    .get(&key)
                    .map(|v| !v.is_empty())
            })
            .unwrap_or(false)
    });
    if has_bindings {
        invoke_object_abilities(
            world,
            player,
            args,
            mud_db::enums::ObjectType::Potion,
            "quaff",
            "You quaff",
            true,
        );
    } else {
        consume_item(
            world,
            player,
            args,
            mud_db::enums::ObjectType::Potion,
            "quaff",
        );
    }
}

pub(crate) fn cmd_drink(world: &mut World, player: Entity, args: &str) {
    drink_amount(world, player, &strip_from_preposition(args), 4, "drink");
}

pub(crate) fn cmd_sip(world: &mut World, player: Entity, args: &str) {
    drink_amount(world, player, &strip_from_preposition(args), 1, "sip");
}

/// Strip a leading or interleaved "from" preposition from a command's
/// args so `drink from fountain` and `fill skin from fountain` parse
/// the same way as the bare-token forms. Case-insensitive on the
/// match; preserves case on the rest. Multiple internal "from"
/// instances all get filtered (rare but cheap).
fn strip_from_preposition(args: &str) -> String {
    args.split_whitespace()
        .filter(|w| !w.eq_ignore_ascii_case("from"))
        .collect::<Vec<_>>()
        .join(" ")
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
    let Some(src) = find_item(world, player, src_word, ItemClass::Carried) else {
        send_to(
            world,
            player,
            format!("You aren't carrying '{src_word}'.\r\n"),
        );
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
        // Identified src → real liquid name; otherwise color desc.
        // Matches the drink-path render convention.
        let identified = world.get::<mud_world::Identified>(src).is_some();
        let liquid_label = pour_label(world, &src_state.liquid, identified);
        send_rendered(
            world,
            player,
            &format!("You pour the {liquid_label} from {src_name} onto the ground.\r\n"),
        );
        return;
    };
    let Some(dest) = find_item(world, player, target_word, ItemClass::Carried) else {
        send_to(
            world,
            player,
            format!("You aren't carrying '{target_word}'.\r\n"),
        );
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
    let same_liquid =
        dest_state.remaining == 0 || dest_state.liquid.eq_ignore_ascii_case(&src_state.liquid);
    if !same_liquid {
        send_rendered(
            world,
            player,
            &format!("{dest_name} already holds something else.\r\n"),
        );
        return;
    }
    let amount = dest_room.min(src_state.remaining);
    // Canonicalize the alias before persisting on the destination —
    // catalog alias wins, so a hand-edited DB row's variant like
    // "Water" becomes "water". Falls back to the raw source string
    // when the catalog doesn't know the alias.
    let canon_alias = world
        .resource::<mud_world::LiquidCatalog>()
        .lookup_alias(&src_state.liquid)
        .map_or_else(|| src_state.liquid.clone(), |def| def.alias.clone());
    if let Some(mut s) = world.get_mut::<mud_world::LiquidContainer>(src) {
        s.remaining -= amount;
    }
    if let Some(mut d) = world.get_mut::<mud_world::LiquidContainer>(dest) {
        if d.remaining == 0 {
            d.liquid = canon_alias;
            d.poisoned = src_state.poisoned;
        } else if src_state.poisoned {
            // Poisoning spreads when topping up a non-poisoned with
            // poisoned: any bad liquid contaminates the lot.
            d.poisoned = true;
        }
        d.remaining += amount;
    }
    let identified = world.get::<mud_world::Identified>(src).is_some();
    let liquid_label = pour_label(world, &src_state.liquid, identified);
    send_rendered(
        world,
        player,
        &format!("You pour {amount} units of {liquid_label} from {src_name} into {dest_name}.\r\n"),
    );
}

/// Render the liquid noun for `pour`/`fill` output. Identified
/// containers show the catalog's real name ("dark ale"); otherwise
/// the color description ("brown liquid"). Unknown aliases fall
/// back to the raw string lowercased — keeps legacy / hand-edited
/// values surfacing instead of silently swallowing them.
fn pour_label(world: &World, alias: &str, identified: bool) -> String {
    world
        .resource::<mud_world::LiquidCatalog>()
        .lookup_alias(alias)
        .map_or_else(
            || alias.to_ascii_lowercase(),
            |def| {
                if identified {
                    def.name.to_ascii_lowercase()
                } else {
                    format!("{} liquid", def.color_desc.to_ascii_lowercase())
                }
            },
        )
}

/// `fill <container> [from] [<source>]`: top up the destination
/// from a liquid source — a carried container or a roomside source
/// like a fountain. With no source word, auto-detects a fountain in
/// the current room (so `fill skin` works when standing at one).
/// "from" between args is optional sugar — `fill skin from fountain`
/// reads the same as `fill skin fountain`. Same liquid-match rules
/// as `pour` (mismatched non-empty containers refuse).
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_fill(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args
        .split_whitespace()
        .filter(|w| !w.eq_ignore_ascii_case("from"))
        .collect();
    let Some(&dest_word) = parts.first() else {
        send_to(
            world,
            player,
            "Usage: fill <container> [from] [<source>]   \
             (omit source to fill from a nearby fountain)\r\n",
        );
        return;
    };

    let Some(dest) = find_item(world, player, dest_word, ItemClass::Carried) else {
        send_to(
            world,
            player,
            format!("You aren't carrying '{dest_word}'.\r\n"),
        );
        return;
    };
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
        send_rendered(world, player, &format!("{dest_name} is already full.\r\n"));
        return;
    }

    // Resolve source. If a name is given, search inventory then
    // room. If not, auto-detect a fountain in the current room.
    let player_room = world.get::<Located>(player).map(|l| l.0);
    let src: Option<Entity> = if let Some(&src_word) = parts.get(1) {
        find_item(world, player, src_word, ItemClass::CarriedFirst)
    } else {
        player_room.and_then(|r| find_fountain_in_room(world, r))
    };
    let Some(src) = src else {
        send_to(
            world,
            player,
            "There's no obvious water source here. Fill from what?\r\n",
        );
        return;
    };
    if src == dest {
        send_to(world, player, "You can't fill something from itself.\r\n");
        return;
    }
    let src_name = name_of(world, src);
    let Some(src_state) = world.get::<mud_world::LiquidContainer>(src).cloned() else {
        send_rendered(
            world,
            player,
            &format!("{src_name} isn't a drink container.\r\n"),
        );
        return;
    };
    // Fountains read as bottomless — same rule the drink path
    // uses. Decrement-on-fill is skipped for them.
    let src_is_fountain = world.get::<WorldKey>(src).is_some_and(|k| {
        world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(k.zone, k.id))
            .is_some_and(|p| p.r#type == mud_db::enums::ObjectType::Fountain)
    });
    if !src_is_fountain && src_state.remaining <= 0 {
        send_rendered(world, player, &format!("{src_name} is empty.\r\n"));
        return;
    }
    // Liquid-type match required when dest already holds something.
    if dest_state.remaining > 0 && !dest_state.liquid.eq_ignore_ascii_case(&src_state.liquid) {
        send_rendered(
            world,
            player,
            &format!("{dest_name} already holds something else.\r\n"),
        );
        return;
    }
    let amount = if src_is_fountain {
        dest_room
    } else {
        dest_room.min(src_state.remaining)
    };
    // Canonicalize alias before persisting on the destination —
    // see the matching block in `cmd_pour`.
    let canon_alias = world
        .resource::<mud_world::LiquidCatalog>()
        .lookup_alias(&src_state.liquid)
        .map_or_else(|| src_state.liquid.clone(), |def| def.alias.clone());
    if !src_is_fountain && let Some(mut s) = world.get_mut::<mud_world::LiquidContainer>(src) {
        s.remaining -= amount;
    }
    if let Some(mut d) = world.get_mut::<mud_world::LiquidContainer>(dest) {
        if d.remaining == 0 {
            d.liquid = canon_alias;
            d.poisoned = src_state.poisoned;
        } else if src_state.poisoned {
            // Poisoning spreads when topping up a non-poisoned with
            // poisoned: any bad liquid contaminates the lot.
            d.poisoned = true;
        }
        d.remaining += amount;
    }
    // Fountains aren't "identified" the way carried containers are,
    // but their proto name is always public knowledge — show the
    // real liquid name. Carried sources get the standard
    // identified-or-color treatment.
    let src_identified = src_is_fountain || world.get::<mud_world::Identified>(src).is_some();
    let liquid_label = pour_label(world, &src_state.liquid, src_identified);
    send_rendered(
        world,
        player,
        &format!("You fill {dest_name} with {liquid_label} from {src_name}.\r\n"),
    );
}

/// Find the first Fountain-type item in `room`. Used by `fill` to
/// auto-detect a water source when the player doesn't name one.
fn find_fountain_in_room(world: &mut World, room: Entity) -> Option<Entity> {
    // Snapshot candidates first so the proto lookup doesn't conflict
    // with the query borrow.
    let candidates: Vec<(Entity, WorldKey)> = {
        let mut q = world.query_filtered::<(Entity, &Located, &WorldKey), With<Item>>();
        q.iter(world)
            .filter(|(_, l, _)| l.0 == room)
            .map(|(e, _, k)| (e, *k))
            .collect()
    };
    let protos = world.resource::<ObjectPrototypes>();
    candidates
        .into_iter()
        .find(|(_, k)| {
            protos
                .by_key
                .get(&(k.zone, k.id))
                .is_some_and(|p| p.r#type == mud_db::enums::ObjectType::Fountain)
        })
        .map(|(e, _)| e)
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
    let Some(item) = find_item(world, player, target_word, ItemClass::Carried) else {
        send_to(
            world,
            player,
            format!("You aren't carrying '{target_word}'.\r\n"),
        );
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
    if state.remaining <= 0 {
        send_rendered(
            world,
            player,
            &format!("{item_name} is empty — nothing to taste.\r\n"),
        );
        return;
    }
    // Taste always identifies. Show the real liquid name from the
    // catalog regardless of `Identified` — that's the point of
    // tasting. Unknown aliases fall back to the lowercased alias.
    let liquid_label = world
        .resource::<mud_world::LiquidCatalog>()
        .lookup_alias(&state.liquid)
        .map_or_else(
            || state.liquid.to_ascii_lowercase(),
            |def| def.name.to_ascii_lowercase(),
        );
    send_rendered(
        world,
        player,
        &format!("It tastes like {liquid_label}.\r\n"),
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

/// Legacy `do_use` (`use` and `play`): the first word names an item
/// being held, the rest is the target handed to the item's magic. Legacy
/// also took a second hand (`WEAR_HOLD2`); Rust has the one `Hold` slot.
fn use_held_item(world: &mut World, player: Entity, args: &str, verb: &str) {
    use mud_db::enums::ObjectType;
    let args = args.trim();
    let (item_word, target) = match args.split_once(char::is_whitespace) {
        Some((w, rest)) => (w, Some(rest.trim()).filter(|t| !t.is_empty())),
        None => (args, None),
    };
    if item_word.is_empty() {
        send_to(world, player, format!("What do you want to {verb}?\r\n"));
        return;
    }
    let needle = item_word.to_ascii_lowercase();
    let held = {
        let mut q = world.query_filtered::<(
            Entity,
            &Located,
            &Named,
            Option<&Keywords>,
            &EquippedSlot,
        ), With<Item>>();
        q.iter(world)
            .find(|(_, l, n, kw, slot)| {
                l.0 == player && slot.0 == Slot::Hold && matches(&needle, n, *kw)
            })
            .map(|(e, ..)| e)
    };
    let Some(item) = held else {
        let article = if "aeiou".contains(needle.chars().next().unwrap_or('x')) {
            "an"
        } else {
            "a"
        };
        send_to(
            world,
            player,
            format!("You don't seem to be holding {article} {item_word}.\r\n"),
        );
        return;
    };
    let proto = world.get::<WorldKey>(item).and_then(|k| {
        world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(k.zone, k.id))
            .map(|p| (p.r#type, p.level))
    });
    let (kind, item_level) = proto.unzip();
    if let (Some(lvl), Some(prof)) = (item_level, world.get::<Profile>(player))
        && lvl > prof.level
    {
        send_to(
            world,
            player,
            "That item is too powerful for you to use.\r\n",
        );
        return;
    }
    let wanted = if verb == "play" {
        ObjectType::Instrument
    } else {
        // Wand or staff; anything else is refused below.
        match kind {
            Some(ObjectType::Staff) => ObjectType::Staff,
            _ => ObjectType::Wand,
        }
    };
    if kind != Some(wanted) {
        let msg = if verb == "play" {
            "You can't seem to figure out how to make sound with it.\r\n"
        } else {
            "You can't seem to figure out how to use it.\r\n"
        };
        send_to(world, player, msg);
        return;
    }
    let item_name = name_of(world, item);
    let actor = name_of(world, player);
    let flavor = match wanted {
        ObjectType::Staff => UseFlavor {
            you: format!("You tap {item_name} three times on the ground."),
            room: format!("{actor} taps {item_name} three times on the ground."),
            victim: None,
        },
        ObjectType::Instrument => UseFlavor {
            you: format!("You play {item_name}."),
            room: format!("{actor} plays {item_name}."),
            victim: None,
        },
        _ => wand_flavor(world, player, &item_name, &actor, target),
    };
    // The legacy target check and charge rules are the wave / tap ones;
    // instruments behave like staves (no target).
    let (cast_verb, intro) = match wanted {
        ObjectType::Staff => ("tap", "You tap"),
        ObjectType::Instrument => ("play", "You play"),
        _ => ("wave", "You wave"),
    };
    invoke_item_abilities(
        world,
        player,
        item,
        target,
        wanted,
        cast_verb,
        intro,
        false,
        Some(flavor),
    );
}

/// Legacy `mag_objectmagic` wand wording: point at a creature or thing,
/// at yourself, or wave it in the air when the spell needs no target.
fn wand_flavor(
    world: &mut World,
    player: Entity,
    item_name: &str,
    actor: &str,
    target: Option<&str>,
) -> UseFlavor {
    let Some(at) = target else {
        return UseFlavor {
            you: format!("You wave {item_name} in the air."),
            room: format!("{actor} waves {item_name} in the air."),
            victim: None,
        };
    };
    if matches_self(actor, at) {
        return UseFlavor {
            you: format!("You point {item_name} at yourself."),
            room: format!("{actor} points {item_name} at themself."),
            victim: None,
        };
    }
    let room = world.get::<Located>(player).map(|l| l.0);
    if let Some(room) = room
        && let Some(victim) = find_actor_in_room(world, at, room, player)
    {
        let vname = name_of(world, victim);
        return UseFlavor {
            you: format!("You point {item_name} at {vname}."),
            room: format!("{actor} points {item_name} at {vname}."),
            victim: Some((victim, format!("{actor} points {item_name} at you."))),
        };
    }
    let thing = room
        .and_then(|_| find_item(world, player, at, ItemClass::CarriedFirst))
        .map_or_else(|| at.to_string(), |e| name_of(world, e));
    UseFlavor {
        you: format!("You point {item_name} at {thing}."),
        room: format!("{actor} points {item_name} at {thing}."),
        victim: None,
    }
}

pub(crate) fn cmd_use(world: &mut World, player: Entity, args: &str) {
    use_held_item(world, player, args, "use");
}

pub(crate) fn cmd_play(world: &mut World, player: Entity, args: &str) {
    use_held_item(world, player, args, "play");
}

pub(crate) fn cmd_remove(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Remove what?\r\n");
        return;
    }
    // `remove all` — strip every equipped item.
    if target_word.eq_ignore_ascii_case("all") {
        let mut worn: Vec<(Entity, String, Slot)> = {
            let mut q =
                world.query_filtered::<(Entity, &Located, &Named, &EquippedSlot), With<Item>>();
            q.iter(world)
                .filter(|(_, l, _, _)| l.0 == player)
                .map(|(e, _, n, eq)| (e, n.name.clone(), eq.0))
                .collect()
        };
        // Legacy `do_remove` walks `where = 0..NUM_WEARS`, so the strip
        // (and its messages) follow equipment slot order, not query order.
        // Legacy `WEAR_OBELT` is 26, after `WEAR_WAIST` (13), but here the
        // waist item drops whatever hangs from the belt as it comes off.
        // The belt item goes first so it is removed normally (message,
        // unapply, Remove trigger, counted) rather than falling off.
        worn.sort_by_key(|(_, _, s)| {
            let slot = if *s == Slot::Belt { Slot::Waist } else { *s };
            let pos = Slot::ORDER
                .iter()
                .position(|x| *x == slot)
                .unwrap_or(usize::MAX);
            // Belt sorts just ahead of the waist slot it borrowed.
            (pos, u8::from(*s != Slot::Belt))
        });
        let items: Vec<(Entity, String)> = worn.into_iter().map(|(e, n, _)| (e, n)).collect();
        if items.is_empty() {
            send_to(world, player, "You aren't wearing anything.\r\n");
            return;
        }
        let actor_name = name_of(world, player);
        let located = world.get::<Located>(player).copied();
        let mut removed = 0usize;
        for (item, item_name) in &items {
            // A belt item that fell off with the waist item above is
            // already in the pack.
            if world.get::<EquippedSlot>(*item).is_none() {
                continue;
            }
            removed += 1;
            // "You remove X." goes out first, then the wear-granted bonuses
            // are reversed and whatever that announces ("You fall to the
            // ground.") follows (issue #87). The reversal still runs BEFORE
            // removing the EquippedSlot — `unapply_object_from_wearer` reads
            // the slot to log effect-grant filtering decisions (no-op
            // semantically, but the order is the legacy contract: unapply,
            // then remove).
            send_rendered(world, player, &format!("You remove {item_name}.\r\n"));
            crate::equip_apply::unapply_object_from_wearer(world, *item, player);
            try_remove::<EquippedSlot>(world, *item);
            crate::triggers::fire_item_event(world, *item, player, mud_world::TriggerEvent::Remove);
        }
        // Single consolidated room broadcast for the bulk strip —
        // one line per item would spam bystanders. Matches the
        // shape `cmd_drop all` uses.
        if let Some(l) = located {
            let count = removed;
            let plural = if count == 1 { "item" } else { "items" };
            broadcast_room_visual(
                world,
                l.0,
                player,
                &[player],
                &cap_sentence_start(&format!(
                    "{actor_name} removes {count} {plural} of gear.\r\n"
                )),
            );
        }
        refresh_player_items_gmcp(world, player);
        return;
    }
    let item = find_item(world, player, target_word, ItemClass::Equipment);
    let Some(item) = item else {
        send_rendered(
            world,
            player,
            &format!("You aren't wearing '{target_word}'.\r\n"),
        );
        return;
    };
    let item_name = name_of(world, item);
    // The removal is announced before its effects are reversed (issue #87).
    send_rendered(world, player, &format!("You remove {item_name}.\r\n"));
    if let Some(located) = world.get::<Located>(player).copied() {
        let actor_name = name_of(world, player);
        broadcast_room_visual(
            world,
            located.0,
            player,
            &[player],
            &cap_sentence_start(&format!("{actor_name} removes {item_name}.\r\n")),
        );
    }
    crate::equip_apply::unapply_object_from_wearer(world, item, player);
    try_remove::<EquippedSlot>(world, item);
    crate::triggers::fire_item_event(world, item, player, mud_world::TriggerEvent::Remove);
    refresh_player_items_gmcp(world, player);
}

pub(crate) fn cmd_equipment(world: &mut World, player: Entity, _args: &str) {
    // Snapshot (entity, slot, name, weight) per worn item. Entity
    // is captured so we can also emit a Char.Items.List frame for
    // the "wear" location alongside the rendered text. Weight
    // comes from the proto via WorldKey; synthetic items without
    // a proto count as 0 (matches the carried_weight contract).
    let worn_items: Vec<(Entity, Slot, String, f64)> = {
        let mut q = world.query_filtered::<(Entity, &Located, &Named, &EquippedSlot), With<Item>>();
        q.iter(world)
            .filter(|(_, l, _, _)| l.0 == player)
            .map(|(e, _, n, eq)| {
                // Legacy `do_equipment`: a worn item the viewer cannot see
                // is just "Something.", with nothing else given away.
                if !crate::commands::senses::item_visible_to(world, player, e) {
                    return (e, eq.0, "Something.".to_string(), 0.0);
                }
                let name =
                    crate::commands::senses::with_item_tags(world, player, e, n.name.clone());
                (e, eq.0, name, item_weight(world, e))
            })
            .collect()
    };
    if worn_items.is_empty() {
        send_to(world, player, "\r\nYou aren't wearing anything.\r\n");
        // Push an empty "wear" list so a previously-displayed
        // Mudlet equipment panel clears.
        send_char_items_list(world, player, "wear", &[]);
        return;
    }
    let mut by_slot: Vec<(Slot, String, f64)> = worn_items
        .iter()
        .map(|(_, s, n, w)| (*s, n.clone(), *w))
        .collect();
    by_slot.sort_by_key(|(s, _, _)| {
        Slot::ORDER
            .iter()
            .position(|x| x == s)
            .unwrap_or(usize::MAX)
    });
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
    let entities: Vec<Entity> = worn_items.iter().map(|(e, _, _, _)| *e).collect();
    send_char_items_list(world, player, "wear", &entities);
}

/// The line a room listing prints for a ground item: the prototype's
/// room (long) description, as legacy `list_obj_to_char` does with
/// SHOW_LONG_DESC. Falls back to the short name for items with no
/// prototype (corpses, synthetic items), an empty long description,
/// or a per-instance rename.
fn item_room_line(world: &World, item: Entity, short_name: &str) -> String {
    // A corpse has no prototype; its short name ("the corpse of a creeping
    // vine") is a noun phrase, so the room line makes it a sentence, as
    // legacy `make_corpse` does ("The corpse of X is lying here.").
    if world.get::<mud_world::Corpse>(item).is_some() {
        return format!(
            "{} is lying here.",
            crate::commands::cap_sentence_start(short_name)
        );
    }
    if world
        .get::<mud_world::ItemCustomization>(item)
        .is_some_and(|c| c.name.is_some())
    {
        return short_name.to_string();
    }
    world
        .get::<WorldKey>(item)
        .and_then(|k| {
            world
                .get_resource::<mud_world::ObjectPrototypes>()
                .and_then(|p| p.by_key.get(&(k.zone, k.id)))
        })
        .map(|p| p.room_description.trim())
        .filter(|d| !d.is_empty())
        .map_or_else(|| short_name.to_string(), str::to_string)
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
                // A racial active's own cooldown is keyed by the negated id.
                let (ability_id, innate) = if *id < 0 { (-*id, true) } else { (*id, false) };
                let name = catalog
                    .by_name
                    .values()
                    .find(|d| d.id == ability_id)
                    .map_or_else(|| format!("ability #{ability_id}"), |d| d.name.clone());
                let name = if innate {
                    format!("{name} (innate)")
                } else {
                    name
                };
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

/// Underscored ability enums ("DETECT_MAGIC", "detect_magic") render as
/// "Detect Magic". The shared `capitalize` helper joins with `-` (it also
/// serves race names like HALF_ELF), so it can't be reused here.
fn pretty_ability(raw: &str) -> String {
    raw.split('_')
        .map(|seg| {
            let mut chars = seg.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => {
                    let head = c.to_ascii_uppercase().to_string();
                    let tail: String = chars.as_str().to_ascii_lowercase();
                    head + &tail
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Display name for an effect row. `modify` effects are named after the
/// stat they move ("eva", "cha", "max_hp"); spell the abbreviations out
/// instead of printing three-letter stubs. Anything unmapped falls back to
/// [`pretty_ability`].
pub(super) fn effect_display_name(raw: &str) -> String {
    let full = match raw.to_ascii_lowercase().as_str() {
        "str" | "str_bonus" => "Strength",
        "dex" | "dex_bonus" => "Dexterity",
        "con" | "con_bonus" => "Constitution",
        "int" | "int_bonus" => "Intelligence",
        "wis" | "wis_bonus" => "Wisdom",
        "cha" | "cha_bonus" => "Charisma",
        "eva" => "Evasion",
        "acc" => "Accuracy",
        "ap" => "Attack Power",
        "max_hp" => "Maximum Hit Points",
        "max_move" | "max_stamina" | "stamina_max" => "Maximum Stamina",
        "regen_hp" => "Hit Point Regeneration",
        "save_spell" => "Spell Saving Throw",
        "ward_pct" => "Ward",
        _ => return pretty_ability(raw),
    };
    full.to_string()
}

/// Player-facing label for an ability: its display name with colour
/// tags stripped ("Invisibility"), or the title-cased `plain_name`
/// ("Detect Magic") when the display name is empty.
pub(crate) fn ability_label(def: &mud_world::AbilityDef) -> String {
    let shown = render_color_tags(&def.name, ColorMode::Strip);
    if shown.trim().is_empty() {
        pretty_ability(&def.plain_name)
    } else {
        shown.trim().to_string()
    }
}

/// Player-facing label for an ability id, `None` when the catalog has no
/// such ability (or no catalog is loaded).
pub(crate) fn ability_label_by_id(world: &World, id: i32) -> Option<String> {
    world
        .get_resource::<AbilityCatalog>()?
        .by_name
        .values()
        .find(|d| d.id == id)
        .map(ability_label)
}

/// Stable machine id of an effect instance for GMCP `Char.Effects.id`: the
/// lowercased `plain_name` of the spawning ability ("stone_skin"), else the
/// lowercased effect name. Unlike [`effect_label`] it never changes with
/// display wording, so clients can key icons on it.
pub(crate) fn effect_id(world: &World, inst: &EffectInstance) -> String {
    inst.ability_id
        .and_then(|id| {
            world
                .get_resource::<AbilityCatalog>()?
                .by_name
                .values()
                .find(|d| d.id == id)
                .map(|d| d.plain_name.to_ascii_lowercase())
        })
        .unwrap_or_else(|| inst.name.to_ascii_lowercase())
}

/// Player-facing label for an effect instance, shared by `effects`,
/// `score`, `cancel`, the prompt `%l` code and GMCP `Char.Effects`: the
/// spawning ability's display name ("Invisibility", as legacy
/// `show_active_spells` printed the spell name), else the effect's own
/// name spelled out ([`effect_display_name`], which title-cases
/// `SNAKE_CASE` identifiers).
pub(crate) fn effect_label(world: &World, inst: &EffectInstance) -> String {
    inst.ability_id
        .and_then(|id| ability_label_by_id(world, id))
        .unwrap_or_else(|| effect_display_name(&inst.name))
}

/// Remaining effect time in MUD hours (75 real seconds each), rounded up
/// and never below one, matching legacy `show_active_spells` where
/// `duration + 1` hours are shown ("1 hour", "3 hours").
pub(super) fn format_effect_hours(remaining_secs: i32) -> String {
    let per_hour = crate::item_decay::SECS_PER_MUD_HOUR;
    let hours = (remaining_secs.max(0).saturating_add(per_hour - 1) / per_hour).max(1);
    if hours == 1 {
        "1 hour".to_string()
    } else {
        format!("{hours} hours")
    }
}

/// One row of the `effects` listing.
struct ActiveEffectRow {
    /// Player-facing label ([`effect_label`]).
    label: String,
    remaining_secs: i32,
    /// The label came from the spawning ability, so a stat change is
    /// worth naming next to it ("Enhance Ability (+4 Charisma)").
    from_ability: bool,
    /// `(stat, amount)` of the instance's `ModifyDelta`, hidden for
    /// invisibility (its evasion bonus is a modelling detail).
    delta: Option<(String, i32)>,
    /// Permanent-effect origin ("racial (Elf)", "from <item>").
    origin: Option<String>,
}

/// Where a permanent effect comes from, for the `effects` listing:
/// race innates read "racial (Elf)", worn-item grants name the item,
/// mob defaults read "innate". `None` for sources with nothing useful
/// to say (the caller falls back to the spawning ability, if any).
fn permanent_origin(
    world: &World,
    owner: Entity,
    source: &mud_world::EffectSource,
    item: Option<Entity>,
) -> Option<String> {
    use mud_world::mob_effects::{is_race_effect, is_worn_item_effect};
    if is_worn_item_effect(source) {
        let raw = item.map(|i| name_of(world, i)).filter(|n| !n.is_empty())?;
        return Some(format!(
            "from {}",
            render_color_tags(&raw, ColorMode::Strip)
        ));
    }
    if is_race_effect(source) {
        let race = world
            .get::<Profile>(owner)
            .map(|p| p.race.as_str())
            .filter(|r| !r.is_empty());
        return Some(race.map_or_else(
            || "racial".to_string(),
            |r| format!("racial ({})", pretty_ability(r)),
        ));
    }
    matches!(source, mud_world::EffectSource::Other(s) if s == "mob_default")
        .then(|| "innate".to_string())
}

/// `cancel [<effect>]`: drop a non-permanent effect from yourself.
/// Empty arg lists cancellable effects; named arg matches by
/// case-insensitive substring on the effect's name.
pub(crate) fn cmd_effects(world: &mut World, player: Entity, _args: &str) {
    // Snapshot effects on the player; pull the optional ModifyDelta
    // companion in the same query so the renderer can show
    // "Bless (+2 Strength)" for stat-bonus effects.
    let active: Vec<ActiveEffectRow> = {
        let mut q = world.query::<(
            &EffectInstance,
            &AppliedTo,
            Option<&mud_world::ModifyDelta>,
            Option<&mud_world::GrantedByItem>,
            Has<mud_world::InvisibleSource>,
        )>();
        let rows: Vec<_> = q
            .iter(world)
            .filter(|(_, a, ..)| a.0 == player)
            .map(|(inst, _, delta, granted, invisible)| {
                (
                    effect_label(world, inst),
                    inst.ability_id
                        .is_some_and(|id| ability_label_by_id(world, id).is_some()),
                    inst.remaining_secs,
                    delta
                        .filter(|_| !invisible)
                        .map(|d| (d.target.clone(), d.amount)),
                    (inst.remaining_secs < 0).then(|| (inst.source.clone(), granted.map(|g| g.0))),
                )
            })
            .collect();
        rows.into_iter()
            .map(|(label, from_ability, remaining_secs, delta, perm)| {
                let origin =
                    perm.and_then(|(src, item)| permanent_origin(world, player, &src, item));
                ActiveEffectRow {
                    label,
                    remaining_secs,
                    from_ability,
                    delta,
                    origin,
                }
            })
            .collect()
    };
    let mut out = if active.is_empty() {
        "\r\nYou have no active effects.\r\n".to_string()
    } else {
        format!("\r\n<b:cyan>{} active effect(s):</>\r\n", active.len())
    };
    for row in active {
        let ActiveEffectRow {
            label: pretty_name,
            remaining_secs: remaining,
            from_ability,
            delta,
            origin,
        } = row;
        // Source attribution dimmed: only permanent effects carry one
        // now, since a spell is named after its ability.
        let suffix = origin.map_or(String::new(), |n| format!(" <dim>— {n}</>"));
        // Modifier delta colored by sign — green for buffs, red for
        // debuffs. A bless (+2 STR) reads green; a curse (-3 DEX)
        // reads red. Player can scan the list and immediately see
        // which way each effect is pulling them. When the row is
        // named after its ability the stat is spelled out after the
        // number ("Enhance Ability (+4 Charisma)"); a row already
        // named for its stat just shows the number.
        let delta_label = delta.map_or(String::new(), |(stat, a)| {
            let (sign, color) = if a >= 0 {
                ("+", "<green>")
            } else {
                ("", "<red>")
            };
            if from_ability {
                format!(" {color}({sign}{a} {})</>", effect_display_name(&stat))
            } else {
                format!(" {color}({sign}{a})</>")
            }
        });
        if remaining < 0 {
            // Permanent effects (innate racials, divine boons, etc.)
            // get a bold-cyan tag — they're not on the clock.
            out.push_str(&format!(
                "  <b:cyan>{pretty_name}</>{delta_label} <b:cyan>(permanent)</>{suffix}\r\n"
            ));
        } else {
            // MUD hours, like legacy `show_active_spells` ("3 hrs
            // remaining"). The colour band still keys off the real
            // seconds left so an about-to-expire buff reads warm.
            #[allow(clippy::cast_sign_loss)]
            let secs = remaining.max(0) as u64;
            let dur_open = effect_duration_color(secs);
            let hours = format_effect_hours(remaining);
            let dur_label = match dur_open {
                Some(open) => format!("{open}{hours}</>"),
                None => hours,
            };
            out.push_str(&format!(
                "  <cyan>{pretty_name}</>{delta_label} ({dur_label} remaining){suffix}\r\n"
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
    let unlocked: std::collections::HashMap<i32, chrono::DateTime<chrono::Utc>> = world
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
                format!(
                    "Unknown category '{other}'. Try: all, combat, exploration, social, crafting, misc.\r\n"
                ),
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
        let unlocked_at = unlocked.get(&def.id).copied();
        let is_unlocked = unlocked_at.is_some();
        if def.hidden && !is_unlocked {
            continue;
        }
        if current_cat != Some(def.category) {
            current_cat = Some(def.category);
            out.push_str(&format!("\r\n  --- {} ---\r\n", def.category.label()));
        }
        let mark = if is_unlocked { "[*]" } else { "[ ]" };
        let when = unlocked_at
            .map(|t| format!("  <dim>(unlocked {})</>", t.format("%Y-%m-%d")))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {mark} {} — {}{when}\r\n",
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
                        if g.can_place {
                            "can place items"
                        } else {
                            "visit only"
                        },
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
                "Unknown subcommand '{other}'. Try 'house info', 'house rooms', 'house guests', 'house place <item>', 'house take <item>', 'house guest add <name> [place]', 'house guest remove <name>', 'house rename <#> <name>', 'house describe <#> <text>'.\r\n"
            ));
        }
    }
    send_to(world, player, out);
}

/// What the `level` readout needs, gathered from the world so the text
/// itself can be built (and tested) without one.
pub(crate) struct LevelReport<'a> {
    pub level: i32,
    /// Staff rank title ("Avatar"), when the level row has one.
    pub title: Option<&'a str>,
    /// Color-tagged class stars when the character is at `**`.
    pub stars: Option<&'a str>,
    /// Progress toward the next level; `None` at `**`, for staff, or when
    /// the level table has no next row.
    pub progress: Option<crate::commands::LevelProgress>,
}

/// Plain-ASCII `level` text: one headline, one progress line.
pub(crate) fn render_level_report(r: &LevelReport<'_>) -> String {
    let mut out = format!("\r\nLevel {}", r.level);
    if let Some(stars) = r.stars {
        out.push_str(&format!(" {stars}"));
    } else if let Some(title) = r.title {
        out.push_str(&format!(" ({title})"));
    }
    out.push_str("\r\n");
    if mud_db::enums::is_staff_level(r.level) {
        out.push_str("Experience has no meaning for you.\r\n");
    } else if r.stars.is_some() {
        out.push_str(crate::commands::MAX_MORTAL_EXP_MESSAGE);
        out.push_str("\r\n");
    } else if let Some(p) = r.progress {
        let to_go = (p.next_level_xp - p.current_xp).max(0);
        out.push_str(&format!(
            "Progress: {} {}%  ({to_go} XP to level {})\r\n",
            crate::commands::progress_bar(p.percent),
            p.percent,
            r.level + 1,
        ));
    } else {
        out.push_str("You are at the maximum level.\r\n");
    }
    out
}

/// `level`: print level and progress toward the next one.
pub(crate) fn cmd_level(world: &mut World, player: Entity, _args: &str) {
    let Some(p) = world.get::<Profile>(player) else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    let stars = mud_world::is_starstar(world, p).then(|| {
        p.class_id
            .and_then(|id| {
                world
                    .resource::<ClassCatalog>()
                    .by_id
                    .get(&id)
                    .map(mud_world::ClassDef::stars)
            })
            .unwrap_or_else(|| "**".to_string())
    });
    let report = LevelReport {
        level: p.level,
        title: world.resource::<mud_world::LevelTable>().title_for(p.level),
        stars: stars.as_deref(),
        progress: crate::commands::level_progress(world, p),
    };
    let out = render_level_report(&report);
    send_rendered(world, player, &out);
}

/// `slots`: display the player's per-circle spell-slot pool and any
/// cooldowns currently in flight. Format: `Circle N: free/max [cd: 12s, 30s]`.
pub(crate) fn cmd_slots(world: &mut World, player: Entity, _args: &str) {
    use mud_world::{SpellSlotData, SpellSlots};
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
    let caps = world.resource::<SpellSlotData>().slots_for(class_id, level);
    if caps.is_empty() {
        send_to(
            world,
            player,
            format!("\r\nLevel {level} {class_name} — no accessible spell circles.\r\n"),
        );
        return;
    }
    let slots = world.get::<SpellSlots>(player).cloned().unwrap_or_default();
    let mut out = format!("\r\nLevel {level} {class_name} spell slots:\r\n");
    for (circle, max) in caps {
        let used = slots.used_in_circle(circle);
        let free = max - used;
        let cooldowns: Vec<String> = slots
            .in_flight
            .iter()
            .filter(|cd| cd.circle == circle)
            .map(|cd| format!("{}s", cd.secs_remaining))
            .collect();
        if cooldowns.is_empty() {
            out.push_str(&format!(
                "  Circle {circle:>2}: {free:>2} free / {max:>2}\r\n"
            ));
        } else {
            out.push_str(&format!(
                "  Circle {circle:>2}: {free:>2} free / {max:>2}  (recovering: {})\r\n",
                cooldowns.join(", "),
            ));
        }
    }
    send_to(world, player, out);
}

pub(crate) fn cmd_spells(world: &mut World, player: Entity, args: &str) {
    use mud_db::abilities::AbilityKind;

    let mode = color_mode_for(world, player);
    // Argument shape:
    //   `spells`               → known spells, grouped by circle
    //   `spells <N>`           → known, circle N only
    //   `spells <N>-<M>`       → known, circles N..=M
    //   `spells <kw>`          → known + keyword filter
    //   `spells <range> <kw>`  → known + circle range + keyword
    //   `spells all [<kw>]`    → full catalog (no circle grouping)
    let raw = args.trim();
    let (show_all, rest) = match raw.strip_prefix("all") {
        Some(r) if r.is_empty() || r.starts_with(char::is_whitespace) => (true, r.trim()),
        _ => (false, raw),
    };
    let mut tokens = rest.split_whitespace();
    let (circle_range, filter) = match tokens.next() {
        Some(first) => match parse_circle_range(first) {
            Some(rng) => (
                Some(rng),
                tokens.collect::<Vec<_>>().join(" ").to_ascii_lowercase(),
            ),
            None => (
                None,
                std::iter::once(first)
                    .chain(tokens)
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_ascii_lowercase(),
            ),
        },
        None => (None, String::new()),
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

    // Resolve the caster's class so we can look up per-circle assignments.
    // Used only when we group by circle; `spells all` ignores it.
    let class_id = world.get::<Profile>(player).and_then(|p| p.class_id);
    let slot_data = world.resource::<mud_world::SpellSlotData>();

    let matches_filter = |def: &mud_world::AbilityDef| -> bool {
        if filter.is_empty() {
            return true;
        }
        let pn = def.plain_name.to_ascii_lowercase();
        if pn.contains(&filter) {
            return true;
        }
        if def
            .sphere
            .as_deref()
            .is_some_and(|s| s.to_ascii_lowercase().contains(&filter))
        {
            return true;
        }
        if def
            .damage_type
            .as_deref()
            .is_some_and(|d| d.to_ascii_lowercase().contains(&filter))
        {
            return true;
        }
        false
    };

    if show_all {
        // Flat catalog dump — no circle grouping (cross-class).
        let mut entries: Vec<String> = world
            .resource::<AbilityCatalog>()
            .by_name
            .values()
            .filter(|d| d.kind == AbilityKind::Spell && matches_filter(d))
            .map(format_ability_with_sphere)
            .collect();
        if entries.is_empty() {
            let msg = if filter.is_empty() {
                "\r\nNo spells loaded.\r\n".to_string()
            } else {
                format!("\r\nNo spells matching '{filter}' in the catalog.\r\n")
            };
            send_rendered(world, player, &msg);
            return;
        }
        entries.sort_unstable();
        let mut out = format!(
            "\r\n<b:cyan>All loaded spells</> <dim>({})</>:\r\n",
            entries.len(),
        );
        let width = crate::layout::wrap_width(world, player);
        let column_width = name_column_width(std::iter::once(entries.as_slice()));
        for row in crate::layout::format_grid(&entries, column_width, 2, width) {
            out.push_str(&render_color_tags(&row, mode));
            out.push_str("\r\n");
        }
        send_to(world, player, out);
        return;
    }

    // Known-spells path: group by circle for the caller's class.
    // Circle 0 catches spells the player knows but their class has
    // no circle assignment for (cross-class scrolls etc.).
    let mut by_circle: std::collections::BTreeMap<i32, Vec<String>> =
        std::collections::BTreeMap::new();
    for def in world.resource::<AbilityCatalog>().by_name.values() {
        if def.kind != AbilityKind::Spell {
            continue;
        }
        if let Some(set) = &known
            && !set.contains(&def.id)
        {
            continue;
        }
        if !matches_filter(def) {
            continue;
        }
        let circle = class_id
            .and_then(|cid| slot_data.ability_circle.get(&(cid, def.id)).copied())
            .unwrap_or(0);
        if let Some((lo, hi)) = circle_range
            && (circle < lo || circle > hi)
        {
            continue;
        }
        by_circle
            .entry(circle)
            .or_default()
            .push(format_ability_with_sphere(def));
    }

    if by_circle.is_empty() {
        let msg = match (circle_range, filter.is_empty()) {
            (Some((lo, hi)), true) if lo == hi => {
                format!("\r\nYou know no circle-{lo} spells.\r\n")
            }
            (Some((lo, hi)), true) => {
                format!("\r\nYou know no spells in circles {lo}-{hi}.\r\n")
            }
            (Some((lo, hi)), false) if lo == hi => {
                format!("\r\nNo circle-{lo} spells matching '{filter}'.\r\n")
            }
            (Some((lo, hi)), false) => {
                format!("\r\nNo spells matching '{filter}' in circles {lo}-{hi}.\r\n")
            }
            (None, true) => "\r\n<dim>You haven't learned any spells yet.</> \
                             Try 'spells all' to browse the catalog.\r\n"
                .to_string(),
            (None, false) => format!(
                "\r\nNo known spells matching '{filter}'. \
                 Try 'spells all {filter}'.\r\n"
            ),
        };
        send_rendered(world, player, &msg);
        return;
    }

    let total: usize = by_circle.values().map(Vec::len).sum();
    let mut out = format!("\r\n<b:cyan>Spells you know</> <dim>({total})</>:\r\n");
    for names in by_circle.values_mut() {
        names.sort_unstable();
    }
    // One column width for the whole listing so every circle's
    // columns line up with the ones above and below it.
    let width = crate::layout::wrap_width(world, player);
    let column_width = name_column_width(by_circle.values().map(Vec::as_slice));
    for (circle, names) in &by_circle {
        let header = if *circle == 0 {
            String::from("(no circle for your class)")
        } else {
            format!("Circle {circle}")
        };
        out.push_str(&format!(
            "<b:yellow>{}</> <dim>({})</>:\r\n",
            header,
            names.len(),
        ));
        for row in crate::layout::format_grid(names, column_width, 2, width) {
            out.push_str(&render_color_tags(&row, mode));
            out.push_str("\r\n");
        }
    }
    send_to(world, player, out);
}

/// Parse a circle filter token: `"3"` → `(3, 3)`; `"1-5"` → `(1, 5)`.
/// Returns `None` when the token isn't a numeric range, so the caller
/// can fall through to treating it as a keyword. Bounds are clamped
/// to 1..=14 (the legacy circle ceiling) and swapped if reversed so
/// `spells 5-1` still does the obvious thing.
fn parse_circle_range(tok: &str) -> Option<(i32, i32)> {
    let parse_one = |s: &str| -> Option<i32> {
        let n: i32 = s.parse().ok()?;
        if (1..=14).contains(&n) { Some(n) } else { None }
    };
    match tok.split_once('-') {
        None => {
            let n = parse_one(tok)?;
            Some((n, n))
        }
        Some((lo, hi)) => {
            let lo = parse_one(lo)?;
            let hi = parse_one(hi)?;
            if lo <= hi {
                Some((lo, hi))
            } else {
                Some((hi, lo))
            }
        }
    }
}

/// Render an ability's display name plus a colored parenthetical
/// for its sphere when one is assigned. Lets the spells / chants
/// / songs / skills listings double as an elemental-affinity scan
/// without forcing players to run `identify` per spell. Sphere
/// hue picks from the palette in `sphere_color_tag`
/// (fire=red, water=cyan, healing=green, etc.); unmapped or
/// missing spheres fall through to dim. Abilities without a
/// sphere assignment render the name unchanged.
pub(crate) fn format_ability_with_sphere(def: &mud_world::AbilityDef) -> String {
    let Some(s) = def.sphere.as_deref().filter(|s| !s.is_empty()) else {
        return def.name.clone();
    };
    let open = sphere_color_tag(s).unwrap_or("<dim>");
    format!("{} {open}({s})</>", def.name)
}

/// Pick the column width for an ability-list grid. Sized to the
/// widest *visible* name across every list on the page plus a 2-space
/// gutter, with a 22-char floor so short-name pages don't render too
/// cramped.
fn name_column_width<'a>(lists: impl IntoIterator<Item = &'a [String]>) -> usize {
    crate::layout::grid_col_width(lists, 2, 22)
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

/// Legacy `do_group` preconditions for `leader` enrolling `joiner`: only
/// the head of a group enrols, and the joiner must not already belong to
/// (or lead) another group. `None` means the join is allowed.
fn group_join_refusal(world: &mut World, leader: Entity, joiner: Entity) -> Option<&'static str> {
    if group_root(world, leader) != leader {
        return Some("You cannot enroll group members without being head of a group.\r\n");
    }
    if group_root(world, joiner) != joiner {
        return Some("That person is already in a group.\r\n");
    }
    if group_members(world, joiner).len() > 1 {
        return Some("That person is leading a group.\r\n");
    }
    None
}

/// `invite <player>`: send a group invite. Recipient gets a
/// `GroupInvite` component carrying the inviter's entity; their
/// `accept` will make them a member of the sender's group.
pub(crate) fn cmd_invite(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Invite whom?\r\n");
        return;
    }
    if name_approval_gate(world, player) {
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
    if world
        .get::<mud_world::GroupMember>(target)
        .is_some_and(|g| g.0 == player)
    {
        let n = name_of(world, target);
        send_rendered(world, player, &format!("{n} is already in your group.\r\n"));
        return;
    }
    if let Some(refusal) = group_join_refusal(world, player, target) {
        send_to(world, player, refusal);
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
            "{inviter} invites you to a group. Type 'accept' to join, \
             'decline' to refuse.\r\n"
        ),
    );
}

/// `accept`: accept whichever pending offer is currently
/// outstanding. A pending `PendingSummon` (from someone casting
/// SUMMON on you) takes priority over a `GroupInvite` because
/// it's time-sensitive (30s expiry vs. 5min). On summon accept:
/// teleports the caller to the caster's room, broadcasts the
/// depart/arrive, applies a 16s cooldown on the caster, and
/// (when the original caster's spell came back successfully)
/// runs the auto-look. Refused if the offer has expired or the
/// other party has disconnected.
pub(crate) fn cmd_accept(world: &mut World, player: Entity, _args: &str) {
    // Summon offer first — it has the shorter expiry window.
    if let Some(summon) = world.get::<mud_world::PendingSummon>(player).cloned() {
        if summon.at.elapsed() > std::time::Duration::from_secs(30) {
            try_remove::<mud_world::PendingSummon>(world, player);
            send_to(world, player, "The summons offer has already expired.\r\n");
            return;
        }
        if world.get_entity(summon.from).is_err() {
            try_remove::<mud_world::PendingSummon>(world, player);
            send_to(world, player, "The summoner is no longer around.\r\n");
            return;
        }
        if world.get_entity(summon.dest_room).is_err() {
            try_remove::<mud_world::PendingSummon>(world, player);
            send_to(world, player, "The destination has crumbled away.\r\n");
            return;
        }
        // The summoner may stand in a room that bars the summoned (a god
        // room): re-check at the moment of the move, not just at cast time.
        if world.get::<Located>(player).map(|l| l.0) != Some(summon.dest_room)
            && !crate::room_access::entry_allowed(world, player, summon.dest_room)
        {
            try_remove::<mud_world::PendingSummon>(world, player);
            send_to(world, player, crate::room_access::ENTRY_REFUSED);
            send_to(world, summon.from, "The summons fizzles out.\r\n");
            return;
        }
        let cur_room = world.get::<Located>(player).map(|l| l.0);
        let actually_moved = cur_room != Some(summon.dest_room);
        if actually_moved {
            crate::combat::relocate(world, player, summon.dest_room);
        }
        let tname = world
            .get::<Named>(player)
            .map_or("Someone".to_string(), |n| n.name.clone());
        if actually_moved {
            if let Some(old_room) = cur_room {
                crate::commands::broadcast_room_visual(
                    world,
                    old_room,
                    player,
                    &[player],
                    &crate::commands::cap_sentence_start(&format!(
                        "{tname} disappears suddenly.\r\n"
                    )),
                );
            }
            crate::commands::broadcast_room_visual(
                world,
                summon.dest_room,
                player,
                &[player],
                &crate::commands::cap_sentence_start(&format!("{tname} arrives suddenly.\r\n")),
            );
        }
        send_to(
            world,
            player,
            format!(
                "You accept the summons and find yourself in {dest}.\r\n",
                dest = summon.dest_room_name
            ),
        );
        send_to(
            world,
            summon.from,
            format!("{tname} accepts your summons.\r\n"),
        );
        // 16-second post-summon cooldown on the caster — matches the
        // legacy WAIT_STATE(4 rounds) on summoner. Cooldown rides
        // the SUMMON ability id so `cast summon` is the only thing
        // locked out, not the caster's other spells.
        let summon_ability_id = world
            .resource::<mud_world::AbilityCatalog>()
            .by_name
            .get("summon")
            .map(|d| d.id);
        if let Some(aid) = summon_ability_id {
            let ready_at = std::time::Instant::now() + std::time::Duration::from_secs(16);
            let mut cd = world
                .get_mut::<mud_world::Cooldowns>(summon.from)
                .map(|mut c| std::mem::take(&mut *c))
                .unwrap_or_default();
            cd.ready_at.insert(aid, ready_at);
            try_insert(world, summon.from, cd);
        }
        try_remove::<mud_world::PendingSummon>(world, player);
        // Show the new surroundings — matches the auto_look_after_cast
        // pattern used by teleport spells.
        cmd_look(world, player, "");
        return;
    }

    let Some(invite) = world.get::<mud_world::GroupInvite>(player).copied() else {
        send_to(world, player, "You have no pending group invites.\r\n");
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
    // Membership may have changed since the invite went out.
    if let Some(refusal) = group_join_refusal(world, invite.from, player) {
        try_remove::<mud_world::GroupInvite>(world, player);
        send_to(world, player, refusal);
        return;
    }
    try_insert(world, player, mud_world::GroupMember(invite.from));
    // The new member falls in behind the leader, unless the leader is
    // already following them (a follow cycle would stall movement).
    if !would_create_cycle(world, invite.from, player) {
        try_insert(world, player, Follower(invite.from));
    }
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

/// Per-tick sweep over outstanding `PendingSummon` markers. Drops
/// any older than 30 seconds, notifying both parties so the prompt
/// doesn't silently disappear. Cheap — n is small in practice (one
/// component per actively-prompted target).
pub(crate) fn pending_summon_tick(world: &mut World) {
    let now = std::time::Instant::now();
    let expired: Vec<(Entity, Entity, String, String)> = {
        let mut q = world.query::<(Entity, &mud_world::PendingSummon)>();
        q.iter(world)
            .filter(|(_, p)| now.duration_since(p.at) > std::time::Duration::from_secs(30))
            .map(|(e, p)| (e, p.from, p.from_name.clone(), p.dest_room_name.clone()))
            .collect()
    };
    for (target, caster, _caster_name, dest_room_name) in expired {
        try_remove::<mud_world::PendingSummon>(world, target);
        let tname = world
            .get::<Named>(target)
            .map_or("Someone".to_string(), |n| n.name.clone());
        send_to(
            world,
            target,
            format!("Your pending summons offer to {dest_room_name} expired.\r\n"),
        );
        if world.get_entity(caster).is_ok() {
            send_to(
                world,
                caster,
                format!("Your summons to {tname} expired without a response.\r\n"),
            );
        }
    }
}

/// `decline`: discard whichever pending offer is outstanding.
/// `PendingSummon` is checked first (shorter expiry); falls
/// through to `GroupInvite`.
pub(crate) fn cmd_decline(world: &mut World, player: Entity, _args: &str) {
    if let Some(summon) = world.get::<mud_world::PendingSummon>(player).cloned() {
        try_remove::<mud_world::PendingSummon>(world, player);
        send_to(world, player, "You decline the summons.\r\n");
        if world.get_entity(summon.from).is_ok() {
            let tname = world
                .get::<Named>(player)
                .map_or("Someone".to_string(), |n| n.name.clone());
            send_to(
                world,
                summon.from,
                format!("{tname} declines your summons.\r\n"),
            );
        }
        return;
    }
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
            "Usage: 'group' (list) or 'group dismiss <player>'.\r\n",
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
        let hp_text = hp_open.map_or(hp_padded.clone(), |open| format!("{open}{hp_padded}</>"));
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
        send_to(world, player, "Usage: order <follower|all> <command>\r\n");
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
        let one = followers.into_iter().find(|e| {
            world.get::<Named>(*e).is_some_and(|n| {
                mud_world::targeting::entity_matches(
                    &needle,
                    &n.name,
                    world.get::<Keywords>(*e).map(|k| k.0.as_slice()),
                )
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
            format!("You only have {} to split.\r\n", format_amount(wealth)),
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
            "You split {} among {} member(s); each receives {}.\r\n",
            format_amount(amount),
            here.len(),
            format_amount(share)
        ),
    );
    for m in &here {
        if *m == player {
            continue;
        }
        send_to(
            world,
            *m,
            format!(
                "{player_name} splits coin with the group; you gain {}.\r\n",
                format_amount(share)
            ),
        );
    }
}

/// `disband`: drop every group member and direct `Follower(self)` link.
/// Followers' own followers stay attached. Self has no component to
/// touch: only entities pointing at self.
pub(crate) fn cmd_disband(world: &mut World, player: Entity, _args: &str) {
    let to_release: Vec<Entity> = {
        let mut q = world.query_filtered::<(
            Entity,
            Option<&Follower>,
            Option<&mud_world::GroupMember>,
        ), With<Player>>();
        q.iter(world)
            .filter(|(_, f, g)| {
                f.is_some_and(|f| f.0 == player) || g.is_some_and(|g| g.0 == player)
            })
            .map(|(e, _, _)| e)
            .collect()
    };
    if to_release.is_empty() {
        send_to(world, player, "Nobody is following you.\r\n");
        return;
    }
    let player_name = name_of(world, player);
    for member in &to_release {
        release_from(world, *member, player);
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
        send_to(
            world,
            player,
            format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };

    // Cycle guard: if target already follows us (or any chain leading back to
    // us), refuse — keeps cmd_move's BFS terminating.
    if would_create_cycle(world, target, player) {
        send_to(world, player, "That would create a follow cycle.\r\n");
        return;
    }

    // Falling in behind someone other than the group leader leaves the group.
    if world
        .get::<mud_world::GroupMember>(player)
        .is_some_and(|g| g.0 != target)
    {
        try_remove::<mud_world::GroupMember>(world, player);
    }
    try_insert(world, player, Follower(target));
    let target_name = name_of(world, target);
    let player_name = name_of(world, player);
    send_rendered(
        world,
        player,
        &format!("You start following {target_name}.\r\n"),
    );
    send_rendered(
        world,
        target,
        &format!("{player_name} starts following you.\r\n"),
    );
    // Broadcast the new follow to other room observers so they
    // see the party shape forming. Skipping player + target since
    // they already got the personalized line above.
    broadcast_room_visual(
        world,
        located.0,
        player,
        &[player, target],
        &cap_sentence_start(&format!(
            "{player_name} falls into step behind {target_name}.\r\n"
        )),
    );
}

pub(crate) fn cmd_unfollow(world: &mut World, player: Entity, _args: &str) {
    // A group member can have no follow link at all (the leader was already
    // following them when they accepted), so leaving the group cannot hinge
    // on one. Walking away from your group leader leaves the group.
    let in_group = world
        .get::<mud_world::GroupMember>(player)
        .is_some_and(|g| world.get_entity(g.0).is_ok());
    if in_group {
        ungroup(world, player, true, false);
    }
    let prev = world.get::<Follower>(player).copied();
    try_remove::<Follower>(world, player);
    if prev.is_none() && in_group {
        // Left the group; there was no follow link to announce.
        return;
    }
    if let Some(Follower(prev_target)) = prev {
        let target_name = name_of(world, prev_target);
        send_rendered(
            world,
            player,
            &format!("You stop following {target_name}.\r\n"),
        );
        let player_name = name_of(world, player);
        send_rendered(
            world,
            prev_target,
            &format!("{player_name} stops following you.\r\n"),
        );
        // Room broadcast — same shape as cmd_follow's join. Only
        // fires when the unfollow target is in the same room; a
        // cross-room unfollow has no shared audience to notify.
        if let Some(located) = world.get::<Located>(player).copied()
            && world.get::<Located>(prev_target).map(|l| l.0) == Some(located.0)
        {
            broadcast_room_visual(
                world,
                located.0,
                player,
                &[player, prev_target],
                &cap_sentence_start(&format!(
                    "{player_name} drops out of step from {target_name}.\r\n"
                )),
            );
        }
    } else {
        send_to(world, player, "You weren't following anyone.\r\n");
    }
}

/// `identify <item>`: dump proto + runtime state for a carried
/// item. Also flips the `Identified` marker on the item itself
/// — once tagged, `look <item>` shows the same stat block, the
/// `Char.Items.List` GMCP frame includes `identified:true` for
/// it, and the item-detail panel in the client renders the full
/// data instead of just the name. The marker survives give/sell
/// (legacy semantic — the receiving player inherits the
/// knowledge) but is cleared on drop / junk / put-into-container,
/// since the player's hands-on link to the item is what's
/// driving the identification in the fiction.
pub(crate) fn cmd_identify(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Identify what?\r\n");
        return;
    }
    let Some(item) = find_item(world, player, needle, ItemClass::Carried) else {
        send_rendered(
            world,
            player,
            &format!("You aren't carrying '{needle}'.\r\n"),
        );
        return;
    };
    // Render the stat block. If the proto is missing this also
    // sends a "no prototype data" line and returns None.
    let Some(out) = render_identify_block(world, player, item) else {
        return;
    };
    // Mark the item as identified for this player's view. Putting
    // the marker on the item entity (not a per-player set) means
    // give/sell automatically carry the knowledge with the item,
    // matching the legacy semantic. The presence of `Identified`
    // also flips the GMCP item-list payload below.
    if let Ok(mut e) = world.get_entity_mut(item)
        && e.get::<mud_world::Identified>().is_none()
    {
        e.insert(mud_world::Identified);
    }
    // Re-emit Char.Items.List for the player so the inventory
    // panel updates to show the new identified state without
    // waiting for the next inventory mutation. Keeps the Mudlet
    // window in sync with the spell's effect immediately.
    crate::commands::refresh_player_items_gmcp(world, player);
    send_rendered(world, player, &out);
}

/// Build the multi-line stat block that `cmd_identify` and the
/// `look <identified item>` path both render. Returns None when
/// the item has no associated prototype (in which case we've
/// already sent the failure line to the player and the caller
/// should bail without further output).
///
/// Takes `&mut World` because the active-effects scan uses a
/// `world.query` which requires mutable access — bevy_ecs has no
/// shared-borrow equivalent.
#[allow(clippy::too_many_lines)]
pub(crate) fn render_identify_block(
    world: &mut World,
    player: Entity,
    item: Entity,
) -> Option<String> {
    let item_name = name_of(world, item);
    let key = world.get::<WorldKey>(item).copied();
    let Some(key) = key else {
        send_rendered(
            world,
            player,
            &format!("{item_name} has no proto link.\r\n"),
        );
        return None;
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
        return None;
    };
    let mode = color_mode_for(world, player);
    // Header — boxed item title with type badge. Width chosen to
    // match common 80-column game windows; the strip-color length
    // matters for the corner alignment, not the rendered width.
    let plain_name = render_color_tags(&p.name, ColorMode::Strip);
    let title_inner = format!(" {plain_name} ");
    let bar_width = title_inner.chars().count().max(40);
    let bar = "─".repeat(bar_width);
    let mut out = String::new();
    out.push_str(&format!("\r\n<b:cyan>╭{bar}╮</>\r\n"));
    let pad = bar_width.saturating_sub(title_inner.chars().count());
    out.push_str(&format!(
        "<b:cyan>│</> <b:white>{}</><b:cyan>{}│</>\r\n",
        render_color_tags(&p.name, mode),
        " ".repeat(pad + 1),
    ));
    out.push_str(&format!("<b:cyan>╰{bar}╯</>\r\n"));

    // Properties block — basics that apply to almost every item.
    out.push_str("  <b:cyan>Properties</>\r\n");
    out.push_str(&format!(
        "    <cyan>Type:</>     <yellow>{}</>\r\n",
        p.r#type.label()
    ));
    out.push_str(&format!(
        "    <cyan>Weight:</>   <dim>{:.1} lbs</>\r\n",
        p.weight
    ));
    if p.level > 0 {
        out.push_str(&format!("    <cyan>Min Lvl:</>  {}\r\n", p.level));
    }
    if p.cost > 0
        && let Some(coin) = format_wealth(i64::from(p.cost))
    {
        out.push_str(&format!("    <cyan>Value:</>    {coin}\r\n"));
    }
    if !p.wear_flags.is_empty() {
        let labels: Vec<&'static str> = p.wear_flags.iter().map(|f| f.label()).collect();
        out.push_str(&format!(
            "    <cyan>Wear:</>     <dim>{}</>\r\n",
            labels.join(", ")
        ));
    }

    if p.armor_pct != 0 {
        out.push_str(&format!(
            "    <cyan>Armor:</>    <yellow>{}%</> <dim>mitigation</>\r\n",
            p.armor_pct
        ));
    }

    // What wearing it does (legacy identify's `Item provides:` and
    // `Apply:` lines): effect flags it grants and stat applies.
    let mut grants = crate::equip_apply::describe_item_grants(world, &p);
    // Enchant Weapon's per-instance applies ride on the item, not the
    // prototype, but wearing it delivers them all the same.
    grants
        .applies
        .extend(crate::equip_apply::describe_instance_applies(world, item));
    if !grants.provides.is_empty() || !grants.applies.is_empty() {
        out.push_str("\r\n  <b:cyan>Worn Effects</>\r\n");
        if !grants.provides.is_empty() {
            out.push_str(&format!(
                "    <cyan>Item provides:</> <b:magenta>{}</>\r\n",
                grants.provides.join(", ")
            ));
        }
        for apply in &grants.applies {
            out.push_str(&format!("    <cyan>Apply:</> {apply}\r\n"));
        }
    }

    // Combat block — only for weapons. Avg damage and damage type
    // are the headline numbers for "should I wield this".
    if p.weapon_dice_num > 0 {
        out.push_str("\r\n  <b:cyan>Combat</>\r\n");
        let bonus = match p.weapon_dice_bonus.cmp(&0) {
            std::cmp::Ordering::Equal => String::new(),
            std::cmp::Ordering::Greater => format!("+{}", p.weapon_dice_bonus),
            std::cmp::Ordering::Less => format!("{}", p.weapon_dice_bonus),
        };
        let dtype_suffix = p
            .weapon_damage_type
            .as_deref()
            .map(|t| format!("  <yellow>({t})</>"))
            .unwrap_or_default();
        out.push_str(&format!(
            "    <cyan>Damage:</>   <b:yellow>{}d{}{bonus}</>  <dim>avg {}</>{dtype_suffix}\r\n",
            p.weapon_dice_num,
            p.weapon_dice_size,
            p.avg_damage(),
        ));
    }

    // Type-specific blocks — drink containers, lights, portals,
    // boards. Each only renders when the proto carries the
    // matching values, so a plain weapon doesn't drag a "Liquid:
    // none" line through the readout.
    if let Some(liq) = &p.liquid {
        out.push_str("\r\n  <b:cyan>Liquid Container</>\r\n");
        let state = world.get::<mud_world::LiquidContainer>(item).cloned();
        let (remaining, capacity) = state
            .as_ref()
            .map_or((liq.remaining, liq.capacity), |s| (s.remaining, s.capacity));
        // The proto's `liquid` is the initial alias; the live
        // `LiquidContainer` overrides on pour/fill. Use the live
        // value when present so a refilled wineskin shows what's
        // actually in it now.
        let live_alias = state
            .as_ref()
            .map_or(liq.liquid.as_str(), |s| s.liquid.as_str());
        let catalog = world.resource::<mud_world::LiquidCatalog>();
        let identified = world.get::<mud_world::Identified>(item).is_some();
        let contents_label = catalog.lookup_alias(live_alias).map_or_else(
            || live_alias.to_string(),
            |def| {
                if identified {
                    def.name.clone()
                } else {
                    // Unidentified: show the color description only
                    // ("clear liquid", "thick red liquid").
                    format!("{} liquid", def.color_desc)
                }
            },
        );
        out.push_str(&format!(
            "    <cyan>Contains:</> {contents_label} <dim>({remaining}/{capacity} units)</>\r\n",
        ));
        // Flavor description from the catalog, shown only when the
        // container is identified. Keeps the unidentified readout
        // short and hint-free.
        if identified
            && let Some(def) = catalog.lookup_alias(live_alias)
            && let Some(desc) = &def.description
        {
            out.push_str(&format!("    <dim>{desc}</>\r\n"));
        }
        if state.as_ref().is_some_and(|s| s.poisoned) {
            out.push_str("    <b:red>! Poisoned</>\r\n");
        }
    }
    if let Some(fuel) = &p.light_fuel {
        out.push_str("\r\n  <b:cyan>Light Source</>\r\n");
        if fuel.remaining < 0 {
            out.push_str("    <cyan>Fuel:</>     <b:yellow>eternal</>\r\n");
        } else {
            out.push_str(&format!(
                "    <cyan>Fuel:</>     {} hours <dim>(of {})</>\r\n",
                fuel.remaining, fuel.capacity,
            ));
        }
    }
    if let Some(dest_vnum) = p.portal_destination_vnum {
        out.push_str("\r\n  <b:cyan>Portal</>\r\n");
        out.push_str(&format!(
            "    <cyan>Destination:</> <dim>vnum {dest_vnum}</>\r\n"
        ));
    }
    if let Some(board) = p.board_id {
        out.push_str("\r\n  <b:cyan>Message Board</>\r\n");
        out.push_str(&format!("    <cyan>Board id:</> <dim>{board}</>\r\n"));
    }

    // Bound abilities — scrolls, wands, staves. Charges-remaining
    // pulled from the per-instance Charges component when present;
    // otherwise the proto's "unlimited / N charges" hint stays.
    let bindings = world
        .resource::<mud_world::ObjectAbilityCatalog>()
        .by_key
        .get(&(key.zone, key.id))
        .cloned()
        .unwrap_or_default();
    if !bindings.is_empty() {
        out.push_str("\r\n  <b:cyan>Bound Abilities</>\r\n");
        let abilities = world.resource::<AbilityCatalog>();
        for b in bindings {
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
        if let Some(c) = world.get::<mud_world::Charges>(item) {
            out.push_str(&format!(
                "    <cyan>Charges remaining:</> <b:yellow>{}</>\r\n",
                c.0
            ));
        }
    } else if let Some(c) = world.get::<mud_world::Charges>(item) {
        out.push_str(&format!(
            "\r\n  <cyan>Charges remaining:</> <b:yellow>{}</>\r\n",
            c.0
        ));
    }

    // Restrictions — who CAN'T wield/wear this. Resolved from the
    // proto's restriction lists: alignments by enum label, classes
    // through the `ClassCatalog`, races by raw name. An empty
    // section is omitted entirely so unrestricted gear stays clean.
    let mut restrictions: Vec<(&'static str, String)> = Vec::new();
    // Alignments barred on the proto, plus any a spell barred on this
    // instance (Enchant Weapon; legacy set `ITEM_ANTI_*`).
    let mut barred_alignments = p.restricted_alignments.clone();
    if let Some(b) = world.get::<mud_world::components::ItemBarredAlignments>(item) {
        for a in &b.0 {
            if !barred_alignments.contains(a) {
                barred_alignments.push(*a);
            }
        }
    }
    if !barred_alignments.is_empty() {
        let labels: Vec<&'static str> = barred_alignments.iter().map(|a| a.label()).collect();
        restrictions.push(("Alignments", labels.join(", ")));
    }
    if !p.restricted_class_ids.is_empty() {
        let catalog = world.resource::<ClassCatalog>();
        let names: Vec<String> = p
            .restricted_class_ids
            .iter()
            .map(|id| {
                catalog
                    .by_id
                    .get(id)
                    .map_or_else(|| format!("class #{id}"), |d| d.plain_name.clone())
            })
            .collect();
        restrictions.push(("Classes", names.join(", ")));
    }
    if !p.restricted_races.is_empty() {
        // Title-case the raw enum strings (HUMAN → Human) so the
        // restriction reads as a sentence rather than shouted SQL.
        let names: Vec<String> = p
            .restricted_races
            .iter()
            .map(|r| {
                let mut chars: Vec<char> = r.to_lowercase().chars().collect();
                if let Some(c) = chars.first_mut() {
                    *c = c.to_ascii_uppercase();
                }
                chars.into_iter().collect()
            })
            .collect();
        restrictions.push(("Races", names.join(", ")));
    }
    if !restrictions.is_empty() {
        // Header reads "Forbidden to" because the list is the set
        // of classes/races/alignments that *can't* equip the item —
        // anyone NOT in the list can. The old "(cannot equip)"
        // hint read as "you can't equip this" which is wrong for
        // most viewers.
        out.push_str("\r\n  <b:red>Forbidden to</>\r\n");
        for (label, body) in restrictions {
            out.push_str(&format!(
                "    <red>·</> <cyan>{label}:</> <dim>{body}</>\r\n"
            ));
        }
    }
    // B6 surfacing: inclusive allow-list + size band. Separate
    // header from the deny-list because the semantic is opposite
    // (a content creator can author one or the other, or both).
    let mut requirements: Vec<(&'static str, String)> = Vec::new();
    if !p.allowed_races.is_empty() {
        let names: Vec<String> = p
            .allowed_races
            .iter()
            .map(|r| {
                let mut chars: Vec<char> = r.to_lowercase().chars().collect();
                if let Some(c) = chars.first_mut() {
                    *c = c.to_ascii_uppercase();
                }
                chars.into_iter().collect()
            })
            .collect();
        requirements.push(("Races", names.join(", ")));
    }
    if let Some(min) = p.min_size.as_deref() {
        requirements.push(("Min size", min.to_string()));
    }
    if let Some(max) = p.max_size.as_deref() {
        requirements.push(("Max size", max.to_string()));
    }
    if !requirements.is_empty() {
        out.push_str("\r\n  <b:green>Requirements</> <dim>(must match to equip)</>\r\n");
        for (label, body) in requirements {
            out.push_str(&format!(
                "    <green>·</> <cyan>{label}:</> <dim>{body}</>\r\n"
            ));
        }
    }

    // Per-instance attribute flags from `ObjectFlags` and
    // `ObjectRestrictions`. These flow from the proto and aren't
    // changeable by the player, but surfacing them on identify
    // lets a player know up-front that this is a NO_DROP quest
    // hook or a GLOW lantern-replacement.
    if !p.flags.is_empty() {
        let labels: Vec<&'static str> = p.flags.iter().map(|f| f.label()).collect();
        out.push_str("\r\n  <b:cyan>Flags</>\r\n");
        out.push_str(&format!("    <cyan>·</> <dim>{}</>\r\n", labels.join(", ")));
    }
    if !p.restrictions.is_empty() {
        let labels: Vec<&'static str> = p.restrictions.iter().map(|r| r.label()).collect();
        out.push_str("\r\n  <b:red>Item Locks</>\r\n");
        out.push_str(&format!("    <red>·</> <dim>{}</>\r\n", labels.join(", ")));
    }

    // Active effects on the item itself (rare today; surfaces if
    // any are applied via consume/quaff bindings later).
    let item_effects: Vec<String> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, a)| a.0 == item)
            .map(|(inst, _)| inst.name.clone())
            .collect()
    };
    if !item_effects.is_empty() {
        out.push_str(&format!(
            "\r\n  <cyan>Effects:</> <b:magenta>{}</>\r\n",
            item_effects.join(", ")
        ));
    }

    Some(out)
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

    crate::combat::relocate(world, player, foyer);
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
                "You can only place items inside your house. Try 'house enter' first.\r\n",
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
    let item = find_item(world, player, target_word, ItemClass::Inventory);
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
    // Captured before the item leaves the player's hands: the label and any
    // enchantment / curse are stored with the row.
    let custom = house_item_custom(world, item);
    let inventory_row_id = world.get::<mud_world::PersistedItemId>(item).map(|p| p.0);
    let character_id = world
        .get::<mud_world::Account>(player)
        .map(|a| a.character_id.clone());
    if world.get::<Located>(item).is_some() {
        world.entity_mut(item).insert(Located(room));
    }
    // From here the house row is the item's only persisted form: drop the
    // stale pack-row stamp, and track the row so a pickup (or `house take`)
    // deletes it even while the insert below is still in flight.
    let placement = mud_world::HousePlacement::pending();
    world
        .entity_mut(item)
        .remove::<mud_world::PersistedItemId>()
        .insert(placement.clone());
    send_rendered(
        world,
        player,
        &format!("You place {item_name} in your house.\r\n"),
    );
    if let Some(character_id) = character_id {
        crate::house_items::persist_placement(
            world,
            crate::house_items::PlacementWrite {
                character_id,
                room_row_id,
                object_zone_id: proto_key.zone,
                object_id: proto_key.id,
                custom,
                inventory_row_id,
                placement,
            },
        );
    }
}

/// `house take <item>` — pick up a placed item back into the
/// player's inventory. Requires the item to be tracked as a house item
/// (loaded from its row, or placed this session). Releasing it deletes
/// its row through `house_items::release_house_item`, the same path a
/// plain `get` takes.
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
    // Find a house item in this room matching the keyword.
    let matched: Option<(Entity, String)> = {
        let mut q = world.query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), (
            With<Item>,
            Or<(With<mud_world::HouseItem>, With<mud_world::HousePlacement>)>,
        )>();
        q.iter(world)
            .filter(|(_, l, _, _)| l.0 == room)
            .find(|(_, _, named, kw)| name_or_keyword_matches(target_word, &named.name, *kw))
            .map(|(e, _, n, _)| (e, n.name.clone()))
    };
    let Some((item, item_name)) = matched else {
        send_rendered(
            world,
            player,
            &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    if world.get::<Located>(item).is_some() {
        world.entity_mut(item).insert(Located(player));
        crate::hiding::set_hiddenness(world, item, 0);
    }
    // Strip the markers and delete the row (the `Located` observer already
    // did when it is registered; this keeps `take` correct without it).
    crate::house_items::release_house_item(world, item);
    send_rendered(
        world,
        player,
        &format!("You take {item_name} from the room.\r\n"),
    );
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
    let new_text = parts.next().unwrap_or("");
    let new_text = mud_net::sanitize_text(new_text, false);
    let new_text = new_text.trim();
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
                        let label = if is_description {
                            "description"
                        } else {
                            "name"
                        };
                        let _ = out.try_send(
                            format!("Updated room #{local_idx} {label}.\r\n").into_bytes(),
                        );
                    }
                    Err(e) => {
                        let _ = out.try_send(format!("DB write failed: {e}\r\n").into_bytes());
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
    let can_place =
        modifier.eq_ignore_ascii_case("place") || modifier.eq_ignore_ascii_case("can_place");
    let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
    match action {
        "add" => {
            tokio::spawn(async move {
                let row = match mud_db::characters::find_by_name(&pool, &name).await {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        if let Some(out) = outbound {
                            let _ = out
                                .try_send(format!("No character named '{name}'.\r\n").into_bytes());
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
                            let _ = out.try_send(
                                format!("{} added to your guest list{suffix}.\r\n", row.name)
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
                                .try_send(format!("No character named '{name}'.\r\n").into_bytes());
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
                            let _ = out.try_send(
                                format!("{} wasn't on your guest list.\r\n", row.name).into_bytes(),
                            );
                        }
                    }
                    Ok(_) => {
                        if let Some(out) = outbound {
                            let _ = out.try_send(
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
                format!("Unknown guest action '{other}'. Use 'add' or 'remove'.\r\n"),
            );
        }
    }
}

/// Kind-filtered listing for `skills` / `songs` / `chants`. Walks
/// the ability catalog like `cmd_spells` but restricts to a single
/// `AbilityKind`. Honors `KnownAbilities` gating and the optional
/// substring filter (passed as `args`).
/// Schema stores proficiency 0..=1000+; surface it as a 0..=100 percent
/// for readouts. Used by `practice`, `skills`, etc. so they all bucket
/// the same way.
#[must_use]
pub(crate) fn proficiency_percent(raw: i32) -> i32 {
    (raw / 10).clamp(0, 100)
}

/// Legacy-style tier label for a 0..=100 proficiency percent. Shared
/// between `practice` (which lists with caps + kind columns) and the
/// kind-filtered `skills` / `songs` / `chants` listings.
#[must_use]
pub(crate) fn proficiency_tier_label(pct: i32) -> &'static str {
    match pct {
        0 => "untrained",
        1..=25 => "novice",
        26..=50 => "apprentice",
        51..=75 => "skilled",
        76..=99 => "expert",
        _ => "master",
    }
}

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
    // Proficiency map drives the per-entry suffix in the known-list
    // view. Empty for `show_all` (no per-player data in the catalog
    // dump). Keyed by ability_id.
    let prof_by_id: std::collections::HashMap<i32, i32> = if show_all {
        std::collections::HashMap::new()
    } else {
        world
            .get::<KnownAbilities>(player)
            .map_or_else(std::collections::HashMap::new, |k| {
                k.entries.iter().map(|(id, p, _)| (*id, *p)).collect()
            })
    };
    let known: Option<std::collections::HashSet<i32>> = if show_all {
        None
    } else {
        Some(prof_by_id.keys().copied().collect())
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
        let base = format_ability_with_sphere(def);
        let entry = if show_all {
            base
        } else {
            // Append proficiency for the known-list view. `<dim>` so
            // the percent + tier sit visually behind the spell name;
            // matches the sphere parenthetical style.
            let raw_prof = prof_by_id.get(&def.id).copied().unwrap_or(0);
            let pct = proficiency_percent(raw_prof);
            let tier = proficiency_tier_label(pct);
            format!("{base} <dim>{pct}% ({tier})</>")
        };
        names.push(entry);
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
                     Type '{kind_label}s all' to browse the catalog.\r\n"
                ),
            );
        } else {
            send_rendered(
                world,
                player,
                &format!(
                    "\r\nNo {kind_label}s matching '{filter}' in your known list. \
                     Try '{kind_label}s all {filter}'.\r\n"
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
    let width = crate::layout::wrap_width(world, player);
    let column_width = name_column_width(std::iter::once(names.as_slice()));
    for row in crate::layout::format_grid(&names, column_width, 2, width) {
        out.push_str(&render_color_tags(&row, mode));
        out.push_str("\r\n");
    }
    send_to(world, player, out);
}

#[cfg(test)]
mod tests {
    use super::{
        carried_weight, cmd_drop, cmd_get, cmd_give, cmd_look, cmd_put, cmd_sell, has_object_flag,
        has_restriction, item_drop_blocked, parse_who_level_filter,
    };
    use bevy_ecs::prelude::*;
    use mud_db::enums::{ObjectFlag, ObjectRestriction, Sector};
    use mud_world::{
        Corpse, Item, Keywords, Located, Mob, Named, ObjectFlags, ObjectPrototypes,
        ObjectRestrictions, Player, Room, RoomSector, ShopCatalog, Shopkeeper, WorldKey,
    };

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

    // ---------------------------------------------------------------
    // Object flag / restriction wiring — wave 2.B
    //
    // These tests construct minimal worlds and exercise the gates
    // we wired through cmd_drop / cmd_get / cmd_sell. Each test
    // proves the gate fires (item stays where it was) AND the
    // unflagged control case lets the action complete.
    // ---------------------------------------------------------------

    /// Spawn a Room + Player + Item-in-inventory minimal setup.
    /// Returns `(world, room, player, item)`. The item starts in the
    /// player's inventory (`Located(player)`). Tests then attach
    /// flag / restriction components as needed.
    fn make_inventory_world() -> (World, Entity, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(ObjectPrototypes::default());
        let room = world.spawn((Room, RoomSector(Sector::Field))).id();
        let player = world
            .spawn((
                Player,
                Named {
                    name: "Strider".to_string(),
                },
                Located(room),
            ))
            .id();
        let item = world
            .spawn((
                Item,
                Named {
                    name: "a sword".to_string(),
                },
                Keywords(vec!["sword".to_string()]),
                Located(player),
            ))
            .id();
        (world, room, player, item)
    }

    #[test]
    fn no_drop_restriction_blocks_drop() {
        // NO_DROP gate refuses to let the item touch the floor. Item
        // stays Located on the player.
        let (mut world, room, player, item) = make_inventory_world();
        if let Ok(mut e) = world.get_entity_mut(item) {
            e.insert(ObjectRestrictions(vec![ObjectRestriction::NoDrop]));
        }
        cmd_drop(&mut world, player, "sword");
        let located = world.get::<Located>(item).expect("item missing Located");
        assert_eq!(
            located.0, player,
            "NO_DROP item should still be on the player after drop attempt"
        );
        let _ = room;
    }

    #[test]
    fn soulbound_flag_blocks_drop() {
        // SOULBOUND has the same "won't leave you" semantic.
        let (mut world, _room, player, item) = make_inventory_world();
        if let Ok(mut e) = world.get_entity_mut(item) {
            e.insert(ObjectFlags(vec![ObjectFlag::Soulbound]));
        }
        cmd_drop(&mut world, player, "sword");
        let located = world.get::<Located>(item).expect("item missing Located");
        assert_eq!(located.0, player, "SOULBOUND item should not drop");
    }

    #[test]
    fn unflagged_item_drops_normally() {
        // Control: a vanilla item with no flags drops to the room.
        let (mut world, room, player, item) = make_inventory_world();
        cmd_drop(&mut world, player, "sword");
        let located = world.get::<Located>(item).expect("item missing Located");
        assert_eq!(
            located.0, room,
            "unflagged item should land in the room on drop"
        );
    }

    /// Spawn a Room + Player + Item-on-floor. Mirror of
    /// `make_inventory_world` but the item is in the room, not on
    /// the player — for `get` gate tests.
    fn make_floor_world() -> (World, Entity, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(ObjectPrototypes::default());
        let room = world.spawn((Room, RoomSector(Sector::Field))).id();
        let player = world
            .spawn((
                Player,
                Named {
                    name: "Strider".to_string(),
                },
                Located(room),
            ))
            .id();
        let item = world
            .spawn((
                Item,
                Named {
                    name: "an anvil".to_string(),
                },
                Keywords(vec!["anvil".to_string()]),
                Located(room),
            ))
            .id();
        (world, room, player, item)
    }

    #[test]
    fn no_take_restriction_blocks_get() {
        // NO_TAKE fixture stays in the room; the gate fires before
        // the carry-weight check so a heavy fixture reads as
        // "fixed in place" rather than "too heavy".
        let (mut world, room, player, item) = make_floor_world();
        if let Ok(mut e) = world.get_entity_mut(item) {
            e.insert(ObjectRestrictions(vec![ObjectRestriction::NoTake]));
        }
        cmd_get(&mut world, player, "anvil");
        let located = world.get::<Located>(item).expect("item missing Located");
        assert_eq!(located.0, room, "NO_TAKE item should remain on the floor");
    }

    #[test]
    fn unrestricted_floor_item_can_be_picked_up() {
        // Control: a vanilla floor item moves to the player.
        let (mut world, _room, player, item) = make_floor_world();
        cmd_get(&mut world, player, "anvil");
        let located = world.get::<Located>(item).expect("item missing Located");
        assert_eq!(
            located.0, player,
            "unrestricted item should move to player inventory"
        );
    }

    #[test]
    fn no_sell_restriction_blocks_sell_in_shop() {
        // NO_SELL refuses the trade. Item stays in inventory (not
        // despawned) and player gains no wealth. We spawn a
        // shopkeeper in the room so cmd_sell finds a buyer; the
        // ShopCatalog is empty (no actual shop) but cmd_sell checks
        // the NO_SELL gate BEFORE the catalog lookup, so the test
        // exercises only the flag path.
        let mut world = World::new();
        world.insert_resource(ObjectPrototypes::default());
        world.insert_resource(ShopCatalog::default());
        let room = world.spawn((Room, RoomSector(Sector::Field))).id();
        let player = world
            .spawn((
                Player,
                Named {
                    name: "Strider".to_string(),
                },
                Located(room),
            ))
            .id();
        let _shopkeeper = world
            .spawn((
                Mob,
                Named {
                    name: "a shopkeeper".to_string(),
                },
                Located(room),
                Shopkeeper {
                    shop_zone_id: 1,
                    shop_id: 1,
                },
            ))
            .id();
        let item = world
            .spawn((
                Item,
                Named {
                    name: "a quest token".to_string(),
                },
                Keywords(vec!["token".to_string()]),
                Located(player),
                ObjectRestrictions(vec![ObjectRestriction::NoSell]),
            ))
            .id();
        cmd_sell(&mut world, player, "token");
        // Item still exists and is on the player.
        assert!(
            world.get_entity(item).is_ok(),
            "NO_SELL item should not be despawned"
        );
        let located = world.get::<Located>(item).expect("item missing Located");
        assert_eq!(located.0, player, "NO_SELL item should remain in inventory");
    }

    #[test]
    fn has_object_flag_returns_false_when_component_absent() {
        // Helper-level smoke check: items without an ObjectFlags
        // component should always read as "no flag" so handlers
        // that gate on a flag never erroneously fire.
        let mut world = World::new();
        let entity = world.spawn(()).id();
        assert!(!has_object_flag(&world, entity, ObjectFlag::Glow));
        assert!(!has_restriction(&world, entity, ObjectRestriction::NoDrop));
        assert!(!item_drop_blocked(&world, entity));
    }

    #[test]
    fn item_drop_blocked_covers_both_kinds() {
        // NO_DROP alone, SOULBOUND alone, both together → all
        // block. Vanilla → false. Sanity that the unified gate
        // catches every sticky path used by `drop all`.
        let mut world = World::new();
        let nothing = world.spawn(()).id();
        let no_drop = world
            .spawn(ObjectRestrictions(vec![ObjectRestriction::NoDrop]))
            .id();
        let bound = world.spawn(ObjectFlags(vec![ObjectFlag::Soulbound])).id();
        let both = world
            .spawn((
                ObjectFlags(vec![ObjectFlag::Soulbound]),
                ObjectRestrictions(vec![ObjectRestriction::NoDrop]),
            ))
            .id();
        assert!(!item_drop_blocked(&world, nothing));
        assert!(item_drop_blocked(&world, no_drop));
        assert!(item_drop_blocked(&world, bound));
        assert!(item_drop_blocked(&world, both));
    }

    // ---------------------------------------------------------------
    // Optional prepositions / bulk filters in object commands.
    // ---------------------------------------------------------------

    fn spawn_item(world: &mut World, name: &str, kw: &str, loc: Entity) -> Entity {
        world
            .spawn((
                Item,
                Named {
                    name: name.to_string(),
                },
                Keywords(vec![kw.to_string()]),
                Located(loc),
            ))
            .id()
    }

    fn spawn_corpse(world: &mut World, room: Entity) -> Entity {
        world
            .spawn((
                Item,
                Corpse,
                Named {
                    name: "the corpse of a goblin".to_string(),
                },
                Keywords(vec!["corpse".to_string()]),
                Located(room),
            ))
            .id()
    }

    #[test]
    fn split_preposition_edge_cases() {
        use crate::commands::split_preposition;
        let preps = ["from", "in"];
        assert_eq!(
            split_preposition("sword from chest", &preps),
            Some(("sword", "chest"))
        );
        // Case-insensitive.
        assert_eq!(
            split_preposition("all.coin FROM the Corpse", &preps),
            Some(("all.coin", "the Corpse"))
        );
        // Leading / trailing prepositions are not separators.
        assert_eq!(split_preposition("from corpse", &preps), None);
        assert_eq!(split_preposition("sword from", &preps), None);
        assert_eq!(split_preposition("sword from  ", &preps), None);
        // Only whole tokens count.
        assert_eq!(split_preposition("window inn", &preps), None);
        // First occurrence wins.
        assert_eq!(
            split_preposition("a in b from c", &preps),
            Some(("a", "b from c"))
        );
        assert_eq!(split_preposition("", &preps), None);
        assert_eq!(split_preposition("sword bob", &["to"]), None);
        assert_eq!(
            split_preposition("2.sword  to   bob", &["to"]),
            Some(("2.sword", "bob"))
        );
    }

    #[test]
    fn get_all_corpse_without_from() {
        let (mut world, room, player, anvil) = make_floor_world();
        let corpse = spawn_corpse(&mut world, room);
        let a = spawn_item(&mut world, "a rusty sword", "sword", corpse);
        let b = spawn_item(&mut world, "a gold coin", "coin", corpse);
        cmd_get(&mut world, player, "all corpse");
        assert_eq!(world.get::<Located>(a).unwrap().0, player);
        assert_eq!(world.get::<Located>(b).unwrap().0, player);
        assert_eq!(world.get::<Located>(anvil).unwrap().0, room);
    }

    #[test]
    fn get_single_item_from_corpse_without_from() {
        let (mut world, room, player, _anvil) = make_floor_world();
        let corpse = spawn_corpse(&mut world, room);
        let a = spawn_item(&mut world, "a rusty sword", "sword", corpse);
        let b = spawn_item(&mut world, "a gold coin", "coin", corpse);
        cmd_get(&mut world, player, "sword corpse");
        assert_eq!(world.get::<Located>(a).unwrap().0, player);
        assert_eq!(world.get::<Located>(b).unwrap().0, corpse);
    }

    #[test]
    fn get_all_filter_from_corpse() {
        let (mut world, room, player, _anvil) = make_floor_world();
        let corpse = spawn_corpse(&mut world, room);
        let a = spawn_item(&mut world, "a rusty sword", "sword", corpse);
        let b = spawn_item(&mut world, "a gold coin", "coin", corpse);
        let c = spawn_item(&mut world, "a silver coin", "coin", corpse);
        cmd_get(&mut world, player, "all.coin from corpse");
        assert_eq!(world.get::<Located>(a).unwrap().0, corpse);
        assert_eq!(world.get::<Located>(b).unwrap().0, player);
        assert_eq!(world.get::<Located>(c).unwrap().0, player);
        // Prepositionless form takes the filter too.
        let d = spawn_item(&mut world, "a bronze coin", "coin", corpse);
        cmd_get(&mut world, player, "all.sword corpse");
        assert_eq!(world.get::<Located>(a).unwrap().0, player);
        assert_eq!(world.get::<Located>(d).unwrap().0, corpse);
    }

    #[test]
    fn get_substring_of_keyword_does_not_match_but_prefix_does() {
        let (mut world, room, player, _anvil) = make_floor_world();
        let sign = spawn_item(&mut world, "a wooden sign", "sign", room);
        let assign = spawn_item(&mut world, "an assignment", "assignment", room);
        // `ign` is a substring of both keywords, a prefix of neither.
        cmd_get(&mut world, player, "ign");
        assert_eq!(world.get::<Located>(sign).unwrap().0, room);
        assert_eq!(world.get::<Located>(assign).unwrap().0, room);
        // `sign` is a prefix of "sign" only, never "assignment".
        cmd_get(&mut world, player, "sign");
        assert_eq!(world.get::<Located>(sign).unwrap().0, player);
        assert_eq!(world.get::<Located>(assign).unwrap().0, room);
        cmd_get(&mut world, player, "assi");
        assert_eq!(world.get::<Located>(assign).unwrap().0, player);
    }

    #[test]
    fn get_ordinal_and_all_dot_still_work_with_prefix_matching() {
        let (mut world, room, player, _anvil) = make_floor_world();
        let a = spawn_item(&mut world, "a sword", "sword", room);
        let b = spawn_item(&mut world, "another sword", "sword", room);
        let c = spawn_item(&mut world, "a third sword", "sword", room);
        cmd_get(&mut world, player, "2.swo");
        assert_eq!(world.get::<Located>(b).unwrap().0, player);
        assert_eq!(world.get::<Located>(a).unwrap().0, room);
        cmd_get(&mut world, player, "all.sw");
        assert_eq!(world.get::<Located>(a).unwrap().0, player);
        assert_eq!(world.get::<Located>(c).unwrap().0, player);
    }

    #[test]
    fn get_two_word_floor_item_is_not_treated_as_container() {
        // `get rusty sword` — the last word ("sword") resolves to a
        // floor item that is not a container, so the plain floor
        // path must win.
        let (mut world, room, player, _anvil) = make_floor_world();
        let sword = spawn_item(&mut world, "a rusty sword", "rusty sword", room);
        cmd_get(&mut world, player, "rusty sword");
        assert_eq!(world.get::<Located>(sword).unwrap().0, player);
    }

    #[test]
    fn get_two_word_container_name_still_picks_up_container() {
        // `get big corpse`: a floor item matches the whole string, so
        // it is picked up rather than reinterpreted as a container get.
        let (mut world, room, player, _anvil) = make_floor_world();
        let corpse = world
            .spawn((
                Item,
                Corpse,
                Named {
                    name: "a big corpse".to_string(),
                },
                Keywords(vec!["big".to_string(), "corpse".to_string()]),
                Located(room),
            ))
            .id();
        let inner = spawn_item(&mut world, "a coin", "coin", corpse);
        cmd_get(&mut world, player, "big corpse");
        assert_eq!(world.get::<Located>(corpse).unwrap().0, player);
        assert_eq!(world.get::<Located>(inner).unwrap().0, corpse);
    }

    fn make_give_world() -> (World, Entity, Entity, Entity, Entity) {
        let (mut world, room, player, sword) = make_inventory_world();
        let bob = world
            .spawn((
                Mob,
                Named {
                    name: "Bob".to_string(),
                },
                Keywords(vec!["bob".to_string()]),
                Located(room),
            ))
            .id();
        (world, room, player, sword, bob)
    }

    #[test]
    fn give_with_and_without_to() {
        let (mut world, _room, player, sword, bob) = make_give_world();
        cmd_give(&mut world, player, "sword to bob");
        assert_eq!(world.get::<Located>(sword).unwrap().0, bob);
        world.entity_mut(sword).insert(Located(player));
        cmd_give(&mut world, player, "sword bob");
        assert_eq!(world.get::<Located>(sword).unwrap().0, bob);
    }

    #[test]
    fn give_indexed_item_with_to() {
        // `second` arrives last, so it is listed first and `2.sword` is
        // the original.
        let (mut world, _room, player, first, bob) = make_give_world();
        let second = spawn_item(&mut world, "a sword", "sword", player);
        cmd_give(&mut world, player, "2.sword to bob");
        assert_eq!(world.get::<Located>(second).unwrap().0, player);
        assert_eq!(world.get::<Located>(first).unwrap().0, bob);
    }

    fn make_bag_world() -> (World, Entity, Entity, Entity) {
        let (mut world, room, player, _sword) = make_inventory_world();
        let bag = world
            .spawn((
                Item,
                Named {
                    name: "a leather bag".to_string(),
                },
                Keywords(vec!["bag".to_string()]),
                Located(room),
            ))
            .id();
        (world, room, player, bag)
    }

    #[test]
    fn put_all_and_prepositions() {
        let (mut world, _room, player, bag) = make_bag_world();
        let apple = spawn_item(&mut world, "an apple", "apple", player);
        cmd_put(&mut world, player, "all in bag");
        assert_eq!(world.get::<Located>(apple).unwrap().0, bag);
        // Container itself never moves into itself.
        assert_eq!(
            world.get::<Located>(bag).unwrap().0,
            world.get::<Located>(player).unwrap().0
        );
        // `into` and `on` are accepted.
        world.entity_mut(apple).insert(Located(player));
        cmd_put(&mut world, player, "apple into bag");
        assert_eq!(world.get::<Located>(apple).unwrap().0, bag);
        world.entity_mut(apple).insert(Located(player));
        cmd_put(&mut world, player, "apple on bag");
        assert_eq!(world.get::<Located>(apple).unwrap().0, bag);
    }

    #[test]
    fn put_all_filter() {
        let (mut world, _room, player, bag) = make_bag_world();
        let apple = spawn_item(&mut world, "an apple", "apple", player);
        let pear = spawn_item(&mut world, "a pear", "pear", player);
        cmd_put(&mut world, player, "all.apple in bag");
        assert_eq!(world.get::<Located>(apple).unwrap().0, bag);
        assert_eq!(world.get::<Located>(pear).unwrap().0, player);
    }

    #[test]
    fn drop_all_filter() {
        let (mut world, room, player, sword) = make_inventory_world();
        let apple = spawn_item(&mut world, "an apple", "apple", player);
        cmd_drop(&mut world, player, "all.sword");
        assert_eq!(world.get::<Located>(sword).unwrap().0, room);
        assert_eq!(world.get::<Located>(apple).unwrap().0, player);
        // Same gates as `drop all`: NO_DROP items stay.
        let cursed = spawn_item(&mut world, "a cursed sword", "sword", player);
        world
            .entity_mut(cursed)
            .insert(ObjectRestrictions(vec![ObjectRestriction::NoDrop]));
        cmd_drop(&mut world, player, "all.sword");
        assert_eq!(world.get::<Located>(cursed).unwrap().0, player);
    }

    #[test]
    fn look_at_matches_plain_look() {
        use crate::commands::test_support::{drain, player_in};
        let mut world = World::new();
        world.insert_resource(ObjectPrototypes::default());
        let room = world.spawn((Room, RoomSector(Sector::City))).id();
        let (player, mut rx) = player_in(&mut world, room);
        spawn_item(&mut world, "a sword", "sword", room);
        cmd_look(&mut world, player, "sword");
        let plain = drain(&mut rx);
        cmd_look(&mut world, player, "at sword");
        let with_at = drain(&mut rx);
        assert!(!plain.is_empty());
        assert!(!plain.contains("don't see"), "plain look failed: {plain}");
        assert_eq!(plain, with_at);
    }

    #[test]
    fn get_coins_from_corpse_drains_pile_only() {
        let (mut world, room, player, _anvil) = make_floor_world();
        let corpse = spawn_corpse(&mut world, room);
        let sword = spawn_item(&mut world, "a rusty sword", "sword", corpse);
        world.entity_mut(corpse).insert(mud_world::CoinPile(120));
        cmd_get(&mut world, player, "coins corpse");
        assert!(world.get::<mud_world::CoinPile>(corpse).is_none());
        assert_eq!(world.get::<mud_world::Wealth>(player).unwrap().0, 120);
        assert_eq!(world.get::<Located>(sword).unwrap().0, corpse);
    }

    #[test]
    fn get_all_coin_from_corpse_drains_pile_and_matching_items() {
        let (mut world, room, player, _anvil) = make_floor_world();
        let corpse = spawn_corpse(&mut world, room);
        let sword = spawn_item(&mut world, "a rusty sword", "sword", corpse);
        let coin = spawn_item(&mut world, "a rare coin", "coin", corpse);
        world.entity_mut(corpse).insert(mud_world::CoinPile(75));
        cmd_get(&mut world, player, "all.coin from corpse");
        assert!(world.get::<mud_world::CoinPile>(corpse).is_none());
        assert_eq!(world.get::<mud_world::Wealth>(player).unwrap().0, 75);
        assert_eq!(world.get::<Located>(coin).unwrap().0, player);
        assert_eq!(world.get::<Located>(sword).unwrap().0, corpse);
    }

    #[test]
    fn get_coins_picks_up_a_loose_floor_pile_into_wealth() {
        let (mut world, room, player, _anvil) = make_floor_world();
        let pile = crate::item_decay::spawn_loose_coin_pile(&mut world, room, 130);
        cmd_get(&mut world, player, "coins");
        assert!(world.get_entity(pile).is_err(), "pile consumed");
        assert_eq!(world.get::<mud_world::Wealth>(player).unwrap().0, 130);
    }

    #[test]
    fn get_all_sweeps_a_loose_floor_pile_into_wealth() {
        let (mut world, room, player, _anvil) = make_floor_world();
        let pile = crate::item_decay::spawn_loose_coin_pile(&mut world, room, 55);
        cmd_get(&mut world, player, "all");
        assert!(world.get_entity(pile).is_err());
        assert_eq!(world.get::<mud_world::Wealth>(player).unwrap().0, 55);
    }

    fn count_at(world: &mut World, loc: Entity) -> usize {
        let mut q = world.query_filtered::<&Located, With<Item>>();
        q.iter(world).filter(|l| l.0 == loc).count()
    }

    #[test]
    fn get_count_takes_first_n_floor_items() {
        let (mut world, room, player, _anvil) = make_floor_world();
        for _ in 0..3 {
            spawn_item(&mut world, "a sword", "sword", room);
        }
        cmd_get(&mut world, player, "2 sword");
        assert_eq!(count_at(&mut world, player), 2);
        // anvil + the one remaining sword.
        assert_eq!(count_at(&mut world, room), 2);
    }

    #[test]
    fn get_count_larger_than_available_takes_all_matching() {
        let (mut world, room, player, _anvil) = make_floor_world();
        for _ in 0..2 {
            spawn_item(&mut world, "a sword", "sword", room);
        }
        cmd_get(&mut world, player, "5 sword");
        assert_eq!(count_at(&mut world, player), 2);
        assert_eq!(count_at(&mut world, room), 1);
    }

    #[test]
    fn get_count_from_container_with_and_without_from() {
        let (mut world, room, player, _anvil) = make_floor_world();
        let corpse = spawn_corpse(&mut world, room);
        for _ in 0..3 {
            spawn_item(&mut world, "a sword", "sword", corpse);
        }
        cmd_get(&mut world, player, "2 sword corpse");
        assert_eq!(count_at(&mut world, player), 2);
        assert_eq!(count_at(&mut world, corpse), 1);
        cmd_get(&mut world, player, "2 sword from corpse");
        assert_eq!(count_at(&mut world, player), 3);
        assert_eq!(count_at(&mut world, corpse), 0);
    }

    #[test]
    fn get_index_syntax_still_takes_only_the_nth() {
        let (mut world, room, player, _anvil) = make_floor_world();
        // Newest arrival is listed first, so `2.sword` is the older one.
        let second = spawn_item(&mut world, "a sword", "sword", room);
        let first = spawn_item(&mut world, "a sword", "sword", room);
        cmd_get(&mut world, player, "2.sword");
        assert_eq!(count_at(&mut world, player), 1);
        assert_eq!(world.get::<Located>(second).unwrap().0, player);
        assert_eq!(world.get::<Located>(first).unwrap().0, room);
    }

    /// Build a player (capacity 105 at level 1) carrying a bag that holds
    /// one heavy item, with real prototype weights.
    fn heavy_item_in_carried_bag(reduction: f64) -> (World, Entity, Entity, Entity) {
        let mut world = World::new();
        let mut protos = ObjectPrototypes::default();
        let mut bag_proto =
            crate::commands::test_support::object_proto(1, 1, mud_db::enums::ObjectType::Container);
        bag_proto.weight = 5.0;
        bag_proto.weight_reduction = reduction;
        let mut rock_proto =
            crate::commands::test_support::object_proto(1, 2, mud_db::enums::ObjectType::Other);
        rock_proto.weight = 90.0;
        protos.by_key.insert((1, 1), bag_proto);
        protos.by_key.insert((1, 2), rock_proto);
        world.insert_resource(protos);
        let room = world.spawn((Room, RoomSector(Sector::Field))).id();
        let player = world
            .spawn((
                Player,
                Named {
                    name: "Strider".to_string(),
                },
                Located(room),
            ))
            .id();
        let bag = world
            .spawn((
                Item,
                WorldKey { zone: 1, id: 1 },
                Named {
                    name: "a leather bag".to_string(),
                },
                Keywords(vec!["bag".to_string()]),
                Located(player),
            ))
            .id();
        let rock = world
            .spawn((
                Item,
                WorldKey { zone: 1, id: 2 },
                Named {
                    name: "a boulder".to_string(),
                },
                Keywords(vec!["boulder".to_string()]),
                Located(bag),
            ))
            .id();
        (world, player, bag, rock)
    }

    #[test]
    fn get_from_carried_bag_does_not_double_count_weight() {
        // Carried: bag 5 + boulder 90 = 95 of 105. Taking the boulder out
        // of the bag leaves the total at 95; the old check tested
        // 95 + 90 = 185 and refused.
        let (mut world, player, _bag, rock) = heavy_item_in_carried_bag(0.0);
        cmd_get(&mut world, player, "boulder from bag");
        assert_eq!(world.get::<Located>(rock).unwrap().0, player);
        assert!((carried_weight(&mut world, player) - 95.0).abs() < 1e-9);
    }

    #[test]
    fn get_all_from_carried_bag_does_not_double_count_weight() {
        let (mut world, player, _bag, rock) = heavy_item_in_carried_bag(0.0);
        cmd_get(&mut world, player, "all from bag");
        assert_eq!(world.get::<Located>(rock).unwrap().0, player);
    }

    #[test]
    fn get_from_carried_bag_with_reduction_nets_out_the_discount() {
        // 50% bag: boulder counted as 45 inside, 90 outside. Carried is
        // 5 + 45 = 50; after the move 95. Still under the cap.
        let (mut world, player, _bag, rock) = heavy_item_in_carried_bag(50.0);
        cmd_get(&mut world, player, "boulder from bag");
        assert_eq!(world.get::<Located>(rock).unwrap().0, player);
        assert!((carried_weight(&mut world, player) - 95.0).abs() < 1e-9);
    }

    #[test]
    fn get_from_floor_bag_still_enforces_the_cap() {
        let (mut world, player, bag, rock) = heavy_item_in_carried_bag(0.0);
        let room = world.get::<Located>(player).unwrap().0;
        world.entity_mut(bag).insert(Located(room));
        // Carrying a 20 lb pack makes 20 + 90 > 105.
        let mut p = world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(1, 1))
            .unwrap()
            .clone();
        p.weight = 20.0;
        world
            .resource_mut::<ObjectPrototypes>()
            .by_key
            .insert((1, 3), p);
        world.spawn((Item, WorldKey { zone: 1, id: 3 }, Located(player)));
        cmd_get(&mut world, player, "boulder from bag");
        assert_eq!(world.get::<Located>(rock).unwrap().0, bag);
    }

    #[test]
    fn get_count_with_all_ignores_count() {
        let (mut world, room, player, _anvil) = make_floor_world();
        for _ in 0..3 {
            spawn_item(&mut world, "a sword", "sword", room);
        }
        cmd_get(&mut world, player, "2 all.sword");
        assert_eq!(count_at(&mut world, player), 3);
    }

    #[test]
    fn drop_count_drops_n_matching_items() {
        let (mut world, room, player, _sword) = make_inventory_world();
        spawn_item(&mut world, "a sword", "sword", player);
        spawn_item(&mut world, "a sword", "sword", player);
        cmd_drop(&mut world, player, "2 sword");
        assert_eq!(count_at(&mut world, room), 2);
        assert_eq!(count_at(&mut world, player), 1);
    }

    #[test]
    fn put_count_moves_n_items() {
        let (mut world, _room, player, bag) = make_bag_world();
        spawn_item(&mut world, "an apple", "apple", player);
        spawn_item(&mut world, "an apple", "apple", player);
        spawn_item(&mut world, "an apple", "apple", player);
        cmd_put(&mut world, player, "2 apple in bag");
        assert_eq!(count_at(&mut world, bag), 2);
    }

    #[test]
    fn give_count_gives_n_items() {
        let (mut world, _room, player, _sword, bob) = make_give_world();
        spawn_item(&mut world, "a sword", "sword", player);
        spawn_item(&mut world, "a sword", "sword", player);
        cmd_give(&mut world, player, "2 sword to bob");
        assert_eq!(count_at(&mut world, bob), 2);
        assert_eq!(count_at(&mut world, player), 1);
    }
}

#[cfg(test)]
mod god_eat_tests {
    use super::cmd_eat;
    use crate::commands::test_support::{self, drain};
    use bevy_ecs::prelude::*;
    use mud_db::enums::ObjectType;
    use mud_world::{Item, Keywords, Located, Named, ObjectPrototypes, Profile, WorldKey};

    fn setup(level: i32) -> (World, Entity, test_support::Rx, test_support::Rx) {
        let mut world = World::new();
        let mut protos = ObjectPrototypes::default();
        protos.by_key.insert(
            (1, 1),
            crate::commands::test_support::object_proto(1, 1, ObjectType::Weapon),
        );
        protos.by_key.insert(
            (1, 2),
            crate::commands::test_support::object_proto(1, 2, ObjectType::Container),
        );
        protos
            .by_key
            .insert((1, 3), test_support::object_proto(1, 3, ObjectType::Other));
        world.insert_resource(protos);
        let room = world.spawn_empty().id();
        let (actor, rx) = test_support::player_in(&mut world, room);
        let (_watcher, watcher_rx) = test_support::player_in(&mut world, room);
        world.entity_mut(actor).insert(Profile {
            level,
            class_id: None,
            race: "HUMAN".into(),
            experience: 0,
            gender: "male".into(),
        });
        (world, actor, rx, watcher_rx)
    }

    fn item(world: &mut World, id: i32, name: &str, kw: &str, parent: Entity) -> Entity {
        world
            .spawn((
                Item,
                Named { name: name.into() },
                Keywords(vec![kw.into()]),
                WorldKey { zone: 1, id },
                Located(parent),
            ))
            .id()
    }

    #[test]
    fn god_eats_a_sword_and_the_room_sees_it() {
        let (mut world, god, mut rx, mut watcher) = setup(105);
        let sword = item(&mut world, 1, "a rusty sword", "sword", god);
        cmd_eat(&mut world, god, "sword");
        assert!(world.get_entity(sword).is_err(), "sword destroyed");
        let out = drain(&mut rx);
        assert!(out.contains("You eat a rusty sword."), "{out}");
        let seen = drain(&mut watcher);
        assert!(seen.contains("Tester eats a rusty sword."), "{seen}");
    }

    #[test]
    fn god_eating_a_bag_destroys_its_contents_too() {
        let (mut world, god, _rx, _w) = setup(100);
        let bag = item(&mut world, 2, "a leather bag", "bag", god);
        let inner = item(&mut world, 1, "a rusty sword", "sword", bag);
        let nested = item(&mut world, 2, "a pouch", "pouch", bag);
        let deep = item(&mut world, 3, "a pebble", "pebble", nested);
        cmd_eat(&mut world, god, "bag");
        for e in [bag, inner, nested, deep] {
            assert!(world.get_entity(e).is_err(), "{e:?} should be gone");
        }
    }

    #[test]
    fn mortal_cannot_eat_a_sword() {
        let (mut world, mortal, mut rx, mut watcher) = setup(99);
        let sword = item(&mut world, 1, "a rusty sword", "sword", mortal);
        cmd_eat(&mut world, mortal, "sword");
        assert!(world.get_entity(sword).is_ok(), "sword kept");
        let out = drain(&mut rx);
        assert!(out.contains("You can't eat a rusty sword."), "{out}");
        assert!(drain(&mut watcher).is_empty());
    }
}

#[cfg(test)]
mod prompt_template_tests {
    use super::{cmd_prompt, prompt_templates};
    use crate::commands::test_support::{self, drain};
    use bevy_ecs::prelude::*;
    use mud_world::{ConfigValue, Prompt, RuntimeConfig};

    fn config(json: &str) -> RuntimeConfig {
        let mut c = RuntimeConfig::default();
        c.by_key.insert(
            ("display".into(), "prompt_templates".into()),
            ConfigValue::Json(json.into()),
        );
        c
    }

    fn setup() -> (World, Entity, test_support::Rx) {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (player, rx) = test_support::player_in(&mut world, room);
        (world, player, rx)
    }

    #[test]
    fn templates_come_from_the_game_config_row_in_order() {
        let mut world = World::new();
        world.insert_resource(config(r#"[["classic","<%h/%H> "],["minimal","> "]]"#));
        assert_eq!(
            prompt_templates(&world),
            vec![
                ("classic".to_string(), "<%h/%H> ".to_string()),
                ("minimal".to_string(), "> ".to_string()),
            ]
        );
    }

    #[test]
    fn prompt_list_and_pick_use_the_row() {
        let (mut world, player, mut rx) = setup();
        world.insert_resource(config(r#"[["classic","<%h/%H hp> "],["minimal","> "]]"#));
        cmd_prompt(&mut world, player, "list");
        let out = drain(&mut rx);
        assert!(
            out.contains("classic") && out.contains("minimal"),
            "{out:?}"
        );
        cmd_prompt(&mut world, player, "MINIMAL");
        assert_eq!(world.get::<Prompt>(player).unwrap().0, "> ");
        assert!(drain(&mut rx).contains("Prompt set to"));
    }

    #[test]
    fn missing_or_malformed_row_means_no_templates() {
        let mut world = World::new();
        assert!(prompt_templates(&world).is_empty(), "no RuntimeConfig");
        world.insert_resource(RuntimeConfig::default());
        assert!(prompt_templates(&world).is_empty(), "no row");
        world.insert_resource(config("not json"));
        assert!(prompt_templates(&world).is_empty(), "malformed row");
    }
}
