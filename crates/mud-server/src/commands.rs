//! Player command system: registry-driven, role/permission gated.
//!
//! Every command is a `Command` value in the `COMMANDS` slice. Adding a new
//! command means appending one entry — names, role, perm, category, help, and
//! a handler `fn(&mut World, Entity, &str)`. Help is a required field; the
//! registry's first-touch initialization asserts on empty `help.summary` and
//! on duplicate names so contract violations surface at server startup, not
//! when a player tries to use the command.

use std::collections::HashMap;
use std::sync::LazyLock;

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, Permission, PlayerFlag, Sector, UserRole};
use mud_net::Outbound;
use mud_world::{
    AbilityCatalog, Account, AccountSummary, AppliedTo, AttachedTriggers, ClassCatalog,
    CombatStats, Cooldowns, CoreStats, Description, EffectCatalog, EffectInstance, EffectSource,
    EquippedSlot, ExitData, Exits, Fighting, Follower, Frozen, Health, IgnoreList, Item, Keywords,
    KnownAbilities, LastInputAt, LastTeller, Located, LoggedInAt, Mob, MobPrototypes, Named,
    ObjectPrototypes, Online, Player, PlayerFlags, Posture, PostureKind, Profile, Prompt,
    BankWealth, BoardCatalog, BoardDraft, BoardLink, MailDraft, RecallPoint, RoomSector,
    ShopCatalog, Shopkeeper, Slot, SocialDef, SocialRegistry, Stamina, Stealth, Stunned, TellLog,
    Title, TriggerCatalog, UiStyle, Wealth, WearableIn, WorldKey, WorldKeyIndex, ZoneClimate,
};
use tracing::{info, info_span};

use crate::{ServerStart, TickCount};

// ---------------------------------------------------------------------------
// Connection component (entity-attached outbound channel)
// ---------------------------------------------------------------------------

/// A network connection attached to an entity. Owning the Outbound here keeps
/// the channel alive for the entity's whole lifetime.
#[derive(Component)]
pub struct Connection(pub Outbound);

// ---------------------------------------------------------------------------
// Command contract
// ---------------------------------------------------------------------------

pub type CommandFn = fn(&mut World, Entity, &str);

#[derive(Debug, Clone, Copy)]
pub struct Command {
    /// First name is canonical; all others are aliases. Names with whitespace
    /// (e.g., "clan storage list") are matched longest-first by the dispatcher.
    pub names: &'static [&'static str],
    pub min_role: UserRole,
    pub required_perm: Option<Permission>,
    pub category: Category,
    pub help: Help,
    pub run: CommandFn,
}

#[derive(Debug, Clone, Copy)]
pub struct Help {
    pub usage: &'static str,
    pub summary: &'static str,
    pub long: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Info,
    Movement,
    Communication,
    Combat,
    Admin,
}

impl Category {
    /// Display order for `help` with no args.
    pub const ORDER: &'static [Self] = &[
        Self::Info,
        Self::Movement,
        Self::Communication,
        Self::Combat,
        Self::Admin,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "Information",
            Self::Movement => "Movement",
            Self::Communication => "Communication",
            Self::Combat => "Combat",
            Self::Admin => "Admin",
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

const MAX_NAME_TOKENS: usize = 3;

/// All built-in commands. Order doesn't affect dispatch (lookup is by name)
/// but does set fallback ordering for `help` listing within a category.
const COMMANDS: &[Command] = &[
    // ----- Info -----
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
    Command {
        names: &["balance", "bal"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "balance",
            summary: "Show your bank-stored coin.",
            long: "Read-only display of the `bank_wealth` column from \
                   your character row. `deposit` / `withdraw` land with \
                   the banker NPC component and shop economy.",
        },
        run: cmd_balance,
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
    Command {
        names: &["where"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "where",
            summary: "List all online players and their rooms.",
            long: "Builder+ command. Shows each player's name and the room \
                   they're currently in.",
        },
        run: cmd_where,
    },
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
    },
    Command {
        names: &["socials"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "socials",
            summary: "List every social emote command.",
            long: "Shows all loaded socials (smile, bow, hug, …) in a \
                   columnar grid. Type the social name directly to run it; \
                   most accept an optional target.",
        },
        run: cmd_socials,
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
    Command {
        names: &["bug"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "bug <description>",
            summary: "Report a bug to the staff.",
            long: "Writes the report to the server log tagged with your \
                   character name. Be specific — what you did, what you \
                   expected, what happened.",
        },
        run: cmd_bug,
    },
    Command {
        names: &["idea"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "idea <suggestion>",
            summary: "Suggest a new feature or improvement.",
            long: "Writes the suggestion to the server log tagged with \
                   your character name. No promises — but the staff reads \
                   them.",
        },
        run: cmd_idea,
    },
    Command {
        names: &["typo"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "typo <correction>",
            summary: "Report a typo or wording problem.",
            long: "Writes the correction to the server log tagged with \
                   your character name. Mention the room/object/mob if \
                   you can — it speeds up the fix.",
        },
        run: cmd_typo,
    },
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
                   HP, %v current stamina, %V max stamina, %% literal \
                   percent. Examples: \
                     prompt <%h/%H hp %v/%V mv> \
                     prompt [%h hp] ",
        },
        run: cmd_prompt,
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
    Command {
        names: &["release"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: Help {
            usage: "release",
            summary: "Leave your corpse and respawn (ghost-only).",
            long: "Used in legacy CircleMUD lineage to release a \
                   ghost from its corpse and return to the recall \
                   point. Today's death handler auto-revives in \
                   place, so there's no ghost state and nothing to \
                   release. Kept as a registered command name to \
                   provide a clear message; `recall` covers the \
                   manual return-to-base case.",
        },
        run: cmd_release,
    },
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
    },
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
    },
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
    },
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
    },
    // ----- Communication -----
    Command {
        names: &["say", "'"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "say <message>",
            summary: "Speak to everyone in your current room.",
            long: "All players in the same room see your message. Use ' as a \
                   shorthand: 'hello there.",
        },
        run: cmd_say,
    },
    Command {
        names: &["whisper"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "whisper <target> <message>",
            summary: "Speak privately to one person in the same room.",
            long: "The target hears the message verbatim; bystanders see \
                   only that you whispered something to them, not the \
                   contents. The target must be in the same room.",
        },
        run: cmd_whisper,
    },
    Command {
        names: &["report"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "report",
            summary: "Announce your HP/stamina to the room.",
            long: "Broadcasts a single line to everyone present: \
                   `You report: HP 50/100, stamina 7/50.` Useful for \
                   coordinating with healers / groupmates before the \
                   group system lands.",
        },
        run: cmd_report,
    },
    Command {
        names: &["tell", "t"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "tell <player> <message>",
            summary: "Send a private message to an online player.",
            long: "The target must be online. Match is case-insensitive and \
                   exact (no substring) to avoid ambiguity.",
        },
        run: cmd_tell,
    },
    Command {
        names: &["emote", ":"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "emote <action>",
            summary: "Perform a third-person action visible to the room.",
            long: "Your name is prepended to the text. \
                   `emote smiles broadly.` shows everyone (including you): \
                   `Strider smiles broadly.`",
        },
        run: cmd_emote,
    },
    Command {
        names: &["shout"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "shout <message>",
            summary: "Yell to every online player.",
            long: "Reaches across rooms and zones; everyone connected sees \
                   it. Use sparingly.",
        },
        run: cmd_shout,
    },
    Command {
        names: &["reply", "r"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "reply <message>",
            summary: "Reply to the last person who told you.",
            long: "Sends a tell to whoever last sent you a private message. \
                   If they've gone offline, you'll get a useful error.",
        },
        run: cmd_reply,
    },
    Command {
        names: &["ignore"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "ignore [<player> | -<player> | clear]",
            summary: "Block tells from a specific player.",
            long: "With no arg, lists ignored names. With a name, adds \
                   them to your ignore list. With `-name` (or `unignore \
                   name`), removes them. With `clear`, drops all. \
                   Session-scoped — list resets on disconnect.",
        },
        run: cmd_ignore,
    },
    Command {
        names: &["unignore"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "unignore <player>",
            summary: "Stop ignoring a player.",
            long: "Removes a name from your ignore list. Equivalent to \
                   `ignore -<name>`.",
        },
        run: cmd_unignore,
    },
    Command {
        names: &["lasttells", "lt"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "lasttells",
            summary: "Show recent senders of `tell` to you (up to 10).",
            long: "Newest first, with how long ago each was received. \
                   Tracks names at receipt time, so the list is stable \
                   even if a sender disconnects.",
        },
        run: cmd_lasttells,
    },
    Command {
        names: &["mail"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "mail <character>",
            summary: "Compose and send mail to another player.",
            long: "Mail is account-scoped — addressing a character \
                   delivers to whichever account owns them. After \
                   `mail <name>`, your input enters compose mode: \
                   first non-blank line is the subject, subsequent \
                   lines accumulate as body. Control verbs: `.send` \
                   ships the draft, `.abort` discards it, `.preview` \
                   shows what's queued, `.clear` wipes the draft.",
        },
        run: cmd_mail_stub,
    },
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
    },
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
    },
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
                   their final status. The Quest table is empty in the \
                   current world; this verb is wired so it'll surface \
                   content the moment builders add it.",
        },
        run: cmd_mail_stub,
    },
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
    },
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
                   proficiency cap. Race is stamped on the character \
                   at creation; you don't pick or change innates here.",
        },
        run: cmd_mail_stub,
    },
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
                   description. Doesn't check whether you've accepted \
                   it — for your own quests, use `quests`.",
        },
        run: cmd_mail_stub,
    },
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
                   that quest is already assigned to you. Useful for \
                   exercising the quests / abandon loop without the \
                   full trigger-acceptance flow.",
        },
        run: cmd_mail_stub,
    },
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
    },
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
                   bumps completion_count. Useful for testing reward \
                   flow without the full objective pipeline.",
        },
        run: cmd_mail_stub,
    },
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
    },
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
    },
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
                   `.clear` to wipe and re-type from scratch. \
                   `.send` commits (and records an audit row in \
                   `BoardMessageEdit`); `.abort` discards. Players \
                   can only edit their own posts; Builder+ can edit \
                   any.",
        },
        run: cmd_mail_stub,
    },
    Command {
        names: &["mailbox"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "mailbox",
            summary: "List inbound mail for your account.",
            long: "Mail is account-scoped — every character on your \
                   account shares one inbox. Each line shows the \
                   slot index, an unread marker (`*`), the sender, \
                   and the subject. Use `readmail <#>` to read; \
                   `delmail <#>` to delete.",
        },
        run: cmd_mail_stub,
    },
    Command {
        names: &["readmail"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "readmail <#>",
            summary: "Read a message from your mailbox.",
            long: "Argument is the slot number from `mailbox`. Renders \
                   the body, marks the row read.",
        },
        run: cmd_mail_stub,
    },
    Command {
        names: &["delmail"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "delmail <#>",
            summary: "Soft-delete a message from your mailbox.",
            long: "Argument is the slot number from `mailbox`. Hides \
                   the row from future listings (audit trail keeps it \
                   in the table).",
        },
        run: cmd_mail_stub,
    },
    Command {
        names: &["gossip", "/"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "gossip <message>",
            summary: "Chat on the global gossip channel.",
            long: "Reaches every online player. Lower-volume than `shout` \
                   but with the same global scope.",
        },
        run: cmd_gossip,
    },
    Command {
        names: &["music"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "music <message>",
            summary: "Sing or hum on the music channel.",
            long: "Global channel for music / song RP — same scope as \
                   gossip, distinct prefix. Respects the `Deaf` toggle \
                   and per-receiver ignore lists.",
        },
        run: cmd_music,
    },
    Command {
        names: &["insult"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "insult <target>",
            summary: "Hurl a random insult at someone in the room.",
            long: "Picks a random insult and emits it to you, the \
                   target, and the rest of the room. Self-targeting \
                   leaves you feeling insulted at yourself.",
        },
        run: cmd_insult,
    },
    Command {
        names: &["petition"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "petition <message>",
            summary: "Send a message to all online immortals.",
            long: "Quick way to ask a staff member for help. Reaches \
                   every online player whose role is Immortal+; the \
                   sender gets a confirmation echo. Mortals never see \
                   anyone else's petitions.",
        },
        run: cmd_petition,
    },
    Command {
        names: &["wiznet", ";"],
        min_role: UserRole::Immortal,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "wiznet <message>",
            summary: "Chat on the staff-only wiznet channel.",
            long: "Immortal+. Sent to every online staff member \
                   (Immortal or higher). Players never see wiznet \
                   traffic. Convention: out-of-character coordination \
                   between staff during play.",
        },
        run: cmd_wiznet,
    },
    // ----- Combat -----
    Command {
        names: &["attack", "kill", "k", "hit", "murder"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "attack <target>",
            summary: "Engage a target in melee combat.",
            long: "Match is by case-insensitive substring on visible names. \
                   Targets with combat stats will fight back. Combat \
                   resolves once per second on the world tick.",
        },
        run: cmd_attack,
    },
    Command {
        names: &["consider", "con"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "consider <target>",
            summary: "Size up a potential opponent.",
            long: "Compares the target's max HP and damage roll to yours \
                   and reports a rough difficulty band. Doesn't engage \
                   the target — just a flavor read.",
        },
        run: cmd_consider,
    },
    Command {
        names: &["flee"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "flee",
            summary: "Run away from combat through a random open exit.",
            long: "Picks an open exit at random and moves you through it. \
                   You stop fighting; attackers stop on the next combat \
                   tick (they auto-disengage when their target leaves the \
                   room).",
        },
        run: cmd_flee,
    },
    Command {
        names: &["kick"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "kick",
            summary: "Make an immediate kick attack on your current target.",
            long: "Extra attack outside the normal combat-tick rhythm. \
                   Damage = dmg_roll + 4. You must already be fighting \
                   someone.",
        },
        run: cmd_kick,
    },
    Command {
        names: &["berserk"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "berserk",
            summary: "Self-buff: rage state for 60s.",
            long: "Costs 8 stamina, spawns a `berserk` EffectInstance \
                   on yourself for 60s. Refused if already berserk. \
                   Combat damage scaling is a follow-up — for now \
                   this is the visible buff state.",
        },
        run: cmd_berserk,
    },
    Command {
        names: &["tripup", "trip"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "tripup [<target>]",
            summary: "Trip target into Resting posture (lighter than stomp).",
            long: "Costs 5 stamina, deals 1/4 your dmg_roll, sets the \
                   target to Resting. Like stomp but cheaper and \
                   leaves them slightly less prone.",
        },
        run: cmd_tripup,
    },
    Command {
        names: &["sweep"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "sweep",
            summary: "Sweeping kick — knock every standing mob in room prone.",
            long: "Costs 12 stamina. Deals 1/4 dmg_roll to every \
                   Standing Mob in the room and sets each to Sitting. \
                   Players never targeted.",
        },
        run: cmd_sweep,
    },
    Command {
        names: &["roundhouse"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "roundhouse",
            summary: "Powerful kick — 1.5x dmg_roll on your current target.",
            long: "Costs 7 stamina. Heavier kick than the basic `kick` \
                   skill (which adds +4); pure dmg_roll multiplier. \
                   Requires you to be fighting someone.",
        },
        run: cmd_roundhouse,
    },
    Command {
        names: &["stomp"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "stomp [<target>]",
            summary: "Knock the target prone (Sitting posture).",
            long: "Costs 6 stamina, deals half your dmg_roll, sets the \
                   target's posture to Sitting. Default target is your \
                   current Fighting target. Refused on already-prone \
                   targets.",
        },
        run: cmd_stomp,
    },
    Command {
        names: &["roar", "howl"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "roar",
            summary: "Intimidate every mob in the room with a fear effect.",
            long: "Costs 8 stamina. Spawns a `fear` EffectInstance on \
                   each mob currently in your room (skipping any \
                   already feared) for 20s. Doesn't damage anyone, \
                   doesn't engage. Players are not targeted.",
        },
        run: cmd_roar,
    },
    Command {
        names: &["rend"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "rend [<target>]",
            summary: "Tearing attack — damage plus bleed effect.",
            long: "Costs 7 stamina, deals dmg_roll damage, applies a \
                   `bleed` EffectInstance for 30s. Default target is \
                   the current Fighting target. Refused if the target \
                   is already bleeding.",
        },
        run: cmd_rend,
    },
    Command {
        names: &["gouge"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "gouge [<target>]",
            summary: "Eye gouge — damage plus a temporary blind effect.",
            long: "Costs 7 stamina, deals dmg_roll damage, applies a \
                   `blind` EffectInstance for 30s. Default target is \
                   your current Fighting target. Refused if the target \
                   is already blinded.",
        },
        run: cmd_gouge,
    },
    Command {
        names: &["springleap"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "springleap <target>",
            summary: "Out-of-combat leaping kick — 1.5x damage opener.",
            long: "Deals 1.5x your dmg_roll on the opening swing and \
                   engages the target. Refused if you're already \
                   fighting or if the target is already in combat.",
        },
        run: cmd_springleap,
    },
    Command {
        names: &["throatcut"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "throatcut <target>",
            summary: "Out-of-combat assassination — 2.5x damage opener.",
            long: "Like backstab but heavier: 2.5x your dmg_roll on \
                   the opening swing. Costs 8 stamina. Same engagement \
                   rules — refused if you or target are already in \
                   combat.",
        },
        run: cmd_throatcut,
    },
    Command {
        names: &["backstab", "bs"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "backstab <target>",
            summary: "Surprise opener for double damage; out-of-combat only.",
            long: "Deals 2x your dmg_roll on the opening swing and \
                   engages the target. Refused if you're already \
                   fighting (the target sees you coming) or if your \
                   target is already in combat with someone else.",
        },
        run: cmd_backstab,
    },
    Command {
        names: &["hitall", "tantrum"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "hitall",
            summary: "One swing at every hostile mob in your room.",
            long: "Costs 10 stamina. Damages each Mob in the room \
                   for half your dmg_roll. Mobs with no Health (test \
                   dummy) are skipped. The first surviving mob \
                   becomes your Fighting target if you weren't \
                   already fighting. Players are never targeted.",
        },
        run: cmd_hitall,
    },
    Command {
        names: &["disarm"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "disarm [<target>]",
            summary: "Knock your opponent's weapon to the ground.",
            long: "Removes the target's wielded item; the weapon drops \
                   to the floor where any combatant can pick it up. \
                   Default target is your current Fighting target. \
                   Costs 5 stamina. Refused if the target isn't \
                   wielding anything.",
        },
        run: cmd_disarm,
    },
    Command {
        names: &["rescue"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "rescue <player>",
            summary: "Take an enemy's aggression onto yourself.",
            long: "Find <player> in your room. Their attacker now \
                   targets you instead and you target them. The ally \
                   is freed from combat. Costs 6 stamina. Refused if \
                   you're already fighting and refused if your ally \
                   isn't being attacked.",
        },
        run: cmd_rescue,
    },
    Command {
        names: &["guard"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "guard <player|off>",
            summary: "Stand bodyguard — intercept incoming swings on a target.",
            long: "Sets a `Guarding` link from you onto the named \
                   player; while you're in the same room, attackers \
                   targeting them swing at you instead. `guard off` \
                   clears the link. `guard` with no arg reports \
                   the current target.",
        },
        run: cmd_guard,
    },
    Command {
        names: &["assist"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "assist <player>",
            summary: "Engage your ally's current target.",
            long: "Looks up <player> in your current room, finds whom \
                   they're fighting, and engages that target — same \
                   stamina cost and rules as `attack`. Refused if \
                   they're not fighting, if their target is gone, or \
                   if you're already fighting someone else.",
        },
        run: cmd_assist,
    },
    Command {
        names: &["layhands", "lay"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "layhands [<target>]",
            summary: "Holy heal — bigger than bandage, works in combat.",
            long: "Heals 30 HP at a cost of 12 stamina. Works while \
                   fighting (unlike `bandage`). Refused on full-HP \
                   targets. Default target is yourself.",
        },
        run: cmd_layhands,
    },
    Command {
        names: &["retreat"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "retreat <direction>",
            summary: "Flee combat in a specific direction.",
            long: "Like `flee` but you choose where to go. Refused if \
                   the direction has no exit, the door's closed, or \
                   the target room is dangling.",
        },
        run: cmd_retreat,
    },
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
    },
    Command {
        names: &["tame"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "tame <target>",
            summary: "Befriend an animal mob into following you.",
            long: "Drains 4 stamina and dispatches the TAME skill at \
                   the named target. The schema's `charmed` status \
                   effect spawns on the mob; the runtime also installs \
                   `Follower(you)` so existing pet-handling treats it \
                   as your follower. Mob charm persists until dismiss \
                   or the mob dies — animal-control checks against \
                   the will save aren't modeled yet, so v1 always \
                   succeeds at the schema-formula amount.",
        },
        run: cmd_tame,
    },
    Command {
        names: &["drag"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "drag",
            summary: "Self-apply the DRAG speed penalty.",
            long: "Drains 3 stamina and dispatches the DRAG skill via \
                   the data path. The schema's `drag` effect doubles \
                   movement stamina cost (speedPenalty 0.5). Legacy \
                   `drag <body>` for hauling corpses isn't modeled — \
                   we have no corpse mechanic — so v1 is a self-cast \
                   that exercises the speed-penalty runtime.",
        },
        run: cmd_drag,
    },
    Command {
        names: &["buck"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "buck <target>",
            summary: "Throw a rider — dismount + knockdown.",
            long: "Drains 5 stamina and dispatches the BUCK skill at \
                   the named target. The schema's data path runs \
                   `dismount` (forced=true) → clears Mounted/RiddenBy, \
                   then `knockdown` (duration=1) → drops the target's \
                   posture. v1 dispatches as a player skill so \
                   characters with BUCK trained (Sorcerer/Druid/etc.) \
                   can fire it; mob-AI usage waits for an autonomous \
                   ability scheduler.",
        },
        run: cmd_buck,
    },
    Command {
        names: &["breathe"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "breathe [<target>]",
            summary: "Dragonborn breath weapon — race-typed.",
            long: "Dispatches one of BREATHE_FIRE / BREATHE_FROST / \
                   BREATHE_ACID / BREATHE_GAS / BREATHE_LIGHTNING \
                   based on your race (only the DRAGONBORN_* races \
                   carry one). Refuses for races with no breath \
                   weapon. Drains 6 stamina; the actual damage / \
                   target gating runs through the data path.",
        },
        run: cmd_breathe,
    },
    Command {
        names: &["lure"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "lure <target>",
            summary: "Bait a mob into engaging you with a stinging hit.",
            long: "Drains 4 stamina and dispatches the LURE skill at \
                   the named target. Effect is a level-scaling \
                   physical-damage application; combat starts via the \
                   normal damage→engage path. Same arg-resolution as \
                   `backstab`.",
        },
        run: cmd_lure,
    },
    Command {
        names: &["corner"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "corner <target>",
            summary: "Pin a mob with a hard hit to keep them in melee.",
            long: "Drains 4 stamina and dispatches the CORNER skill at \
                   the named target. Effect is a level-scaling \
                   physical-damage application like LURE; \
                   pin-in-place mechanics aren't modeled in the schema, \
                   so v1 is the damage hit and the engage.",
        },
        run: cmd_corner,
    },
    Command {
        names: &["sneak"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "sneak",
            summary: "Move silently — stealth that survives footsteps.",
            long: "Drains 3 stamina and dispatches the SNEAK skill \
                   via the data path. Spawns a `sneak` status effect \
                   and installs the Stealth marker (same gate as \
                   `hide`). Movement-stealth-break logic isn't wired \
                   yet, so sneak is functionally identical to hide \
                   until that lands.",
        },
        run: cmd_sneak,
    },
    Command {
        names: &["conceal"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "conceal",
            summary: "Magical concealment — improved hiding.",
            long: "Drains 4 stamina and dispatches the CONCEAL skill \
                   via the data path. Spawns a `hidden` status effect \
                   and installs the Stealth marker. Difference vs. \
                   `hide` is in the schema (different proficiency \
                   curve, longer duration), not in the runtime path.",
        },
        run: cmd_conceal,
    },
    Command {
        names: &["firstaid"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "firstaid [<target>]",
            summary: "Quick self/ally heal — wisdom-scaling.",
            long: "Drains 4 stamina and dispatches the FIRST_AID \
                   skill via the data path. Heal amount comes from \
                   the schema formula `skill / 4` scaled by wisdom. \
                   Defaults to self when no target given. The shim \
                   gates `Fighting` since first aid isn't an in-combat \
                   action.",
        },
        run: cmd_firstaid,
    },
    Command {
        names: &["bandage"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "bandage [<target>]",
            summary: "Apply first aid for a small heal (out of combat).",
            long: "Heals 10 HP at a cost of 4 stamina. With no arg or \
                   `me`/`self`, bandages yourself. Otherwise tries to \
                   find the target in your room. Refused while fighting \
                   and refused on full-HP targets.",
        },
        run: cmd_bandage,
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
    Command {
        names: &["disengage"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "disengage",
            summary: "Stop fighting your current target.",
            long: "Removes your Fighting state — you stop swinging. \
                   Opponents may keep attacking until they auto-disengage \
                   or you leave the room.",
        },
        run: cmd_disengage,
    },
    Command {
        names: &["doorbash"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "doorbash <direction>",
            summary: "Force-open a closed or locked door.",
            long: "Costs 10 stamina. Flips closed/locked exits to \
                   Open on both sides — useful when you don't have \
                   the key. Refused on already-open exits and when \
                   no exit exists in the named direction.",
        },
        run: cmd_doorbash,
    },
    Command {
        names: &["bash", "bodyslam", "maul"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "bash <target>",
            summary: "Slam a target, knocking them off their feet.",
            long: "Deals dmg_roll+3 damage and forces the target into a \
                   sitting posture. Targets without combat stats simply \
                   take the damage.",
        },
        run: cmd_bash,
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
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
    },
    Command {
        names: &["gsay", "gtell", "gecho", "gt"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "gsay <message>",
            summary: "Speak to your group regardless of location.",
            long: "Reaches every member of your follow-chain group — \
                   leader and all transitive followers — even across \
                   rooms. Players outside the group don't hear it.",
        },
        run: cmd_gsay,
    },
    // ----- Movement -----
    Command {
        names: &["north", "n"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_north,
    },
    Command {
        names: &["south", "s"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_south,
    },
    Command {
        names: &["east", "e"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_east,
    },
    Command {
        names: &["west", "w"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_west,
    },
    Command {
        names: &["up", "u"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_up,
    },
    Command {
        names: &["down", "d"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_down,
    },
    Command {
        names: &["northeast", "ne"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_northeast,
    },
    Command {
        names: &["northwest", "nw"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_northwest,
    },
    Command {
        names: &["southeast", "se"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_southeast,
    },
    Command {
        names: &["southwest", "sw"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_southwest,
    },
    Command {
        names: &["recall", "home"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: Help {
            usage: "recall",
            summary: "Teleport to your recall point.",
            long: "Move instantly to your saved recall room. If you haven't \
                   set one, you're told so. Use `setrecall` in your current \
                   room to bind it as your recall point.",
        },
        run: cmd_recall,
    },
    Command {
        names: &["enter"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: Help {
            usage: "enter <portal>",
            summary: "Step into a portal in the room.",
            long: "Reads the portal's `Destination` and teleports you to \
                   the matching room. Refused while fighting. Portals \
                   with a missing or unresolved destination shimmer \
                   harmlessly.",
        },
        run: cmd_enter,
    },
    Command {
        names: &["setrecall"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "setrecall",
            summary: "Bind your recall point to the current room.",
            long: "Saves the current room as your recall destination. \
                   Persists across logins.",
        },
        run: cmd_setrecall,
    },
    Command {
        names: &["in"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_in,
    },
    Command {
        names: &["out"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: MOVE_HELP,
        run: cmd_out,
    },
    // ----- Admin -----
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
    },
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
                   you are. Both the source and destination rooms see \
                   the gesture; the transferred player gets a brief \
                   notice and an automatic look at the new room.",
        },
        run: cmd_transfer,
    },
    Command {
        names: &["teleport"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "teleport <player> <zone> <room>",
            summary: "Send an online player to a specific room.",
            long: "Builder+. Inverse of `transfer` (which pulls them \
                   to you) and `goto` (which moves you). Looks up the \
                   target by case-insensitive name, looks up the \
                   destination via `WorldKeyIndex`, then re-attaches \
                   their `Located` and (if mounted) their mount's. The \
                   target gets an automatic `look` at the new room.",
        },
        run: cmd_teleport,
    },
    Command {
        names: &["force"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "force <player> <command>",
            summary: "Make a player run a command as themselves.",
            long: "Implementor-only. Dispatches <command> with <player> \
                   as the actor — exactly as if they had typed it. The \
                   player sees their command's normal output and a note \
                   that you forced it. Useful for testing and unsticking \
                   stuck sessions.",
        },
        run: cmd_force,
    },
    Command {
        names: &["freeze"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "freeze <player>",
            summary: "Toggle a player's input-dispatch lockout.",
            long: "Implementor-only. While frozen, the target's commands \
                   are refused (except `quit`) and they see a sanction \
                   notice. Run `freeze <player>` again to thaw them. \
                   Session-scoped — disconnect clears it.",
        },
        run: cmd_freeze,
    },
    Command {
        names: &["summon"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "summon <zone_id> <mob_id>",
            summary: "Spawn a mob from its prototype into your current room.",
            long: "Builder+. Looks up `MobPrototypes[(zone, id)]`, rolls HP \
                   from the prototype's hp dice, derives damage from the \
                   damage dice (average), and spawns a fresh entity with \
                   Mob/Health/CombatStats/Posture/Located in your room.",
        },
        run: cmd_summon,
    },
    Command {
        names: &["apply"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "apply <effect_name> <target> [seconds]",
            summary: "Apply an effect to a target.",
            long: "Spawns an EffectInstance attached to the target. Target \
                   can be 'me'/'self' or a substring of any named entity \
                   in your current room. Duration defaults to 30 seconds; \
                   use -1 for permanent.",
        },
        run: cmd_apply,
    },
    Command {
        names: &["restore"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "restore [target]",
            summary: "Refill HP and stamina to max on yourself or another actor.",
            long: "Builder+ debug/utility. Sets Health.hp = max and \
                   Stamina.current = max. With no argument, restores the \
                   caster. With a target (substring match in the room), \
                   restores them and notifies the room.",
        },
        run: cmd_restore,
    },
    Command {
        names: &["slay"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "slay <mob>",
            summary: "Instantly destroy a mob in your current room.",
            long: "Builder+ shortcut for combat testing. Despawns the mob \
                   and ends any combat against it. Players are refused — \
                   use `restore` if a player is in trouble, or `attack` \
                   if you really mean to fight.",
        },
        run: cmd_slay,
    },
    Command {
        names: &["zstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "zstat [<zone_id>]",
            summary: "Dump ECS state of a zone.",
            long: "Builder+. With no arg, inspects the zone you're in \
                   (resolved from your room's WorldKey). Prints zone \
                   name + entity, loaded room count, mob/object \
                   prototype counts, and live spawned mob/item counts \
                   originating from this zone's protos.",
        },
        run: cmd_zstat,
    },
    Command {
        names: &["mstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "mstat <zone> <id>",
            summary: "Dump a Mob prototype's catalog data + live count.",
            long: "Builder+. Looks up `MobPrototypes[(zone, id)]` and \
                   prints name, level, alignment, role, hit/damage \
                   dice, AC, hit_roll, wealth, attached trigger \
                   count, and how many live instances of this proto \
                   currently exist in the world.",
        },
        run: cmd_mstat,
    },
    Command {
        names: &["ostat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "ostat <zone> <id>",
            summary: "Dump an Object prototype's catalog data + live count.",
            long: "Builder+. Looks up `ObjectPrototypes[(zone, id)]` and \
                   prints name, type, wear flags, examine description, \
                   liquid/board metadata if present, attached trigger \
                   count, and how many live instances exist.",
        },
        run: cmd_ostat,
    },
    Command {
        names: &["sstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "sstat <zone> <id>",
            summary: "Dump a Shop's catalog row.",
            long: "Builder+. Looks up `ShopCatalog[(zone, id)]` and \
                   prints keeper, buy/sell profit, item offerings, \
                   accept-filter rules, and pet offerings.",
        },
        run: cmd_sstat,
    },
    Command {
        names: &["tstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "tstat <zone> <id>",
            summary: "Dump a Lua trigger row.",
            long: "Builder+. Looks up `TriggerCatalog[(zone, id)]` \
                   and prints attach type, event flags, arg list, and \
                   the body (commands) text. Read-only — does not \
                   fire the trigger.",
        },
        run: cmd_tstat,
    },
    Command {
        names: &["setweather"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "setweather <climate> [<zone_id>]",
            summary: "Override the climate for a zone (current zone by default).",
            long: "Builder+. Mutates the zone's `ZoneClimate` \
                   component. Climate is one of: none / semiarid / \
                   arid / oceanic / temperate / subtropical / \
                   tropical / subarctic / arctic / alpine. Persists \
                   only in-memory until the next world load.",
        },
        run: cmd_setweather,
    },
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
    },
    Command {
        names: &["set"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "set <target|me> <field> <value>",
            summary: "Mutate a numeric stat on a character.",
            long: "Builder+. `target` is `me` for self or a keyword \
                   matching a player in the current room. `field` is \
                   one of: level, xp, hp, maxhp, stamina, maxstamina, \
                   gold (= copper), alignment. Session-only — \
                   persists on disconnect via the normal save path \
                   for the level/xp/hp/stamina/gold subset.",
        },
        run: cmd_set,
    },
    Command {
        names: &["show"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "show <subsystem>",
            summary: "Diagnostic dump for a runtime subsystem.",
            long: "Builder+. Subsystems: \
                   `players` (online list with role/level/room), \
                   `triggers` (catalog totals + per-event tally), \
                   `effects` (active EffectInstance count by tag), \
                   `clock` (MudClock + TickCount), \
                   `resets` (mob/object refresh counts). \
                   `show` with no arg lists the subsystems.",
        },
        run: cmd_show,
    },
    Command {
        names: &["scripterrors", "scripterr"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "scripterrors [<count>]",
            summary: "Recent Lua trigger fire failures.",
            long: "Builder+. Walks the in-memory ScriptErrorLog \
                   (most-recent first) and prints each failure with \
                   timestamp, (zone, id), trigger name, event, and \
                   the lua error message. Capped at 256 entries; \
                   default `count` is 20.",
        },
        run: cmd_scripterrors,
    },
    Command {
        names: &["syslog"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "syslog [<count>] [<filter>]",
            summary: "Recent tracing log lines.",
            long: "Builder+. Walks the in-memory syslog ring buffer \
                   (most-recent first) and prints each entry with \
                   wall-clock seconds-ago, level, target, and message. \
                   `filter` (case-insensitive) matches level \
                   (TRACE/DEBUG/INFO/WARN/ERROR) or any substring of \
                   target/message. Capped at 512 entries; default \
                   `count` is 30.",
        },
        run: cmd_syslog,
    },
    Command {
        names: &["astat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "astat [<target>]",
            summary: "Detailed `affects` listing for any character in the room.",
            long: "Builder+. Without an argument, dumps your own \
                   `EffectInstance` rows in detail. With a keyword, \
                   resolves to a player or mob in the same room. \
                   Shows each effect's tag, remaining duration, \
                   strength, source, and the ability that spawned \
                   it (when known).",
        },
        run: cmd_astat,
    },
    Command {
        names: &["rstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "rstat [<zone_id> <room_id>]",
            summary: "Dump ECS state of a room.",
            long: "Builder+. With no arg, inspects the room you're in. \
                   With two integer ids, looks up the matching room via \
                   WorldKeyIndex. Prints zone, id, sector, occupant \
                   counts, and the populated exit table.",
        },
        run: cmd_rstat,
    },
    Command {
        names: &["stat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "stat [<target>]",
            summary: "Dump ECS state of an entity in your room (or self).",
            long: "Builder+ diagnostic. With no arg or `me`/`self`, \
                   inspects you. With a keyword, finds the matching mob/ \
                   player/item in the room and prints its components: \
                   WorldKey, Health, Stamina, Posture, CombatStats, \
                   Profile (players), and any active EffectInstances \
                   pointing at it.",
        },
        run: cmd_stat,
    },
    Command {
        names: &["load"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "load <obj|mob> <zone> <id>",
            summary: "Unified spawn dispatcher for obj or mob protos.",
            long: "Builder+. `load obj <z> <i>` is `loadobj` and \
                   `load mob <z> <i>` is `summon`. Same prototype \
                   lookups; same room target (your current room).",
        },
        run: cmd_load,
    },
    Command {
        names: &["loadobj", "loado"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "loadobj <zone_id> <obj_id>",
            summary: "Spawn an object prototype into your current room.",
            long: "Counterpart to `summon` for objects. Resolves the \
                   prototype via ObjectPrototypes[(zone, id)], spawns a \
                   fresh Item entity in your room with WorldKey + Named + \
                   Keywords (+ Description if the proto has an examine \
                   text). Builder+.",
        },
        run: cmd_loadobj,
    },
    Command {
        names: &["dumpworld"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "dumpworld [<path>]",
            summary: "JSON snapshot of live world state to disk.",
            long: "Implementor-only diagnostic. Writes a JSON file \
                   with current tick, MudClock, online player roster \
                   (name/level/role/room/hp/stamina/wealth), entity \
                   counts (mobs/items/effects), and trigger catalog \
                   stats. Default path is \
                   /tmp/world_dump_<unix_ts>.json. The world keeps \
                   running — this is a checkpoint, not a freeze.",
        },
        run: cmd_dumpworld,
    },
    Command {
        names: &["purge"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "purge [<target>]",
            summary: "Despawn mobs + items in this room (or a single target).",
            long: "With no argument, removes every mob and every \
                   non-equipped item from the room — players are never \
                   touched. With an argument, finds that mob/item by \
                   keyword and despawns just it. Items inside containers \
                   on a purged mob go with the mob (Located → mob).",
        },
        run: cmd_purge,
    },
    Command {
        names: &["lua"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "lua <code>",
            summary: "Run a snippet of Lua code.",
            long: "Runs `code` with `actor` bound to your character. \
                   Captured `print` output is sent back to you. Useful for \
                   inspecting entity state and prototyping triggers. \
                   Examples: \
                     lua print(actor:name()) \
                     lua print(actor:hp() .. '/' .. actor:max_hp())",
        },
        run: cmd_lua,
    },
    Command {
        names: &["triggers", "trigs"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "triggers [here|<keyword>]",
            summary: "List Lua triggers attached to entities.",
            long: "With `here` (default), lists triggers on your room and \
                   on every mob/item in it. With a keyword, finds the \
                   single matching mob or item and lists just its \
                   triggers. Each line is `(zone, id) name [FLAG ...]`. \
                   Read-only diagnostic — does not fire anything.",
        },
        run: cmd_triggers,
    },
    Command {
        names: &["firetrig"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "firetrig <zone> <id> [<keyword>]",
            summary: "Hand-fire a Lua trigger body for testing.",
            long: "Looks up the trigger in TriggerCatalog by `(zone, id)` \
                   and executes its `commands` body via the LuaHost. \
                   Without a keyword, `self` and `actor` bind to YOU. \
                   With a keyword, they bind to the matching mob/item in \
                   the current room. Captured `print` output and any \
                   error are sent back. Useful for validating trigger \
                   data without waiting for the natural event.",
        },
        run: cmd_firetrig,
    },
];

const MOVE_HELP: Help = Help {
    usage: "<direction>",
    summary: "Walk through an exit.",
    long: "Moves you through the named exit if one is open. Standard \
           directions: n/s/e/w/u/d/ne/nw/se/sw/in/out.",
};

/// Force-initialize the registry so contract violations (duplicate names,
/// empty help) surface at startup rather than on first command dispatch.
pub fn validate_registry() {
    let count = REGISTRY.len();
    tracing::info!(commands = count, "command registry initialized");
}

static REGISTRY: LazyLock<HashMap<&'static str, &'static Command>> = LazyLock::new(|| {
    let mut m: HashMap<&'static str, &'static Command> = HashMap::new();
    for cmd in COMMANDS {
        assert!(
            !cmd.help.summary.is_empty(),
            "command {:?} has empty help.summary",
            cmd.names[0]
        );
        assert!(!cmd.names.is_empty(), "command has no names");
        for &name in cmd.names {
            assert!(!name.is_empty(), "command {:?} has empty name", cmd.names);
            let token_count = name.split_whitespace().count();
            assert!(
                token_count <= MAX_NAME_TOKENS,
                "command name {name:?} exceeds MAX_NAME_TOKENS={MAX_NAME_TOKENS}"
            );
            assert!(
                m.insert(name, cmd).is_none(),
                "duplicate command name: {name}"
            );
        }
    }
    m
});

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Async pre-dispatch hook for commands that need DB access. Today
/// this is just the mail commands (`mailbox` / `readmail` / `delmail`
/// / `mail`). Returns true when the input was handled here; false
/// to fall through to the sync `dispatch`.
#[allow(clippy::too_many_lines)]
pub async fn try_dispatch_async(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    line: &str,
) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Composition mode: when the player has a `MailDraft` or
    // `BoardDraft` component, every line is routed to the matching
    // composer until `.send` / `.abort` clears it.
    if world.get::<MailDraft>(player).is_some() {
        mark_for_prompt(player);
        try_insert(world, player, LastInputAt(std::time::Instant::now()));
        compose_mail_step(world, player, pool, trimmed).await;
        return true;
    }
    if world.get::<BoardDraft>(player).is_some() {
        mark_for_prompt(player);
        try_insert(world, player, LastInputAt(std::time::Instant::now()));
        compose_board_step(world, player, pool, trimmed).await;
        return true;
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("").to_ascii_lowercase();
    let args = parts.next().unwrap_or("").trim();
    match head.as_str() {
        "mail" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_mail(world, player, pool, args).await;
            true
        }
        "boards" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_boards(world, player, pool).await;
            true
        }
        "quests" | "qstat" | "qlist" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_quests(world, player, pool).await;
            true
        }
        "abandon" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_abandon(world, player, pool, args).await;
            true
        }
        "questinfo" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_questinfo(world, player, pool, args).await;
            true
        }
        "innate" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_innate(world, player, pool).await;
            true
        }
        "qload" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_qload(world, player, pool, args).await;
            true
        }
        "qgive" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_qgive(world, player, pool, args).await;
            true
        }
        "qcomplete" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_qcomplete(world, player, pool, args).await;
            true
        }
        // Numeric `read <#>` while standing near a board → render
        // that board's message body. Non-numeric `read <item>`
        // falls through to the sync handler that looks at item
        // Description.
        "read" if args.trim().parse::<usize>().is_ok() => {
            let target_room = world.get::<Located>(player).map(|l| l.0);
            let board_id_in_room = target_room.and_then(|room| {
                let mut q = world
                    .query_filtered::<(&Located, &BoardLink), With<Item>>();
                q.iter(world)
                    .find(|(l, _)| l.0 == room)
                    .map(|(_, b)| b.0)
            });
            if let Some(board_id) = board_id_in_room {
                mark_for_prompt(player);
                try_insert(world, player, LastInputAt(std::time::Instant::now()));
                cmd_read_board_msg(world, player, pool, board_id, args).await;
                true
            } else {
                false
            }
        }
        // `look <keyword>` / `examine <keyword>` where the keyword
        // resolves to a BOARD-tagged item in the room → render the
        // board's full listing inline. Falls through when no such
        // item is matched, so plain look/examine on non-board
        // targets goes through the sync handler.
        "look" | "examine" | "exa" => {
            let target_room = world.get::<Located>(player).map(|l| l.0);
            let needle = args.trim().to_ascii_lowercase();
            let board_id = target_room.and_then(|room| {
                let mut q = world
                    .query_filtered::<(&Located, &Named, Option<&Keywords>, &BoardLink), With<Item>>();
                q.iter(world)
                    .find(|(l, n, kw, _)| {
                        l.0 == room && (needle.is_empty() || matches(&needle, n, *kw))
                    })
                    .map(|(_, _, _, b)| b.0)
            });
            if !needle.is_empty()
                && let Some(board_id) = board_id
            {
                mark_for_prompt(player);
                try_insert(world, player, LastInputAt(std::time::Instant::now()));
                cmd_look_board(world, player, pool, board_id).await;
                true
            } else {
                false
            }
        }
        "board" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_board(world, player, pool, args).await;
            true
        }
        "post" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_post(world, player, pool, args).await;
            true
        }
        "editpost" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_editpost(world, player, pool, args).await;
            true
        }
        "delpost" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_delpost(world, player, pool, args).await;
            true
        }
        "mailbox" | "mailboxes" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_mailbox(world, player, pool).await;
            true
        }
        "readmail" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_readmail(world, player, pool, args).await;
            true
        }
        "delmail" => {
            mark_for_prompt(player);
            try_insert(world, player, LastInputAt(std::time::Instant::now()));
            cmd_delmail(world, player, pool, args).await;
            true
        }
        _ => false,
    }
}

/// State summary of one composition step — needed because we have to
/// release the `Mut<MailDraft>` borrow before sending feedback to the
/// player (`send_to` re-borrows the world).
enum ComposeStep {
    Nudge,
    SubjectSet,
    BodyAdded,
}

/// Process one line of input from a player who has an active
/// `MailDraft`. Recognized control verbs:
///   `.send`    — finalize and persist the mail
///   `.abort`   — discard the draft
///   `.preview` — show the current draft so far
/// Anything else is a content line: first non-blank line becomes
/// the subject, subsequent lines append to the body.
#[allow(clippy::too_many_lines)]
async fn compose_mail_step(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    line: &str,
) {
    let trimmed = line.trim();
    if trimmed.eq_ignore_ascii_case(".abort") {
        try_remove::<MailDraft>(world, player);
        send_to(world, player, "Mail composition aborted.\r\n");
        return;
    }
    if trimmed.eq_ignore_ascii_case(".clear") {
        if let Some(mut draft) = world.get_mut::<MailDraft>(player) {
            draft.subject = None;
            draft.body.clear();
        }
        send_to(
            world,
            player,
            "Cleared. Type a new subject, then the body.\r\n",
        );
        return;
    }
    if trimmed.eq_ignore_ascii_case(".preview") {
        let Some(draft) = world.get::<MailDraft>(player).cloned() else {
            return;
        };
        let mut out = String::from("\r\n--- DRAFT ---\r\n");
        out.push_str(&format!("To:      {}\r\n", draft.recipient_label));
        out.push_str(&format!(
            "Subject: {}\r\n",
            draft.subject.as_deref().unwrap_or("(none yet)"),
        ));
        out.push_str("---\r\n");
        if draft.body.is_empty() {
            out.push_str("(empty body)\r\n");
        } else {
            for ln in &draft.body {
                out.push_str(ln);
                out.push_str("\r\n");
            }
        }
        out.push_str("--- end of draft ---\r\n");
        send_to(world, player, out);
        return;
    }
    if trimmed.eq_ignore_ascii_case(".send") {
        let Some(draft) = world.get::<MailDraft>(player).cloned() else {
            return;
        };
        let Some(subject) = draft.subject else {
            send_to(
                world,
                player,
                "No subject set yet — type a subject line first.\r\n",
            );
            return;
        };
        if draft.body.is_empty() {
            send_to(world, player, "Body is empty — type some lines first.\r\n");
            return;
        }
        let body = draft.body.join("\n");
        let sender_user_id = world
            .get::<Account>(player)
            .map(|a| a.user_id.clone());
        let Some(sender_user_id) = sender_user_id else {
            send_to(world, player, "No account info; can't send.\r\n");
            return;
        };
        match mud_db::mail::send(
            pool,
            &sender_user_id,
            &draft.recipient_user_id,
            &subject,
            &body,
        )
        .await
        {
            Ok(_id) => {
                try_remove::<MailDraft>(world, player);
                send_to(
                    world,
                    player,
                    format!("Mail sent to {}.\r\n", draft.recipient_label),
                );
            }
            Err(e) => {
                send_to(world, player, format!("Send failed: {e}\r\n"));
            }
        }
        return;
    }
    // Plain content line: first non-blank line is the subject; rest
    // accumulate as body. Compute what to do under the mutable
    // borrow, then release before sending feedback.
    let step = if let Some(mut draft) = world.get_mut::<MailDraft>(player) {
        if draft.subject.is_none() {
            if trimmed.is_empty() {
                ComposeStep::Nudge
            } else {
                draft.subject = Some(trimmed.to_string());
                ComposeStep::SubjectSet
            }
        } else {
            draft.body.push(line.to_string());
            ComposeStep::BodyAdded
        }
    } else {
        return;
    };
    match step {
        ComposeStep::Nudge => send_to(
            world,
            player,
            "Type a subject line, then the body. `.send` to ship; `.abort` to cancel.\r\n",
        ),
        ComposeStep::SubjectSet => send_to(
            world,
            player,
            "Subject set. Type the body, one line at a time. \
             `.send` to ship, `.abort` to cancel, `.preview` to review.\r\n",
        ),
        // Body lines: silent acceptance — the player sees their own typing
        // already; an echo on every line would feel chatty.
        ComposeStep::BodyAdded => {}
    }
}

/// Process one line of input from a player who has an active
/// `BoardDraft`. Same control verbs as mail (`.send` / `.abort` /
/// `.preview`), same first-line-is-subject rule.
#[allow(clippy::too_many_lines)]
async fn compose_board_step(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    line: &str,
) {
    let trimmed = line.trim();
    if trimmed.eq_ignore_ascii_case(".abort") {
        try_remove::<BoardDraft>(world, player);
        send_to(world, player, "Board post aborted.\r\n");
        return;
    }
    if trimmed.eq_ignore_ascii_case(".clear") {
        if let Some(mut draft) = world.get_mut::<BoardDraft>(player) {
            draft.subject = None;
            draft.body.clear();
        }
        send_to(
            world,
            player,
            "Cleared. Type a new subject, then the body.\r\n",
        );
        return;
    }
    if trimmed.eq_ignore_ascii_case(".preview") {
        let Some(draft) = world.get::<BoardDraft>(player).cloned() else {
            return;
        };
        let mut out = String::from("\r\n--- DRAFT ---\r\n");
        out.push_str(&format!("Board:   {} ({})\r\n", draft.board_title, draft.board_alias));
        out.push_str(&format!(
            "Subject: {}\r\n",
            draft.subject.as_deref().unwrap_or("(none yet)"),
        ));
        out.push_str("---\r\n");
        if draft.body.is_empty() {
            out.push_str("(empty body)\r\n");
        } else {
            for ln in &draft.body {
                out.push_str(ln);
                out.push_str("\r\n");
            }
        }
        out.push_str("--- end of draft ---\r\n");
        send_to(world, player, out);
        return;
    }
    if trimmed.eq_ignore_ascii_case(".send") {
        let Some(draft) = world.get::<BoardDraft>(player).cloned() else {
            return;
        };
        let Some(subject) = draft.subject else {
            send_to(
                world,
                player,
                "No subject set yet — type a subject line first.\r\n",
            );
            return;
        };
        if draft.body.is_empty() {
            send_to(world, player, "Body is empty — type some lines first.\r\n");
            return;
        }
        let body = draft.body.join("\n");
        let poster = name_of(world, player);
        let level = world.get::<Profile>(player).map_or(1, |p| p.level);
        let result = if let Some(edit_id) = draft.edit_message_id {
            mud_db::boards::update_message(pool, edit_id, &subject, &body, &poster)
                .await
                .map(|_| edit_id)
        } else {
            mud_db::boards::post_message(
                pool,
                draft.board_id,
                &poster,
                level,
                &subject,
                &body,
            )
            .await
        };
        match result {
            Ok(_id) => {
                try_remove::<BoardDraft>(world, player);
                let verb = if draft.edit_message_id.is_some() { "Updated" } else { "Posted" };
                send_to(
                    world,
                    player,
                    format!(
                        "{verb} on {} ({}).\r\n",
                        draft.board_title, draft.board_alias
                    ),
                );
            }
            Err(e) => {
                send_to(world, player, format!("Save failed: {e}\r\n"));
            }
        }
        return;
    }
    let step = if let Some(mut draft) = world.get_mut::<BoardDraft>(player) {
        if draft.subject.is_none() {
            if trimmed.is_empty() {
                ComposeStep::Nudge
            } else {
                draft.subject = Some(trimmed.to_string());
                ComposeStep::SubjectSet
            }
        } else {
            draft.body.push(line.to_string());
            ComposeStep::BodyAdded
        }
    } else {
        return;
    };
    match step {
        ComposeStep::Nudge => send_to(
            world,
            player,
            "Type a subject line, then the body. `.send` to ship; `.abort` to cancel.\r\n",
        ),
        ComposeStep::SubjectSet => send_to(
            world,
            player,
            "Subject set. Type the body, one line at a time. \
             `.send` to ship, `.abort` to cancel, `.preview` to review.\r\n",
        ),
        ComposeStep::BodyAdded => {}
    }
}

/// `delpost <board> <#>`: delete one of your own posts on a board.
/// Builders+ can delete anyone's posts (matches the legacy "moderator
/// can edit/remove any" privilege; refining via `Board.privileges`
/// JSON is a follow-up). The `poster` column is a string compare
/// against the caller's current character `Named`.
pub(crate) async fn cmd_delpost(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let Some(alias) = parts.next() else {
        send_to(world, player, "Usage: delpost <board-alias> <#>\r\n");
        return;
    };
    let Some(slot_raw) = parts.next() else {
        send_to(world, player, "Usage: delpost <board-alias> <#>\r\n");
        return;
    };
    let Ok(slot) = slot_raw.parse::<usize>() else {
        send_to(world, player, "Slot number must be a positive integer.\r\n");
        return;
    };
    if slot == 0 {
        send_to(world, player, "Slots are 1-based.\r\n");
        return;
    }
    let board = match mud_db::boards::find_board_by_alias(pool, alias).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            send_to(world, player, format!("No board called '{alias}'.\r\n"));
            return;
        }
        Err(e) => {
            send_to(world, player, format!("Board lookup failed: {e}\r\n"));
            return;
        }
    };
    let messages = match mud_db::boards::messages_for_board(pool, board.id).await {
        Ok(m) => m,
        Err(e) => {
            send_to(world, player, format!("Message fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(msg) = messages.get(slot - 1) else {
        send_to(
            world,
            player,
            format!("No message at slot {slot} on '{alias}'.\r\n"),
        );
        return;
    };
    let caller_name = name_of(world, player);
    let is_builder = world
        .get::<Account>(player)
        .is_some_and(|a| a.role.at_least(UserRole::Builder));
    let is_owner = msg.poster.eq_ignore_ascii_case(&caller_name);
    if !is_owner && !is_builder {
        send_to(
            world,
            player,
            "You can only delete your own posts (builders+ can delete any).\r\n",
        );
        return;
    }
    let preview_subject = msg.subject.clone();
    let preview_poster = msg.poster.clone();
    match mud_db::boards::delete_message(pool, msg.id).await {
        Ok(0) => {
            send_to(
                world,
                player,
                "Message was already gone — nothing deleted.\r\n",
            );
        }
        Ok(_) => {
            send_to(
                world,
                player,
                format!(
                    "Deleted '{preview_subject}' by {preview_poster} from {}.\r\n",
                    board.title,
                ),
            );
        }
        Err(e) => {
            send_to(world, player, format!("Delete failed: {e}\r\n"));
        }
    }
}

/// `post <board>`: open a board-composition draft. Locked boards
/// refuse the open. Resolves the alias, attaches `BoardDraft`, and
/// prompts for a subject. Subsequent input flows through
/// `compose_board_step` until `.send` / `.abort` clears the draft.
pub(crate) async fn cmd_post(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let alias = args.trim();
    if alias.is_empty() {
        send_to(world, player, "Usage: post <board-alias>\r\n");
        return;
    }
    let board = match mud_db::boards::find_board_by_alias(pool, alias).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            send_to(world, player, format!("No board called '{alias}'.\r\n"));
            return;
        }
        Err(e) => {
            send_to(world, player, format!("Board lookup failed: {e}\r\n"));
            return;
        }
    };
    if board.locked {
        send_to(
            world,
            player,
            format!("'{}' is locked; no posts accepted.\r\n", board.title),
        );
        return;
    }
    try_insert(
        world,
        player,
        BoardDraft {
            board_id: board.id,
            board_alias: board.alias.clone(),
            board_title: board.title.clone(),
            subject: None,
            body: Vec::new(),
            edit_message_id: None,
        },
    );
    send_to(
        world,
        player,
        format!(
            "Posting to {} ({}).\r\n\
             First line is the subject. Then type the body, one line at a time.\r\n\
             `.send` ships it; `.abort` cancels; `.preview` shows the draft.\r\n",
            board.title, board.alias,
        ),
    );
}

/// `editpost <alias> <#>`: re-open one of your posts (or any if
/// Builder+) for editing. Pre-loads the existing subject and body
/// into a `BoardDraft`; `.send` triggers `update_message` (which
/// inserts a `BoardMessageEdit` audit row in the same transaction).
pub(crate) async fn cmd_editpost(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let Some(alias) = parts.next() else {
        send_to(world, player, "Usage: editpost <board-alias> <#>\r\n");
        return;
    };
    let Some(slot_raw) = parts.next() else {
        send_to(world, player, "Usage: editpost <board-alias> <#>\r\n");
        return;
    };
    let Ok(slot) = slot_raw.parse::<usize>() else {
        send_to(world, player, "Slot number must be a positive integer.\r\n");
        return;
    };
    if slot == 0 {
        send_to(world, player, "Slots are 1-based.\r\n");
        return;
    }
    let board = match mud_db::boards::find_board_by_alias(pool, alias).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            send_to(world, player, format!("No board called '{alias}'.\r\n"));
            return;
        }
        Err(e) => {
            send_to(world, player, format!("Board lookup failed: {e}\r\n"));
            return;
        }
    };
    if board.locked {
        send_to(
            world,
            player,
            format!("'{}' is locked; no edits accepted.\r\n", board.title),
        );
        return;
    }
    let messages = match mud_db::boards::messages_for_board(pool, board.id).await {
        Ok(m) => m,
        Err(e) => {
            send_to(world, player, format!("Message fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(msg) = messages.get(slot - 1).cloned() else {
        send_to(
            world,
            player,
            format!("No message at slot {slot} on '{alias}'.\r\n"),
        );
        return;
    };
    let caller_name = name_of(world, player);
    let is_builder = world
        .get::<Account>(player)
        .is_some_and(|a| a.role.at_least(UserRole::Builder));
    let is_owner = msg.poster.eq_ignore_ascii_case(&caller_name);
    if !is_owner && !is_builder {
        send_to(
            world,
            player,
            "You can only edit your own posts (builders+ can edit any).\r\n",
        );
        return;
    }
    // Seed the draft with the existing body, line-split. Subject is
    // pre-set so the first input line goes straight to the body.
    let body_lines: Vec<String> = msg
        .content
        .split('\n')
        .map(str::to_string)
        .collect();
    try_insert(
        world,
        player,
        BoardDraft {
            board_id: board.id,
            board_alias: board.alias.clone(),
            board_title: board.title.clone(),
            subject: Some(msg.subject.clone()),
            body: body_lines,
            edit_message_id: Some(msg.id),
        },
    );
    send_to(
        world,
        player,
        format!(
            "Editing message #{slot} on {} ({}).\r\n\
             Subject and existing body are preserved. Add lines to append \
             (or `.abort` to bail without saving). Use `.preview` to see \
             the current state, `.send` to commit (records an audit row).\r\n",
            board.title, board.alias,
        ),
    );
}

/// `mail <character>`: open a mail-composition draft addressed to
/// the named character's account. Resolves the recipient via DB
/// (case-insensitive name match), attaches a `MailDraft` component,
/// and prompts the player for a subject. Subsequent input is routed
/// to `compose_mail_step` until `.send` / `.abort` clears the draft.
pub(crate) async fn cmd_mail(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: mail <character>\r\n");
        return;
    }
    let resolved = match mud_db::mail::user_for_character_name(pool, arg).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Lookup failed: {e}\r\n"));
            return;
        }
    };
    let Some((user_id, name)) = resolved else {
        send_to(
            world,
            player,
            format!("No character named '{arg}' on this realm.\r\n"),
        );
        return;
    };
    try_insert(
        world,
        player,
        MailDraft {
            recipient_user_id: user_id,
            recipient_label: name.clone(),
            subject: None,
            body: Vec::new(),
        },
    );
    send_to(
        world,
        player,
        format!(
            "Composing mail to {name}.\r\n\
             First line is the subject. Then type the body, one line at a time.\r\n\
             `.send` ships it; `.abort` cancels; `.preview` shows the draft; \
             `.clear` wipes and starts over.\r\n"
        ),
    );
}

pub fn dispatch(world: &mut World, player: Entity, line: &str) {
    // Whatever happens (success, error, unknown command, empty input), the
    // typing player gets a prompt at end-of-turn via flush_prompts. Marking
    // here also dedupes against any send_to(player, …) inside the handler.
    mark_for_prompt(player);
    // Stamp activity even for empty input — pressing return to "wake up" a
    // session counts as activity for idle-timer purposes.
    try_insert(world, player, LastInputAt(std::time::Instant::now()));
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    // Per-character alias expansion: rewrite `<alias> <args>` to
    // `<command> <args>` once before lookup. v1 is plain prefix
    // replacement (no $1/$* substitution). One pass only — no recursion
    // into a chain of aliases.
    let expanded = expand_alias(world, player, trimmed);
    let trimmed = expanded.as_deref().unwrap_or(trimmed);

    // Lower-case the input so the registry (which is case-sensitive) matches
    // however the player typed it.
    let lower = trimmed.to_ascii_lowercase();
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    if tokens.is_empty() {
        return;
    }

    // Frozen players: refuse anything except `quit` (always-allowed escape
    // hatch so the player isn't trapped indefinitely if the admin forgets).
    if world.get::<Frozen>(player).is_some() && tokens[0] != "quit" {
        send_to(
            world,
            player,
            "You are frozen by an Implementor and cannot act. \
             Type `quit` to disconnect, or wait to be thawed.\r\n",
        );
        return;
    }

    // Fire COMMAND-flagged triggers in the player's room first.
    // If any trigger returns `false`, the command is consumed (a
    // mob intercepted it) and we stop dispatch here.
    if let Some(located) = world.get::<Located>(player).copied() {
        let cmd_word = tokens[0].to_string();
        let cmd_args = skip_n_tokens(trimmed, 1).to_string();
        if crate::triggers::fire_command_in_room(world, player, located.0, &cmd_word, &cmd_args) {
            return;
        }
    }

    let Some((cmd, n_consumed)) = longest_prefix_match(&tokens) else {
        // Fall through to socials before declaring unknown.
        if try_dispatch_social(world, player, tokens[0], skip_n_tokens(trimmed, 1)) {
            return;
        }
        send_to(
            world,
            player,
            format!("Unknown command: {}\r\n", tokens[0]),
        );
        return;
    };

    // Permission gate. Players check Account.role; mobs (no Account)
    // are allowed Player-level commands only — that's the path used
    // by `order <mob> <cmd>` and by `actor:command()` queued from Lua
    // triggers running on a mob. Admin commands always require an
    // account at the right role + perms.
    let allowed = if let Some(a) = world.get::<Account>(player) {
        a.role.at_least(cmd.min_role)
            && cmd.required_perm.is_none_or(|p| a.perms.contains(&p))
    } else if world.get::<Mob>(player).is_some() {
        cmd.min_role == UserRole::Player && cmd.required_perm.is_none()
    } else {
        false
    };
    if !allowed {
        send_to(world, player, "You can't do that.\r\n");
        return;
    }

    let span = info_span!("cmd", name = cmd.names[0]);
    let _g = span.enter();
    let args = skip_n_tokens(trimmed, n_consumed);
    (cmd.run)(world, player, args);
}

/// If the first whitespace-delimited token of `line` matches one of
/// the player's defined aliases, return a new line with the alias
/// replaced by its expansion. Returns `None` if no expansion applies.
fn expand_alias(world: &World, player: Entity, line: &str) -> Option<String> {
    let aliases = world.get::<mud_world::Aliases>(player)?;
    if aliases.entries.is_empty() {
        return None;
    }
    let mut parts = line.splitn(2, char::is_whitespace);
    let head = parts.next()?;
    let expansion = aliases.get(head)?;
    let rest = parts.next().unwrap_or("");
    if rest.is_empty() {
        Some(expansion.to_string())
    } else {
        Some(format!("{expansion} {rest}"))
    }
}

fn longest_prefix_match(tokens: &[&str]) -> Option<(&'static Command, usize)> {
    let max_n = MAX_NAME_TOKENS.min(tokens.len());
    for n in (1..=max_n).rev() {
        let candidate = if n == 1 {
            tokens[0].to_string()
        } else {
            tokens[..n].join(" ")
        };
        if let Some(cmd) = REGISTRY.get(candidate.as_str()) {
            return Some((cmd, n));
        }
    }
    None
}

fn skip_n_tokens(s: &str, n: usize) -> &str {
    let mut r = s.trim_start();
    for _ in 0..n {
        match r.find(char::is_whitespace) {
            Some(i) => r = r[i..].trim_start(),
            None => return "",
        }
    }
    r
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn send_to(world: &World, target: Entity, text: impl Into<String>) {
    if let Some(conn) = world.get::<Connection>(target) {
        let _ = conn.0.send(text.into().into_bytes());
    }
    // Track for end-of-batch prompt refresh. Tracking mobs (no Connection)
    // is harmless; `flush_prompts` is a no-op for them via `send_prompt`'s
    // Connection lookup.
    PROMPT_RECIPIENTS.with(|r| {
        r.borrow_mut().insert(target);
    });
}

thread_local! {
    /// Recipients of any `send_to` call since the last flush. Drained by
    /// `flush_prompts` after each command-dispatch turn (`login::on_line`)
    /// and after each `schedule.run` (`main`). Single-threaded by the
    /// `current_thread` tokio runtime; `RefCell` is sound here.
    static PROMPT_RECIPIENTS: std::cell::RefCell<std::collections::HashSet<Entity>>
        = std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Drain `LuaOutbox` queued by `room.send(msg)` /
/// `room.send_except(target, msg)` calls during a Lua trigger fire.
/// Each `(room, msg, except)` is broadcast to every player whose
/// `Located.0 == room`, skipping `except` if set. Called from
/// command handlers (`cmd_lua`, `cmd_firetrig`) and the trigger
/// dispatcher after each `exec_for_actor` returns.
pub(crate) fn drain_lua_outbox(world: &mut World) {
    use mud_world::LuaOutbox;
    let (messages, direct, commands) = if world.contains_resource::<LuaOutbox>() {
        let mut out = world.resource_mut::<LuaOutbox>();
        (
            std::mem::take(&mut out.messages),
            std::mem::take(&mut out.direct),
            std::mem::take(&mut out.commands),
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    if messages.is_empty() && direct.is_empty() && commands.is_empty() {
        return;
    }
    // Room broadcasts: snapshot recipients per room so the inner loop
    // doesn't re-borrow World mid-send.
    for (room, msg, except) in messages {
        let mut recipients: Vec<Entity> = Vec::new();
        let mut q = world.query_filtered::<(Entity, &Located), With<Connection>>();
        for (e, l) in q.iter(world) {
            if l.0 == room && Some(e) != except {
                recipients.push(e);
            }
        }
        for r in recipients {
            send_to(world, r, format!("{msg}\r\n"));
        }
    }
    // Direct one-to-one delivery (actor:send). send_to silently no-ops
    // if the target has no Connection (mob targets, disconnected
    // players) — that's the desired behavior.
    for (target, msg) in direct {
        send_to(world, target, format!("{msg}\r\n"));
    }

    // Queued `actor:command(text)` invocations. Re-enters dispatch as
    // if the actor had typed each line. Bounded recursion: any Lua
    // these commands fire pushes onto the outbox again, which is
    // drained by THAT command handler before this loop continues.
    for (actor, line) in commands {
        dispatch(world, actor, &line);
    }
}

/// Add an entity to the pending-prompt set without sending output. Used by
/// `dispatch` so the typing player always gets a prompt — even when the
/// command produced no output (e.g., empty input, silent commands).
pub(crate) fn mark_for_prompt(target: Entity) {
    PROMPT_RECIPIENTS.with(|r| {
        r.borrow_mut().insert(target);
    });
}

/// Send a fresh prompt to everyone who's received output via `send_to` since
/// the last flush. Idempotent — calling on an empty set is free. Despawned
/// entities are skipped via `get_entity`; entities without a Connection are
/// no-ops via `send_prompt`.
pub(crate) fn flush_prompts(world: &World) {
    let recipients =
        PROMPT_RECIPIENTS.with(|r| std::mem::take(&mut *r.borrow_mut()));
    for entity in recipients {
        if world.get_entity(entity).is_ok() {
            send_prompt(world, entity);
        }
    }
}

/// Decide which `ColorMode` a player should see based on their flags.
/// `COLOR_BLIND` opts out to plain text; everyone else gets ANSI.
pub(crate) fn color_mode_for(world: &World, player: Entity) -> ColorMode {
    if has_flag(world, player, PlayerFlag::ColorBlind) {
        ColorMode::Strip
    } else {
        ColorMode::Ansi
    }
}

/// How to handle the `FieryMUD` XML-Lite markup in player-facing strings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ColorMode {
    /// Translate tags to ANSI escape sequences. The default for
    /// color-capable clients.
    Ansi,
    /// Drop every tag, leaving plain text. Used for the `COLOR_BLIND`
    /// flag and for log lines / tests where escape codes would be noise.
    Strip,
}

/// Per-layer style state. Each opening tag pushes one of these to the
/// stack; closes pop. Anonymous tags (`<b:red>` style) keep `name`
/// empty and can only be closed via `</>`. The 8 bool fields map 1:1
/// to ANSI attribute codes (1, 2, 3, 4, 5, 7, 8, 9) — a bitflags type
/// would compile to the same thing, just with an extra dependency.
#[allow(clippy::struct_excessive_bools)]
#[derive(Default, Clone, Debug)]
struct StyleLayer {
    name: String,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    reverse: bool,
    hidden: bool,
    strikethrough: bool,
    /// ANSI foreground code (30–37 for normal, 90–97 for bright).
    fg: Option<u8>,
    /// ANSI background code (40–47 for normal, 100–107 for bright).
    bg: Option<u8>,
}

/// Render `FieryMUD` XML-Lite markup. Stack-based: `<name>` pushes,
/// `</name>` pops the most recent matching layer, `</>` clears the
/// whole stack. Multi-modifier opens (`<b:yellow>`) push an anonymous
/// layer that must be closed with `</>`.
///
/// Supported subset (matches the markup in our seeded content):
/// - Attributes: `b`, `u`, `i`, `s`, `dim`, `blink`, `reverse`, `hide`
/// - Named foreground: red/green/blue/yellow/cyan/purple/magenta/
///   white/black/brown/orange (last two are aliases per the docs)
/// - Named background via `bg-NAME`
///
/// Indexed (`cN` / `bgcN`) and RGB (`#RRGGBB` / `bg#RRGGBB`) tags are
/// not yet implemented — they parse as no-op modifiers (the layer is
/// pushed but contributes nothing). No content in the world uses them.
///
/// Malformed input is tolerated quietly — unterminated `<` swallows
/// the rest of the string, empty `<>` drops cleanly. Both match the
/// previous strip-only behavior.
pub(crate) fn render_color_tags(s: &str, mode: ColorMode) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let mut stack: Vec<StyleLayer> = Vec::new();
    let mut chars = s.chars();

    while let Some(c) = chars.next() {
        if c != '<' {
            out.push(c);
            continue;
        }
        // Read up to the matching `>`. If we hit end-of-input before
        // `>`, drain — matches the historical strip-only behavior.
        let mut tag = String::new();
        let mut closed = false;
        for next in chars.by_ref() {
            if next == '>' {
                closed = true;
                break;
            }
            tag.push(next);
        }
        if !closed {
            break;
        }
        // Only consume `<...>` as a tag if the content actually looks
        // tag-shaped. This is what lets the default prompt template
        // `<%h/%H>` survive: after %-substitution it's `<42/100>`, which
        // contains a `/` mid-content (not the leading-slash close form)
        // and so doesn't match any color-tag shape — we emit it literally.
        if !is_tag_shaped(&tag) {
            out.push('<');
            out.push_str(&tag);
            out.push('>');
            continue;
        }
        if apply_tag(&tag, &mut stack) && mode == ColorMode::Ansi {
            emit_ansi_state(&mut out, &stack);
        }
    }
    if mode == ColorMode::Ansi && !stack.is_empty() {
        out.push_str("\x1b[0m");
    }
    out
}

/// True if `<tag>` looks like an XML-Lite color/style tag — i.e. its
/// contents only contain characters the spec uses (alphanumerics, `:`
/// for modifier separators, `#` for RGB, `-` and `_` for `bg-NAME`-
/// style names) plus an optional leading `/` for close tags. The empty
/// string also returns true to preserve the previous "drop empty `<>`"
/// behavior. Anything else (most importantly `<%h/%H>`-style prompt
/// vars) is treated as literal text.
fn is_tag_shaped(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    let body = if bytes.first() == Some(&b'/') {
        &bytes[1..]
    } else {
        bytes
    };
    body.iter()
        .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'#' | b'_' | b'-'))
}

/// Mutate the style stack in response to one parsed tag. Returns true
/// if the stack changed; the caller uses that to skip a no-op ANSI
/// re-emit (empty `<>`, `</no-such-name>`).
fn apply_tag(tag: &str, stack: &mut Vec<StyleLayer>) -> bool {
    if let Some(name) = tag.strip_prefix('/') {
        if name.is_empty() {
            if stack.is_empty() {
                return false;
            }
            stack.clear();
            return true;
        }
        if let Some(pos) = stack.iter().rposition(|l| l.name == name) {
            stack.truncate(pos);
            return true;
        }
        return false;
    }
    if tag.is_empty() {
        return false;
    }
    let parts: Vec<&str> = tag.split(':').collect();
    let mut layer = StyleLayer {
        // Single-modifier tags are named (closeable via `</name>`);
        // multi-modifier tags are anonymous (only closeable via `</>`).
        name: if parts.len() == 1 { parts[0].to_string() } else { String::new() },
        ..StyleLayer::default()
    };
    for p in parts {
        apply_modifier(&mut layer, p);
    }
    stack.push(layer);
    true
}

fn apply_modifier(layer: &mut StyleLayer, m: &str) {
    match m {
        "b" => layer.bold = true,
        "u" => layer.underline = true,
        "i" => layer.italic = true,
        "s" => layer.strikethrough = true,
        "dim" | "d" => layer.dim = true,
        "blink" => layer.blink = true,
        "reverse" => layer.reverse = true,
        "hide" => layer.hidden = true,
        _ => {
            if let Some(rest) = m.strip_prefix("bg-") {
                if let Some(c) = named_color(rest) {
                    layer.bg = Some(c + 10); // bg ANSI = fg + 10
                }
            } else if let Some(c) = named_color(m) {
                layer.fg = Some(c);
            }
            // Other modifier shapes (cN / #RRGGBB / etc.) parse as
            // no-ops; layer contributes nothing for those positions.
        }
    }
}

/// Map a named color word to its base ANSI foreground code. Aliases
/// (`magenta`/`purple`, `cyan`/`teal`, `brown`/`yellow`, `orange` →
/// bright yellow) follow the `FieryMUD` `XMLLite` docs.
fn named_color(s: &str) -> Option<u8> {
    Some(match s {
        "black" => 30,
        "red" => 31,
        "green" => 32,
        "yellow" | "brown" => 33,
        "blue" => 34,
        "purple" | "magenta" => 35,
        "cyan" | "teal" => 36,
        "white" => 37,
        // Bright variants
        "orange" => 93,
        _ => return None,
    })
}

/// Emit `\x1b[0m` plus the cumulative codes for the merged stack
/// state. Called after every push/pop so the rendered output reflects
/// the active style at that point.
fn emit_ansi_state(out: &mut String, stack: &[StyleLayer]) {
    out.push_str("\x1b[0m");
    if stack.is_empty() {
        return;
    }
    let merged = merge_stack(stack);
    let mut codes: Vec<u8> = Vec::new();
    if merged.bold {
        codes.push(1);
    }
    if merged.dim {
        codes.push(2);
    }
    if merged.italic {
        codes.push(3);
    }
    if merged.underline {
        codes.push(4);
    }
    if merged.blink {
        codes.push(5);
    }
    if merged.reverse {
        codes.push(7);
    }
    if merged.hidden {
        codes.push(8);
    }
    if merged.strikethrough {
        codes.push(9);
    }
    if let Some(fg) = merged.fg {
        codes.push(fg);
    }
    if let Some(bg) = merged.bg {
        codes.push(bg);
    }
    if codes.is_empty() {
        return;
    }
    out.push_str("\x1b[");
    let strs: Vec<String> = codes.iter().map(u8::to_string).collect();
    out.push_str(&strs.join(";"));
    out.push('m');
}

/// Collapse the stack into one effective style: attributes OR-combined,
/// foreground/background = most-recent (deepest layer wins).
fn merge_stack(stack: &[StyleLayer]) -> StyleLayer {
    let mut m = StyleLayer::default();
    for layer in stack {
        m.bold |= layer.bold;
        m.dim |= layer.dim;
        m.italic |= layer.italic;
        m.underline |= layer.underline;
        m.blink |= layer.blink;
        m.reverse |= layer.reverse;
        m.hidden |= layer.hidden;
        m.strikethrough |= layer.strikethrough;
        if layer.fg.is_some() {
            m.fg = layer.fg;
        }
        if layer.bg.is_some() {
            m.bg = layer.bg;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::{
        ColorMode, amount_from_blob, apply_damage, apply_heal_hp, apply_heal_stamina,
        apply_knockdown_posture, check_ability_restrictions, check_target_type, condition_label,
        direction_name, evaluate_formula, evaluate_simple_formula, format_idle, has_effect_named,
        is_being_attacked, is_immobilized, normalize_dice_notation, parse_direction,
        remove_effect_named, render_color_tags, render_prompt, resolve_dispel_filter,
        resolve_dispel_scope, resolve_effect_conditions, resolve_effect_resource,
        resolve_knockdown_posture, resolve_redirect_aggro, sector_movement_cost,
    };
    use bevy_ecs::prelude::*;
    use mud_db::enums::Sector;
    use mud_world::{Health, Stamina};

    fn strip(s: &str) -> String {
        render_color_tags(s, ColorMode::Strip)
    }
    fn ansi(s: &str) -> String {
        render_color_tags(s, ColorMode::Ansi)
    }

    #[test]
    fn render_color_tags_strip_mode_matches_legacy() {
        // No tags: identity.
        assert_eq!(strip("plain text"), "plain text");
        // Single tag pair.
        assert_eq!(strip("<r>red</>"), "red");
        // Multi-modifier open + full reset close.
        assert_eq!(strip("<b:yellow>warning:</> watch out"), "warning: watch out");
        // Unterminated tag: drains rest of string.
        assert_eq!(strip("hello <b:yellow"), "hello ");
        // Empty tags drop cleanly.
        assert_eq!(strip("<>x<>y"), "xy");
    }

    #[test]
    fn render_color_tags_named_color_emits_fg_then_reset() {
        // <green>...</> → \x1b[0m \x1b[32m text \x1b[0m \x1b[0m
        let out = ansi("<green>grass</>");
        assert!(out.contains("\x1b[32m"), "fg green present: {out:?}");
        assert!(out.starts_with("\x1b[0m\x1b[32m"));
        assert!(out.ends_with("\x1b[0m"));
        assert!(out.contains("grass"));
    }

    #[test]
    fn render_color_tags_bold_named() {
        let out = ansi("<b:yellow>warning</>");
        // Bold + fg yellow merged: \x1b[1;33m
        assert!(out.contains("\x1b[1;33m"), "bold+yellow merged: {out:?}");
        assert!(out.contains("warning"));
        assert!(out.ends_with("\x1b[0m"));
    }

    #[test]
    fn render_color_tags_close_named_pops_only_that_layer() {
        // <b><red>X</red>Y</b>: red closes, bold persists for Y.
        let out = ansi("<b><red>X</red>Y</b>");
        // After </red>, state should be just bold (1m). Y rendered with bold only.
        // Easiest assert: substring "Y" preceded by "\x1b[1m" before any 31m for it.
        assert!(out.contains('Y'));
        assert!(out.contains("\x1b[1m"), "bold-only state present: {out:?}");
    }

    #[test]
    fn render_color_tags_full_reset_clears_stack() {
        // </> in the middle should fully reset.
        let out = ansi("<b><red>X</> plain");
        // After </>, should emit a reset and "plain" should NOT be wrapped in any code.
        // We test: "plain" appears in output, and the last escape before "plain" is a reset.
        assert!(out.contains("plain"));
        assert!(out.contains("\x1b[0m plain") || out.contains("\x1b[0mplain"));
    }

    #[test]
    fn render_color_tags_anonymous_open_only_closes_with_full_reset() {
        // <b:red>...</b> shouldn't close — </b> doesn't match anonymous layer.
        // The anonymous layer only closes on </> or end of string.
        let out = ansi("<b:red>X</b>Y");
        // Both X and Y should still be styled (bold+red), since </b> didn't match.
        // We expect the trailing reset at end-of-string.
        assert!(out.contains('X'));
        assert!(out.contains('Y'));
        assert!(out.ends_with("\x1b[0m"));
    }

    #[test]
    fn render_color_tags_unknown_modifier_does_not_panic() {
        // RGB and indexed forms aren't implemented yet — they parse as
        // no-op modifiers (push a layer with no effect).
        let out = ansi("<#ff0000>red?</>");
        // Layer has no fg/bg/attributes, so emit_state produces just \x1b[0m.
        assert!(out.contains("red?"));
    }

    #[test]
    fn render_color_tags_empty_tag_is_dropped() {
        assert_eq!(ansi("<>x<>y"), "xy");
    }

    #[test]
    fn render_color_tags_preserves_prompt_var_shapes() {
        // The default prompt template after %-substitution looks like
        // <42/100> — not tag-shaped (slash mid-content), so emit literally.
        assert_eq!(strip("<42/100>"), "<42/100>");
        assert_eq!(ansi("<42/100>"), "<42/100>");
        // Mixed: a real tag pair around a tag-shaped-but-pseudo content.
        // <green>...</> still renders; the inner <42/100> stays literal.
        let out = ansi("<green><42/100></>");
        assert!(out.contains("<42/100>"), "literal prompt-var preserved: {out:?}");
        assert!(out.contains("\x1b[32m"), "outer green still renders: {out:?}");
    }

    #[test]
    fn render_color_tags_rejects_unknown_punctuation_in_tags() {
        // Spaces aren't valid tag chars per the spec ("no whitespace in tags").
        assert_eq!(strip("<r ed>X</>"), "<r ed>X");
        // '/' mid-content (not the leading-slash close form) means literal.
        assert_eq!(strip("<a/b>X</>"), "<a/b>X");
        // Hash, hyphen, underscore are valid tag chars (RGB / bg- / cN_etc).
        assert_eq!(strip("<#FF0000>red</>"), "red");
        assert_eq!(strip("<bg-red>x</>"), "x");
    }

    #[test]
    fn sector_movement_cost_brackets() {
        // Easy terrain: 1.
        assert_eq!(sector_movement_cost(Sector::City), 1);
        assert_eq!(sector_movement_cost(Sector::Road), 1);
        assert_eq!(sector_movement_cost(Sector::Field), 1);
        // Magical planes: 1 (floating, not walking).
        assert_eq!(sector_movement_cost(Sector::Air), 1);
        assert_eq!(sector_movement_cost(Sector::Astralplane), 1);
        // Standard wilderness: 2.
        assert_eq!(sector_movement_cost(Sector::Forest), 2);
        assert_eq!(sector_movement_cost(Sector::Hills), 2);
        // Slogging: 3.
        assert_eq!(sector_movement_cost(Sector::Mountain), 3);
        assert_eq!(sector_movement_cost(Sector::Swamp), 3);
        // Swimming: 4 / underwater: 6.
        assert_eq!(sector_movement_cost(Sector::Water), 4);
        assert_eq!(sector_movement_cost(Sector::Underwater), 6);
    }

    #[test]
    fn render_prompt_substitutes_hp_and_stamina() {
        let hp = Some(Health { hp: 42, max: 100 });
        let st = Some(Stamina { current: 7, max: 50 });
        let name = Some("Strider");
        let room = Some("The Void");
        let g = Some(12345i64);
        assert_eq!(render_prompt("<%h/%H>", hp, st, name, room, g), "<42/100> ");
        assert_eq!(render_prompt("<%v/%V mv>", hp, st, name, room, g), "<7/50 mv> ");
        assert_eq!(
            render_prompt("<%h/%H %v/%V>", hp, st, name, room, g),
            "<42/100 7/50> "
        );
        // Trailing space already present — don't double-add.
        assert_eq!(render_prompt("<%h> ", hp, st, name, room, g), "<42> ");
        // Literal percent.
        assert_eq!(render_prompt("100%%", hp, st, name, room, g), "100% ");
        // Name substitution.
        assert_eq!(render_prompt("[%n]", hp, st, name, room, g), "[Strider] ");
        // Room substitution.
        assert_eq!(render_prompt("[%r]", hp, st, name, room, g), "[The Void] ");
        // Wealth substitution: raw copper.
        assert_eq!(render_prompt("[%g cp]", hp, st, name, room, g), "[12345 cp] ");
        // Unknown variable: pass through literally so the player sees they
        // typed something we don't implement.
        assert_eq!(render_prompt("[%z]", hp, st, name, room, g), "[%z] ");
        // Missing Health: question marks.
        assert_eq!(render_prompt("<%h/%H>", None, st, name, room, g), "<?/?> ");
        // Missing Stamina: question marks for v/V.
        assert_eq!(render_prompt("<%v/%V>", hp, None, name, room, g), "<?/?> ");
        // Missing name: question mark.
        assert_eq!(render_prompt("[%n]", hp, st, None, room, g), "[?] ");
        // Missing room: question mark.
        assert_eq!(render_prompt("[%r]", hp, st, name, None, g), "[?] ");
        // Missing wealth: question mark.
        assert_eq!(render_prompt("[%g]", hp, st, name, room, None), "[?] ");
        // Empty template still gets a trailing space.
        assert_eq!(render_prompt("", hp, st, name, room, g), " ");
    }

    #[test]
    fn format_idle_picks_a_unit() {
        assert_eq!(format_idle(0), "0s");
        assert_eq!(format_idle(45), "45s");
        assert_eq!(format_idle(60), "1m");
        assert_eq!(format_idle(125), "2m");
        assert_eq!(format_idle(3599), "59m");
        assert_eq!(format_idle(3600), "1h");
        assert_eq!(format_idle(3660), "1h1m");
        assert_eq!(format_idle(7320), "2h2m");
    }

    fn spawn_with_hp(world: &mut World, hp: i32, max: i32) -> Entity {
        world.spawn(Health { hp, max }).id()
    }

    #[test]
    fn apply_damage_reports_thresholds() {
        let mut w = World::new();
        // Max 100 → hurt=50, badly=25, near=10.

        // Crossing only the 50% line: 80 → 40.
        let e = spawn_with_hp(&mut w, 80, 100);
        let (dead, msg) = apply_damage(&mut w, e, 40);
        assert!(!dead);
        assert_eq!(msg, Some("You are hurt.\r\n"));
        assert_eq!(w.get::<Health>(e).unwrap().hp, 40);

        // Crossing only the 25% line: 40 → 20 (already past 50% → no re-fire).
        let e = spawn_with_hp(&mut w, 40, 100);
        let (_, msg) = apply_damage(&mut w, e, 20);
        assert_eq!(msg, Some("You are badly hurt!\r\n"));

        // Crossing only the 10% line.
        let e = spawn_with_hp(&mut w, 20, 100);
        let (_, msg) = apply_damage(&mut w, e, 12);
        assert_eq!(msg, Some("You are near death!\r\n"));

        // Skip-crossing: 80 → 5 should report the deepest band only.
        let e = spawn_with_hp(&mut w, 80, 100);
        let (_, msg) = apply_damage(&mut w, e, 75);
        assert_eq!(msg, Some("You are near death!\r\n"));

        // Lethal blow: dead, no threshold message.
        let e = spawn_with_hp(&mut w, 5, 100);
        let (dead, msg) = apply_damage(&mut w, e, 5);
        assert!(dead);
        assert_eq!(msg, None);

        // No crossing: 90 → 80 (still above 50%).
        let e = spawn_with_hp(&mut w, 90, 100);
        let (_, msg) = apply_damage(&mut w, e, 10);
        assert_eq!(msg, None);

        // Same-band damage: 40 → 30 (already in 25%-50% band, no new line).
        let e = spawn_with_hp(&mut w, 40, 100);
        let (_, msg) = apply_damage(&mut w, e, 10);
        assert_eq!(msg, None);

        // No Health component → no-op.
        let e = w.spawn_empty().id();
        let (dead, msg) = apply_damage(&mut w, e, 10);
        assert!(!dead);
        assert_eq!(msg, None);
    }

    #[test]
    fn condition_label_bands() {
        let h = |hp, max| Health { hp, max };
        // Boundary tests at each cutoff. (hp*100)/max is the pct.
        assert_eq!(condition_label(h(100, 100)), "is in excellent shape"); // 100
        assert_eq!(condition_label(h(86, 100)), "is in excellent shape");
        assert_eq!(condition_label(h(85, 100)), "has some scrapes");
        assert_eq!(condition_label(h(61, 100)), "has some scrapes");
        assert_eq!(condition_label(h(60, 100)), "is bleeding");
        assert_eq!(condition_label(h(36, 100)), "is bleeding");
        assert_eq!(condition_label(h(35, 100)), "is badly hurt");
        assert_eq!(condition_label(h(16, 100)), "is badly hurt");
        assert_eq!(condition_label(h(15, 100)), "is mortally wounded");
        assert_eq!(condition_label(h(1, 100)), "is mortally wounded");
        assert_eq!(condition_label(h(0, 100)), "is dying");
        // Negative HP: dying.
        assert_eq!(condition_label(h(-5, 100)), "is dying");
        // max=0 special: any hp → 0% → dying. Defensive against bad data.
        assert_eq!(condition_label(h(50, 0)), "is dying");
    }

    #[test]
    fn parse_direction_handles_full_words_and_aliases() {
        use mud_db::enums::Direction;
        assert_eq!(parse_direction("north"), Some(Direction::North));
        assert_eq!(parse_direction("n"), Some(Direction::North));
        assert_eq!(parse_direction("NW"), Some(Direction::Northwest));
        assert_eq!(parse_direction("northwest"), Some(Direction::Northwest));
        assert_eq!(parse_direction("up"), Some(Direction::Up));
        assert_eq!(parse_direction("d"), Some(Direction::Down));
        assert_eq!(parse_direction("in"), Some(Direction::In));
        assert_eq!(parse_direction("out"), Some(Direction::Out));
        // Unknown / non-direction input.
        assert_eq!(parse_direction("portal"), None, "Direction::Portal isn't a movement direction");
        assert_eq!(parse_direction(""), None);
        assert_eq!(parse_direction("ne!"), None, "trailing punctuation rejects");
        assert_eq!(parse_direction("sword"), None);
    }

    #[test]
    fn direction_round_trip() {
        use mud_db::enums::Direction;
        // Every direction `direction_name` produces should parse back.
        for d in [
            Direction::North, Direction::South, Direction::East, Direction::West,
            Direction::Up, Direction::Down,
            Direction::Northeast, Direction::Northwest,
            Direction::Southeast, Direction::Southwest,
            Direction::In, Direction::Out,
        ] {
            let name = direction_name(d);
            assert_eq!(parse_direction(name), Some(d), "round-trip {name}");
        }
    }

    // Dispatch-level integration tests. dispatch() writes to a thread-local
    // PROMPT_RECIPIENTS set and may mutate world state through registered
    // command handlers. We focus on observable component state since
    // recipients without a Connection don't actually receive any output.
    use crate::commands::{dispatch, Frozen};
    use mud_db::enums::UserRole;
    use mud_world::{Account, Named, Online, Player, Posture, PostureKind};

    fn spawn_player_for_dispatch(world: &mut World, role: UserRole) -> Entity {
        world
            .spawn((
                Player,
                Online,
                Named { name: "Tester".to_string() },
                Account {
                    user_id: "u".into(),
                    character_id: "c".into(),
                    role,
                    perms: vec![],
                },
                Posture(PostureKind::Sitting),
            ))
            .id()
    }

    #[test]
    fn dispatch_stand_changes_posture() {
        let mut world = World::new();
        let p = spawn_player_for_dispatch(&mut world, UserRole::Player);
        dispatch(&mut world, p, "stand");
        assert_eq!(
            world.get::<Posture>(p).map(|p| p.0),
            Some(PostureKind::Standing),
            "dispatched 'stand' lifted the sitting player"
        );
    }

    #[test]
    fn dispatch_admin_command_refused_for_player_role() {
        let mut world = World::new();
        let p = spawn_player_for_dispatch(&mut world, UserRole::Player);
        // `goto` is Builder+; a plain Player should bounce. No state change
        // since no movement happens; we just verify nothing panics and the
        // posture (irrelevant to goto) is untouched. The "You can't do that."
        // line is sent via a Connection-less send_to and is therefore silent
        // in this harness — registry-gating is the actual coverage here.
        dispatch(&mut world, p, "goto 30 5");
        // No Located component was inserted (cmd_goto would have set one).
        assert!(world.get::<mud_world::Located>(p).is_none());
    }

    #[test]
    fn dispatch_blocks_frozen_player_but_allows_quit() {
        let mut world = World::new();
        let p = spawn_player_for_dispatch(&mut world, UserRole::Player);
        world.get_entity_mut(p).unwrap().insert(Frozen);
        // Attempt 'stand' — should be refused; posture unchanged.
        dispatch(&mut world, p, "stand");
        assert_eq!(
            world.get::<Posture>(p).map(|p| p.0),
            Some(PostureKind::Sitting),
            "frozen player can't change posture"
        );
        // 'quit' is whitelisted but the actual quit handler tries to close
        // a Connection — without one it's effectively a no-op for state.
        // We only verify the gate doesn't panic.
        dispatch(&mut world, p, "quit");
    }

    #[test]
    fn formula_eval_single_term() {
        assert_eq!(evaluate_simple_formula("level", 12, 0), Some(12));
        assert_eq!(evaluate_simple_formula("skill", 0, 250), Some(250));
        assert_eq!(evaluate_simple_formula("7", 0, 0), Some(7));
    }

    #[test]
    fn formula_eval_binary_ops() {
        assert_eq!(evaluate_simple_formula("level * 2", 10, 0), Some(20));
        assert_eq!(evaluate_simple_formula("level * 10", 5, 0), Some(50));
        assert_eq!(evaluate_simple_formula("skill / 4", 0, 100), Some(25));
        assert_eq!(evaluate_simple_formula("level + 3", 10, 0), Some(13));
        assert_eq!(evaluate_simple_formula("level - 1", 10, 0), Some(9));
    }

    #[test]
    fn formula_eval_div_by_zero_returns_none() {
        // Won't divide; falls through to next fallback.
        assert_eq!(evaluate_simple_formula("level / 0", 10, 0), None);
    }

    #[test]
    fn formula_eval_parens_and_multi_op() {
        // Expressions previously rejected now resolve via the recursive
        // descent parser — operator precedence and parens both work.
        assert_eq!(evaluate_simple_formula("(level)", 10, 0), Some(10));
        assert_eq!(evaluate_simple_formula("level * 2 + skill", 10, 5), Some(25));
        assert_eq!(
            evaluate_simple_formula("100 + skill / 5", 0, 25),
            Some(105)
        );
        assert_eq!(
            evaluate_simple_formula("(level + skill) * 2", 3, 4),
            Some(14)
        );
    }

    #[test]
    fn formula_eval_unknown_still_returns_none() {
        // Unknown identifiers and unsupported calls still fall through.
        assert_eq!(evaluate_simple_formula("base_damage + skill", 10, 5), None);
        // pow() is now supported (see formula_eval_pow_with_float_exp).
        assert_eq!(evaluate_simple_formula("foo(1, 2)", 0, 0), None);
        // Malformed: dangling operator.
        assert_eq!(evaluate_simple_formula("level +", 10, 0), None);
        assert_eq!(evaluate_simple_formula("(level", 10, 0), None);
    }

    #[test]
    fn formula_eval_pow_with_float_exp() {
        let mut det = |_name: &str, _n: i32, _m: i32| 0;
        // Integer base, float exp: pow(8, 2) = 64
        assert_eq!(evaluate_formula("pow(skill, 2)", &super::FormulaCtx::base(0, 8), &mut det), Some(64));
        // Float exp: pow(50, 1.44) ≈ 50^1.44 ≈ 297.something
        let r = evaluate_formula("pow(skill, 1.44)", &super::FormulaCtx::base(0, 50), &mut det).unwrap();
        #[allow(clippy::cast_possible_truncation)]
        let expected = (50f64).powf(1.44).round() as i32;
        assert_eq!(r, expected);
        // Composite: roll_dice(8, 25) + pow(skill, 1.44) — substitute
        // deterministic dice. dice closure returns 0; 0 + pow(0, 1.44) = 0
        // (0^anything = 0 by convention).
        assert_eq!(
            evaluate_formula("roll_dice(8, 25) + pow(skill, 1.44)", &super::FormulaCtx::base(0, 0), &mut det),
            Some(0)
        );
        // amount_from_blob uses the live RNG for roll_dice; verify it
        // returns *something* in the plausible range for skill=0
        // (8d25 = 8..200, pow(0, 1.44) = 0).
        let blob = serde_json::json!({"amount": "roll_dice(8, 25) + pow(skill, 1.44)"});
        let v = amount_from_blob(Some(&blob), &super::FormulaCtx::base(0, 0)).expect("formula resolves");
        assert!((8..=200).contains(&v), "8d25 result {v} in range");
        // Float literal outside pow → unsupported, returns None.
        assert_eq!(evaluate_formula("1.5 + skill", &super::FormulaCtx::base(0, 5), &mut det), None);
        // Malformed pow (missing exp) → None.
        assert_eq!(evaluate_formula("pow(skill,)", &super::FormulaCtx::base(0, 5), &mut det), None);
        assert_eq!(evaluate_formula("pow(skill", &super::FormulaCtx::base(0, 5), &mut det), None);
    }

    #[test]
    fn formula_eval_recognizes_caster_symbols() {
        use super::FormulaCtx;
        let mut zero = |_name: &str, _a: i32, _b: i32| 0;
        // weapon_damage symbol resolves from ctx.
        let ctx = FormulaCtx {
            level: 10,
            skill: 50,
            weapon_damage: 12,
            ..FormulaCtx::default()
        };
        // BACKSTAB-style: weapon_damage * (2 + skill / 25)
        // = 12 * (2 + 2) = 48
        assert_eq!(
            evaluate_formula("weapon_damage * (2 + skill / 25)", &ctx, &mut zero),
            Some(48)
        );
        // Stat bonuses + their short aliases.
        let ctx = FormulaCtx {
            level: 10,
            skill: 30,
            str_bonus: 3,
            dex_bonus: 2,
            con_bonus: 1,
            int_bonus: 4,
            wis_bonus: 5,
            cha_bonus: -1,
            ..FormulaCtx::default()
        };
        // BASH-style: skill / 3 + str_bonus = 10 + 3 = 13
        assert_eq!(
            evaluate_formula("skill / 3 + str_bonus", &ctx, &mut zero),
            Some(13)
        );
        // KICK-style: level + dex_bonus + skill / 4 = 10 + 2 + 7 = 19
        assert_eq!(
            evaluate_formula("level + dex_bonus + skill / 4", &ctx, &mut zero),
            Some(19)
        );
        // Short aliases match.
        assert_eq!(evaluate_formula("str + dex", &ctx, &mut zero), Some(5));
        assert_eq!(evaluate_formula("wis + cha", &ctx, &mut zero), Some(4));
        // Unrecognized symbol still returns None.
        assert_eq!(
            evaluate_formula("base_damage + 5", &ctx, &mut zero),
            None
        );
        // hidden symbol resolves from ctx.hidden (0/1 from Stealth marker presence).
        let mut ctx_hidden = FormulaCtx {
            level: 10,
            skill: 50,
            ..FormulaCtx::default()
        };
        ctx_hidden.hidden = 1;
        // BACKSTAB's bonusIfHidden formula: hidden * 0.5 — but our
        // evaluator is integer-only; use multiplicative integer form.
        assert_eq!(evaluate_formula("hidden", &ctx_hidden, &mut zero), Some(1));
        assert_eq!(
            evaluate_formula("skill * hidden", &ctx_hidden, &mut zero),
            Some(50)
        );
        // Without Stealth marker, hidden=0.
        let ctx_open = FormulaCtx { level: 10, skill: 50, ..FormulaCtx::default() };
        assert_eq!(evaluate_formula("hidden", &ctx_open, &mut zero), Some(0));
        assert_eq!(
            evaluate_formula("skill * hidden", &ctx_open, &mut zero),
            Some(0)
        );
    }

    #[test]
    fn core_stats_bonus_d_n_d_style() {
        use mud_world::CoreStats;
        // Standard D&D bonuses: (score - 10) / 2 with truncation toward 0.
        assert_eq!(CoreStats::bonus(10), 0);
        assert_eq!(CoreStats::bonus(11), 0);
        assert_eq!(CoreStats::bonus(12), 1);
        assert_eq!(CoreStats::bonus(13), 1);
        assert_eq!(CoreStats::bonus(18), 4);
        assert_eq!(CoreStats::bonus(20), 5);
        assert_eq!(CoreStats::bonus(8), -1);
        assert_eq!(CoreStats::bonus(3), -3);
    }

    #[test]
    fn formula_eval_random_dispatched_by_name() {
        // Deterministic stub by name: random → 42, everything else 0.
        let mut stub = |name: &str, _a: i32, _b: i32| {
            if name == "random" { 42 } else { 0 }
        };
        assert_eq!(evaluate_formula("random(1, 10)", &super::FormulaCtx::base(0, 0), &mut stub), Some(42));
        // Composite: skill + random(1, skill*2). With skill=10:
        // 10 + 42 = 52 (stub returns 42 for any random).
        assert_eq!(
            evaluate_formula("skill + random(1, skill * 2)", &super::FormulaCtx::base(0, 10), &mut stub),
            Some(52)
        );
        // Backwards range refused → falls through.
        let mut zero = |_name: &str, _a: i32, _b: i32| 0;
        assert_eq!(evaluate_formula("random(10, 5)", &super::FormulaCtx::base(0, 0), &mut zero), None);
    }

    #[test]
    fn formula_eval_roll_dice_uses_callback() {
        // Deterministic dice closure: every roll_dice(N, M) returns N * M.
        // Deterministic stub: roll_dice/random both return n * m so
        // tests are reproducible.
        let mut det = |_name: &str, n: i32, m: i32| n * m;
        assert_eq!(evaluate_formula("roll_dice(2, 9)", &super::FormulaCtx::base(0, 0), &mut det), Some(18));
        // Precedence: roll_dice + skill / 5 with skill=25 → 18 + 5 = 23
        assert_eq!(
            evaluate_formula("roll_dice(2, 9) + skill / 5", &super::FormulaCtx::base(0, 25), &mut det),
            Some(23)
        );
        // The dice-notation normalizer rewrites NdM → roll_dice(N, M)
        // before evaluation. `1d8` with the same stub is 8.
        assert_eq!(amount_blob_eval("1d8", &super::FormulaCtx::base(0, 0), &mut det), Some(8));
        // Constant `100 + 1d8 + skill / 5` with skill=20 is 100 + 8 + 4 = 112.
        assert_eq!(
            amount_blob_eval("100 + 1d8 + skill / 5", &super::FormulaCtx::base(0, 20), &mut det),
            Some(112)
        );
    }

    fn amount_blob_eval(
        s: &str,
        ctx: &super::FormulaCtx,
        rng_call: &mut dyn FnMut(&str, i32, i32) -> i32,
    ) -> Option<i32> {
        evaluate_formula(&normalize_dice_notation(s), ctx, rng_call)
    }

    #[test]
    fn dice_notation_normalizer_rewrites_n_d_m() {
        assert_eq!(normalize_dice_notation("1d8"), "roll_dice(1, 8)");
        assert_eq!(normalize_dice_notation("2D6"), "roll_dice(2, 6)");
        assert_eq!(
            normalize_dice_notation("100 + 2d9 + skill / 5"),
            "100 + roll_dice(2, 9) + skill / 5"
        );
        // Bare number untouched; no `d<digits>` pattern.
        assert_eq!(normalize_dice_notation("100 + skill"), "100 + skill");
    }

    #[test]
    fn amount_from_blob_reads_override_then_default() {
        // Override-priority: amount=42 wins.
        let blob = serde_json::json!({"amount": 42});
        assert_eq!(amount_from_blob(Some(&blob), &super::FormulaCtx::base(0, 0)), Some(42));
        // String formula with skill substitution.
        let blob = serde_json::json!({"amount": "skill / 4"});
        assert_eq!(amount_from_blob(Some(&blob), &super::FormulaCtx::base(0, 100)), Some(25));
        // Missing field → None (caller falls through).
        let blob = serde_json::json!({"duration": 5});
        assert_eq!(amount_from_blob(Some(&blob), &super::FormulaCtx::base(0, 0)), None);
    }

    use mud_world::{AppliedTo, EffectInstance, EffectSource};

    fn spawn_effect_named(world: &mut World, target: Entity, name: &str) -> Entity {
        world
            .spawn((
                EffectInstance {
                    kind: 0,
                    name: name.to_string(),
                    strength: 1,
                    remaining_secs: 30,
                    source: EffectSource::Other("test".to_string()),
                    ability_id: None,
                },
                AppliedTo(target),
            ))
            .id()
    }

    #[test]
    fn has_effect_named_true_when_present() {
        let mut world = World::new();
        let target = world.spawn(()).id();
        spawn_effect_named(&mut world, target, "bleed");
        assert!(has_effect_named(&mut world, target, "bleed"));
        assert!(has_effect_named(&mut world, target, "BLEED"));
        assert!(!has_effect_named(&mut world, target, "blind"));
    }

    #[test]
    fn has_effect_named_false_when_target_differs() {
        let mut world = World::new();
        let target_a = world.spawn(()).id();
        let target_b = world.spawn(()).id();
        spawn_effect_named(&mut world, target_a, "bleed");
        assert!(has_effect_named(&mut world, target_a, "bleed"));
        assert!(!has_effect_named(&mut world, target_b, "bleed"));
    }

    #[test]
    fn apply_heal_hp_caps_at_max() {
        let mut world = World::new();
        let target = world.spawn(Health { hp: 50, max: 100 }).id();
        let healed = apply_heal_hp(&mut world, target, 30);
        assert_eq!(healed, 30);
        assert_eq!(world.get::<Health>(target).unwrap().hp, 80);
        // Overheal: only fills to max.
        let healed = apply_heal_hp(&mut world, target, 50);
        assert_eq!(healed, 20);
        assert_eq!(world.get::<Health>(target).unwrap().hp, 100);
        // Already-full: no-op.
        let healed = apply_heal_hp(&mut world, target, 25);
        assert_eq!(healed, 0);
        assert_eq!(world.get::<Health>(target).unwrap().hp, 100);
    }

    #[test]
    fn apply_heal_hp_ignores_nonpositive() {
        let mut world = World::new();
        let target = world.spawn(Health { hp: 50, max: 100 }).id();
        assert_eq!(apply_heal_hp(&mut world, target, 0), 0);
        assert_eq!(apply_heal_hp(&mut world, target, -10), 0);
        assert_eq!(world.get::<Health>(target).unwrap().hp, 50);
    }

    #[test]
    fn apply_heal_hp_returns_zero_when_no_health() {
        let mut world = World::new();
        let target = world.spawn(()).id();
        assert_eq!(apply_heal_hp(&mut world, target, 30), 0);
    }

    #[test]
    fn apply_heal_stamina_caps_at_max() {
        let mut world = World::new();
        let target = world.spawn(Stamina { current: 20, max: 50 }).id();
        let healed = apply_heal_stamina(&mut world, target, 100);
        assert_eq!(healed, 30);
        assert_eq!(world.get::<Stamina>(target).unwrap().current, 50);
    }

    #[test]
    fn resolve_knockdown_posture_defaults_to_sitting() {
        use mud_world::PostureKind;
        // No params at all → Sitting (default).
        assert_eq!(resolve_knockdown_posture(None, None), PostureKind::Sitting);
        // Default params with target=resting → Resting.
        let default_p = serde_json::json!({"target": "resting"});
        assert_eq!(
            resolve_knockdown_posture(None, Some(&default_p)),
            PostureKind::Resting
        );
        // Override wins. Target=sitting overrides default=resting.
        let override_p = serde_json::json!({"target": "sitting"});
        assert_eq!(
            resolve_knockdown_posture(Some(&override_p), Some(&default_p)),
            PostureKind::Sitting
        );
        // Unknown target name falls through to Sitting.
        let bogus = serde_json::json!({"target": "floor"});
        assert_eq!(resolve_knockdown_posture(Some(&bogus), None), PostureKind::Sitting);
    }

    #[test]
    fn apply_knockdown_posture_only_downgrades() {
        use mud_world::{Posture, PostureKind};
        let mut world = World::new();
        let standing = world.spawn(Posture(PostureKind::Standing)).id();
        let already_sitting = world.spawn(Posture(PostureKind::Sitting)).id();
        let resting = world.spawn(Posture(PostureKind::Resting)).id();

        // Standing → Sitting: change.
        assert!(apply_knockdown_posture(&mut world, standing, PostureKind::Sitting));
        assert_eq!(
            world.get::<Posture>(standing).map(|p| p.0),
            Some(PostureKind::Sitting)
        );
        // Sitting → Sitting: no-op.
        assert!(!apply_knockdown_posture(&mut world, already_sitting, PostureKind::Sitting));
        assert_eq!(
            world.get::<Posture>(already_sitting).map(|p| p.0),
            Some(PostureKind::Sitting)
        );
        // Resting → Sitting: would be an UPGRADE, refuse.
        assert!(!apply_knockdown_posture(&mut world, resting, PostureKind::Sitting));
        assert_eq!(
            world.get::<Posture>(resting).map(|p| p.0),
            Some(PostureKind::Resting)
        );
        // Sitting → Resting: legitimate further knockdown.
        assert!(apply_knockdown_posture(
            &mut world,
            already_sitting,
            PostureKind::Resting
        ));
        assert_eq!(
            world.get::<Posture>(already_sitting).map(|p| p.0),
            Some(PostureKind::Resting)
        );
    }

    #[test]
    fn resolve_dispel_filter_lowercases_with_override_priority() {
        // No params → empty.
        assert_eq!(resolve_dispel_filter(None, None), "");
        // Default-only.
        let default_p = serde_json::json!({"filter": "Magic"});
        assert_eq!(resolve_dispel_filter(None, Some(&default_p)), "magic");
        // Override wins.
        let override_p = serde_json::json!({"filter": "BUFF"});
        assert_eq!(
            resolve_dispel_filter(Some(&override_p), Some(&default_p)),
            "buff"
        );
    }

    #[test]
    fn resolve_dispel_scope_defaults_to_all() {
        use super::DispelScope;
        // Default to All when missing.
        assert!(matches!(resolve_dispel_scope(None, None), DispelScope::All));
        // "first" → First.
        let first = serde_json::json!({"scope": "first"});
        assert!(matches!(
            resolve_dispel_scope(Some(&first), None),
            DispelScope::First
        ));
        // Anything else (typo, "all", "everything") → All.
        let bogus = serde_json::json!({"scope": "everything"});
        assert!(matches!(
            resolve_dispel_scope(Some(&bogus), None),
            DispelScope::All
        ));
    }

    #[test]
    fn resolve_redirect_aggro_defaults_false_picks_override() {
        // No params → default false (damage redirect — not implemented).
        assert!(!resolve_redirect_aggro(None, None));
        // Default with aggro=true — works without an override.
        let default_p = serde_json::json!({"aggro": true});
        assert!(resolve_redirect_aggro(None, Some(&default_p)));
        // Override wins. Override false → false even if default true.
        let override_p = serde_json::json!({"aggro": false});
        assert!(!resolve_redirect_aggro(Some(&override_p), Some(&default_p)));
        // Non-bool aggro field → falls through to default.
        let bogus = serde_json::json!({"aggro": "yes"});
        assert!(resolve_redirect_aggro(Some(&bogus), Some(&default_p)));
    }

    #[test]
    fn target_type_enemy_pc_refuses_self_and_mob() {
        use mud_world::{Mob, Player};
        let mut world = World::new();
        let caster = world.spawn(Player).id();
        let other_player = world.spawn(Player).id();
        let mob = world.spawn(Mob).id();
        let valid: Vec<String> = vec!["ENEMY_PC".to_string()];
        // Other player → passes.
        assert_eq!(check_target_type(&mut world, caster, other_player, &valid), None);
        // Self → refused.
        assert!(check_target_type(&mut world, caster, caster, &valid).is_some());
        // Mob → refused (not a Player).
        assert!(check_target_type(&mut world, caster, mob, &valid).is_some());
    }

    #[test]
    fn target_type_enemy_npc_refuses_player() {
        use mud_world::{Mob, Player};
        let mut world = World::new();
        let caster = world.spawn(Player).id();
        let other_player = world.spawn(Player).id();
        let mob = world.spawn(Mob).id();
        let valid: Vec<String> = vec!["ENEMY_NPC".to_string()];
        // Mob → passes.
        assert_eq!(check_target_type(&mut world, caster, mob, &valid), None);
        // Other player → refused.
        assert!(check_target_type(&mut world, caster, other_player, &valid).is_some());
    }

    #[test]
    fn target_type_or_semantics() {
        use mud_world::{Mob, Player};
        let mut world = World::new();
        let caster = world.spawn(Player).id();
        let other_player = world.spawn(Player).id();
        let mob = world.spawn(Mob).id();
        let valid: Vec<String> = vec!["ENEMY_PC".to_string(), "ENEMY_NPC".to_string()];
        // Either passes.
        assert_eq!(check_target_type(&mut world, caster, mob, &valid), None);
        assert_eq!(check_target_type(&mut world, caster, other_player, &valid), None);
        // Self still refused (ENEMY_PC excludes self; ENEMY_NPC requires Mob).
        assert!(check_target_type(&mut world, caster, caster, &valid).is_some());
    }

    #[test]
    fn target_type_unrecognized_kind_passes_silently() {
        let mut world = World::new();
        let caster = world.spawn(()).id();
        let target = world.spawn(()).id();
        // CORPSE / UNCONSCIOUS aren't yet modeled; they pass silently
        // so DRAG / RESURRECT aren't blocked.
        let valid: Vec<String> = vec!["CORPSE".to_string()];
        assert_eq!(check_target_type(&mut world, caster, target, &valid), None);
        let valid: Vec<String> = vec!["UNCONSCIOUS".to_string()];
        assert_eq!(check_target_type(&mut world, caster, target, &valid), None);
    }

    #[test]
    fn target_type_empty_list_passes() {
        let mut world = World::new();
        let caster = world.spawn(()).id();
        let target = world.spawn(()).id();
        let valid: Vec<String> = vec![];
        assert_eq!(check_target_type(&mut world, caster, target, &valid), None);
    }

    #[test]
    fn restriction_alignment_prohibits_evil_caster() {
        use mud_world::CombatStats;
        let mut world = World::new();
        let evil = world.spawn(CombatStats { alignment: -500, ..Default::default() }).id();
        let neutral = world.spawn(CombatStats::default()).id();
        let dummy = world.spawn(()).id();
        let rule = serde_json::json!([{
            "type": "alignment",
            "target": "caster",
            "value": "evil",
            "prohibited": true,
            "message": "The gods reject you.",
        }]);
        let rules: Vec<serde_json::Value> = rule.as_array().unwrap().clone();
        // Evil caster: refused.
        let r = check_ability_restrictions(&mut world, evil, dummy, &rules);
        assert_eq!(r.as_deref(), Some("The gods reject you."));
        // Neutral caster: passes.
        let r = check_ability_restrictions(&mut world, neutral, dummy, &rules);
        assert_eq!(r, None);
    }

    #[test]
    fn restriction_alignment_required_target() {
        use mud_world::CombatStats;
        let mut world = World::new();
        let caster = world.spawn(()).id();
        let undead = world.spawn(CombatStats { alignment: -500, ..Default::default() }).id();
        let good = world.spawn(CombatStats { alignment: 500, ..Default::default() }).id();
        let rules: Vec<serde_json::Value> = vec![serde_json::json!({
            "type": "alignment",
            "target": "victim",
            "value": "evil",
            "required": true,
            "message": "Target must be evil.",
        })];
        // Evil target: passes.
        assert_eq!(check_ability_restrictions(&mut world, caster, undead, &rules), None);
        // Good target: refused.
        let r = check_ability_restrictions(&mut world, caster, good, &rules);
        assert_eq!(r.as_deref(), Some("Target must be evil."));
    }

    #[test]
    fn restriction_unknown_rule_passes() {
        let mut world = World::new();
        let caster = world.spawn(()).id();
        let target = world.spawn(()).id();
        let rules: Vec<serde_json::Value> = vec![serde_json::json!({
            "type": "future_unknown_check",
            "message": "Should not appear.",
        })];
        // Unknown type → pass.
        assert_eq!(
            check_ability_restrictions(&mut world, caster, target, &rules),
            None
        );
    }

    #[test]
    fn restriction_not_immobilized_detects_stun_and_effects() {
        use mud_world::Stunned;
        let mut world = World::new();
        let caster_a = world.spawn(()).id();
        let caster_b = world.spawn(()).id();
        let target = world.spawn(()).id();
        // Caster A: stunned (marker present).
        world.entity_mut(caster_a).insert(Stunned);
        assert!(is_immobilized(&mut world, caster_a));
        // Caster B: spawn a paralysis effect targeting B.
        spawn_effect_named(&mut world, caster_b, "paralysis");
        assert!(is_immobilized(&mut world, caster_b));
        // Free caster: no stun, no immobilizers.
        let free = world.spawn(()).id();
        assert!(!is_immobilized(&mut world, free));
        // Now wire it through the rules evaluator: caster_a refused.
        let rules: Vec<serde_json::Value> = vec![serde_json::json!({
            "type": "not_immobilized",
            "message": "You can't move!",
        })];
        let r = check_ability_restrictions(&mut world, caster_a, target, &rules);
        assert_eq!(r.as_deref(), Some("You can't move!"));
        // Free caster passes.
        let r = check_ability_restrictions(&mut world, free, target, &rules);
        assert_eq!(r, None);
    }

    #[test]
    fn restriction_not_tanking_refuses_when_attacked() {
        use mud_world::Fighting;
        let mut world = World::new();
        let caster = world.spawn(()).id();
        let attacker = world.spawn(Fighting(caster)).id();
        let target = world.spawn(()).id();
        let rules: Vec<serde_json::Value> = vec![serde_json::json!({
            "type": "not_tanking",
            "message": "You're being attacked!",
        })];
        let r = check_ability_restrictions(&mut world, caster, target, &rules);
        assert_eq!(r.as_deref(), Some("You're being attacked!"));
        // Sanity: helper agrees.
        assert!(is_being_attacked(&mut world, caster));
        // Despawn the attacker — caster passes.
        world.entity_mut(attacker).despawn();
        let r = check_ability_restrictions(&mut world, caster, target, &rules);
        assert_eq!(r, None);
    }

    #[test]
    fn try_remove_fighting_clears_component() {
        use crate::commands::try_remove;
        use mud_world::Fighting;
        let mut world = World::new();
        let foe = world.spawn(()).id();
        let me = world.spawn(Fighting(foe)).id();
        assert!(world.get::<Fighting>(me).is_some());
        try_remove::<Fighting>(&mut world, me);
        assert!(world.get::<Fighting>(me).is_none());
        // Removing again is a no-op.
        try_remove::<Fighting>(&mut world, me);
        assert!(world.get::<Fighting>(me).is_none());
    }

    #[test]
    fn apply_knockdown_posture_no_component_is_noop() {
        let mut world = World::new();
        let target = world.spawn(()).id();
        assert!(!apply_knockdown_posture(
            &mut world,
            target,
            mud_world::PostureKind::Sitting
        ));
    }

    #[test]
    fn resolve_effect_conditions_string_or_array() {
        let s_blob = serde_json::json!({"condition": "Poison"});
        assert_eq!(
            resolve_effect_conditions(Some(&s_blob), None),
            vec!["poison".to_string()]
        );
        let arr_blob = serde_json::json!({"condition": ["bleed", "POISON", "curse"]});
        assert_eq!(
            resolve_effect_conditions(Some(&arr_blob), None),
            vec!["bleed".to_string(), "poison".to_string(), "curse".to_string()]
        );
        // Default-only fallback still works.
        let default = serde_json::json!({"condition": "all"});
        assert_eq!(
            resolve_effect_conditions(None, Some(&default)),
            vec!["all".to_string()]
        );
        // Override missing the field falls through to default.
        let override_p = serde_json::json!({"resource": "hp"});
        assert_eq!(
            resolve_effect_conditions(Some(&override_p), Some(&default)),
            vec!["all".to_string()]
        );
        // Both missing → empty.
        let blob = serde_json::json!({});
        assert_eq!(resolve_effect_conditions(Some(&blob), Some(&blob)), Vec::<String>::new());
    }

    #[test]
    fn resolve_effect_resource_picks_override_first() {
        let override_p = serde_json::json!({"resource": "Move"});
        let default_p = serde_json::json!({"resource": "hp"});
        // Override wins, lowercased.
        assert_eq!(
            resolve_effect_resource(Some(&override_p), Some(&default_p)),
            "move"
        );
        // No override → default.
        assert_eq!(
            resolve_effect_resource(None, Some(&default_p)),
            "hp"
        );
        // Neither → default to "hp".
        assert_eq!(resolve_effect_resource(None, None), "hp");
    }

    #[test]
    fn remove_effect_named_despawns_matches() {
        let mut world = World::new();
        let target = world.spawn(()).id();
        let bleed_a = spawn_effect_named(&mut world, target, "bleed");
        let bleed_b = spawn_effect_named(&mut world, target, "bleed");
        let blind = spawn_effect_named(&mut world, target, "blind");
        assert_eq!(remove_effect_named(&mut world, target, "bleed"), 2);
        assert!(world.get_entity(bleed_a).is_err(), "bleed_a despawned");
        assert!(world.get_entity(bleed_b).is_err(), "bleed_b despawned");
        assert!(world.get_entity(blind).is_ok(), "blind survives");
    }

    #[test]
    fn remove_effect_named_returns_zero_when_no_match() {
        let mut world = World::new();
        let target = world.spawn(()).id();
        spawn_effect_named(&mut world, target, "bleed");
        assert_eq!(remove_effect_named(&mut world, target, "blind"), 0);
    }

    #[test]
    fn remove_all_effects_on_despawns_every_applied_effect() {
        use super::remove_all_effects_on;
        let mut world = World::new();
        let target = world.spawn(()).id();
        let other = world.spawn(()).id();
        let a = spawn_effect_named(&mut world, target, "bleed");
        let b = spawn_effect_named(&mut world, target, "poison");
        let c = spawn_effect_named(&mut world, target, "curse");
        let untouched = spawn_effect_named(&mut world, other, "bleed");
        assert_eq!(remove_all_effects_on(&mut world, target), 3);
        assert!(world.get_entity(a).is_err());
        assert!(world.get_entity(b).is_err());
        assert!(world.get_entity(c).is_err());
        assert!(
            world.get_entity(untouched).is_ok(),
            "effects on other entities are untouched"
        );
    }
}

/// Send the player's prompt template with variables substituted. Falls back
/// to a sensible default if no Prompt component is attached or the template
/// is empty.
pub(crate) fn send_prompt(world: &World, target: Entity) {
    let Some(conn) = world.get::<Connection>(target) else {
        return;
    };
    let template = world
        .get::<Prompt>(target)
        .map(|p| p.0.as_str())
        .filter(|t| !t.is_empty())
        .unwrap_or("<%h/%H> ");
    let hp = world.get::<Health>(target).copied();
    let stamina = world.get::<Stamina>(target).copied();
    let name = world.get::<Named>(target).map(|n| n.name.as_str());
    let room = world
        .get::<Located>(target)
        .and_then(|l| world.get::<Named>(l.0))
        .map(|n| n.name.as_str());
    let wealth = world.get::<Wealth>(target).map(|w| w.0);
    let rendered = render_prompt(template, hp, stamina, name, room, wealth);
    // Prompts can carry color tags both directly in the template
    // (`prompt <red>%h</>`) and indirectly via %r / %n (room and player
    // names that may have embedded tags). render_color_tags handles
    // both — and is_tag_shaped lets the default `<%h/%H>` survive
    // since `<42/100>` isn't tag-shaped after %-substitution.
    let mode = color_mode_for(world, target);
    let _ = conn.0.send(render_color_tags(&rendered, mode).into_bytes());

    // Piggyback Char.Vitals on the prompt cadence — same once-per-
    // command frequency, which is reasonable for HUD-style clients.
    // Mudlet / MUSHclient parse the GMCP frame; plain telnet clients
    // see the IAC bytes as garbage which most terminal emulators
    // strip (they're outside the ASCII range). A future commit will
    // add inbound IAC parsing and gate the push on the client
    // confirming `IAC DO 201`.
    if let (Some(h), Some(s)) = (hp, stamina) {
        let level = world.get::<Profile>(target).map_or(0, |p| p.level);
        let payload = format!(
            "{{\"hp\":{},\"max_hp\":{},\"sp\":{},\"max_sp\":{},\"level\":{}}}",
            h.hp, h.max, s.current, s.max, level
        );
        let _ = conn.0.send(mud_net::gmcp_packet("Char.Vitals", &payload));
    }
}

fn render_prompt(
    template: &str,
    hp: Option<Health>,
    stamina: Option<Stamina>,
    name: Option<&str>,
    room: Option<&str>,
    wealth: Option<i64>,
) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('h') => match hp {
                    Some(hp) => out.push_str(&hp.hp.to_string()),
                    None => out.push('?'),
                },
                Some('H') => match hp {
                    Some(hp) => out.push_str(&hp.max.to_string()),
                    None => out.push('?'),
                },
                Some('v') => match stamina {
                    Some(s) => out.push_str(&s.current.to_string()),
                    None => out.push('?'),
                },
                Some('V') => match stamina {
                    Some(s) => out.push_str(&s.max.to_string()),
                    None => out.push('?'),
                },
                Some('n') => match name {
                    Some(n) => out.push_str(n),
                    None => out.push('?'),
                },
                Some('r') => match room {
                    Some(r) => out.push_str(r),
                    None => out.push('?'),
                },
                // %g = on-hand wealth in copper (raw integer; players
                // do their own math). Skipped denomination split here
                // because the prompt is a tight one-line readout.
                Some('g') => match wealth {
                    Some(w) => out.push_str(&w.to_string()),
                    None => out.push('?'),
                },
                Some('%') | None => out.push('%'),
                Some(other) => {
                    // Unknown variable: leave the literal `%X` so it's
                    // visible the template wants something we don't yet
                    // implement.
                    out.push('%');
                    out.push(other);
                }
            }
        } else {
            out.push(c);
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

fn has_flag(world: &World, entity: Entity, flag: PlayerFlag) -> bool {
    world
        .get::<PlayerFlags>(entity)
        .is_some_and(|f| f.has(flag))
}

// ---------------------------------------------------------------------------
// Info handlers
// ---------------------------------------------------------------------------

fn cmd_help(world: &mut World, player: Entity, args: &str) {
    let (role, perms) = world
        .get::<Account>(player)
        .map_or((UserRole::Player, Vec::new()), |a| (a.role, a.perms.clone()));

    let topic = args.trim().to_ascii_lowercase();
    if topic.is_empty() {
        let mut by_cat: HashMap<Category, Vec<&Command>> = HashMap::new();
        for cmd in COMMANDS {
            if !visible(cmd, role, &perms) {
                continue;
            }
            by_cat.entry(cmd.category).or_default().push(cmd);
        }
        let mut out = String::from("\r\nAvailable commands:\r\n");
        for cat in Category::ORDER {
            if let Some(cmds) = by_cat.get(cat) {
                out.push_str(&format!("\r\n  {}\r\n", cat.label()));
                let mut names: Vec<&str> = cmds.iter().map(|c| c.names[0]).collect();
                names.sort_unstable();
                out.push_str(&format!("    {}\r\n", names.join(", ")));
            }
        }
        out.push_str("\r\nType `help <command>` for details.\r\n");
        send_to(world, player, out);
        return;
    }

    if let Some(cmd) = REGISTRY.get(topic.as_str()) {
        if !visible(cmd, role, &perms) {
            send_to(world, player, format!("No help on '{topic}'.\r\n"));
            return;
        }
        let mut out = format!("\r\n{}\r\n", cmd.names[0]);
        out.push_str(&format!("\r\n  {}\r\n", cmd.help.summary));
        out.push_str(&format!("\r\n  Usage: {}\r\n", cmd.help.usage));
        if !cmd.help.long.is_empty() {
            out.push_str(&format!("\r\n  {}\r\n", cmd.help.long));
        }
        if cmd.names.len() > 1 {
            out.push_str(&format!("\r\n  Aliases: {}\r\n", cmd.names[1..].join(", ")));
        }
        send_to(world, player, out);
    } else {
        send_to(world, player, format!("No help on '{topic}'.\r\n"));
    }
}

fn visible(cmd: &Command, role: UserRole, perms: &[Permission]) -> bool {
    role.at_least(cmd.min_role) && cmd.required_perm.is_none_or(|p| perms.contains(&p))
}

#[allow(clippy::too_many_lines)]
fn cmd_examine(world: &mut World, player: Entity, args: &str) {
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

    // Self-target. Surfaces the same state lines as examining
    // another player would — Stealth (only visible to self anyway),
    // Flying, Mounted — so a player can confirm their state without
    // running multiple commands.
    if needle == "me" || needle == "self" {
        let name = name_of(world, player);
        let mut out = format!("\r\nYou look at yourself: {name}.\r\n");
        if world.get::<mud_world::Flying>(player).is_some() {
            out.push_str("You're hovering in mid-air.\r\n");
        }
        if world.get::<Stealth>(player).is_some() {
            out.push_str("You are hidden.\r\n");
        }
        if let Some(mud_world::Mounted(mount)) = world.get::<mud_world::Mounted>(player).copied() {
            let mount_name = name_or(world, mount, "<unknown>");
            out.push_str(&format!("You're riding {mount_name}.\r\n"));
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
        send_rendered(world, player, &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };

    let name = name_of(world, target);
    let description = world
        .get::<Description>(target)
        .map(|d| d.0.clone())
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
    if world.get::<mud_world::Flying>(target).is_some() {
        out.push_str(&format!("{name_rendered} hovers in mid-air.\r\n"));
    }
    if let Some(mud_world::Mounted(mount)) = world.get::<mud_world::Mounted>(target).copied() {
        let mount_name = name_or(world, mount, "<unknown>");
        out.push_str(&format!("{name_rendered} is riding {mount_name}.\r\n"));
    }
    if let Some(mud_world::RiddenBy(rider)) = world.get::<mud_world::RiddenBy>(target).copied() {
        let rider_name = name_or(world, rider, "<unknown>");
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
    send_to(world, player, out);
}

/// Map an entity's Health to a flavorful condition string for `examine`.
/// Six bands by HP percentage: 0% / 1-15 / 16-35 / 36-60 / 61-85 / 86+.
/// `max=0` is treated as 0% (entity has been zeroed somehow).
pub(crate) fn condition_label(hp: Health) -> &'static str {
    let pct = if hp.max > 0 { (hp.hp * 100) / hp.max } else { 0 };
    match pct {
        i32::MIN..=0 => "is dying",
        1..=15 => "is mortally wounded",
        16..=35 => "is badly hurt",
        36..=60 => "is bleeding",
        61..=85 => "has some scrapes",
        _ => "is in excellent shape",
    }
}

/// `title [<text> | clear]`: show / set / remove the player's epithet
/// shown on `who`. Stored as a Title component; persisted to
/// `Characters.title` on disconnect via `save_state`. Capped at 60
/// chars to keep the `who` columns sane.
const MAX_TITLE_LEN: usize = 60;
fn cmd_title(world: &mut World, player: Entity, args: &str) {
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

/// `description` / `desc`: show / set / clear the player's `examine`
/// prose. Stored as a `Description` component (the same component
/// rooms and mobs use); persisted to `Characters.description` on
/// disconnect via `save_state`. Capped at 500 chars to keep examine
/// from runaway-pasting.
const MAX_DESCRIPTION_LEN: usize = 500;
fn cmd_description(world: &mut World, player: Entity, args: &str) {
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
fn cmd_experience(world: &mut World, player: Entity, _args: &str) {
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
fn cmd_wealth(world: &mut World, player: Entity, _args: &str) {
    let total = world.get::<Wealth>(player).map_or(0, |w| w.0);
    let msg = if let Some(parts) = format_wealth(total) {
        format!("\r\nYou have {parts}.\r\n")
    } else {
        "\r\nYou have no coin to your name.\r\n".to_string()
    };
    send_to(world, player, msg);
}

/// `bribe <amount> <target>`: transfer copper to a mob and fire
/// its BRIBE triggers. Refuses on insufficient funds, missing
/// target, or self-target.
fn cmd_bribe(world: &mut World, player: Entity, args: &str) {
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
fn cmd_list(world: &mut World, player: Entity, _args: &str) {
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
    send_rendered(world, player, &out);
}

/// Render an `ObjectType` as the token shape used by `ShopAccepts.type`
/// and the underlying enum (uppercase, no underscores). The schema's
/// `Objects.type` uses sqlx-encoded `SCREAMING_SNAKE_CASE`, but
/// `ShopAccepts.type` is a free-form text column where some legacy
/// entries use underscores (e.g. `DRINK_CONTAINER`). Normalizing both
/// sides to uppercase + underscore-stripped lets matches succeed
/// across both spellings.
fn object_type_token(t: mud_db::enums::ObjectType) -> String {
    format!("{t:?}").to_ascii_uppercase()
}

/// Compute the copper price of one shop offering: override wins,
/// otherwise `proto.cost * shop.buy_profit` rounded.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn shop_offer_price(offer: &mud_world::ShopOffering, base_cost: i32, buy_profit: f64) -> i64 {
    if offer.price > 0 {
        i64::from(offer.price)
    } else {
        (f64::from(base_cost) * buy_profit).round() as i64
    }
}

/// `buy <#|name>`: purchase an item from the shopkeeper in the room.
/// Argument is either a 1-based catalog index or a substring of the
/// item's name. Deducts coin from `Wealth`; spawns the item directly
/// into the player's inventory. Stock is advisory only — the catalog
/// resource is not mutated, so unlimited / 0 / N entries all sell.
/// (Real stock decrement waits on per-shop instance state.)
#[allow(clippy::too_many_lines)]
fn cmd_buy(world: &mut World, player: Entity, args: &str) {
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
fn cmd_hire(world: &mut World, player: Entity, args: &str) {
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
fn cmd_sell(world: &mut World, player: Entity, args: &str) {
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
fn cmd_value(world: &mut World, player: Entity, args: &str) {
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
    let cost = world
        .get::<WorldKey>(target)
        .and_then(|k| {
            world
                .resource::<ObjectPrototypes>()
                .by_key
                .get(&(k.zone, k.id))
                .map(|p| p.cost)
        })
        .unwrap_or(0);
    let item_name = name_of(world, target);
    let msg = if let Some(parts) = format_wealth(i64::from(cost)) {
        format!("{item_name} is worth {parts}.\r\n")
    } else {
        format!("{item_name} is worthless.\r\n")
    };
    send_rendered(world, player, &msg);
}

/// `balance` / `bal`: show the bank-stored balance separate from
/// on-hand wealth. Read-only today — `deposit` / `withdraw` will
/// land with the banker NPC component and shop economy.
fn cmd_balance(world: &mut World, player: Entity, _args: &str) {
    let total = world.get::<BankWealth>(player).map_or(0, |b| b.0);
    let msg = if let Some(parts) = format_wealth(total) {
        format!("\r\nYour bank balance is {parts}.\r\n")
    } else {
        "\r\nYour bank balance is empty.\r\n".to_string()
    };
    send_to(world, player, msg);
}

/// Split an on-hand copper total into the four denominations and
/// render as `"X platinum, Y gold, Z silver, W copper"`. Returns
/// None when the total is zero or negative so callers can render
/// the empty case differently.
pub(crate) fn format_wealth(total: i64) -> Option<String> {
    if total <= 0 {
        return None;
    }
    let mut remainder = total;
    let platinum = remainder / 1000;
    remainder %= 1000;
    let gold = remainder / 100;
    remainder %= 100;
    let silver = remainder / 10;
    let copper = remainder % 10;
    let mut parts: Vec<String> = Vec::new();
    if platinum > 0 {
        parts.push(format!("{platinum} platinum"));
    }
    if gold > 0 {
        parts.push(format!("{gold} gold"));
    }
    if silver > 0 {
        parts.push(format!("{silver} silver"));
    }
    if copper > 0 {
        parts.push(format!("{copper} copper"));
    }
    Some(parts.join(", "))
}

/// `practice` / `prac`: with no arg, list `KnownAbilities` with
/// proficiency rendered as a tier label. With an ability name,
/// raise that ability's proficiency by 5 (capped at the class's
/// `proficiency_cap` from `ClassAbilities`).
fn cmd_practice(world: &mut World, player: Entity, args: &str) {
    let trimmed = args.trim();
    if !trimmed.is_empty() {
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
    let mut rows: Vec<(String, String, i32, bool)> = Vec::with_capacity(known.len());
    for (id, prof, learned) in &known {
        let def = catalog.by_name.values().find(|d| d.id == *id);
        let name = def.map_or_else(|| format!("ability #{id}"), |d| d.plain_name.clone());
        let kind = def.map_or("?", |d| match d.kind {
            mud_db::abilities::AbilityKind::Skill => "skill",
            mud_db::abilities::AbilityKind::Spell => "spell",
            mud_db::abilities::AbilityKind::Song => "song",
            mud_db::abilities::AbilityKind::Chant => "chant",
        });
        rows.push((name, kind.to_string(), *prof, *learned));
    }
    rows.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut out = format!("\r\nKnown abilities ({}):\r\n", rows.len());
    for (name, kind, prof, learned) in &rows {
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
        out.push_str(&format!(
            "  {learn_mark}{kind:<8} {name:<24} {pct:>3}% ({tier})\r\n"
        ));
    }
    out.push_str("\r\n* = learning (not yet mastered).\r\n");
    out.push_str(&format!("Practice points: {points}\r\n"));
    send_to(world, player, out);
}

/// `practice <ability>`: bump proficiency by 5, capped at the
/// class's `proficiency_cap`. Refuses unknown abilities, abilities
/// off the player's class list, abilities not in `KnownAbilities`,
/// and abilities already at the cap.
fn practice_one(world: &mut World, player: Entity, name: &str) {
    let Some(profile) = world.get::<Profile>(player).cloned() else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    let Some(class_id) = profile.class_id else {
        send_to(world, player, "You have no class.\r\n");
        return;
    };
    let key = name.trim().to_ascii_lowercase();
    let Some(def) = world.resource::<AbilityCatalog>().by_name.get(&key).cloned() else {
        send_to(world, player, format!("'{name}' isn't a known ability.\r\n"));
        return;
    };
    let cap = world
        .resource::<mud_world::SpellSlotData>()
        .ability_cap
        .get(&(class_id, def.id))
        .copied();
    let Some(cap) = cap else {
        send_to(
            world,
            player,
            format!("{} isn't on your class's list.\r\n", def.plain_name),
        );
        return;
    };
    let current_prof = world
        .get::<KnownAbilities>(player)
        .and_then(|k| k.entries.iter().find(|(id, _, _)| *id == def.id).copied())
        .map(|(_, p, _)| p);
    let Some(current_prof) = current_prof else {
        send_to(
            world,
            player,
            format!("You haven't learned {} yet — `study` it first.\r\n", def.plain_name),
        );
        return;
    };
    if current_prof >= cap {
        send_to(
            world,
            player,
            format!(
                "Your {} is already at its class cap of {cap}.\r\n",
                def.plain_name
            ),
        );
        return;
    }
    let points = world
        .get::<mud_world::SkillPoints>(player)
        .map_or(0, |s| s.0);
    if points <= 0 {
        send_to(
            world,
            player,
            "You have no practice points to spend. Earn more by leveling up.\r\n",
        );
        return;
    }
    let new_prof = (current_prof + 5).min(cap);
    if let Some(mut known) = world.get_mut::<KnownAbilities>(player)
        && let Some(slot) = known.entries.iter_mut().find(|(id, _, _)| *id == def.id)
    {
        slot.1 = new_prof;
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
            "You practice {} — proficiency now {new_prof} / {cap}. \
             ({remaining} practice point(s) remaining.)\r\n",
            def.plain_name
        ),
    );
}

/// `train [<stat>]`: bump a `CoreStat` by 1 in exchange for one
/// `SkillPoints`. Hard-capped at 18 per legacy `CircleMUD`
/// convention — characters with rolled stats above 18 (e.g. magical
/// bonuses) can't be trained higher than 18, but their existing
/// values aren't clamped.
const TRAIN_STAT_CAP: i32 = 18;

fn cmd_train(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim().to_ascii_lowercase();
    let stats = world
        .get::<CoreStats>(player)
        .copied()
        .unwrap_or_default();
    let points = world
        .get::<mud_world::SkillPoints>(player)
        .map_or(0, |s| s.0);
    if arg.is_empty() {
        let mut out = format!("\r\nCurrent stats (cap {TRAIN_STAT_CAP}):\r\n");
        out.push_str(&format!(
            "  str {:>2}   dex {:>2}   con {:>2}   int {:>2}   wis {:>2}   cha {:>2}\r\n",
            stats.strength,
            stats.dexterity,
            stats.constitution,
            stats.intelligence,
            stats.wisdom,
            stats.charisma,
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
fn cmd_track(world: &mut World, player: Entity, args: &str) {
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
    let mut visited: HashSet<Entity> = HashSet::new();
    visited.insert(start);
    let mut queue: VecDeque<(Entity, Direction, i32)> = VecDeque::new();
    if let Some(exits) = world.get::<Exits>(start) {
        for (dir, ed) in &exits.0 {
            if ed.state != ExitState::Open {
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
            for ed in exits.0.values() {
                if ed.state != ExitState::Open {
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
fn cmd_scan(world: &mut World, player: Entity, _args: &str) {
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
    // Sort by direction enum order so output is stable.
    let mut entries: Vec<(Direction, ExitData)> = exits.0.into_iter().collect();
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
            out.push_str(&format!("  {dir_label:>9}: <dangling>\r\n"));
            continue;
        };
        let target_name = name_or(world, target_room, "<unknown>");
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

fn direction_rank(d: Direction) -> u8 {
    match d {
        Direction::North => 0,
        Direction::East => 1,
        Direction::South => 2,
        Direction::West => 3,
        Direction::Up => 4,
        Direction::Down => 5,
        Direction::Northeast => 6,
        Direction::Southeast => 7,
        Direction::Southwest => 8,
        Direction::Northwest => 9,
        Direction::In => 10,
        Direction::Out => 11,
        Direction::Portal => 12,
        Direction::None => 13,
    }
}

/// One-line snapshot: name + posture + HP condition + current target.
/// Useful for a quick teammate / enemy check without the wall of text
/// from `examine`.
fn cmd_glance(world: &mut World, player: Entity, args: &str) {
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
    let cond = world
        .get::<Health>(target)
        .copied()
        .map_or("looks fine", condition_label);
    let posture = world
        .get::<Posture>(target)
        .map_or("standing", |p| p.0.label());
    let fighting = world
        .get::<Fighting>(target)
        .map(|f| name_or(world, f.0, "<gone>"));
    let mut line = format!("\r\n{name} ({posture}) {cond}");
    if let Some(target_name) = fighting {
        line.push_str(&format!(" — fighting {target_name}"));
    }
    line.push_str(".\r\n");
    send_rendered(world, player, &line);
}

fn cmd_look(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if !arg.is_empty() {
        if let Some(dir) = parse_direction(arg) {
            look_direction(world, player, dir);
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

    let room_name = name_or(world, room, "<nowhere>");
    let room_desc = world
        .get::<Description>(room)
        .map(|d| d.0.clone())
        .unwrap_or_default();
    let exits: Vec<Direction> = world
        .get::<Exits>(room)
        .map(|e| e.0.keys().copied().collect())
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
    // back to the name if Description is missing or empty.
    let mob_lines: Vec<String> = {
        let mut q = world
            .query_filtered::<(&Located, &Named, Option<&Description>), With<Mob>>();
        q.iter(world)
            .filter(|(l, _, _)| l.0 == room)
            .map(|(_, n, desc)| {
                desc.filter(|d| !d.0.trim().is_empty())
                    .map_or_else(|| n.name.clone(), |d| d.0.trim_end().to_string())
            })
            .collect()
    };
    // Items on the ground in this room.
    let items: Vec<String> = {
        let mut q = world.query_filtered::<(&Located, &Named), With<Item>>();
        q.iter(world)
            .filter(|(l, _)| l.0 == room)
            .map(|(_, n)| n.name.clone())
            .collect()
    };

    let mode = color_mode_for(world, player);
    let mut out = String::new();
    out.push_str(&format!("\r\n{}\r\n", render_color_tags(&room_name, mode)));
    // BRIEF flag suppresses the description — name/occupants/exits only.
    // CircleMUD-standard "brief mode".
    if !has_flag(world, player, PlayerFlag::Brief) && !room_desc.trim().is_empty() {
        out.push_str(&format!(
            "{}\r\n",
            render_color_tags(room_desc.trim_end(), mode)
        ));
    }
    for line in &mob_lines {
        out.push_str(&format!("{}\r\n", render_color_tags(line, mode)));
    }
    if !other_players.is_empty() {
        let rendered: Vec<String> = other_players
            .iter()
            .map(|p| render_color_tags(p, mode))
            .collect();
        out.push_str(&format!("Also here: {}\r\n", rendered.join(", ")));
    }
    if !items.is_empty() {
        let rendered: Vec<String> = items
            .iter()
            .map(|i| render_color_tags(i, mode))
            .collect();
        out.push_str(&format!("On the ground: {}\r\n", rendered.join(", ")));
    }
    // Auto-exits: only render the exits line on look when the player has the
    // AUTO_EXIT flag set. Without it, the room shows clean and the player
    // types `exits` (or peeks with `look <dir>`) on demand. Classic CircleMUD
    // semantics — kept opt-in to avoid clutter.
    if has_flag(world, player, PlayerFlag::AutoExit) {
        if exits.is_empty() {
            out.push_str("Exits: none\r\n");
        } else {
            let names: Vec<&str> = exits.iter().map(|d| direction_name(*d)).collect();
            out.push_str(&format!("Exits: {}\r\n", names.join(", ")));
        }
    }
    send_to(world, player, out);
}

struct WhoRow {
    entity: Entity,
    name: String,
    title: Option<String>,
    afk: bool,
    idle: Option<u64>,
}

fn cmd_who(world: &mut World, player: Entity, _args: &str) {
    // Two-pass: first collect rows, then resolve group roots so we
    // can mark grouped players with [G].
    let raw: Vec<WhoRow> = {
        let mut q = world.query_filtered::<(
            Entity,
            &Named,
            Option<&Title>,
            Option<&PlayerFlags>,
            Option<&LastInputAt>,
        ), (With<Player>, With<Online>)>();
        q.iter(world)
            .map(|(e, n, t, f, last)| WhoRow {
                entity: e,
                name: n.name.clone(),
                title: t.map(|t| t.0.clone()),
                afk: f.is_some_and(|pf| pf.has(PlayerFlag::Afk)),
                idle: last.map(|l| l.0.elapsed().as_secs()),
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

    let mut out = format!("\r\n{} online:\r\n", raw.len());
    for r in &raw {
        let root = roots.get(&r.entity).copied().unwrap_or(r.entity);
        let in_group = group_size.get(&root).copied().unwrap_or(0) > 1;
        out.push_str("  ");
        out.push_str(&r.name);
        if let Some(t) = &r.title {
            out.push(' ');
            out.push_str(t);
        }
        if in_group {
            out.push_str(" [G]");
        }
        if r.afk {
            out.push_str(" [AFK]");
        }
        if let Some(secs) = r.idle
            && secs >= 60
        {
            out.push_str(&format!(" [idle {}]", format_idle(secs)));
        }
        out.push_str("\r\n");
    }
    send_to(world, player, out);
}

fn cmd_idle(world: &mut World, player: Entity, _args: &str) {
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
    // Highest idle first; fresh-never-typed go to the bottom.
    rows.sort_by(|a, b| match (a.1, b.1) {
        (Some(x), Some(y)) => y.cmp(&x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.0.cmp(&b.0),
    });
    let mut out = format!("\r\n{} online by idle:\r\n", rows.len());
    out.push_str("  Name                     Idle      Online\r\n");
    for (name, idle, online) in &rows {
        let idle_label = match idle {
            None => "fresh".to_string(),
            Some(s) if *s < 60 => "active".to_string(),
            Some(s) => format_idle(*s),
        };
        let online_label = online.map_or_else(|| "?".to_string(), format_idle);
        out.push_str(&format!(
            "  {name:<24} {idle_label:<9} {online_label}\r\n"
        ));
    }
    send_to(world, player, out);
}

fn format_idle(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 { format!("{h}h") } else { format!("{h}h{m}m") }
    }
}

/// Bundle of all the data the `score` renderers consume. Building it once
/// in `cmd_score` avoids re-querying components per render variant and
/// keeps the renderer signatures from blowing past clippy's
/// `too_many_arguments` threshold.
struct ScoreData<'a> {
    name: &'a str,
    hp: Option<Health>,
    stamina: Option<Stamina>,
    cs: Option<CombatStats>,
    posture: Option<Posture>,
    logged_in: Option<LoggedInAt>,
    fight_target: Option<&'a str>,
    flags: &'a [&'static str],
    /// `(level, class_label, race, experience)` from the Profile component.
    /// `class_label` is the catalog `name` (with color tags) when the
    /// character has a class assigned, "Classless" otherwise.
    profile: Option<(i32, &'a str, &'a str, i32)>,
    /// On-hand copper total. Rendered as a `wealth`-style platinum/
    /// gold/silver/copper line. Zero is omitted from the score sheet.
    wealth: i64,
    /// Bank-stored copper total. Rendered as a separate `Bank:` line
    /// when nonzero so players can tell on-hand vs. saved at a glance.
    bank: i64,
}

fn cmd_score(world: &mut World, player: Entity, _args: &str) {
    let name = name_of(world, player);
    let hp = world.get::<Health>(player).copied();
    let stamina = world.get::<Stamina>(player).copied();
    let cs = world.get::<CombatStats>(player).copied();
    let fighting = world.get::<Fighting>(player).copied();
    let posture = world.get::<Posture>(player).copied();
    let logged_in = world.get::<LoggedInAt>(player).copied();
    let fight_target_name = fighting.map(|f| name_or(world, f.0, "<gone>"));
    let flags: Vec<&'static str> = world
        .get::<PlayerFlags>(player)
        .map(|f| f.0.iter().map(|fl| fl.label()).collect())
        .unwrap_or_default();
    let style = world.get::<UiStyle>(player).copied().unwrap_or_default();
    // Profile + class catalog lookup: resolve the display name once here so
    // renderers stay pure (no &World access). Uses `plain_name` (no color
    // tags) so the fixed-width fancy box aligns correctly; once a visible-
    // width-aware writer lands, this can switch to the colored `name`.
    let profile_owned: Option<(i32, String, String, i32)> =
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
            (prof.level, class_label, prof.race.clone(), prof.experience)
        });

    let wealth = world.get::<Wealth>(player).map_or(0, |w| w.0);
    let bank = world.get::<BankWealth>(player).map_or(0, |b| b.0);
    let data = ScoreData {
        name: &name,
        hp,
        stamina,
        cs,
        posture,
        logged_in,
        fight_target: fight_target_name.as_deref(),
        flags: &flags,
        profile: profile_owned
            .as_ref()
            .map(|(lvl, cls, race, xp)| (*lvl, cls.as_str(), race.as_str(), *xp)),
        wealth,
        bank,
    };
    let out = match style {
        UiStyle::Standard => render_score_standard(&data),
        UiStyle::Fancy => render_score_fancy(&data),
        UiStyle::Minimal => render_score_minimal(&data),
    };
    send_to(world, player, out);
}

fn render_score_standard(d: &ScoreData) -> String {
    let mut out = format!("\r\n{}\r\n", d.name);
    if let Some((level, class, race, xp)) = d.profile {
        out.push_str(&format!(
            "  Level {level} {race} ({class})    XP: {xp}\r\n",
        ));
    }
    if let Some(hp) = d.hp {
        out.push_str(&format!("  HP: {} / {}\r\n", hp.hp, hp.max));
    }
    if let Some(s) = d.stamina {
        out.push_str(&format!("  Stamina: {} / {}\r\n", s.current, s.max));
    }
    if let Some(cs) = d.cs {
        out.push_str(&format!(
            "  Hit roll: {}    Damage roll: {}    AC: {}    Alignment: {}\r\n",
            cs.hit_roll, cs.dmg_roll, cs.ac, cs.alignment
        ));
    }
    if let Some(p) = d.posture {
        out.push_str(&format!("  Posture: {}\r\n", p.0.label()));
    }
    if let Some(coin) = format_wealth(d.wealth) {
        out.push_str(&format!("  Wealth: {coin}\r\n"));
    }
    if let Some(coin) = format_wealth(d.bank) {
        out.push_str(&format!("  Bank:   {coin}\r\n"));
    }
    if let Some(l) = d.logged_in {
        out.push_str(&format!("  Online for: {}\r\n", format_idle(l.0.elapsed().as_secs())));
    }
    if let Some(target) = d.fight_target {
        out.push_str(&format!("  Fighting: {target}\r\n"));
    }
    if !d.flags.is_empty() {
        out.push_str(&format!("  Flags: {}\r\n", d.flags.join(", ")));
    }
    out
}

fn render_score_fancy(d: &ScoreData) -> String {
    // Box width = 56 chars between the borders.
    const W: usize = 56;
    let name = d.name;
    let mut out = String::from("\r\n");
    out.push_str(&format!("+{}+\r\n", "-".repeat(W)));
    let title = format!("{name:^W$}");
    out.push_str(&format!("|{title}|\r\n"));
    out.push_str(&format!("+{}+\r\n", "-".repeat(W)));
    let mut row = |s: String| {
        out.push_str(&format!("| {s:<width$} |\r\n", width = W - 2));
    };
    if let Some((level, class, race, xp)) = d.profile {
        row(format!("Level:     {level} {race} ({class})"));
        row(format!("XP:        {xp}"));
    }
    if let Some(hp) = d.hp {
        row(format!("HP:        {} / {}", hp.hp, hp.max));
    }
    if let Some(s) = d.stamina {
        row(format!("Stamina:   {} / {}", s.current, s.max));
    }
    if let Some(cs) = d.cs {
        row(format!(
            "Hit: {}   Dmg: {}   AC: {}   Align: {}",
            cs.hit_roll, cs.dmg_roll, cs.ac, cs.alignment
        ));
    }
    if let Some(p) = d.posture {
        row(format!("Posture:   {}", p.0.label()));
    }
    if let Some(coin) = format_wealth(d.wealth) {
        row(format!("Wealth:    {coin}"));
    }
    if let Some(coin) = format_wealth(d.bank) {
        row(format!("Bank:      {coin}"));
    }
    if let Some(l) = d.logged_in {
        row(format!("Online:    {}", format_idle(l.0.elapsed().as_secs())));
    }
    if let Some(target) = d.fight_target {
        row(format!("Fighting:  {target}"));
    }
    if !d.flags.is_empty() {
        row(format!("Flags:     {}", d.flags.join(", ")));
    }
    out.push_str(&format!("+{}+\r\n", "-".repeat(W)));
    out
}

fn render_score_minimal(d: &ScoreData) -> String {
    let mut parts = vec![d.name.to_string()];
    if let Some((level, class, race, xp)) = d.profile {
        parts.push(format!("L{level} {race}/{class}"));
        parts.push(format!("xp:{xp}"));
    }
    if let Some(hp) = d.hp {
        parts.push(format!("hp:{}/{}", hp.hp, hp.max));
    }
    if let Some(s) = d.stamina {
        parts.push(format!("st:{}/{}", s.current, s.max));
    }
    if let Some(cs) = d.cs {
        parts.push(format!("dmg:{} ac:{}", cs.dmg_roll, cs.ac));
    }
    if let Some(p) = d.posture {
        parts.push(format!("p:{}", p.0.label()));
    }
    if d.wealth > 0 {
        parts.push(format!("c:{}", d.wealth));
    }
    if d.bank > 0 {
        parts.push(format!("bank:{}", d.bank));
    }
    if let Some(target) = d.fight_target {
        parts.push(format!("vs:{target}"));
    }
    format!("{}\r\n", parts.join("  "))
}

fn cmd_style(world: &mut World, player: Entity, args: &str) {
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

fn cmd_stand(world: &mut World, player: Entity, _args: &str) {
    set_posture(world, player, PostureKind::Standing);
}
fn cmd_sit(world: &mut World, player: Entity, _args: &str) {
    set_posture(world, player, PostureKind::Sitting);
}
fn cmd_kneel(world: &mut World, player: Entity, _args: &str) {
    set_posture(world, player, PostureKind::Kneeling);
}
fn cmd_rest(world: &mut World, player: Entity, _args: &str) {
    set_posture(world, player, PostureKind::Resting);
}
fn cmd_sleep(world: &mut World, player: Entity, _args: &str) {
    set_posture(world, player, PostureKind::Sleeping);
}

fn cmd_wake(world: &mut World, player: Entity, args: &str) {
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

fn set_posture(world: &mut World, player: Entity, new: PostureKind) {
    let current = world.get::<Posture>(player).map(|p| p.0);
    if current == Some(new) {
        send_to(
            world,
            player,
            format!("You are already {}.\r\n", new.label()),
        );
        return;
    }
    try_insert(world, player, Posture(new));
    let verb = match new {
        PostureKind::Standing => "stand up",
        PostureKind::Sitting => "sit down",
        PostureKind::Kneeling => "kneel",
        PostureKind::Resting => "begin resting",
        PostureKind::Sleeping => "lie down and sleep",
    };
    send_to(world, player, format!("You {verb}.\r\n"));

    // Announce to the room.
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let mover_name = name_of(world, player);
    let third = match new {
        PostureKind::Standing => "stands up",
        PostureKind::Sitting => "sits down",
        PostureKind::Kneeling => "kneels",
        PostureKind::Resting => "begins resting",
        PostureKind::Sleeping => "lies down and sleeps",
    };
    broadcast_room_except_players_rendered(
        world,
        located.0,
        &[player],
        &format!("{mover_name} {third}.\r\n"),
    );
}

fn cmd_roles(world: &mut World, player: Entity, _args: &str) {
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

fn cmd_quit(world: &mut World, player: Entity, _args: &str) {
    send_to(world, player, "Goodbye!\r\n");
}

fn cmd_prompt(world: &mut World, player: Entity, args: &str) {
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
                 Variables: %h current HP, %H max HP, %v current stamina, \
                 %V max stamina, %n character name, %r room name, \
                 %g on-hand wealth (copper), %% literal %.\r\n"
            ),
        );
        return;
    }
    try_insert(world, player, Prompt(template.to_string()));
    send_to(world, player, format!("Prompt set to: {template}\r\n"));
}

fn cmd_toggle(world: &mut World, player: Entity, args: &str) {
    let raw = args.trim();
    if raw.is_empty() {
        send_to(world, player, "Toggle which flag? Try `flags` to see what's set, or `help toggle`.\r\n");
        return;
    }
    let Some(flag) = PlayerFlag::from_label(raw) else {
        send_to(world, player, format!("Unknown flag '{raw}'.\r\n"));
        return;
    };
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

/// Toggle a single `PlayerFlag` and emit a friendlier message than the
/// generic `toggle` command. `on_msg` / `off_msg` are written verbatim
/// after the toggle. Used by the dedicated `afk` / `notell` / `deaf`
/// / `color` commands so muscle-memory players don't have to type
/// `toggle <flag>`.
fn toggle_player_flag(
    world: &mut World,
    player: Entity,
    flag: PlayerFlag,
    on_msg: &str,
    off_msg: &str,
) {
    let now_on = world
        .get_mut::<PlayerFlags>(player)
        .map(|mut pf| pf.toggle(flag));
    let Some(now_on) = now_on else {
        send_to(world, player, "You have no player flags slot.\r\n");
        return;
    };
    send_to(world, player, format!("{}\r\n", if now_on { on_msg } else { off_msg }));
}

fn cmd_afk(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Afk,
        "You are now marked AFK.",
        "You're back from AFK.",
    );
}

/// Names that would lock the player out of dispatch entirely if
/// allowed as aliases. `quit` is the always-allowed escape hatch and
/// must never be aliased away. `alias` and `unalias` themselves can't
/// be redirected or the player can't reach them after one bad set.
const RESERVED_ALIAS_NAMES: &[&str] = &["quit", "alias", "unalias"];

fn cmd_alias(world: &mut World, player: Entity, args: &str) {
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

fn cmd_unalias(world: &mut World, player: Entity, args: &str) {
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

fn cmd_notell(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::NoTell,
        "You will no longer receive tells.",
        "You will now receive tells.",
    );
}

fn cmd_deaf(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Deaf,
        "You no longer hear gossip or shouts.",
        "You can hear gossip and shouts again.",
    );
}

// COLOR_BLIND is the underlying flag (semantics inverted relative to
// the command name): COLOR_BLIND ON ⇒ colors stripped. The messages
// flip accordingly so the player reads the visible behaviour, not the
// flag state.
fn cmd_color(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::ColorBlind,
        "Colors are now OFF.",
        "Colors are now ON.",
    );
}

// `wimpy` doubles as a toggle-with-threshold command. Three forms:
//   `wimpy`         — show current state.
//   `wimpy off|0`   — clear the WIMPY flag and threshold.
//   `wimpy <1..99>` — set threshold and ensure the flag is on.
// Combat checks `WimpyThreshold` (default 25%) only when the flag is
// also set, so clearing the flag is sufficient to disable; we still
// drop the component on `off` to keep state tidy.
fn cmd_wimpy(world: &mut World, player: Entity, args: &str) {
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

fn cmd_autoexit(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::AutoExit,
        "Exits will be shown automatically with each `look`.",
        "Exits will no longer auto-list — use `exits` to see them.",
    );
}

fn cmd_autoloot(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::AutoLoot,
        "Auto-loot enabled.",
        "Auto-loot disabled.",
    );
}

fn cmd_autogold(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::AutoGold,
        "Auto-gold enabled.",
        "Auto-gold disabled.",
    );
}

fn cmd_autoassist(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::AutoAssist,
        "Auto-assist enabled.",
        "Auto-assist disabled.",
    );
}

fn cmd_autosplit(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::AutoSplit,
        "Auto-split enabled.",
        "Auto-split disabled.",
    );
}

fn cmd_brief(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Brief,
        "Room descriptions will now be terse on `look`.",
        "Full room descriptions restored.",
    );
}

fn cmd_compact(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Compact,
        "Compact mode enabled.",
        "Compact mode disabled.",
    );
}

fn cmd_norepeat(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::NoRepeat,
        "Suppressing duplicate consecutive lines.",
        "All output lines will be shown.",
    );
}

fn cmd_nosummon(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::NoSummon,
        "You can no longer be summoned by spells.",
        "You can again be summoned by spells.",
    );
}

// `dice` is the legacy verb for SHOW_DICE_ROLLS — when on, combat
// surfaces hit/damage rolls in the output.
fn cmd_dicerolls(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::ShowDiceRolls,
        "Showing dice rolls.",
        "Hiding dice rolls.",
    );
}

fn cmd_pk(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::PkEnabled,
        "PK is now enabled — you may attack and be attacked by other players.",
        "PK is now disabled.",
    );
}

fn cmd_quest_flag(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Quest,
        "Quest mode enabled — you'll be flagged for quest-only zones once those land.",
        "Quest mode disabled.",
    );
}

fn cmd_consent(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::Consent,
        "You consent to group/share interactions.",
        "You revoke group/share consent.",
    );
}

// `holylight` is admin/builder-only in legacy FieryMUD: with the flag
// on you can see invisible/dark/hidden things in `look`. The flag is
// set, but no behaviour is wired into the renderer yet — this command
// exists so the muscle-memory toggle works and lands the flag for
// later renderer plumbing.
fn cmd_holylight(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::HolyLight,
        "Holy light surrounds you — the unseen is now seen.",
        "Holy light fades.",
    );
}

// `showids` exposes (zone, id) coordinates in command output for
// builders/admins. The flag is set; renderers that want to surface
// IDs check it.
fn cmd_showids(world: &mut World, player: Entity, _args: &str) {
    toggle_player_flag(
        world,
        player,
        PlayerFlag::ShowIds,
        "Showing entity IDs.",
        "Hiding entity IDs.",
    );
}

fn cmd_flags(world: &mut World, player: Entity, _args: &str) {
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

/// Parse a direction word or its short alias to a Direction enum.
/// Returns None for anything that doesn't match a movement direction.
fn parse_direction(s: &str) -> Option<Direction> {
    match s.to_ascii_lowercase().as_str() {
        "north" | "n" => Some(Direction::North),
        "south" | "s" => Some(Direction::South),
        "east" | "e" => Some(Direction::East),
        "west" | "w" => Some(Direction::West),
        "up" | "u" => Some(Direction::Up),
        "down" | "d" => Some(Direction::Down),
        "northeast" | "ne" => Some(Direction::Northeast),
        "northwest" | "nw" => Some(Direction::Northwest),
        "southeast" | "se" => Some(Direction::Southeast),
        "southwest" | "sw" => Some(Direction::Southwest),
        "in" => Some(Direction::In),
        "out" => Some(Direction::Out),
        _ => None,
    }
}

/// Peek at a neighboring room through the named exit. Reports whether
/// the exit is closed/locked, and otherwise prints the target room's
/// name and description (no occupants — that requires actually being
/// there).
fn look_direction(world: &mut World, player: Entity, dir: Direction) {
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let Some(exits) = world.get::<Exits>(located.0).cloned() else {
        send_to(world, player, "You see nothing in that direction.\r\n");
        return;
    };
    let Some(ed) = exits.0.get(&dir).copied() else {
        send_to(world, player, "You see nothing in that direction.\r\n");
        return;
    };
    if ed.state == mud_db::enums::ExitState::Closed
        || ed.state == mud_db::enums::ExitState::Locked
    {
        send_to(world, player, "The way is closed.\r\n");
        return;
    }
    let Some(target_room) = ed.to else {
        send_to(world, player, "The way fades into the unknown.\r\n");
        return;
    };
    let name = name_or(world, target_room, "<unknown>");
    let mode = color_mode_for(world, player);
    let name = render_color_tags(&name, mode);
    let desc = world
        .get::<Description>(target_room)
        .map(|d| render_color_tags(&d.0, mode))
        .unwrap_or_default();
    let mut out = format!("\r\nYou peer {}.\r\n  {name}\r\n", direction_name(dir));
    if !desc.trim().is_empty() {
        out.push_str("  ");
        out.push_str(desc.trim_end());
        out.push_str("\r\n");
    }
    send_to(world, player, out);
}

fn cmd_exits(world: &mut World, player: Entity, _args: &str) {
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
    // Resolve each exit's target room name; sort by direction's canonical order.
    let mut rows: Vec<(mud_db::enums::Direction, String)> = exits
        .0
        .iter()
        .map(|(dir, ed)| {
            let target_name = ed
                .to
                .and_then(|e| world.get::<Named>(e).map(|n| n.name.clone()))
                .unwrap_or_else(|| "(beyond)".to_string());
            (*dir, target_name)
        })
        .collect();
    rows.sort_by_key(|(d, _)| direction_order(*d));
    let mut out = String::from("\r\nExits:\r\n");
    for (dir, room) in &rows {
        out.push_str(&format!("  {:>10} - {}\r\n", direction_name(*dir), room));
    }
    send_to(world, player, out);
}

fn direction_order(d: mud_db::enums::Direction) -> u8 {
    use mud_db::enums::Direction::{
        Down, East, In, North, Northeast, Northwest, Out, Portal, South, Southeast, Southwest, Up,
        West,
    };
    match d {
        North => 0,
        East => 1,
        South => 2,
        West => 3,
        Up => 4,
        Down => 5,
        Northeast => 6,
        Southeast => 7,
        Southwest => 8,
        Northwest => 9,
        In => 10,
        Out => 11,
        Portal => 12,
        mud_db::enums::Direction::None => 13,
    }
}

/// Apply `new_state` to the door at `(room, dir)` *and* to its
/// counterpart on the other side of the connection (via
/// `opposite(dir)` from the exit's `to` room). One-sided edits would
/// drift over time as players walk through and re-open the same
/// door from each side.
fn flip_door_both_sides(world: &mut World, room: Entity, dir: Direction, new_state: ExitState) {
    let mut other_room: Option<Entity> = None;
    if let Some(mut exits) = world.get_mut::<Exits>(room)
        && let Some(ed) = exits.0.get_mut(&dir)
    {
        ed.state = new_state;
        other_room = ed.to;
    }
    if let (Some(other), Some(opp)) = (other_room, opposite(dir))
        && let Some(mut exits) = world.get_mut::<Exits>(other)
        && let Some(ed) = exits.0.get_mut(&opp)
    {
        ed.state = new_state;
    }
}

/// `doorbash <direction>`: force-open a closed/locked exit via
/// stamina. Same two-sided sync as `open`/`close`.
fn cmd_doorbash(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "doorbash") {
        return;
    }
    if !check_stamina(world, player, DOORBASH_COST, "doorbash") {
        return;
    }
    let arg = args.trim();
    let Some(dir) = parse_direction(arg) else {
        send_to(world, player, "Doorbash which way?\r\n");
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
    if state == ExitState::Open {
        send_to(world, player, format!("It's already open {}.\r\n", direction_name(dir)));
        return;
    }
    drain_stamina(world, player, DOORBASH_COST);
    flip_door_both_sides(world, room, dir, ExitState::Open);

    let player_name = name_of(world, player);
    send_to(world, player, format!(
        "You bash open the way {}!\r\n",
        direction_name(dir),
    ));
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} bashes the door {} wide open!\r\n", direction_name(dir)),
    );
}

/// `unlock <direction>`: find a key item in inventory whose name or
/// keyword matches the exit's `key` and flip Locked → Closed (still
/// needs `open` afterward). Two-sided sync.
/// `pick <direction>`: rogue lock-pick. Refuses if the exit isn't
/// locked, lacks a keyhole, the player hasn't trained `PICK_LOCK`,
/// or they're out of stamina. Costs 5 stamina either way; success
/// flips Locked → Closed.
fn cmd_pick(world: &mut World, player: Entity, args: &str) {
    const STAMINA_COST: i32 = 5;
    let arg = args.trim();
    let Some(dir) = parse_direction(arg) else {
        send_to(world, player, "Pick which way?\r\n");
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
        send_to(
            world,
            player,
            format!("It's not locked {}.\r\n", direction_name(dir)),
        );
        return;
    }
    if key_req.is_none() {
        send_to(
            world,
            player,
            format!("There's no keyhole {}.\r\n", direction_name(dir)),
        );
        return;
    }

    // PICK_LOCK ability id 272.
    let proficiency = world
        .get::<KnownAbilities>(player)
        .and_then(|k| k.entries.iter().find(|(id, _, _)| *id == 272).copied())
        .map(|(_, p, _)| p);
    let Some(proficiency) = proficiency else {
        send_to(
            world,
            player,
            "You don't know how to pick locks.\r\n",
        );
        return;
    };

    let stamina_ok = world
        .get::<Stamina>(player)
        .is_some_and(|s| s.current >= STAMINA_COST);
    if !stamina_ok {
        send_to(
            world,
            player,
            "You don't have the stamina for a steady hand.\r\n",
        );
        return;
    }
    if let Some(mut s) = world.get_mut::<Stamina>(player) {
        s.current = (s.current - STAMINA_COST).max(0);
    }

    // d100 roll vs proficiency. Proficiency is 0–1000 in the schema;
    // divide by 10 to get a 0–100 chance.
    let roll = rand::random_range(1..=100);
    let chance = (proficiency / 10).clamp(0, 100);
    let player_name = name_of(world, player);
    if roll <= chance {
        flip_door_both_sides(world, room, dir, ExitState::Closed);
        send_to(
            world,
            player,
            format!(
                "*click* The lock {} yields to your tools.\r\n",
                direction_name(dir)
            ),
        );
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} picks the lock {}.\r\n", direction_name(dir)),
        );
    } else {
        send_to(
            world,
            player,
            format!(
                "Your tools slip — the lock {} stays shut.\r\n",
                direction_name(dir)
            ),
        );
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} fumbles with the lock {}.\r\n", direction_name(dir)),
        );
    }
}

fn cmd_unlock(world: &mut World, player: Entity, args: &str) {
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
fn cmd_open(world: &mut World, player: Entity, args: &str) {
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
fn cmd_close(world: &mut World, player: Entity, args: &str) {
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
fn cmd_lock(world: &mut World, player: Entity, args: &str) {
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
fn cmd_read(world: &mut World, player: Entity, args: &str) {
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

/// `compare <a> <b>`: side-by-side weight + level + type comparison
/// for two carried-or-worn items, plus a small deltas line. Splits
/// the args at the first run of whitespace; multi-word keywords on
/// either side aren't supported (a quoted-arg parser would be more
/// general but no other command needs one yet).
fn cmd_compare(world: &mut World, player: Entity, args: &str) {
    let mut parts = args.trim().splitn(2, char::is_whitespace);
    let Some(a_word) = parts.next().filter(|s| !s.is_empty()) else {
        send_to(world, player, "Compare what to what?\r\n");
        return;
    };
    let Some(b_word) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
        send_to(world, player, "Compare to what?\r\n");
        return;
    };

    let Some(a) = find_carried_by(world, a_word, player, EquipFilter::Anywhere) else {
        send_to(world, player, format!("You don't have '{a_word}'.\r\n"));
        return;
    };
    let Some(b) = find_carried_by(world, b_word, player, EquipFilter::Anywhere) else {
        send_to(world, player, format!("You don't have '{b_word}'.\r\n"));
        return;
    };
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
    let mut out = String::from("\r\n");
    out.push_str(&format!(
        "  A: {}    weight: {:.1}   level: {}   ({:?})\r\n",
        render_color_tags(&a_name, mode),
        ap.weight,
        ap.level,
        ap.r#type,
    ));
    out.push_str(&format!(
        "  B: {}    weight: {:.1}   level: {}   ({:?})\r\n",
        render_color_tags(&b_name, mode),
        bp.weight,
        bp.level,
        bp.r#type,
    ));
    let weight_delta = ap.weight - bp.weight;
    let level_delta = ap.level - bp.level;
    let weight_line = if weight_delta.abs() < f64::EPSILON {
        "Same weight.".to_string()
    } else if weight_delta > 0.0 {
        format!("A heavier by {weight_delta:.1}.")
    } else {
        format!("B heavier by {:.1}.", -weight_delta)
    };
    let level_line = match level_delta.cmp(&0) {
        std::cmp::Ordering::Equal => "Same level.".to_string(),
        std::cmp::Ordering::Greater => format!("A higher level by {level_delta}."),
        std::cmp::Ordering::Less => format!("B higher level by {}.", -level_delta),
    };
    out.push_str(&format!("  {weight_line}  {level_line}\r\n"));
    send_to(world, player, out);
}

/// `motd` / `news` / `credits` / `policies`: static-text dumps.
/// Each command prints a hardcoded constant for now; once a
/// `GameConfig` table or files-on-disk source lands, the bodies move
/// to a dynamic lookup. Today the goal is: muscle-memory commands
/// shouldn't error out, and players get useful prose.
const MOTD_TEXT: &str = "\
\r\n=== Welcome to fierymud-rs ===\r\n\
\r\n\
A Rust ECS rewrite of FieryMUD, in active development. Many\r\n\
commands work; many don't yet. Type `commands` for the full list\r\n\
or `help <name>` for details. File a bug with `bug <message>` if\r\n\
something looks broken.\r\n\
\r\n\
Combat is fully functional but unbalanced — be cautious in\r\n\
high-level guild rooms (the Cleric's Guild guards hit for ~250).\r\n\
";
const NEWS_TEXT: &str = "\
\r\n=== Recent Changes ===\r\n\
\r\n\
This list is curated by hand from the commit log. The most recent\r\n\
runtime changes:\r\n\
\r\n\
- Combat skills landed: bandage, layhands, rescue, assist, disarm,\r\n\
  hitall, backstab, springleap, gouge, roar, berserk, rend, retreat.\r\n\
- Bleed and other DoT effects tick HP damage every second.\r\n\
- Bandage staunches bleed.\r\n\
- Berserk attackers deal +50% damage in combat.\r\n\
\r\n\
Run `commands` for everything you can use today.\r\n\
";
const CREDITS_TEXT: &str = "\
\r\n=== Credits ===\r\n\
\r\n\
fierymud-rs is a clean-slate rewrite inspired by:\r\n\
  - FieryMUD (the C++ codebase from Mielikki et al.)\r\n\
  - DikuMUD / CircleMUD lineage\r\n\
\r\n\
Stack: Rust, bevy_ecs, sqlx, tokio, mlua. Thanks to those\r\n\
projects' authors and to everyone who keeps a public MUD running.\r\n\
";
const POLICIES_TEXT: &str = "\
\r\n=== Server Policies ===\r\n\
\r\n\
1. No harassment, slurs, or threats — to anyone, in any channel.\r\n\
2. No cheating: bug exploits should be reported via `bug`, not\r\n\
   used.\r\n\
3. No multi-charing for an unfair advantage. Multi-charing is\r\n\
   fine for socializing.\r\n\
4. Admins enforce rules; appeals through `tell <admin> <message>`\r\n\
   or by emailing the address in `motd`.\r\n\
\r\n\
This is a hobby server; please be kind.\r\n\
";

fn cmd_motd(world: &mut World, player: Entity, _args: &str) {
    send_to(world, player, MOTD_TEXT.to_string());
}

fn cmd_news(world: &mut World, player: Entity, _args: &str) {
    send_to(world, player, NEWS_TEXT.to_string());
}

fn cmd_credits(world: &mut World, player: Entity, _args: &str) {
    send_to(world, player, CREDITS_TEXT.to_string());
}

fn cmd_policies(world: &mut World, player: Entity, _args: &str) {
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
fn cmd_richtest(world: &mut World, player: Entity, _args: &str) {
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
fn cmd_clientinfo(world: &mut World, player: Entity, _args: &str) {
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
    let mut out = String::from("\r\n");
    out.push_str(&format!("  Character: {char_name}\r\n"));
    out.push_str(&format!("  Role:      {}\r\n", role.label()));
    out.push_str(&format!(
        "  Uptime:    {} (since login)\r\n",
        format_dur(uptime_secs)
    ));
    out.push_str(&format!(
        "  Idle:      {} (since last input)\r\n",
        format_dur(idle_secs)
    ));
    send_to(world, player, out);
}

fn cmd_account(world: &mut World, player: Entity, _args: &str) {
    let Some(summary) = world.get::<AccountSummary>(player).cloned() else {
        send_to(world, player, "No account info available.\r\n");
        return;
    };
    let active_name = name_of(world, player);
    let role = world
        .get::<Account>(player)
        .map_or(UserRole::Player, |a| a.role);

    let mut out = String::from("\r\n");
    out.push_str(&format!("  Email:        {}\r\n", summary.email));
    out.push_str(&format!("  Display name: {}\r\n", summary.display_name));
    out.push_str(&format!("  Role:         {}\r\n", role.label()));
    out.push_str(&format!("  Characters    ({}):\r\n", summary.characters.len()));
    for (name, level) in &summary.characters {
        let marker = if name == &active_name { " *" } else { "  " };
        out.push_str(&format!("   {marker} {name} (level {level})\r\n"));
    }
    out.push_str("\r\n  * = currently playing\r\n");
    send_to(world, player, out);
}

/// `commands`: flat alphabetical list of every command the player has
/// access to (after role + permission gating). Each command appears
/// once under its primary name; aliases are folded into the same slot
/// — `help <name>` still surfaces them per-command.
// 4 cols of width 18 = 72-char body — fits standard 80-col terminals
// after the 2-space leading indent. Width chosen for our longest
// command names today (`autoassist`, `description`, `lasttells`).
const COMMANDS_LIST_COLS: usize = 4;
const COMMANDS_LIST_COL_WIDTH: usize = 18;
fn cmd_commands(world: &mut World, player: Entity, _args: &str) {
    let (role, perms) = world
        .get::<Account>(player)
        .map_or((UserRole::Player, Vec::new()), |a| (a.role, a.perms.clone()));
    let mut names: Vec<&'static str> = COMMANDS
        .iter()
        .filter(|c| visible(c, role, &perms))
        .map(|c| c.names[0])
        .collect();
    names.sort_unstable();

    let mut out = format!("\r\n{} commands available:\r\n", names.len());
    for chunk in names.chunks(COMMANDS_LIST_COLS) {
        out.push_str("  ");
        for name in chunk {
            out.push_str(&format!("{name:<COMMANDS_LIST_COL_WIDTH$}"));
        }
        out.push_str("\r\n");
    }
    out.push_str("\r\nUse `help <command>` for details.\r\n");
    send_to(world, player, out);
}

fn cmd_world(world: &mut World, player: Entity, _args: &str) {
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

    let mut out = String::from("\r\n");
    out.push_str(&format!("  Zones loaded:    {zones}\r\n"));
    out.push_str(&format!("  Rooms loaded:    {rooms}\r\n"));
    out.push_str(&format!("  Mobs spawned:    {mobs}\r\n"));
    out.push_str(&format!("  Items spawned:   {items}\r\n"));
    out.push_str(&format!("  Players online:  {players_online}\r\n"));
    out.push_str(&format!("  Active effects:  {effects}\r\n"));
    out.push_str(&format!("  Server tick:     {tick}\r\n"));
    out.push_str(&format!("  Uptime:          {h}h {m}m {s}s\r\n"));
    send_to(world, player, out);
}

fn cmd_time(world: &mut World, player: Entity, _args: &str) {
    let tick = world.resource::<TickCount>().0;
    let started = world.resource::<ServerStart>().0;
    let uptime = started.elapsed();
    let now = chrono::Utc::now();

    let secs = uptime.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;

    // MUD time. Tick rate is 10 Hz; 1 MUD hour = 75 real seconds =
    // 750 ticks; 24 hours = 1 day = 18000 ticks. Day 0 / hour 0
    // starts at server boot.
    let mud_hour = (tick / 750) % 24;
    let mud_day = tick / 18000;
    let period = match mud_hour {
        0..=4 => "deep night",
        5..=7 => "early morning",
        8..=11 => "morning",
        12..=13 => "midday",
        14..=17 => "afternoon",
        18..=20 => "evening",
        _ => "night",
    };

    let mut out = String::from("\r\n");
    out.push_str(&format!("  Server time: {}\r\n", now.format("%Y-%m-%d %H:%M:%S UTC")));
    out.push_str(&format!("  Uptime:      {h}h {m}m {s}s\r\n"));
    out.push_str(&format!("  World tick:  {tick}\r\n"));
    out.push_str(&format!(
        "  Game time:   day {mud_day}, {mud_hour:02}:00 ({period})\r\n",
    ));
    send_to(world, player, out);
}

/// `weather`: render an atmospheric flavor line based on the player's
/// current zone's `Climate` and the in-game time of day. The
/// underlying weather model is rule-of-thumb only — there's no
/// per-tick simulation; same input gives the same output. Players
/// pull this when they want to feel the world's character; admins
/// could also use it as a quick climate-tag readout.
fn cmd_weather(world: &mut World, player: Entity, _args: &str) {
    use mud_db::enums::Climate;
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; the sky is blank.\r\n");
        return;
    };
    let room = located.0;
    let zone = world
        .get::<WorldKey>(room)
        .and_then(|k| world.resource::<WorldKeyIndex>().zones.get(&k.zone).copied());
    let climate = zone.and_then(|z| world.get::<ZoneClimate>(z).map(|c| c.0));
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
    send_to(
        world,
        player,
        format!("\r\n{line}\r\n"),
    );
}

fn cmd_version(world: &mut World, player: Entity, _args: &str) {
    let mut out = String::from("\r\n");
    out.push_str(&format!(
        "  {} {}\r\n",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
    ));
    out.push_str(&format!("  Profile: {}\r\n", if cfg!(debug_assertions) { "debug" } else { "release" }));
    out.push_str(&format!("  Tick rate: {} Hz\r\n", crate::TICK_HZ));
    send_to(world, player, out);
}

fn cmd_where(world: &mut World, player: Entity, _args: &str) {
    let mut rows: Vec<(String, String)> = {
        let mut q = world
            .query_filtered::<(&Named, &Located), (With<Player>, With<Online>)>();
        q.iter(world)
            .map(|(n, l)| {
                let room_name = name_or(world, l.0, "<unknown>");
                (n.name.clone(), room_name)
            })
            .collect()
    };
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = format!("\r\n{} player(s) online:\r\n", rows.len());
    for (name, room) in &rows {
        out.push_str(&format!("  {name:<24} {room}\r\n"));
    }
    send_to(world, player, out);
}

fn cmd_inventory(world: &mut World, player: Entity, _args: &str) {
    let items: Vec<String> = {
        let mut q = world
            .query_filtered::<(&Located, &Named, Option<&EquippedSlot>), With<Item>>();
        q.iter(world)
            .filter(|(l, _, eq)| l.0 == player && eq.is_none())
            .map(|(_, n, _)| n.name.clone())
            .collect()
    };
    let mode = color_mode_for(world, player);
    let mut out = if items.is_empty() {
        "\r\nYou are carrying nothing.\r\n".to_string()
    } else {
        format!("\r\nYou are carrying {} item(s):\r\n", items.len())
    };
    for name in &items {
        out.push_str(&format!("  {}\r\n", render_color_tags(name, mode)));
    }
    send_to(world, player, out);
}

fn cmd_get(world: &mut World, player: Entity, args: &str) {
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
    // player is carrying or which sits in the room.
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
        let item = find_in_container(world, needle, container);
        let Some(item) = item else {
            let cn = name_of(world, container);
            send_rendered(world, player, &format!("There's no '{needle}' in {cn}.\r\n"));
            return;
        };
        let item_name = name_of(world, item);
        let container_name = name_of(world, container);
        let player_name = name_of(world, player);
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
}

/// Split `<item> from <container>` into `(item, container)` if the
/// `from` keyword appears as a separator. Returns None for inputs
/// without the keyword.
fn split_from_keyword(input: &str) -> Option<(&str, &str)> {
    let lower = input.to_ascii_lowercase();
    let pat = " from ";
    let i = lower.find(pat)?;
    let (a, _) = input.split_at(i);
    let b = &input[i + pat.len()..];
    let a = a.trim();
    let b = b.trim();
    if a.is_empty() || b.is_empty() {
        return None;
    }
    Some((a, b))
}

/// Find an item Located on `container` whose Named or Keywords
/// match `needle` (case-insensitive substring).
fn find_in_container(world: &mut World, needle: &str, container: Entity) -> Option<Entity> {
    let needle = needle.to_ascii_lowercase();
    let mut q = world.query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
    q.iter(world)
        .find(|(_, l, n, kw)| l.0 == container && matches(&needle, n, *kw))
        .map(|(e, _, _, _)| e)
}

/// `put <item> <container>`: move a carried item into a container
/// the player is carrying or which sits in the room.
fn cmd_put(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[1].trim().is_empty() {
        send_to(world, player, "Usage: put <item> <container>\r\n");
        return;
    }
    let item_word = parts[0].trim();
    let container_word = parts[1].trim();

    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;

    let item = find_carried_by(world, item_word, player, EquipFilter::Inventory);
    let Some(item) = item else {
        send_rendered(
            world,
            player,
            &format!("You aren't carrying '{item_word}'.\r\n"),
        );
        return;
    };
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
    if container == item {
        send_to(world, player, "You can't put something inside itself.\r\n");
        return;
    }
    let item_name = name_of(world, item);
    let container_name = name_of(world, container);
    let player_name = name_of(world, player);
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
fn cmd_junk(world: &mut World, player: Entity, args: &str) {
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
fn cmd_donate(world: &mut World, player: Entity, args: &str) {
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

fn cmd_drop(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Drop what?\r\n");
        return;
    }
    let item = find_carried_by(world, target_word, player, EquipFilter::Inventory);
    let Some(item) = item else {
        send_rendered(world, player, &format!("You aren't carrying '{target_word}'.\r\n"),
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

    send_rendered(world, player, &format!("You drop {item_name}.\r\n"));
    broadcast_room_except_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} drops {item_name}.\r\n"),
    );
    crate::triggers::fire_item_event(world, item, player, mud_world::TriggerEvent::Drop);
}

fn cmd_give(world: &mut World, player: Entity, args: &str) {
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
}

fn cmd_wear(world: &mut World, player: Entity, args: &str) {
    wear_into(world, player, args.trim(), None);
}

fn cmd_wield(world: &mut World, player: Entity, args: &str) {
    wear_into(world, player, args.trim(), Some(Slot::Wield));
}

fn cmd_hold(world: &mut World, player: Entity, args: &str) {
    wear_into(world, player, args.trim(), Some(Slot::Hold));
}

/// `light <item>`: mark a Light-type carried item as lit. Refused
/// on non-Light items or already-lit ones.
fn cmd_light(world: &mut World, player: Entity, args: &str) {
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
fn cmd_extinguish(world: &mut World, player: Entity, args: &str) {
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
fn cmd_mount(world: &mut World, player: Entity, args: &str) {
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
fn cmd_dismount(world: &mut World, player: Entity, _args: &str) {
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
fn cmd_fly(world: &mut World, player: Entity, _args: &str) {
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
fn cmd_walk(world: &mut World, player: Entity, _args: &str) {
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
fn cmd_hide(world: &mut World, player: Entity, _args: &str) {
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
fn cmd_visible(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Stealth>(player).is_none() {
        send_to(world, player, "You're already visible.\r\n");
        return;
    }
    try_remove::<Stealth>(world, player);
    send_to(world, player, "You stop hiding.\r\n");
}

/// `eat <item>` / `quaff <item>`: consume a Food / Potion. Looks up
/// the item's proto, checks the type, then despawns. Effects are a
/// follow-up — they need `ConsumableEffects` loading.
fn consume_item(world: &mut World, player: Entity, args: &str, expected: mud_db::enums::ObjectType, verb: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, format!("{} what?\r\n", capitalize(verb)));
        return;
    }
    let item = find_carried_by(world, target_word, player, EquipFilter::Inventory);
    let Some(item) = item else {
        send_to(world, player, format!("You aren't carrying '{target_word}'.\r\n"));
        return;
    };
    let item_name = name_of(world, item);
    let kind = world
        .get::<WorldKey>(item)
        .and_then(|k| world.resource::<ObjectPrototypes>().by_key.get(&(k.zone, k.id)).map(|p| p.r#type));
    if kind != Some(expected) {
        send_to(world, player, format!(
            "You can't {verb} {item_name}.\r\n",
        ));
        return;
    }
    send_rendered(world, player, &format!("You {verb} {item_name}.\r\n"));
    // Apply ConsumableEffects bound to this object proto. Per-row
    // chance gate, EffectInstance spawned with the row's duration
    // (or the EffectDef's default_params.duration when null).
    apply_consumable_object_effects(world, player, item);
    // Fire CONSUME on the item before despawn so the body can read
    // self.id / self.name and emit a final flavor line.
    crate::triggers::fire_item_event(world, item, player, mud_world::TriggerEvent::Consume);
    if let Ok(e) = world.get_entity_mut(item) {
        e.despawn();
    }
}

/// Spawn ConsumableEffect-bound effects on `player` for object
/// proto behind `item`. No-op when `ConsumableEffects` has no rows
/// for the proto. Per-row `chance` (0.0–1.0) gates spawning.
fn apply_consumable_object_effects(world: &mut World, player: Entity, item: Entity) {
    let key = world.get::<WorldKey>(item).copied();
    let Some(key) = key else { return };
    let bindings = world
        .resource::<mud_world::ConsumableEffectCatalog>()
        .by_object
        .get(&(key.zone, key.id))
        .cloned()
        .unwrap_or_default();
    for b in bindings {
        spawn_consumable_effect(world, player, &b);
    }
}

/// Same as `apply_consumable_object_effects` but for a Liquid name.
/// Resolves the name through `LiquidIndex` to the schema's id, then
/// fans out to the catalog's per-liquid bindings.
fn apply_consumable_liquid_effects(world: &mut World, player: Entity, liquid_name: &str) {
    let needle = liquid_name.to_ascii_lowercase();
    let liquid_id = world
        .resource::<mud_world::LiquidIndex>()
        .by_name
        .get(&needle)
        .copied();
    let Some(liquid_id) = liquid_id else { return };
    let bindings = world
        .resource::<mud_world::ConsumableEffectCatalog>()
        .by_liquid
        .get(&liquid_id)
        .cloned()
        .unwrap_or_default();
    for b in bindings {
        spawn_consumable_effect(world, player, &b);
    }
}

fn spawn_consumable_effect(
    world: &mut World,
    player: Entity,
    binding: &mud_world::ConsumableEffectBinding,
) {
    if binding.chance < 1.0 {
        let roll = f64::from(rand::random_range(0..1000)) / 1000.0;
        if roll > binding.chance {
            return;
        }
    }
    let effect_def = world
        .resource::<EffectCatalog>()
        .by_id
        .get(&binding.effect_id)
        .cloned();
    let Some(def) = effect_def else {
        return;
    };
    // Duration: explicit binding > effect default_params.duration > 30s
    let dur_secs = binding.duration_secs.unwrap_or_else(|| {
        def.default_params
            .get("duration")
            .and_then(serde_json::Value::as_i64)
            .map_or(30, |v| i32::try_from(v).unwrap_or(30))
    });
    world.spawn((
        EffectInstance {
            kind: def.id,
            name: def.name.clone(),
            strength: 1,
            remaining_secs: dur_secs,
            source: EffectSource::Item,
            ability_id: None,
        },
        AppliedTo(player),
    ));
}

fn cmd_eat(world: &mut World, player: Entity, args: &str) {
    consume_item(world, player, args, mud_db::enums::ObjectType::Food, "eat");
}

fn cmd_quaff(world: &mut World, player: Entity, args: &str) {
    consume_item(world, player, args, mud_db::enums::ObjectType::Potion, "quaff");
}

/// `drink <container>` / `sip <container>`: take a swig from a
/// DRINKCONTAINER. `drink` consumes 4 units, `sip` consumes 1.
/// Empty containers refuse; reaching 0 mid-action leaves the
/// container empty for next time but still completes the swig.
/// Poisoned containers print a warning line — a real poison effect
/// can wire later.
fn drink_amount(world: &mut World, player: Entity, args: &str, units: i32, verb: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, format!("{} from what?\r\n", capitalize(verb)));
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
    if state.remaining <= 0 {
        send_rendered(world, player, &format!("{item_name} is empty.\r\n"));
        return;
    }
    let drank = state.remaining.min(units);
    let liquid_lc = state.liquid.to_ascii_lowercase();
    if let Some(mut lc) = world.get_mut::<mud_world::LiquidContainer>(item) {
        lc.remaining -= drank;
    }
    send_rendered(
        world,
        player,
        &format!("You {verb} some {liquid_lc} from {item_name}.\r\n"),
    );
    if state.poisoned {
        send_to(
            world,
            player,
            "You feel a sudden burning in your gut — that was poisoned!\r\n",
        );
    }
    apply_consumable_liquid_effects(world, player, &state.liquid);
    let was_last = state.remaining == drank;
    if was_last {
        send_rendered(
            world,
            player,
            &format!("{item_name} is empty now.\r\n"),
        );
    }
}

fn cmd_drink(world: &mut World, player: Entity, args: &str) {
    drink_amount(world, player, args, 4, "drink");
}

fn cmd_sip(world: &mut World, player: Entity, args: &str) {
    drink_amount(world, player, args, 1, "sip");
}

/// `pour <container> [target]`: transfer liquid from a held
/// container. With no target, empties to the floor. With a target
/// container, transfers as much as the target can accept (limited
/// by capacity − remaining). Liquid types must match — pouring
/// water into wine refuses.
#[allow(clippy::too_many_lines)]
fn cmd_pour(world: &mut World, player: Entity, args: &str) {
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
fn cmd_fill(world: &mut World, player: Entity, args: &str) {
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
fn cmd_taste(world: &mut World, player: Entity, args: &str) {
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

fn cmd_recite(world: &mut World, player: Entity, args: &str) {
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

fn cmd_wave(world: &mut World, player: Entity, args: &str) {
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

fn cmd_tap(world: &mut World, player: Entity, args: &str) {
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

/// Shared body for `recite` / `wave` / `tap`: look up the held
/// item's `ObjectAbilities` bindings, dispatch each through the
/// cast pipeline, then either despawn (`single_use=true`, scrolls)
/// or decrement `Charges` (`single_use=false`, wands/staves —
/// despawn at 0).
fn invoke_object_abilities(
    world: &mut World,
    player: Entity,
    args: &str,
    expected_type: mud_db::enums::ObjectType,
    verb: &str,
    intro_phrase: &str,
    single_use: bool,
) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    let item_word = parts.first().map(|s| s.trim()).filter(|s| !s.is_empty());
    let target_word = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty());
    let Some(item_word) = item_word else {
        send_to(world, player, format!("{} what?\r\n", capitalize(verb)));
        return;
    };
    let item = find_carried_by(world, item_word, player, EquipFilter::Inventory);
    let Some(item) = item else {
        send_to(
            world,
            player,
            format!("You aren't carrying '{item_word}'.\r\n"),
        );
        return;
    };
    let item_name = name_of(world, item);
    let key = world.get::<WorldKey>(item).copied();
    let Some(key) = key else {
        send_rendered(world, player, &format!("{item_name} has no proto link.\r\n"));
        return;
    };
    let kind = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(key.zone, key.id))
        .map(|p| p.r#type);
    if kind != Some(expected_type) {
        send_rendered(
            world,
            player,
            &format!("You can't {verb} {item_name}.\r\n"),
        );
        return;
    }
    // Empty Charges → refuse before any output. Without a Charges
    // component, treat as unlimited (covers freshly-spawned items
    // until `Charges` populates from binding.charges on every
    // spawn site).
    if !single_use {
        let charges = world.get::<mud_world::Charges>(item).copied();
        if matches!(charges, Some(mud_world::Charges(0))) {
            send_rendered(world, player, &format!("{item_name} is depleted.\r\n"));
            return;
        }
    }
    let bindings: Vec<i32> = world
        .resource::<mud_world::ObjectAbilityCatalog>()
        .by_key
        .get(&(key.zone, key.id))
        .map(|v| v.iter().map(|b| b.ability_id).collect())
        .unwrap_or_default();
    if bindings.is_empty() {
        send_rendered(
            world,
            player,
            &format!("{item_name} has no bound magic.\r\n"),
        );
        return;
    }
    send_rendered(world, player, &format!("{intro_phrase} {item_name}.\r\n"));
    // Fire USE on the item before spell dispatch — bodies may
    // gate (return false) or emit additional flavor.
    crate::triggers::fire_item_event(world, item, player, mud_world::TriggerEvent::Use);
    for ability_id in bindings {
        let ability_name = world
            .resource::<AbilityCatalog>()
            .by_name
            .values()
            .find(|d| d.id == ability_id)
            .map(|d| d.plain_name.to_ascii_lowercase());
        let Some(ability_name) = ability_name else {
            continue;
        };
        let dispatched = if let Some(t) = target_word {
            format!("{ability_name} {t}")
        } else {
            ability_name
        };
        invoke_ability(
            world,
            player,
            &dispatched,
            mud_db::abilities::AbilityKind::Spell,
            "cast",
        );
    }
    if single_use {
        if let Ok(e) = world.get_entity_mut(item) {
            e.despawn();
        }
    } else if let Some(mut c) = world.get_mut::<mud_world::Charges>(item) {
        if c.0 > 0 {
            c.0 -= 1;
        }
        let depleted = c.0 == 0;
        if depleted {
            send_rendered(world, player, &format!("{item_name} crumbles to dust.\r\n"));
            if let Ok(e) = world.get_entity_mut(item) {
                e.despawn();
            }
        }
    }
}

fn wear_into(world: &mut World, player: Entity, target_word: &str, force_slot: Option<Slot>) {
    if target_word.is_empty() {
        send_to(world, player, "Wear what?\r\n");
        return;
    }

    let item = find_carried_by(world, target_word, player, EquipFilter::Inventory);
    let Some(item) = item else {
        send_to(
            world,
            player,
            format!("You aren't carrying '{target_word}'.\r\n"),
        );
        return;
    };

    let item_name = name_of(world, item);

    let Some(WearableIn(slot)) = world.get::<WearableIn>(item).copied() else {
        send_rendered(world, player, &format!("{item_name} can't be worn.\r\n"));
        return;
    };

    if let Some(forced) = force_slot
        && forced != slot
    {
        let verb = match forced {
            Slot::Wield => "wielded",
            Slot::Hold => "held",
            _ => "worn there",
        };
        send_rendered(world, player, &format!("{item_name} can't be {verb}.\r\n"),
        );
        return;
    }

    // Check the slot is free.
    let slot_taken = {
        let mut q = world.query_filtered::<(&Located, &EquippedSlot), With<Item>>();
        q.iter(world)
            .any(|(l, eq)| l.0 == player && eq.0 == slot)
    };
    if slot_taken {
        send_rendered(world, player, &format!("Your {} is already occupied.\r\n", slot.label()),
        );
        return;
    }

    try_insert(world, item, EquippedSlot(slot));

    let verb = match slot {
        Slot::Wield => "wield",
        Slot::Hold => "hold",
        _ => "wear",
    };
    send_rendered(world, player, &format!("You {verb} {item_name}.\r\n"));
    crate::triggers::fire_item_event(world, item, player, mud_world::TriggerEvent::Wear);
}

fn cmd_remove(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Remove what?\r\n");
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

fn cmd_equipment(world: &mut World, player: Entity, _args: &str) {
    // Build a Slot -> name map in canonical order.
    let mut by_slot: Vec<(Slot, String)> = {
        let mut q =
            world.query_filtered::<(&Located, &Named, &EquippedSlot), With<Item>>();
        q.iter(world)
            .filter(|(l, _, _)| l.0 == player)
            .map(|(_, n, eq)| (eq.0, n.name.clone()))
            .collect()
    };
    if by_slot.is_empty() {
        send_to(world, player, "\r\nYou aren't wearing anything.\r\n");
        return;
    }
    by_slot.sort_by_key(|(s, _)| Slot::ORDER.iter().position(|x| x == s).unwrap_or(usize::MAX));
    let mode = color_mode_for(world, player);
    let mut out = String::from("\r\nEquipment:\r\n");
    for (slot, name) in &by_slot {
        out.push_str(&format!(
            "  {:>14}: {}\r\n",
            slot.label(),
            render_color_tags(name, mode)
        ));
    }
    send_to(world, player, out);
}

/// Match by Keywords substring first, falling back to Name substring.
fn matches(needle: &str, name: &Named, kw: Option<&Keywords>) -> bool {
    if let Some(kw) = kw
        && kw.0.iter().any(|k| k.to_ascii_lowercase().contains(needle))
    {
        return true;
    }
    name.name.to_ascii_lowercase().contains(needle)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EquipFilter {
    /// Carried but not equipped (i.e. in inventory).
    Inventory,
    /// Currently equipped.
    Equipped,
    /// Either. Reserved for "look in self" flows we'll add later.
    #[allow(dead_code)]
    Anywhere,
}

fn find_carried_by(
    world: &mut World,
    needle: &str,
    carrier: Entity,
    filter: EquipFilter,
) -> Option<Entity> {
    let needle = needle.to_ascii_lowercase();
    let mut q = world.query_filtered::<(
        Entity,
        &Located,
        &Named,
        Option<&Keywords>,
        Option<&EquippedSlot>,
    ), With<Item>>();
    q.iter(world)
        .find(|(_, l, n, kw, eq)| {
            if l.0 != carrier {
                return false;
            }
            let is_equipped = eq.is_some();
            let pass_filter = match filter {
                EquipFilter::Inventory => !is_equipped,
                EquipFilter::Equipped => is_equipped,
                EquipFilter::Anywhere => true,
            };
            pass_filter && matches(&needle, n, *kw)
        })
        .map(|(e, _, _, _, _)| e)
}

fn find_in_room(world: &mut World, needle: &str, room: Entity) -> Option<Entity> {
    let needle = needle.to_ascii_lowercase();
    let mut q = world.query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
    q.iter(world)
        .find(|(_, l, n, kw)| l.0 == room && matches(&needle, n, *kw))
        .map(|(e, _, _, _)| e)
}

/// Find a non-Item entity in `room` (player or mob) for give/attack-style
/// targeting.
fn find_actor_in_room(
    world: &mut World,
    needle: &str,
    room: Entity,
    exclude: Entity,
) -> Option<Entity> {
    let needle = needle.to_ascii_lowercase();
    let mut q = world.query::<(Entity, &Located, &Named, Option<&Keywords>, Option<&Item>)>();
    q.iter(world)
        .find(|(e, l, n, kw, item)| {
            *e != exclude && l.0 == room && item.is_none() && matches(&needle, n, *kw)
        })
        .map(|(e, _, _, _, _)| e)
}

/// `cooldowns` / `cd`: list active ability cooldowns for the player.
/// Reads the `Cooldowns` component (set by `invoke_ability` after a
/// successful cast). Stale entries (`ready_at` in the past) are
/// skipped — they're effectively expired even if not pruned yet.
fn cmd_cooldowns(world: &mut World, player: Entity, _args: &str) {
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
        out.push_str(&format!("  {name:<24} {remaining:.1}s remaining\r\n"));
    }
    send_to(world, player, out);
}

/// `cancel [<effect>]`: drop a non-permanent effect from yourself.
/// Empty arg lists cancellable effects; named arg matches by
/// case-insensitive substring on the effect's name.
/// `abort`: stub for the legacy in-progress-cast / queued-spell
/// mechanic — neither exists in this runtime today (casts resolve
/// immediately, no queue), so the command's only job is to give a
/// clearer-than-default response to `FieryMUD` veterans typing it.
fn cmd_abort(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        "You aren't casting anything. (Use `cancel <effect>` to drop an active buff.)\r\n",
    );
}

/// `release`: stub for the legacy ghost-release-from-corpse flow.
/// Today's death handler auto-revives the player in place, so the
/// ghost state never arises. The stub stays for muscle memory.
fn cmd_release(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        "You aren't dead. (Use `recall` to return to your home temple.)\r\n",
    );
}

fn cmd_cancel(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim().to_ascii_lowercase();
    let cancellable: Vec<(Entity, String, i32)> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, inst, a)| a.0 == player && inst.remaining_secs >= 0)
            .map(|(e, inst, _)| (e, inst.name.clone(), inst.remaining_secs))
            .collect()
    };
    if cancellable.is_empty() {
        send_to(
            world,
            player,
            "You have no effects you can cancel.\r\n",
        );
        return;
    }
    if needle.is_empty() {
        let mut out = format!("\r\n{} cancellable effect(s):\r\n", cancellable.len());
        for (_, name, remaining) in &cancellable {
            out.push_str(&format!("  {name} ({remaining}s)\r\n"));
        }
        out.push_str("\r\nUse `cancel <name>` to drop one.\r\n");
        send_to(world, player, out);
        return;
    }
    let target = cancellable
        .iter()
        .find(|(_, name, _)| name.to_ascii_lowercase().contains(&needle))
        .map(|(e, _, _)| *e);
    let Some(target_effect) = target else {
        send_to(
            world,
            player,
            format!("No cancellable effect matching '{needle}' on you.\r\n"),
        );
        return;
    };
    let removed_name = world
        .get::<EffectInstance>(target_effect)
        .map_or_else(|| "?".to_string(), |i| i.name.clone());
    if let Ok(e) = world.get_entity_mut(target_effect) {
        e.despawn();
    }
    send_to(
        world,
        player,
        format!("You cancel {removed_name}.\r\n"),
    );
}

fn cmd_effects(world: &mut World, player: Entity, _args: &str) {
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
        format!("\r\n{} active effect(s):\r\n", active.len())
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
        let suffix = from
            .as_deref()
            .map_or(String::new(), |n| format!(" — from {n}"));
        let delta_label = delta_amount.map_or(String::new(), |a| {
            let sign = if a >= 0 { "+" } else { "" };
            format!(" ({sign}{a})")
        });
        if remaining < 0 {
            out.push_str(&format!("  {name}{delta_label} (permanent){suffix}\r\n"));
        } else {
            out.push_str(&format!(
                "  {name}{delta_label} ({remaining}s remaining){suffix}\r\n"
            ));
        }
    }
    send_to(world, player, out);
}

// ---------------------------------------------------------------------------
// Communication handlers
// ---------------------------------------------------------------------------

fn cmd_say(world: &mut World, player: Entity, message: &str) {
    let message = message.trim();
    if message.is_empty() {
        send_to(world, player, "Say what?\r\n");
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let speaker = name_of(world, player);

    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == located.0)
            .map(|(e, _)| e)
            .collect()
    };

    for target in targets {
        let line = if target == player {
            format!("You say, \"{message}\"\r\n")
        } else {
            format!("{speaker} says, \"{message}\"\r\n")
        };
        send_rendered(world, target, &line);
    }

    // Fire SPEECH-flagged triggers for every entity in the room
    // that carries AttachedTriggers (skipping the speaker
    // themselves). Bodies do their own keyword matching against
    // the `speech` Lua global.
    crate::triggers::fire_speech_in_room(world, player, located.0, message);
}

/// `report`: announce your current HP/stamina to your group (when
/// you're in one) or to everyone in the room (when solo). Group
/// reports cross rooms — useful for healers in adjacent rooms; room
/// reports stay local to encourage situational coordination.
fn cmd_report(world: &mut World, player: Entity, _args: &str) {
    let hp = world.get::<Health>(player).copied();
    let stamina = world.get::<Stamina>(player).copied();
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let speaker = name_of(world, player);
    let body = match (hp, stamina) {
        (Some(h), Some(s)) => format!(
            "HP {}/{}, stamina {}/{}",
            h.hp, h.max, s.current, s.max
        ),
        (Some(h), None) => format!("HP {}/{}", h.hp, h.max),
        (None, Some(s)) => format!("stamina {}/{}", s.current, s.max),
        (None, None) => "(no vital stats)".to_string(),
    };
    let root = group_root(world, player);
    let group = group_members(world, root);
    let (targets, self_label, third_label) = if group.len() > 1 {
        (group, "your group", "the group")
    } else {
        let in_room: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
            q.iter(world)
                .filter(|(_, l)| l.0 == located.0)
                .map(|(e, _)| e)
                .collect()
        };
        (in_room, "the room", "the room")
    };
    for target in targets {
        let line = if target == player {
            format!("You report to {self_label}: {body}.\r\n")
        } else {
            format!("{speaker} reports to {third_label}: {body}.\r\n")
        };
        send_rendered(world, target, &line);
    }
}

fn cmd_whisper(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[1].trim().is_empty() {
        send_to(world, player, "Usage: whisper <target> <message>\r\n");
        return;
    }
    let target_word = parts[0].trim();
    let message = parts[1].trim();
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(target) = find_actor_in_room(world, target_word, located.0, player) else {
        send_rendered(world, player, &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    let speaker = name_of(world, player);
    let target_name = name_of(world, target);

    send_rendered(world, player, &format!("You whisper to {target_name}, \"{message}\"\r\n"),
    );
    send_to(
        world,
        target,
        format!("{speaker} whispers to you, \"{message}\"\r\n"),
    );
    broadcast_room_except_players_rendered(
        world,
        located.0,
        &[player, target],
        &format!("{speaker} whispers something to {target_name}.\r\n"),
    );
}

fn cmd_bug(world: &mut World, player: Entity, args: &str) {
    submit_feedback(world, player, "bug", args);
}
fn cmd_idea(world: &mut World, player: Entity, args: &str) {
    submit_feedback(world, player, "idea", args);
}
fn cmd_typo(world: &mut World, player: Entity, args: &str) {
    submit_feedback(world, player, "typo", args);
}

/// Log a player feedback report (`bug`/`idea`/`typo`) to the tracing
/// pipeline so it ends up in the server log for staff review. Includes
/// the player's name, character id, and current room id when available
/// — useful for `typo` reports that almost always need location context.
fn submit_feedback(world: &mut World, player: Entity, kind: &'static str, args: &str) {
    let body = args.trim();
    if body.is_empty() {
        send_to(world, player, format!("Usage: {kind} <message>\r\n"));
        return;
    }
    let name = name_of(world, player);
    let char_id = world
        .get::<Account>(player)
        .map(|a| a.character_id.clone())
        .unwrap_or_default();
    let room_key = world
        .get::<Located>(player)
        .and_then(|l| world.get::<WorldKey>(l.0))
        .map(|wk| format!("{}:{}", wk.zone, wk.id));
    info!(
        kind,
        player = %name,
        character_id = %char_id,
        room = room_key.as_deref().unwrap_or("?"),
        body = %body,
        "player feedback"
    );
    send_to(
        world,
        player,
        format!("Thanks. Your {kind} report has been logged.\r\n"),
    );
}

/// `level`: print level / XP / next-level delta.
fn cmd_level(world: &mut World, player: Entity, _args: &str) {
    use mud_world::LevelTable;
    let Some(p) = world.get::<Profile>(player) else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    let level = p.level;
    let xp = p.experience;
    let table = world.resource::<LevelTable>();
    let level_name = table.name_for(level);
    let next_threshold = table.exp_for(level + 1);
    let mut out = format!("\r\n{level_name} (level {level})\r\n");
    out.push_str(&format!("Experience: {xp}\r\n"));
    if let Some(threshold) = next_threshold {
        let to_go = (threshold - xp).max(0);
        let next_name = table.name_for(level + 1);
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
fn cmd_slots(world: &mut World, player: Entity, _args: &str) {
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

/// `study <spell>`: permanently add a spell to the player's
/// `KnownAbilities` at proficiency=1, gated on the spell being on
/// the player's class list. Persisted to `CharacterAbilities` on
/// disconnect.
fn cmd_study(world: &mut World, player: Entity, args: &str) {
    use mud_world::SpellSlotData;
    let Some(profile) = world.get::<Profile>(player).cloned() else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    let Some(class_id) = profile.class_id else {
        send_to(world, player, "You have no class.\r\n");
        return;
    };
    let key = args.trim().to_ascii_lowercase();
    if key.is_empty() {
        send_to(world, player, "Study what?\r\n");
        return;
    }
    let Some(def) = world.resource::<AbilityCatalog>().by_name.get(&key).cloned() else {
        send_to(world, player, format!("'{key}' isn't a known ability.\r\n"));
        return;
    };
    if !world
        .resource::<SpellSlotData>()
        .ability_circle
        .contains_key(&(class_id, def.id))
    {
        send_to(
            world,
            player,
            format!("{} isn't on your class's list.\r\n", def.plain_name),
        );
        return;
    }
    if let Some(known) = world.get::<KnownAbilities>(player)
        && known.has_any(def.id)
    {
        send_to(
            world,
            player,
            format!("You already know {}.\r\n", def.plain_name),
        );
        return;
    }
    if let Some(mut known) = world.get_mut::<KnownAbilities>(player) {
        known.entries.push((def.id, 1, true));
        known.entries.sort_by_key(|(id, _, _)| *id);
    } else {
        world.entity_mut(player).insert(KnownAbilities {
            entries: vec![(def.id, 1, true)],
        });
    }
    send_to(
        world,
        player,
        format!(
            "You commit {} to memory. (proficiency 1)\r\n",
            def.plain_name
        ),
    );
}

/// Resolve a spell name to (`ability_id`, circle) for the player's
/// class. Returns Err with a player-facing message on failure.
fn resolve_spell_for_class(
    world: &World,
    class_id: i32,
    name: &str,
) -> Result<(i32, i32), String> {
    let key = name.trim().to_ascii_lowercase();
    if key.is_empty() {
        return Err("Memorize what?".into());
    }
    let Some(def) = world.resource::<AbilityCatalog>().by_name.get(&key) else {
        return Err(format!("'{name}' isn't a known ability."));
    };
    if !matches!(def.kind, mud_db::abilities::AbilityKind::Spell) {
        return Err(format!("{} isn't a memorizable spell.", def.plain_name));
    }
    let Some(&circle) = world
        .resource::<mud_world::SpellSlotData>()
        .ability_circle
        .get(&(class_id, def.id))
    else {
        return Err(format!(
            "{} isn't on your class's spell list.",
            def.plain_name
        ));
    };
    Ok((def.id, circle))
}

/// `memorize <spell>`: prepare a spell into one of your circle slots.
fn cmd_memorize(world: &mut World, player: Entity, args: &str) {
    use mud_world::{MemorizedSpells, SpellSlotData};
    let Some(profile) = world.get::<Profile>(player).cloned() else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    let Some(class_id) = profile.class_id else {
        send_to(world, player, "You have no class.\r\n");
        return;
    };
    let (ability_id, circle) = match resolve_spell_for_class(world, class_id, args) {
        Ok(t) => t,
        Err(e) => {
            send_to(world, player, format!("{e}\r\n"));
            return;
        }
    };
    let max = world
        .resource::<SpellSlotData>()
        .progression
        .get(&(profile.level, circle))
        .copied()
        .unwrap_or(0);
    if max <= 0 {
        send_to(
            world,
            player,
            format!("You can't memorize circle {circle} spells yet.\r\n"),
        );
        return;
    }
    let used = world
        .get::<MemorizedSpells>(player)
        .map_or(0, |m| m.used_in_circle(circle));
    if used >= max {
        send_to(
            world,
            player,
            format!("All circle {circle} slots ({used}/{max}) are already prepared.\r\n"),
        );
        return;
    }
    let plain_name = world
        .resource::<AbilityCatalog>()
        .by_name
        .values()
        .find(|d| d.id == ability_id)
        .map_or_else(String::new, |d| d.plain_name.clone());
    let prep_secs = (circle * 5).max(5); // default 5s/circle until Ability.memorization_time is seeded
    let entry = mud_world::MemEntry {
        ability_id,
        circle,
        ready: false,
        prep_secs_remaining: prep_secs,
    };
    if let Some(mut mem) = world.get_mut::<MemorizedSpells>(player) {
        mem.entries.push(entry);
    } else {
        world
            .entity_mut(player)
            .insert(MemorizedSpells { entries: vec![entry] });
    }
    send_to(
        world,
        player,
        format!(
            "You begin memorizing {plain_name} (circle {circle}, ~{prep_secs}s while resting).\r\n"
        ),
    );
}

/// `forget <spell>`: drop the first matching memorized spell.
fn cmd_forget(world: &mut World, player: Entity, args: &str) {
    use mud_world::MemorizedSpells;
    let Some(profile) = world.get::<Profile>(player).cloned() else {
        return;
    };
    let Some(class_id) = profile.class_id else {
        send_to(world, player, "You have no class.\r\n");
        return;
    };
    let (ability_id, _) = match resolve_spell_for_class(world, class_id, args) {
        Ok(t) => t,
        Err(e) => {
            send_to(world, player, format!("{e}\r\n"));
            return;
        }
    };
    let plain_name = world
        .resource::<AbilityCatalog>()
        .by_name
        .values()
        .find(|d| d.id == ability_id)
        .map_or_else(String::new, |d| d.plain_name.clone());
    let removed = if let Some(mut mem) = world.get_mut::<MemorizedSpells>(player) {
        // Prefer dropping a not-yet-ready entry (cheaper to lose).
        let idx = mem
            .entries
            .iter()
            .position(|e| e.ability_id == ability_id && !e.ready)
            .or_else(|| mem.entries.iter().position(|e| e.ability_id == ability_id));
        if let Some(idx) = idx {
            mem.entries.remove(idx);
            true
        } else {
            false
        }
    } else {
        false
    };
    if removed {
        send_to(world, player, format!("You forget {plain_name}.\r\n"));
    } else {
        send_to(
            world,
            player,
            format!("{plain_name} isn't currently memorized.\r\n"),
        );
    }
}

fn cmd_spells(world: &mut World, player: Entity, args: &str) {
    use mud_db::abilities::AbilityKind;

    let filter = args.trim().to_ascii_lowercase();
    let mode = color_mode_for(world, player);

    // If the player has a KnownAbilities component with any entries, only
    // show abilities they actually know. Empty KnownAbilities (or no
    // component at all) falls back to the full catalog — useful for
    // bare admin tests and for characters whose ability list hasn't
    // been seeded yet.
    let known: Option<std::collections::HashSet<i32>> = world
        .get::<KnownAbilities>(player)
        .filter(|k| !k.entries.is_empty())
        .map(|k| k.entries.iter().map(|(id, _, _)| *id).collect());

    let mut buckets: std::collections::BTreeMap<&'static str, Vec<String>> =
        std::collections::BTreeMap::new();
    for def in world.resource::<AbilityCatalog>().by_name.values() {
        if let Some(set) = &known
            && !set.contains(&def.id)
        {
            continue;
        }
        if !filter.is_empty() && !def.plain_name.to_ascii_lowercase().contains(&filter) {
            continue;
        }
        let bucket = match def.kind {
            AbilityKind::Spell => "Spells",
            AbilityKind::Chant => "Chants",
            AbilityKind::Song => "Songs",
            AbilityKind::Skill => "Skills",
        };
        buckets.entry(bucket).or_default().push(def.name.clone());
    }
    if buckets.is_empty() {
        let scope = if known.is_some() { "you know" } else { "loaded" };
        if filter.is_empty() {
            send_to(world, player, format!("\r\nNo abilities {scope}.\r\n"));
        } else {
            send_rendered(
                world,
                player,
                &format!("\r\nNo abilities matching '{filter}' {scope}.\r\n"),
            );
        }
        return;
    }

    let header = if known.is_some() {
        "Abilities you know"
    } else {
        "All loaded abilities"
    };
    let mut out = format!("\r\n{header}:\r\n");
    for (bucket, names) in &mut buckets {
        names.sort_unstable();
        out.push_str(&format!("{} ({}):\r\n", bucket, names.len()));
        for chunk in names.chunks(3) {
            out.push_str("  ");
            for n in chunk {
                out.push_str(&format!("{:<26}", render_color_tags(n, mode)));
            }
            out.push_str("\r\n");
        }
    }
    send_to(world, player, out);
}

/// Kind-filtered listing for `skills` / `songs` / `chants`. Walks
/// the ability catalog like `cmd_spells` but restricts to a single
/// `AbilityKind`. Honors `KnownAbilities` gating and the optional
/// substring filter (passed as `args`).
fn cmd_abilities_kind(
    world: &mut World,
    player: Entity,
    args: &str,
    kind: mud_db::abilities::AbilityKind,
) {
    let filter = args.trim().to_ascii_lowercase();
    let mode = color_mode_for(world, player);
    let known: Option<std::collections::HashSet<i32>> = world
        .get::<KnownAbilities>(player)
        .filter(|k| !k.entries.is_empty())
        .map(|k| k.entries.iter().map(|(id, _, _)| *id).collect());
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
        if !filter.is_empty() && !def.plain_name.to_ascii_lowercase().contains(&filter) {
            continue;
        }
        names.push(def.name.clone());
    }
    if names.is_empty() {
        let scope = if known.is_some() { "you know" } else { "loaded" };
        let kind_label = kind.label();
        if filter.is_empty() {
            send_to(world, player, format!("\r\nNo {kind_label}s {scope}.\r\n"));
        } else {
            send_rendered(
                world,
                player,
                &format!("\r\nNo {kind_label}s matching '{filter}' {scope}.\r\n"),
            );
        }
        return;
    }
    names.sort_unstable();
    let header = if known.is_some() {
        format!("{}s you know", capitalize(kind.label()))
    } else {
        format!("All loaded {}s", kind.label())
    };
    let mut out = format!("\r\n{header} ({}):\r\n", names.len());
    for chunk in names.chunks(3) {
        out.push_str("  ");
        for n in chunk {
            out.push_str(&format!("{:<26}", render_color_tags(n, mode)));
        }
        out.push_str("\r\n");
    }
    send_to(world, player, out);
}

fn cmd_skills(world: &mut World, player: Entity, args: &str) {
    cmd_abilities_kind(world, player, args, mud_db::abilities::AbilityKind::Skill);
}

fn cmd_songs(world: &mut World, player: Entity, args: &str) {
    cmd_abilities_kind(world, player, args, mud_db::abilities::AbilityKind::Song);
}

fn cmd_chants(world: &mut World, player: Entity, args: &str) {
    cmd_abilities_kind(world, player, args, mud_db::abilities::AbilityKind::Chant);
}

fn cmd_cast(world: &mut World, player: Entity, args: &str) {
    invoke_ability(world, player, args, mud_db::abilities::AbilityKind::Spell, "cast");
}

fn cmd_chant(world: &mut World, player: Entity, args: &str) {
    invoke_ability(world, player, args, mud_db::abilities::AbilityKind::Chant, "chant");
}

fn cmd_perform(world: &mut World, player: Entity, args: &str) {
    invoke_ability(world, player, args, mud_db::abilities::AbilityKind::Song, "perform");
}

/// `skill <name> [<target>]` — Phase A of the data-driven migration.
/// Sibling to `cast`/`chant`/`perform`: looks up an `Ability` row of
/// kind SKILL by name and invokes it through the same `invoke_ability`
/// pipeline (effects, restrictions, posture gate, target resolution).
/// Once Phase B effect-type consumers land, this dispatcher will be
/// the entry point for any combat skill that's just data — gouge,
/// stomp, berserk, etc. all migrate to plain Ability rows.
fn cmd_skill(world: &mut World, player: Entity, args: &str) {
    invoke_ability(world, player, args, mud_db::abilities::AbilityKind::Skill, "use");
}

/// Default duration when an ability spawns an `EffectInstance` from one
/// of its `AbilityEffect` rows. Real per-effect duration lives in
/// `override_params` / `Effect.duration`, but the runtime doesn't yet
/// interpret those; one global default keeps the surface simple until
/// the casting pipeline actually reads them.
const APPLIED_EFFECT_DURATION_SECS: i32 = 60;

/// Shared cast/chant/perform body. Looks up the ability filtered by
/// `kind`, gates on `KnownAbilities`, prints metadata, and spawns
/// `EffectInstance` entities for each linked `AbilityEffect` attached
/// to the caster. Real targeting / damage / restriction-checking is
/// still a follow-up.
// Linear top-to-bottom flow with a few inline metadata blocks; splitting
// into helpers would just hide the ordering.
#[allow(clippy::too_many_lines)]
fn invoke_ability(
    world: &mut World,
    player: Entity,
    args: &str,
    kind: mud_db::abilities::AbilityKind,
    verb: &str,
) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.is_empty() || parts[0].trim().is_empty() {
        send_to(world, player, format!("{} what?\r\n", capitalize(verb)));
        return;
    }
    let needle = parts[0].trim().to_ascii_lowercase();
    let target_word = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty());

    // Find by exact key (and right kind) first, then fall back to the
    // first substring match restricted to the same kind.
    let catalog = world.resource::<AbilityCatalog>();
    let def = catalog
        .by_name
        .get(&needle)
        .filter(|d| d.kind == kind)
        .cloned()
        .or_else(|| {
            catalog
                .by_name
                .values()
                .find(|d| d.kind == kind && d.plain_name.to_ascii_lowercase().contains(&needle))
                .cloned()
        });
    let Some(def) = def else {
        send_to(
            world,
            player,
            format!("No {} matching '{needle}'.\r\n", kind.label()),
        );
        return;
    };

    // Anti-magic / silence gate. SPELL/CHANT/SONG kinds are
    // verbal-magical; SKILL bypasses the gate (pure-physical action).
    if !matches!(kind, mud_db::abilities::AbilityKind::Skill)
        && effect_prevents(world, player, Prevent::Casting)
    {
        send_to(world, player, "Your magic is suppressed.\r\n");
        return;
    }

    // Gate on KnownAbilities when the player has any. Empty/missing
    // component falls through (admin testing path).
    if let Some(known) = world.get::<KnownAbilities>(player)
        && !known.entries.is_empty()
        && !known.has_any(def.id)
    {
        send_to(
            world,
            player,
            format!("You don't know how to {} {}.\r\n", verb, def.plain_name),
        );
        return;
    }

    // Gate on memorization when the ability is a Spell AND the
    // caster's class has it in `ClassAbilities` (i.e. it lands in
    // a circle slot for this class). Off-class spells, non-Spell
    // kinds (Skill / Chant / Song), and classless casters skip the
    // gate. Successful gate consumes one entry from MemorizedSpells
    // — failed dispatches downstream still pay the slot, mirroring
    // legacy "fizzles burn the prep" semantics.
    if matches!(def.kind, mud_db::abilities::AbilityKind::Spell) {
        let class_id = world.get::<Profile>(player).and_then(|p| p.class_id);
        if let Some(class_id) = class_id
            && world
                .resource::<mud_world::SpellSlotData>()
                .ability_circle
                .contains_key(&(class_id, def.id))
        {
            // Find the first READY entry for this ability. A
            // not-ready entry doesn't satisfy the gate — bodies
            // that are still preparing don't count.
            let memorized_idx = world
                .get::<mud_world::MemorizedSpells>(player)
                .and_then(|m| {
                    m.entries
                        .iter()
                        .position(|e| e.ability_id == def.id && e.ready)
                });
            let Some(idx) = memorized_idx else {
                send_to(
                    world,
                    player,
                    format!(
                        "You haven't memorized {}. Use `memorize {}` first.\r\n",
                        def.plain_name,
                        def.plain_name.to_ascii_lowercase()
                    ),
                );
                return;
            };
            if let Some(mut mem) = world.get_mut::<mud_world::MemorizedSpells>(player) {
                mem.entries.remove(idx);
            }
        }
    }

    // Combat-state gates (Ability.in_combat_only / combat_ok).
    // `in_combat_only` refuses casts when the caster has no Fighting;
    // `combat_ok=false` refuses while engaged. Both flags are
    // displayed in the cast/skill output today; this turns them into
    // live gates.
    let caster_in_combat = world.get::<Fighting>(player).is_some();
    if def.in_combat_only && !caster_in_combat {
        send_to(world, player, format!("You can only {verb} {} in combat.\r\n", def.plain_name));
        return;
    }
    if !def.combat_ok && caster_in_combat {
        send_to(world, player, format!("You can't {verb} {} while fighting.\r\n", def.plain_name));
        return;
    }

    // Posture gate (Ability.minPosition). Most abilities require STANDING;
    // a few are SITTING-OK. Anything below the runtime's modeled postures
    // (rank ≤ 6 SLEEPING) passes for every alive player.
    let cur_rank = world.get::<Posture>(player).map_or(9, |p| p.0.rank());
    if cur_rank < def.min_posture_rank {
        send_to(
            world,
            player,
            format!(
                "You can't {verb} {} while {}.\r\n",
                def.plain_name,
                world
                    .get::<Posture>(player)
                    .map_or("incapacitated", |p| p.0.label()),
            ),
        );
        return;
    }

    // Cooldown gate (Ability.cooldown_ms). Only abilities with
    // cooldown_ms > 0 are enforced; the per-character `Cooldowns`
    // component carries an Instant per ability.id at which the cooldown
    // expires. Stale entries (in the past) are silently treated as
    // expired and overwritten on next successful cast.
    if def.cooldown_ms > 0
        && let Some(cd) = world.get::<Cooldowns>(player)
        && let Some(ready_at) = cd.ready_at.get(&def.id).copied()
    {
        let now = std::time::Instant::now();
        if ready_at > now {
            let remaining = ready_at.saturating_duration_since(now);
            let secs = remaining.as_secs_f32().max(0.1);
            send_to(
                world,
                player,
                format!(
                    "You can't {verb} {} yet — {secs:.1}s remaining.\r\n",
                    def.plain_name,
                ),
            );
            return;
        }
    }

    let mode = color_mode_for(world, player);
    let mut out = String::from("\r\n");
    out.push_str(&format!(
        "  {} ({})\r\n",
        render_color_tags(&def.name, mode),
        def.kind.label()
    ));
    if let Some(desc) = &def.description {
        out.push_str(&format!("    {}\r\n", render_color_tags(desc.trim(), mode)));
    }
    out.push_str(&format!(
        "    cast time: {} round(s)   cooldown: {}ms   {}area\r\n",
        def.cast_time_rounds,
        def.cooldown_ms,
        if def.is_area { "" } else { "single-target / not " }
    ));
    out.push_str(&format!(
        "    requires posture: {}\r\n",
        def.min_position_label,
    ));
    out.push_str(&format!(
        "    {}{}{}\r\n",
        if def.violent { "violent  " } else { "" },
        if def.in_combat_only { "combat-only  " } else { "" },
        if def.combat_ok { "" } else { "non-combat  " },
    ));
    // Resolve the target. Empty / "me" / "self" → the caster.
    // Anything else → look up an actor in the caster's room. If the
    // word doesn't resolve, abort before applying any effects.
    let target_entity = if let Some(word) = target_word
        && !word.eq_ignore_ascii_case("me")
        && !word.eq_ignore_ascii_case("self")
    {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere; can't target.\r\n");
            return;
        };
        let Some(found) = find_actor_in_room(world, word, located.0, player) else {
            send_to(
                world,
                player,
                format!("You don't see '{word}' here to target.\r\n"),
            );
            return;
        };
        found
    } else {
        player
    };
    if target_entity == player {
        out.push_str("    target: yourself\r\n");
    } else {
        let target_name = name_or(world, target_entity, "<unknown>");
        out.push_str(&format!(
            "    target: {}\r\n",
            render_color_tags(&target_name, mode),
        ));
    }
    // AbilityTargeting gate: refuse if the resolved target doesn't
    // match the schema's `valid_targets` list. Only enforces the
    // recognized types (ENEMY_PC, ENEMY_NPC); CORPSE / RIDER /
    // OBJECT_INV / UNCONSCIOUS pass silently until those entity
    // categories are modeled. Abilities without a row pass through.
    if let Some(rule) = world
        .resource::<AbilityCatalog>()
        .targeting
        .get(&def.id)
        .cloned()
        && let Some(refusal) =
            check_target_type(world, player, target_entity, &rule.valid_targets)
    {
        send_to(world, player, format!("{refusal}\r\n"));
        return;
    }
    // Live gate: walk AbilityRestrictions and refuse the cast on the
    // first failing rule, emitting that rule's `message` to the
    // caster. Unknown rule types pass — the runtime grows interpretation
    // incrementally. Falls back to no-op for abilities without a
    // restrictions row.
    if let Some(rules) = world
        .resource::<AbilityCatalog>()
        .restriction_rules
        .get(&def.id)
        .cloned()
        && let Some(refusal) =
            check_ability_restrictions(world, player, target_entity, &rules)
    {
        let actor_name = name_of(world, player);
        let target_name = if target_entity == player {
            actor_name.clone()
        } else {
            name_or(world, target_entity, "<unknown>")
        };
        let rendered = render_ability_template(
            &refusal,
            &actor_name,
            &target_name,
            target_entity == player,
        );
        send_to(world, player, format!("{rendered}\r\n"));
        return;
    }
    // (The legacy "requires:" informational block was removed once
    // the rules became live — the messages are written as failure
    // text, so showing them on success is misleading. The player
    // sees them only when the gate refuses the cast above.)
    // Look up the effects this ability applies and dispatch each by
    // its `Effect.effectType`. `heal` is applied immediately to the
    // target's `Health` (or `Stamina` when `resource = "move"`); other
    // types (`status`, `modify`, ...) spawn an `EffectInstance` whose
    // duration the effect/regen ticks decrement.
    let caster_level = world.get::<Profile>(player).map_or(1, |p| p.level.max(1));
    let caster_skill = world
        .get::<KnownAbilities>(player)
        .and_then(|k| k.entries.iter().find(|(id, _, _)| *id == def.id).map(|(_, p, _)| *p))
        .unwrap_or(0);
    let caster_weapon_damage = caster_weapon_damage(world, player);
    let caster_stats = world.get::<CoreStats>(player).copied().unwrap_or_default();
    let caster_hidden = i32::from(world.get::<Stealth>(player).is_some());
    let formula_ctx = FormulaCtx {
        level: caster_level,
        skill: caster_skill,
        weapon_damage: caster_weapon_damage,
        str_bonus: CoreStats::bonus(caster_stats.strength),
        dex_bonus: CoreStats::bonus(caster_stats.dexterity),
        con_bonus: CoreStats::bonus(caster_stats.constitution),
        int_bonus: CoreStats::bonus(caster_stats.intelligence),
        wis_bonus: CoreStats::bonus(caster_stats.wisdom),
        cha_bonus: CoreStats::bonus(caster_stats.charisma),
        hidden: caster_hidden,
    };
    let effect_specs: Vec<EffectSpec> = {
        let mappings = world
            .resource::<AbilityCatalog>()
            .effects_for
            .get(&def.id)
            .cloned()
            .unwrap_or_default();
        let effect_catalog = world.resource::<EffectCatalog>();
        mappings
            .iter()
            .filter_map(|(id, override_params)| {
                effect_catalog.by_id.get(id).map(|e| {
                    // Per-instance name: prefer `flag` from
                    // override_params (the schema's per-mapping label
                    // — BERSERK sets flag="berserk" on a generic
                    // `status` effect). Fall back to the EffectDef's
                    // name. Without this, BERSERK / BLESS / BLUR /
                    // CHARM all spawn EffectInstance.name="status",
                    // which loses meaningful identity for the
                    // effects-list display, the combat tick's
                    // berserk damage bonus, and cleanse/dispel
                    // matching.
                    let flag = override_params
                        .as_ref()
                        .and_then(|p| p.get("flag"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                    EffectSpec {
                        id: *id,
                        name: flag.unwrap_or_else(|| e.name.clone()),
                        effect_type: e.effect_type.clone(),
                        override_params: override_params.clone(),
                        default_params: e.default_params.clone(),
                    }
                })
            })
            .collect()
    };
    // Capture caster + target names *before* effects apply. The
    // damage arm can despawn the target mid-loop; later rendering
    // would otherwise see `<unknown>` and angle-bracket-eat the
    // template through XML-Lite color rendering. Same for the
    // AbilityMessages set lookup — pull it once up front.
    let messages_pre = world
        .resource::<AbilityCatalog>()
        .messages
        .get(&def.id)
        .cloned();
    let actor_name_pre = name_of(world, player);
    let target_name_pre = if target_entity == player {
        actor_name_pre.clone()
    } else {
        name_or(world, target_entity, "<unknown>")
    };
    // Saving-throw resolution. If the ability has a row in
    // AbilitySavingThrow, evaluate the DC against caster's
    // FormulaCtx, roll d20 + target's level (proxy for save bonus
    // until full per-stat save calc lands), and branch on
    // on_save_action: NEGATE → skip all effects; HALF_DURATION →
    // halve the duration that's spawned for status/modify/knockdown
    // arms. Self-targeted saves auto-fail (caster doesn't resist
    // their own buff).
    let save_action = if target_entity == player {
        SaveOutcome::Failed
    } else {
        save_action_for(world, &def, target_entity, &formula_ctx)
    };
    if matches!(save_action, SaveOutcome::Negated) {
        let target_name = if target_entity == player {
            actor_name_pre.clone()
        } else {
            name_or(world, target_entity, "<unknown>")
        };
        send_to(
            world,
            player,
            format!("{target_name} resists your {}.\r\n", def.plain_name),
        );
        if target_entity != player {
            send_rendered(
                world,
                target_entity,
                &format!(
                    "You resist {}'s {}.\r\n",
                    actor_name_pre, def.plain_name,
                ),
            );
        }
        return;
    }
    let halve_duration = matches!(save_action, SaveOutcome::HalfDuration);
    let mut applied_msgs: Vec<String> = Vec::with_capacity(effect_specs.len());
    let mut spawn_count: usize = 0;
    for spec in &effect_specs {
        match spec.effect_type.as_str() {
            "damage" => {
                // Resolve `amount`. If the ability has
                // AbilityDamageComponent rows, sum each component's
                // formula scaled by its percentage — that's the
                // multi-element damage path used by spells like
                // CONE_OF_COLD (90% COLD, 10% FORCE). Otherwise
                // fall back to override_params.amount.
                // Per-element resistance application is a follow-up
                // that needs Resistances components on entities.
                let components = world
                    .resource::<AbilityCatalog>()
                    .damage_components
                    .get(&def.id)
                    .cloned()
                    .unwrap_or_default();
                let mut amount = if components.is_empty() {
                    resolve_effect_amount(
                        spec.override_params.as_ref(),
                        Some(&spec.default_params),
                        &formula_ctx,
                    )
                    .unwrap_or(0)
                } else {
                    let mut total = 0i32;
                    for c in &components {
                        let raw = evaluate_simple_formula_ctx(
                            &normalize_dice_notation(&c.damage_formula),
                            &formula_ctx,
                        )
                        .unwrap_or(0);
                        let scaled = raw.saturating_mul(c.percentage) / 100;
                        total = total.saturating_add(scaled);
                    }
                    total
                };
                // BACKSTAB-style `bonusIfHidden` — extra damage when
                // the caster has the Stealth marker. Field lives on
                // the AbilityEffect override; reads as either a
                // literal int or a formula string (e.g. `hidden * 0.5`).
                // Skipped when caster.hidden == 0.
                if formula_ctx.hidden > 0
                    && let Some(bonus) = bonus_if_hidden_from_blob(
                        spec.override_params.as_ref(),
                        &formula_ctx,
                    )
                {
                    amount = amount.saturating_add(bonus);
                }
                if amount > 0 {
                    let (dead, threshold_msg) =
                        crate::commands::apply_damage(world, target_entity, amount);
                    // Surface the apply_damage threshold message
                    // ("You are hurt." / "...badly hurt!" / "...near
                    // death!") to the target so they get the same
                    // feedback melee combat already provides.
                    // Always to the target — even for self-cast damage,
                    // the caster benefits from the threshold cue.
                    if !dead
                        && let Some(line) = threshold_msg
                    {
                        send_to(world, target_entity, line.to_string());
                    }
                    if dead
                        && let Some(located) = world.get::<Located>(target_entity).copied()
                    {
                        let target_name = name_or(world, target_entity, "<unknown>");
                        crate::combat::handle_death(
                            world,
                            target_entity,
                            &target_name,
                            located.0,
                        );
                    }
                }
                applied_msgs.push(format!("{} (-{} HP)", spec.name, amount));
            }
            "heal" => {
                let amount = resolve_effect_amount(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                    &formula_ctx,
                );
                let Some(amount) = amount else {
                    applied_msgs.push(format!("{} (no amount resolved)", spec.name));
                    continue;
                };
                let resource = resolve_effect_resource(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                );
                let healed = match resource.as_str() {
                    "move" | "stamina" => apply_heal_stamina(world, target_entity, amount),
                    _ => apply_heal_hp(world, target_entity, amount),
                };
                let resource_label = if resource == "move" || resource == "stamina" {
                    "stamina"
                } else {
                    "HP"
                };
                applied_msgs.push(format!("{} (+{healed} {resource_label})", spec.name));
            }
            "cleanse" => {
                let conditions = resolve_effect_conditions(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                );
                if conditions.is_empty() {
                    applied_msgs.push(format!("{} (no condition specified)", spec.name));
                    continue;
                }
                let removed: usize = if conditions.iter().any(|c| c == "all") {
                    remove_all_effects_on(world, target_entity)
                } else {
                    let mut total = 0usize;
                    for cond in &conditions {
                        total += remove_effect_named(world, target_entity, cond);
                    }
                    total
                };
                applied_msgs.push(if removed == 0 {
                    format!("{} (nothing to cleanse)", spec.name)
                } else {
                    format!("{} (cleansed {removed} effect(s))", spec.name)
                });
            }
            "stun" => {
                // Mark the target as Stunned (skips combat swings)
                // and also spawn the EffectInstance so the effect
                // appears in the listing and the duration ticks down.
                // `effects_tick` removes the Stunned marker once the
                // last "stun" EffectInstance on the target expires.
                crate::commands::try_insert(world, target_entity, Stunned);
                let mut dur_secs = resolve_effect_duration(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                    &formula_ctx,
                );
                if halve_duration {
                    dur_secs = (dur_secs / 2).max(1);
                }
                world.spawn((
                    EffectInstance {
                        kind: spec.id,
                        name: spec.name.clone(),
                        strength: 1,
                        remaining_secs: dur_secs,
                        source: EffectSource::Spell,
                        ability_id: Some(def.id),
                    },
                    AppliedTo(target_entity),
                ));
                spawn_count += 1;
                applied_msgs.push(format!("{} (stunned)", spec.name));
            }
            "dispel" => {
                // Remove EffectInstances on the target whose source
                // EffectDef carries the configured tag (e.g. "magic",
                // "buff", "debuff"). Power/saving-throw resistance
                // not yet modeled — every dispel succeeds. Scope
                // "first" stops after one removal; "all" strips
                // everything matching.
                let filter = resolve_dispel_filter(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                );
                let scope = resolve_dispel_scope(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                );
                if filter.is_empty() {
                    applied_msgs.push(format!("{} (no filter specified)", spec.name));
                    continue;
                }
                let removed = remove_effects_by_tag(world, target_entity, &filter, scope);
                applied_msgs.push(if removed == 0 {
                    format!("{} (nothing to dispel)", spec.name)
                } else {
                    format!("{} (dispelled {removed} effect(s))", spec.name)
                });
            }
            "redirect" => {
                // Two semantics live under `redirect`:
                //   aggro=true  → rescue/intercept: take the target's
                //                 attacker as your own combatant.
                //   aggro=false → damage redirect (percent of damage
                //                 from target sent to caster) — not
                //                 yet implemented.
                let aggro = resolve_redirect_aggro(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                );
                if !aggro {
                    applied_msgs.push(format!(
                        "{} (damage-redirect not implemented)",
                        spec.name
                    ));
                    continue;
                }
                if target_entity == player {
                    applied_msgs.push(format!("{} (can't rescue yourself)", spec.name));
                    continue;
                }
                let Some(Fighting(attacker)) =
                    world.get::<Fighting>(target_entity).copied()
                else {
                    applied_msgs.push(format!("{} (target isn't being attacked)", spec.name));
                    continue;
                };
                if world.get_entity(attacker).is_err() {
                    applied_msgs.push(format!("{} (attacker has vanished)", spec.name));
                    continue;
                }
                crate::commands::try_remove::<Fighting>(world, target_entity);
                crate::commands::try_insert(world, attacker, Fighting(player));
                crate::commands::try_insert(world, player, Fighting(attacker));
                applied_msgs.push(format!("{} (drew attacker's aggro)", spec.name));
            }
            "stop_combat" => {
                // Remove `Fighting` from the target so it disengages.
                // Doesn't disengage *attackers of* the target — for
                // that, use `disengage_attackers_of`. The effect is
                // instant; no EffectInstance is spawned.
                let was_fighting = world.get::<Fighting>(target_entity).is_some();
                if was_fighting {
                    crate::commands::try_remove::<Fighting>(world, target_entity);
                    applied_msgs.push(format!("{} (combat ended)", spec.name));
                } else {
                    applied_msgs.push(format!("{} (not in combat)", spec.name));
                }
            }
            "portal" => {
                // Spawn a specific Object proto in the caster's room
                // (Heavens Gate, Hell's Gate, Moonwell). The schema
                // pins exact prototypes via objectZoneId/objectId; we
                // also spawn a `decay`-named EffectInstance applied
                // to the new object so `effects_tick` despawns it
                // when the lifetime expires.
                let proto_zone = spec
                    .override_params
                    .as_ref()
                    .and_then(|p| p.get("objectZoneId"))
                    .and_then(serde_json::Value::as_i64)
                    .map(|v| i32::try_from(v).unwrap_or(0));
                let proto_id = spec
                    .override_params
                    .as_ref()
                    .and_then(|p| p.get("objectId"))
                    .and_then(serde_json::Value::as_i64)
                    .map(|v| i32::try_from(v).unwrap_or(0));
                let (Some(proto_zone), Some(proto_id)) = (proto_zone, proto_id) else {
                    applied_msgs.push(format!("{} (no object proto specified)", spec.name));
                    continue;
                };
                let proto = world
                    .resource::<ObjectPrototypes>()
                    .by_key
                    .get(&(proto_zone, proto_id))
                    .cloned();
                let Some(proto) = proto else {
                    applied_msgs.push(format!(
                        "{} (object proto ({proto_zone}, {proto_id}) not loaded)",
                        spec.name
                    ));
                    continue;
                };
                let Some(located) = world.get::<Located>(player).copied() else {
                    applied_msgs.push(format!("{} (caster has no room)", spec.name));
                    continue;
                };
                let mut bundle = world.spawn((
                    Item,
                    Named { name: proto.name.clone() },
                    Keywords(proto.keywords.clone()),
                    WorldKey {
                        zone: proto.zone_id,
                        id: proto.id,
                    },
                    Located(located.0),
                ));
                if let Some(desc) = proto.examine_description.clone() {
                    bundle.insert(Description(desc));
                }
                let portal_entity = bundle.id();
                // Decay duration: the schema's `decay` is in hours
                // (matches other duration units). Convert to seconds
                // for the EffectInstance.
                let decay_hours = spec
                    .override_params
                    .as_ref()
                    .and_then(|p| p.get("decay"))
                    .and_then(serde_json::Value::as_i64)
                    .map_or(1, |v| i32::try_from(v).unwrap_or(1));
                let decay_secs = decay_hours.saturating_mul(3600);
                world.spawn((
                    EffectInstance {
                        kind: spec.id,
                        name: "decay".to_string(),
                        strength: 1,
                        remaining_secs: decay_secs,
                        source: EffectSource::Spell,
                        ability_id: Some(def.id),
                    },
                    AppliedTo(portal_entity),
                ));
                applied_msgs.push(format!("{} ({} appears)", spec.name, proto.name));
            }
            "modify" => {
                // Stat-bonus stacking. Read `target` (which stat) and
                // `amount` (signed delta) from params; resolve the
                // amount through the formula evaluator. Apply the
                // delta to the target's component now and stash a
                // `ModifyDelta` companion on the effect entity so the
                // tick can subtract the same delta on expiry — that
                // keeps stacking buffs from each other's expiries.
                //
                // Supported stat targets (see `apply_modify_delta`):
                //   - CoreStats:    str/dex/con/int/wis/cha
                //   - CombatStats:  hitroll, damroll, ward (lower
                //                   AC = better; ward+N → ac-=N)
                //   - Maxes:        max_hp, max_move/max_stamina
                // Unsupported targets (eva, acc, focus, size,
                // unarmed_damage, weapon_hitroll, save_spell, ...)
                // spawn a labeled effect without applying anything.
                let target_stat = spec
                    .override_params
                    .as_ref()
                    .and_then(|p| p.get("target"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_ascii_lowercase);
                let amount = resolve_effect_amount(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                    &formula_ctx,
                );
                let mut dur_secs = resolve_effect_duration(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                    &formula_ctx,
                );
                if halve_duration {
                    dur_secs = (dur_secs / 2).max(1);
                }
                let applied_amount = match (target_stat.as_deref(), amount) {
                    (Some(t), Some(a)) if a != 0 => {
                        if apply_modify_delta(world, target_entity, t, a) {
                            Some(a)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                let mut bundle = world.spawn((
                    EffectInstance {
                        kind: spec.id,
                        name: target_stat
                            .clone()
                            .unwrap_or_else(|| spec.name.clone()),
                        strength: applied_amount.unwrap_or(0),
                        remaining_secs: dur_secs,
                        source: EffectSource::Spell,
                        ability_id: Some(def.id),
                    },
                    AppliedTo(target_entity),
                ));
                if let (Some(t), Some(a)) = (target_stat.as_deref(), applied_amount) {
                    bundle.insert(mud_world::ModifyDelta {
                        target: t.to_string(),
                        amount: a,
                    });
                }
                spawn_count += 1;
                applied_msgs.push(match (target_stat.as_deref(), applied_amount) {
                    (Some(t), Some(a)) => {
                        let sign = if a >= 0 { "+" } else { "" };
                        format!("{} ({sign}{a} {t})", spec.name)
                    }
                    (Some(t), None) => format!("{} ({t}: unsupported target)", spec.name),
                    (None, _) => format!("{} (no target specified)", spec.name),
                });
            }
            "intercept" => {
                // GUARD's bodyguard semantics: install
                // `Guarding(target)` on the caster so the existing
                // combat tick redirects ally-targeted swings to the
                // caster. Refuses self-target — guarding yourself is
                // a no-op the schema doesn't model.
                if target_entity == player {
                    applied_msgs.push(format!("{} (can't guard yourself)", spec.name));
                    continue;
                }
                if world.get_entity(target_entity).is_err() {
                    applied_msgs.push(format!("{} (target has vanished)", spec.name));
                    continue;
                }
                try_insert(world, player, mud_world::Guarding(target_entity));
                applied_msgs.push(format!("{} (guarding {})", spec.name, name_of(world, target_entity)));
            }
            "extract" => {
                // Remove the target from the world. Used by Banish
                // (and any future "send back to home plane" /
                // "evict from this dimension" abilities). Players are
                // never extracted — that path leads to lost data and
                // is reserved for admin commands. Mobs are despawned
                // outright; their effects, equipment, and triggers
                // get the same cleanup as mob death.
                if world.get::<Player>(target_entity).is_some() {
                    applied_msgs.push(format!("{} (can't extract a player)", spec.name));
                    continue;
                }
                if world.get::<Mob>(target_entity).is_none() {
                    applied_msgs.push(format!("{} (target isn't a creature)", spec.name));
                    continue;
                }
                disengage_attackers_of(world, target_entity);
                if let Ok(e) = world.get_entity_mut(target_entity) {
                    e.despawn();
                }
                applied_msgs.push(format!("{} (banished)", spec.name));
            }
            "dismount" => {
                // Force-end the rider/mount relationship on the
                // target entity. Works both directions: target might
                // be the rider (Mounted → mount) or the mount itself
                // (RiddenBy → rider). Either way, both sides clear.
                // BUCK uses this with `forced: true` (the mount throws
                // its rider); the explicit DISMOUNT skill uses
                // `forced: false`. The schema flag is informational
                // for now — both branches do the same removal.
                let mut cleared = false;
                if let Some(mud_world::Mounted(mount)) =
                    world.get::<mud_world::Mounted>(target_entity).copied()
                {
                    try_remove::<mud_world::Mounted>(world, target_entity);
                    try_remove::<mud_world::RiddenBy>(world, mount);
                    cleared = true;
                } else if let Some(mud_world::RiddenBy(rider)) =
                    world.get::<mud_world::RiddenBy>(target_entity).copied()
                {
                    try_remove::<mud_world::RiddenBy>(world, target_entity);
                    try_remove::<mud_world::Mounted>(world, rider);
                    cleared = true;
                }
                applied_msgs.push(if cleared {
                    format!("{} (dismounted)", spec.name)
                } else {
                    format!("{} (not mounted)", spec.name)
                });
            }
            "teleport" => {
                // Move the target to a destination resolved from
                // params. v1 handles:
                //   - "recall" / "home" → target's RecallPoint
                //   - "caster"          → the ability's caster's room
                //   - "target"          → the *original* target's room
                //                         (only meaningful when the
                //                         caller passes another entity)
                // Other destinations ("random", "object") fall through
                // to a log message — nothing teleports.
                let destination = resolve_teleport_destination(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                );
                let dest_room: Option<Entity> = match destination.as_deref() {
                    Some("recall" | "home") => {
                        world.get::<RecallPoint>(target_entity).map(|r| r.0)
                    }
                    Some("caster") => world.get::<Located>(player).map(|l| l.0),
                    Some("target") => {
                        // For caster-target, the schema's "target" usually
                        // means the targeted entity from the cast. Since
                        // target_entity is already that entity, this is a
                        // no-op (target already there).
                        if target_entity == player {
                            None
                        } else {
                            world.get::<Located>(target_entity).map(|l| l.0)
                        }
                    }
                    _ => None,
                };
                let Some(dest_room) = dest_room else {
                    applied_msgs.push(format!(
                        "{} (destination {:?} not resolvable)",
                        spec.name, destination
                    ));
                    continue;
                };
                let cur_room = world.get::<Located>(target_entity).map(|l| l.0);
                if cur_room == Some(dest_room) {
                    applied_msgs.push(format!("{} (already there)", spec.name));
                    continue;
                }
                if let Some(mut l) = world.get_mut::<Located>(target_entity) {
                    l.0 = dest_room;
                }
                applied_msgs.push(format!("{} (teleported)", spec.name));
            }
            "knockdown" => {
                // Knockdown has two parts: an immediate posture
                // mutation (so the target is on the ground *now*) and
                // a duration-tracked EffectInstance (so the effect
                // shows up in `effects` and decays). Posture isn't
                // bound to the effect's lifetime — matches the C++
                // behavior where `stand` is the recovery action.
                let posture = resolve_knockdown_posture(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                );
                let toppled = apply_knockdown_posture(world, target_entity, posture);
                let mut dur_secs = resolve_effect_duration(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                    &formula_ctx,
                );
                if halve_duration {
                    dur_secs = (dur_secs / 2).max(1);
                }
                world.spawn((
                    EffectInstance {
                        kind: spec.id,
                        name: spec.name.clone(),
                        strength: 1,
                        remaining_secs: dur_secs,
                        source: EffectSource::Spell,
                        ability_id: Some(def.id),
                    },
                    AppliedTo(target_entity),
                ));
                spawn_count += 1;
                applied_msgs.push(if toppled {
                    format!("{} (knocked {})", spec.name, posture.label())
                } else {
                    format!("{} (already {} or lower)", spec.name, posture.label())
                });
            }
            _ => {
                let mut dur_secs = resolve_effect_duration(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                    &formula_ctx,
                );
                if halve_duration {
                    dur_secs = (dur_secs / 2).max(1);
                }
                world.spawn((
                    EffectInstance {
                        kind: spec.id,
                        name: spec.name.clone(),
                        strength: 1,
                        remaining_secs: dur_secs,
                        source: EffectSource::Spell,
                        ability_id: Some(def.id),
                    },
                    AppliedTo(target_entity),
                ));
                spawn_count += 1;
                // Stealth-flag status effects (HIDE, SNEAK, CONCEAL,
                // and a few buff spells) install the `Stealth` marker
                // on the target so existing visibility gates fire. The
                // marker is removed in `effects_tick` once the last
                // backing EffectInstance fades — mirroring the
                // Stunned tick pattern.
                if spec.name.eq_ignore_ascii_case("hidden")
                    || spec.name.eq_ignore_ascii_case("sneak")
                {
                    try_insert(world, target_entity, mud_world::Stealth);
                }
                // Charmed-flag status effects (TAME, CHARM-PERSON,
                // SUMMON-FAMILIAR, etc.) install `Follower(caster)`
                // on a Mob target so the existing pet-handling and
                // group-walk code treats it as the player's pet. Not
                // applied to Player targets since charming a player
                // through Follower would corrupt their group state.
                // No auto-remove on expiry — mob charm in legacy
                // MUDs persists until dismiss / death.
                if spec.name.eq_ignore_ascii_case("charmed")
                    && world.get::<Mob>(target_entity).is_some()
                    && world.get::<Player>(target_entity).is_none()
                {
                    try_insert(world, target_entity, Follower(player));
                }
                applied_msgs.push(spec.name.clone());
            }
        }
    }
    // Pull pre-loop captures forward — the damage arm can despawn
    // the target mid-loop, so we use the names captured before the
    // effects fired.
    let messages = messages_pre;
    let actor_name = actor_name_pre;
    let target_name_raw = target_name_pre;
    if applied_msgs.is_empty() {
        out.push_str(&format!(
            "    (no effects defined for this {} — nothing to apply)\r\n",
            kind.label()
        ));
    } else {
        // Caster line: templated success_to_self (when self-targeted)
        // → success_to_caster → fall through to the dispatcher's
        // terse "you {verb} X" form.
        let caster_template = messages.as_ref().and_then(|m| {
            if target_entity == player {
                m.success_to_self.as_deref().or(m.success_to_caster.as_deref())
            } else {
                m.success_to_caster.as_deref()
            }
        });
        if let Some(t) = caster_template {
            let rendered = render_ability_template(
                t,
                &actor_name,
                &target_name_raw,
                target_entity == player,
            );
            out.push_str(&format!("    {}\r\n", render_color_tags(&rendered, mode)));
        } else if target_entity == player {
            out.push_str(&format!("    you {verb} {}\r\n", def.plain_name));
        } else {
            out.push_str(&format!(
                "    you {verb} {} on {}\r\n",
                def.plain_name,
                render_color_tags(&target_name_raw, mode),
            ));
        }
        // Diagnostic effect summary. Always shown so the player can
        // see HP/posture/duration outcomes regardless of whether the
        // template emitted.
        out.push_str(&format!("    ({})\r\n", applied_msgs.join(", ")));
    }
    send_to(world, player, out);
    // Target-side: templated success_to_victim → terse default.
    if target_entity != player && !applied_msgs.is_empty() {
        let target_template = messages.as_ref().and_then(|m| m.success_to_victim.as_deref());
        let line = if let Some(t) = target_template {
            // success_to_victim is rendered for the *victim* — they're
            // never the actor, so reflexive collapse doesn't apply.
            render_ability_template(t, &actor_name, &target_name_raw, false)
        } else {
            format!(
                "{actor_name} {verb}s {} on you. ({} effect(s))",
                def.plain_name,
                applied_msgs.len()
            )
        };
        send_rendered(world, target_entity, &format!("{line}\r\n"));
    }
    // Room broadcast: success_to_room (or success_self_room when
    // self-targeted). Skipped if the ability has no template — the
    // dispatcher previously emitted nothing to bystanders, so this
    // is purely additive.
    let room_template = messages.as_ref().and_then(|m| {
        if target_entity == player {
            m.success_self_room
                .as_deref()
                .or(m.success_to_room.as_deref())
        } else {
            m.success_to_room.as_deref()
        }
    });
    if !applied_msgs.is_empty()
        && let Some(t) = room_template
        && let Some(located) = world.get::<Located>(player).copied()
    {
        // Bystanders see actor + target as third parties — never reflexive.
        let rendered = render_ability_template(t, &actor_name, &target_name_raw, false);
        let mut except: Vec<Entity> = vec![player];
        if target_entity != player {
            except.push(target_entity);
        }
        broadcast_room_except_rendered(world, located.0, &except, &format!("{rendered}\r\n"));
    }
    // Cooldown write-back: only when the cast actually applied at
    // least one effect (skips no-op casts like "nothing to dispel" so
    // a player isn't penalized for misfires).
    if def.cooldown_ms > 0 && !applied_msgs.is_empty() {
        let ready_at = std::time::Instant::now()
            + std::time::Duration::from_millis(u64::try_from(def.cooldown_ms).unwrap_or(0));
        let mut cd = world
            .get_mut::<Cooldowns>(player)
            .map(|mut c| std::mem::take(&mut *c))
            .unwrap_or_default();
        cd.ready_at.insert(def.id, ready_at);
        crate::commands::try_insert(world, player, cd);
    }
    let _ = spawn_count;
}

/// Validate that `target` matches at least one entry in
/// `valid_targets`. Returns Some(message) on refusal, None on pass
/// (including when no recognized type can be evaluated — partially
/// modeled abilities pass rather than break).
///
/// Recognized target types in v1:
/// - `ENEMY_PC`  : target is a `Player` and ≠ caster
/// - `ENEMY_NPC` : target is a `Mob`
///
/// Other types (`CORPSE`, `OBJECT_INV`, `RIDER`, `UNCONSCIOUS`) pass
/// silently — they need entity categories the runtime doesn't model
/// yet.
/// What happens when a target makes a saving throw.
#[derive(Debug, Clone, Copy)]
enum SaveOutcome {
    /// No save was rolled, or the target failed it. Effects apply
    /// normally.
    Failed,
    /// Target made the save and the action is `NEGATE` — skip all
    /// effect application, send a "resists" message.
    Negated,
    /// Target made the save and the action is `HALF_DURATION` —
    /// effects still apply but spawn with half their normal
    /// duration.
    HalfDuration,
}

/// Roll a saving throw against an ability's `AbilitySavingThrow`
/// row when one exists. Returns `Failed` (effects apply normally)
/// when there's no row, the formula doesn't resolve, or the roll
/// misses the DC. The save bonus is target's `Profile.level`
/// today — full per-stat save calc is a follow-up that needs mob
/// `CoreStats` first.
fn save_action_for(
    world: &mut World,
    def: &mud_world::AbilityDef,
    target: Entity,
    formula_ctx: &FormulaCtx,
) -> SaveOutcome {
    let Some(save) = world
        .resource::<AbilityCatalog>()
        .saves
        .get(&def.id)
        .cloned()
    else {
        return SaveOutcome::Failed;
    };
    let Some(dc) = evaluate_simple_formula_ctx(&save.dc_formula, formula_ctx) else {
        return SaveOutcome::Failed;
    };
    let target_level = world
        .get::<Profile>(target)
        .map_or(1, |p| p.level.max(1));
    // Roll a d20 plus target's level. Save succeeds if total ≥ DC.
    let roll = rand::random_range(1..=20);
    let total = roll + target_level;
    if total < dc {
        return SaveOutcome::Failed;
    }
    let action = save
        .on_save_action
        .as_str()
        .map(str::to_ascii_uppercase)
        .unwrap_or_default();
    match action.as_str() {
        "NEGATE" => SaveOutcome::Negated,
        "HALF_DURATION" => SaveOutcome::HalfDuration,
        // Unknown / unsupported action: effects apply at full
        // strength as if the save failed. The runtime grows
        // interpretation incrementally.
        _ => SaveOutcome::Failed,
    }
}

fn check_target_type(
    world: &mut World,
    caster: Entity,
    target: Entity,
    valid_targets: &[String],
) -> Option<String> {
    if valid_targets.is_empty() {
        return None;
    }
    // The list is OR — target matches if any entry matches. Any
    // unrecognized entry counts as a free pass so abilities like
    // DRAG (CORPSE/UNCONSCIOUS) don't get blocked.
    let mut any_recognized = false;
    let target_is_player = world.get::<Player>(target).is_some();
    let target_is_mob = world.get::<Mob>(target).is_some();
    let target_is_self = caster == target;
    let target_is_item_in_inv = world.get::<Item>(target).is_some()
        && world.get::<Located>(target).is_some_and(|l| l.0 == caster);
    // RIDER target is the caster's current mount.
    let target_is_caster_mount = world
        .get::<mud_world::Mounted>(caster)
        .is_some_and(|m| m.0 == target);
    for kind in valid_targets {
        match kind.as_str() {
            "ENEMY_PC" => {
                any_recognized = true;
                if target_is_player && !target_is_self {
                    return None;
                }
            }
            "ENEMY_NPC" => {
                any_recognized = true;
                if target_is_mob {
                    return None;
                }
            }
            "OBJECT_INV" => {
                any_recognized = true;
                if target_is_item_in_inv {
                    return None;
                }
            }
            "RIDER" => {
                any_recognized = true;
                if target_is_caster_mount {
                    return None;
                }
            }
            // Unrecognized types: free pass via the early-return below.
            _ => return None,
        }
    }
    if !any_recognized {
        return None;
    }
    Some("That's not a valid target for this ability.".to_string())
}

/// Walk an ability's restriction rules and return the first failing
/// rule's `message`, or None if all rules pass / are unknown types.
/// Supported rule types (the runtime grows interpretation
/// incrementally; unknown types pass silently rather than refuse):
///
/// - `alignment` — `value`: "good"|"evil"|"neutral", `target`: "caster"|"victim",
///   `prohibited`/`required`: bool. Threshold: ±350.
/// - `target_standing` / `position` — target's `Posture` is `Standing`.
/// - `not_blind` — caster lacks any `EffectInstance` named "blind"
///   (override with `"target": "victim"` to check the target instead).
/// - `in_combat` / `not_in_combat` — caster has / lacks `Fighting`
///   (override with `"target": "victim"` to check the target).
/// - `not_tanking` — caster has no attackers (no entity Fighting them).
/// - `not_immobilized` — caster lacks the `Stunned` marker and any
///   recognized immobilizing effect (`paralysis`, `web`, `hold_person`, ...).
/// - `npc_only` — target has the `Mob` marker.
/// - `has_weapon` — caster has any item equipped in `Slot::Wield`.
fn check_ability_restrictions(
    world: &mut World,
    caster: Entity,
    target: Entity,
    rules: &[serde_json::Value],
) -> Option<String> {
    for rule in rules {
        let Some(rule_type) = rule.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let target_kind = rule.get("target").and_then(serde_json::Value::as_str);
        let resolved_target = if target_kind == Some("caster") {
            caster
        } else {
            target
        };
        let passed = match rule_type {
            "alignment" => check_rule_alignment(world, resolved_target, rule),
            "target_standing" | "position" => check_rule_standing(world, target),
            // Self-state rules: schema convention is for these to refer
            // to the caster (rule messages like "You can't see a thing!"
            // and "You're not in combat!" are written from the caster's
            // POV). An explicit `"target": "victim"` overrides via
            // `resolved_target`.
            "not_blind" => {
                let who = if target_kind == Some("victim") { target } else { caster };
                !has_effect_named(world, who, "blind")
            }
            "in_combat" => {
                let who = if target_kind == Some("victim") { target } else { caster };
                world.get::<Fighting>(who).is_some()
            }
            "not_in_combat" => {
                let who = if target_kind == Some("victim") { target } else { caster };
                world.get::<Fighting>(who).is_none()
            }
            "not_tanking" => !is_being_attacked(world, caster),
            "not_immobilized" => !is_immobilized(world, caster),
            "npc_only" => world.get::<Mob>(resolved_target).is_some(),
            "has_weapon" => caster_has_equipped(world, caster, Slot::Wield),
            // `has_shield` and other equipment-flag rules need
            // wear-flag plumbing not yet modeled — pass for now.
            // Unknown type → pass (don't refuse) so adding new rule
            // types in Muditor doesn't accidentally lock players out.
            _ => true,
        };
        if !passed {
            return rule
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(String::from)
                .or_else(|| Some(format!("Restricted: {rule_type} check failed.")));
        }
    }
    None
}

/// Evaluate the `alignment` rule. Standard MUD thresholds: alignment
/// of 350+ is "good", of -350 or less is "evil", in between is
/// "neutral". Rule semantics: `prohibited=true` refuses when target
/// matches the value; `required=true` (or unset) refuses when target
/// doesn't match. Returns true when the rule passes.
fn check_rule_alignment(world: &World, target: Entity, rule: &serde_json::Value) -> bool {
    let Some(value) = rule.get("value").and_then(serde_json::Value::as_str) else {
        return true;
    };
    let alignment = world.get::<CombatStats>(target).map_or(0, |s| s.alignment);
    let matches = match value.to_ascii_lowercase().as_str() {
        "good" => alignment >= 350,
        "evil" => alignment <= -350,
        "neutral" => alignment > -350 && alignment < 350,
        _ => return true,
    };
    let prohibited = rule
        .get("prohibited")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if prohibited {
        !matches
    } else {
        matches
    }
}

/// `target_standing` / `position` — target is upright.
fn check_rule_standing(world: &World, target: Entity) -> bool {
    world
        .get::<Posture>(target)
        .is_none_or(|p| p.0 == PostureKind::Standing)
}

/// True iff any entity is currently `Fighting(caster)` — i.e. the
/// caster has at least one attacker. Used by the `not_tanking`
/// restriction rule (e.g. BACKSTAB refuses while being attacked).
fn is_being_attacked(world: &mut World, caster: Entity) -> bool {
    let mut q = world.query::<&Fighting>();
    q.iter(world).any(|f| f.0 == caster)
}

const IMMOBILIZER_EFFECT_NAMES: &[&str] = &[
    "paralysis",
    "paralyze",
    "web",
    "frozen",
    "freeze",
    "hold_person",
    "immobilize",
];

/// True iff `caster` is immobilized — has the `Stunned` marker or
/// any active `EffectInstance` named with a recognized immobilizing
/// effect. Used by the `not_immobilized` restriction rule
/// (`KICK`, `TRIP_UP`, `DISENGAGE`).
fn is_immobilized(world: &mut World, caster: Entity) -> bool {
    if world.get::<Stunned>(caster).is_some() {
        return true;
    }
    IMMOBILIZER_EFFECT_NAMES
        .iter()
        .any(|n| has_effect_named(world, caster, n))
}

/// True iff `caster` has any item equipped in the named slot.
fn caster_has_equipped(world: &mut World, caster: Entity, slot: Slot) -> bool {
    let mut q = world.query::<(&Located, &EquippedSlot)>();
    q.iter(world)
        .any(|(loc, eq)| loc.0 == caster && eq.0 == slot)
}

/// Read the caster's wielded weapon's average damage for the
/// formula evaluator's `weapon_damage` symbol. Reads
/// `ObjectProto.avg_damage()` which derives from the `Hit Dice`
/// JSONB extracted at load time. Returns 0 if nothing is equipped
/// in `Slot::Wield`, the equipped item lacks a `WorldKey`, or the
/// proto has no weapon dice.
fn caster_weapon_damage(world: &mut World, caster: Entity) -> i32 {
    let weapon: Option<Entity> = {
        let mut q = world.query::<(Entity, &Located, &EquippedSlot)>();
        q.iter(world)
            .find(|(_, loc, eq)| loc.0 == caster && eq.0 == Slot::Wield)
            .map(|(e, _, _)| e)
    };
    let Some(weapon) = weapon else {
        return 0;
    };
    let Some(key) = world.get::<WorldKey>(weapon).copied() else {
        return 0;
    };
    world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(key.zone, key.id))
        .map_or(0, mud_world::ObjectProto::avg_damage)
}

/// Substitute `{actor.X}` / `{target.X}` placeholders in an
/// `AbilityMessages` template. Names use the entity's `Named.name`
/// verbatim; unknown pronouns default to gender-neutral
/// they/them/their (entities don't carry gender yet — Phase E).
/// `reflexive=true` collapses target-side placeholders to second-person
/// reflexive forms (`yourself` / `your`) so a self-targeted spell
/// without a `success_to_self` row still reads naturally.
fn render_ability_template(
    template: &str,
    actor_name: &str,
    target_name: &str,
    reflexive: bool,
) -> String {
    let target_sub = if reflexive { "yourself" } else { target_name };
    let target_obj = if reflexive { "yourself" } else { "them" };
    let target_poss = if reflexive { "your" } else { "their" };
    let target_subj = if reflexive { "you" } else { "they" };
    template
        .replace("{actor.name}", actor_name)
        .replace("{target.name}", target_sub)
        .replace("{actor.he}", "they")
        .replace("{actor.she}", "they")
        .replace("{actor.it}", "they")
        .replace("{actor.him}", "them")
        .replace("{actor.her}", "them")
        .replace("{actor.his}", "their")
        .replace("{actor.pos}", "their")
        .replace("{target.he}", target_subj)
        .replace("{target.she}", target_subj)
        .replace("{target.it}", target_subj)
        .replace("{target.him}", target_obj)
        .replace("{target.her}", target_obj)
        .replace("{target.his}", target_poss)
        .replace("{target.pos}", target_poss)
}

/// One row from the effect-mapping fanout: id, presentational name,
/// the effect's `effectType` (so the dispatcher can branch heal /
/// damage / status / modify / ...), plus both params blobs so amount
/// or duration can be resolved with the right precedence.
#[derive(Debug, Clone)]
struct EffectSpec {
    id: i32,
    name: String,
    effect_type: String,
    override_params: Option<serde_json::Value>,
    default_params: serde_json::Value,
}

/// Pick the `resource` field out of `override_params` first, then
/// `default_params`. Defaults to "hp" — matches the schema convention
/// for heal effects whose blob omits the field.
fn resolve_effect_resource(
    override_params: Option<&serde_json::Value>,
    default_params: Option<&serde_json::Value>,
) -> String {
    let pick = |p: Option<&serde_json::Value>| -> Option<String> {
        p?.get("resource")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase)
    };
    pick(override_params)
        .or_else(|| pick(default_params))
        .unwrap_or_else(|| "hp".to_string())
}

/// "first" stops after one removal; everything else (including the
/// schema default `"all"`) means strip every match.
#[derive(Debug, Clone, Copy)]
enum DispelScope {
    All,
    First,
}

/// Read `filter` from a dispel effect's params (override → default).
/// Lowercased for case-insensitive tag matching against
/// `EffectDef.tags`. Returns empty when neither blob has a filter
/// — caller falls through to a "no filter specified" message.
fn resolve_dispel_filter(
    override_params: Option<&serde_json::Value>,
    default_params: Option<&serde_json::Value>,
) -> String {
    let pick = |p: Option<&serde_json::Value>| -> Option<String> {
        p?.get("filter")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase)
    };
    pick(override_params)
        .or_else(|| pick(default_params))
        .unwrap_or_default()
}

/// Read `destination` from a teleport effect's params (e.g. "recall",
/// "random", "caster", "target", "home", "object"). Returns the
/// raw value lowercased, or None if neither override nor default
/// carries one.
fn resolve_teleport_destination(
    override_params: Option<&serde_json::Value>,
    default_params: Option<&serde_json::Value>,
) -> Option<String> {
    let pick = |p: Option<&serde_json::Value>| -> Option<String> {
        p?.get("destination")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase)
    };
    pick(override_params).or_else(|| pick(default_params))
}

/// Read `scope` ("first" or "all") from a dispel effect's params.
/// Defaults to All — matches the schema default and the historical
/// dispel-everything behavior.
fn resolve_dispel_scope(
    override_params: Option<&serde_json::Value>,
    default_params: Option<&serde_json::Value>,
) -> DispelScope {
    let pick = |p: Option<&serde_json::Value>| -> Option<String> {
        p?.get("scope")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase)
    };
    match pick(override_params).or_else(|| pick(default_params)).as_deref() {
        Some("first") => DispelScope::First,
        _ => DispelScope::All,
    }
}

/// Remove `EffectInstance`s on `target` whose source `EffectDef`
/// carries `tag` in its `tags` list. Returns the number despawned.
/// With `scope = First`, stops after one removal.
fn remove_effects_by_tag(
    world: &mut World,
    target: Entity,
    tag: &str,
    scope: DispelScope,
) -> usize {
    let tag_match: std::collections::HashSet<i32> = world
        .resource::<EffectCatalog>()
        .by_id
        .iter()
        .filter(|(_, def)| def.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)))
        .map(|(id, _)| *id)
        .collect();
    if tag_match.is_empty() {
        return 0;
    }
    let mut to_remove: Vec<Entity> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, eff, applied)| applied.0 == target && tag_match.contains(&eff.kind))
            .map(|(e, _, _)| e)
            .collect()
    };
    if matches!(scope, DispelScope::First) {
        to_remove.truncate(1);
    }
    let count = to_remove.len();
    for e in to_remove {
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
    count
}

/// Read `aggro` from a redirect effect's params. True selects the
/// rescue/intercept semantics (take the target's attacker as your
/// own combatant). False (or missing) leaves the effect in the
/// not-yet-implemented damage-redirect category.
fn resolve_redirect_aggro(
    override_params: Option<&serde_json::Value>,
    default_params: Option<&serde_json::Value>,
) -> bool {
    let pick = |p: Option<&serde_json::Value>| -> Option<bool> {
        p?.get("aggro").and_then(serde_json::Value::as_bool)
    };
    pick(override_params)
        .or_else(|| pick(default_params))
        .unwrap_or(false)
}

/// Read knockdown's `target` field from override (then default)
/// params. Maps `"resting"` to `Resting`; everything else (including
/// missing) defaults to `Sitting` — matches the schema's
/// knockdown-default semantics where the assumption is "you're on
/// the ground" without specifying the exact subposture.
fn resolve_knockdown_posture(
    override_params: Option<&serde_json::Value>,
    default_params: Option<&serde_json::Value>,
) -> PostureKind {
    let pick = |p: Option<&serde_json::Value>| -> Option<String> {
        p?.get("target")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase)
    };
    match pick(override_params).or_else(|| pick(default_params)).as_deref() {
        Some("resting") => PostureKind::Resting,
        _ => PostureKind::Sitting,
    }
}

/// Set `target.Posture` to `posture` only if the target is currently
/// at a *higher* rank (i.e. is upright relative to the desired
/// knockdown posture). Returns true on actual change. No-op if
/// the target lacks a Posture component (mobs without one stay
/// implicit).
fn apply_knockdown_posture(world: &mut World, target: Entity, posture: PostureKind) -> bool {
    let current = world
        .get::<Posture>(target)
        .map_or(PostureKind::Standing, |p| p.0);
    if current.rank() <= posture.rank() {
        return false;
    }
    if let Some(mut p) = world.get_mut::<Posture>(target) {
        p.0 = posture;
        return true;
    }
    false
}

/// Pull a list of condition tags from an effect's params blob — the
/// schema uses `"condition": "<tag>"` for a single tag and
/// `"condition": ["<tag>", ...]` for a multi-tag cleanse. Override
/// wins fully over default (no merging — empty override means "no
/// override"). Returns an empty vec when neither blob carries a
/// `condition`. Tags are lowercased for case-insensitive matching
/// against `EffectInstance.name`.
fn resolve_effect_conditions(
    override_params: Option<&serde_json::Value>,
    default_params: Option<&serde_json::Value>,
) -> Vec<String> {
    let pick = |p: Option<&serde_json::Value>| -> Option<Vec<String>> {
        let v = p?.get("condition")?;
        match v {
            serde_json::Value::String(s) => Some(vec![s.to_ascii_lowercase()]),
            serde_json::Value::Array(arr) => Some(
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_ascii_lowercase))
                    .collect(),
            ),
            _ => None,
        }
    };
    pick(override_params)
        .or_else(|| pick(default_params))
        .unwrap_or_default()
}

/// Add `amount` to `target.Health.hp`, capped at `max`. Returns the
/// HP actually restored (0 if `target` has no Health, already full,
/// or `amount <= 0`).
/// Mutate the named stat on `target` by `amount` (signed). Returns
/// true when the change was applied (target is supported), false
/// when the target name doesn't map to anything we model — caller
/// uses the bool to decide whether to record a `ModifyDelta` for
/// later reversal. Pairs with `reverse_modify_delta` (same mapping
/// flipped).
fn apply_modify_delta(world: &mut World, target: Entity, stat: &str, amount: i32) -> bool {
    match stat {
        "str" | "strength" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.strength = s.strength.saturating_add(amount);
            }
            true
        }
        "dex" | "dexterity" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.dexterity = s.dexterity.saturating_add(amount);
            }
            true
        }
        "con" | "constitution" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.constitution = s.constitution.saturating_add(amount);
            }
            true
        }
        "int" | "intelligence" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.intelligence = s.intelligence.saturating_add(amount);
            }
            true
        }
        "wis" | "wisdom" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.wisdom = s.wisdom.saturating_add(amount);
            }
            true
        }
        "cha" | "charisma" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.charisma = s.charisma.saturating_add(amount);
            }
            true
        }
        "hitroll" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.hit_roll = cs.hit_roll.saturating_add(amount);
            }
            true
        }
        "damroll" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.dmg_roll = cs.dmg_roll.saturating_add(amount);
            }
            true
        }
        // Lower AC = better in CircleMUD lineage; the schema's
        // `ward` is positive-buff so subtract from ac.
        "ward" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.ac = cs.ac.saturating_sub(amount);
            }
            true
        }
        "max_hp" => {
            if let Some(mut h) = world.get_mut::<Health>(target) {
                h.max = h.max.saturating_add(amount);
                if amount > 0 {
                    h.hp = h.hp.saturating_add(amount);
                }
            }
            true
        }
        "max_move" | "max_stamina" => {
            if let Some(mut s) = world.get_mut::<Stamina>(target) {
                s.max = s.max.saturating_add(amount);
                if amount > 0 {
                    s.current = s.current.saturating_add(amount);
                }
            }
            true
        }
        _ => false,
    }
}

/// Inverse of `apply_modify_delta` — subtracts the recorded delta
/// from the same stat. Used by `effects_tick` when a `ModifyDelta`
/// companion records a stat change made on spawn.
pub(crate) fn reverse_modify_delta(
    world: &mut World,
    target: Entity,
    stat: &str,
    amount: i32,
) {
    apply_modify_delta(world, target, stat, -amount);
}

fn apply_heal_hp(world: &mut World, target: Entity, amount: i32) -> i32 {
    if amount <= 0 {
        return 0;
    }
    let Some(h) = world.get::<Health>(target).copied() else {
        return 0;
    };
    let new_hp = h.hp.saturating_add(amount).min(h.max);
    let actual = (new_hp - h.hp).max(0);
    if actual > 0
        && let Some(mut hh) = world.get_mut::<Health>(target)
    {
        hh.hp = new_hp;
    }
    actual
}

/// Same as `apply_heal_hp` but for `Stamina.current`. Used by heal
/// effects whose `resource` is `"move"` (the schema's name for the
/// stamina pool).
fn apply_heal_stamina(world: &mut World, target: Entity, amount: i32) -> i32 {
    if amount <= 0 {
        return 0;
    }
    let Some(s) = world.get::<Stamina>(target).copied() else {
        return 0;
    };
    let new_v = s.current.saturating_add(amount).min(s.max);
    let actual = (new_v - s.current).max(0);
    if actual > 0
        && let Some(mut ss) = world.get_mut::<Stamina>(target)
    {
        ss.current = new_v;
    }
    actual
}

/// Pull a numeric duration out of an `AbilityEffect.override_params`
/// blob, falling back to the `Effect.default_params` blob, and finally
/// to the global default. Schema convention is `{"duration": <int>,
/// "durationUnit": "hours"}` for constants and `{"duration":
/// "<formula>", ...}` for expressions like `"level * 2"` or `"skill"`.
/// Constants and resolved formulas are converted via 1 MUD hour = 75
/// real seconds when no `durationUnit` is set.
/// True iff `target` has an `EffectInstance` whose name matches
/// `name` case-insensitively. Used by skills (gouge, berserk, ...)
/// to refuse re-applying an already-active debuff/buff. O(E) over
/// active effects; cheap at typical world scale (low hundreds).
fn has_effect_named(world: &mut World, target: Entity, name: &str) -> bool {
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    q.iter(world).any(|(eff, applied)| {
        applied.0 == target && eff.name.eq_ignore_ascii_case(name)
    })
}

/// Which prevent-flag the caller is checking on a target's active
/// effects. Each maps to one of the schema's `Effect.prevents_*`
/// columns surfaced through `EffectDef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prevent {
    Speaking,
    Casting,
    Movement,
}

/// True iff any active `EffectInstance` on `target` was sourced
/// from an `EffectDef` whose corresponding `prevents_*` flag is
/// set. Looks up each effect's catalog row by `EffectInstance.kind`
/// — admin-spawned effects without a real catalog mapping fall
/// through cleanly.
pub(crate) fn effect_prevents(world: &mut World, target: Entity, kind: Prevent) -> bool {
    let active_kinds: Vec<i32> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, a)| a.0 == target)
            .map(|(eff, _)| eff.kind)
            .collect()
    };
    if active_kinds.is_empty() {
        return false;
    }
    let catalog = world.resource::<EffectCatalog>();
    active_kinds.iter().any(|id| {
        catalog.by_id.get(id).is_some_and(|def| match kind {
            Prevent::Speaking => def.prevents_speaking,
            Prevent::Casting => def.prevents_casting,
            Prevent::Movement => def.prevents_movement,
        })
    })
}

/// Despawn every `EffectInstance` on `target` whose name matches
/// `name` (case-insensitive). Returns the number despawned. Used by
/// curative skills (bandage stops bleed) and by the `cleanse`
/// effect-type consumer in `invoke_ability`.
fn remove_effect_named(world: &mut World, target: Entity, name: &str) -> usize {
    let to_remove: Vec<Entity> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, eff, applied)| {
                applied.0 == target && eff.name.eq_ignore_ascii_case(name)
            })
            .map(|(e, _, _)| e)
            .collect()
    };
    let count = to_remove.len();
    for e in to_remove {
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
    count
}

/// Despawn every `EffectInstance` on `target`, regardless of name.
/// Used by `cleanse` effects whose `condition` is `"all"`.
fn remove_all_effects_on(world: &mut World, target: Entity) -> usize {
    let to_remove: Vec<Entity> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, _, applied)| applied.0 == target)
            .map(|(e, _, _)| e)
            .collect()
    };
    let count = to_remove.len();
    for e in to_remove {
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
    count
}

fn resolve_effect_duration(
    override_params: Option<&serde_json::Value>,
    default_params: Option<&serde_json::Value>,
    ctx: &FormulaCtx,
) -> i32 {
    if let Some(secs) = duration_from_blob(override_params, ctx) {
        return secs;
    }
    if let Some(secs) = duration_from_blob(default_params, ctx) {
        return secs;
    }
    APPLIED_EFFECT_DURATION_SECS
}

/// Pull a numeric `amount` out of an `AbilityEffect.override_params`
/// blob first, falling back to the `Effect.default_params`. Used by
/// the heal effect-type consumer in `invoke_ability` (and, eventually,
/// the damage consumer). Returns None when neither blob carries an
/// amount the formula evaluator can interpret — caller decides the
/// fallback (e.g. drop the effect, log a default).
fn resolve_effect_amount(
    override_params: Option<&serde_json::Value>,
    default_params: Option<&serde_json::Value>,
    ctx: &FormulaCtx,
) -> Option<i32> {
    if let Some(v) = amount_from_blob(override_params, ctx) {
        return Some(v);
    }
    amount_from_blob(default_params, ctx)
}

/// Try to extract an amount from one JSONB blob. The `amount` field
/// can be an integer literal, a formula string the evaluator
/// understands (e.g. `"roll_dice(2,9) + skill / 5"`), or a plain dice
/// notation like `"1d8"` which is normalized to `roll_dice(N, M)`.
fn amount_from_blob(params: Option<&serde_json::Value>, ctx: &FormulaCtx) -> Option<i32> {
    let p = params?;
    let v = p.get("amount")?;
    numeric_or_formula(v, ctx)
}

/// Pull a `bonusIfHidden` field — schema convention for "extra damage
/// when the caster has the Stealth marker". Same numeric/formula
/// shape as `amount`. Returns None when the field is absent.
fn bonus_if_hidden_from_blob(
    params: Option<&serde_json::Value>,
    ctx: &FormulaCtx,
) -> Option<i32> {
    let p = params?;
    let v = p.get("bonusIfHidden")?;
    numeric_or_formula(v, ctx)
}

/// Shared parser for amount-shaped JSON fields: integer literal,
/// formula string, or the dice-notation shorthand normalized to
/// `roll_dice(N, M)` before eval.
fn numeric_or_formula(v: &serde_json::Value, ctx: &FormulaCtx) -> Option<i32> {
    match v {
        serde_json::Value::Number(n) => i32::try_from(n.as_i64()?).ok(),
        serde_json::Value::String(s) => {
            let normalized = normalize_dice_notation(s);
            evaluate_simple_formula_ctx(&normalized, ctx)
        }
        _ => None,
    }
}

/// Rewrite simple dice notation `NdM` (e.g. `1d8`, `2d6`) as
/// `roll_dice(N, M)` so the formula evaluator can handle the shorthand
/// the schema's heal/damage blobs use. Conservative: only matches
/// whole-token `<digits>d<digits>` segments; leaves anything else
/// alone.
fn normalize_dice_notation(expr: &str) -> String {
    // Single-pass scanner: walk chars, copy through, splice on `NdM`.
    let bytes = expr.as_bytes();
    let mut out = String::with_capacity(expr.len());
    let mut idx: usize = 0;
    while idx < bytes.len() {
        let ch = bytes[idx] as char;
        if ch.is_ascii_digit() {
            let num_start = idx;
            while idx < bytes.len() && (bytes[idx] as char).is_ascii_digit() {
                idx += 1;
            }
            // Look for `d<digits>` directly after the number.
            if idx < bytes.len() && (bytes[idx] == b'd' || bytes[idx] == b'D') {
                let after_d = idx + 1;
                let mut sides_end = after_d;
                while sides_end < bytes.len()
                    && (bytes[sides_end] as char).is_ascii_digit()
                {
                    sides_end += 1;
                }
                if sides_end > after_d {
                    let num_str = &expr[num_start..idx];
                    let sides_str = &expr[after_d..sides_end];
                    out.push_str("roll_dice(");
                    out.push_str(num_str);
                    out.push_str(", ");
                    out.push_str(sides_str);
                    out.push(')');
                    idx = sides_end;
                    continue;
                }
            }
            out.push_str(&expr[num_start..idx]);
        } else {
            out.push(ch);
            idx += 1;
        }
    }
    out
}

/// Try to extract a duration in seconds from one JSONB blob. The
/// `duration` field can be an integer literal (e.g. `2`) or a simple
/// formula string (e.g. `"level"`, `"level * 2"`, `"skill / 4"`).
/// Returns None if the blob is missing, has no `duration`, or the
/// formula is too complex for the simple evaluator (parens, multi-op,
/// `pow()`, etc.) — caller falls through to the next fallback.
fn duration_from_blob(params: Option<&serde_json::Value>, ctx: &FormulaCtx) -> Option<i32> {
    const SECS_PER_MUD_HOUR: i32 = 75;
    let p = params?;
    let d = p.get("duration")?;
    let raw = match d {
        serde_json::Value::Number(n) => i32::try_from(n.as_i64()?).ok()?,
        serde_json::Value::String(s) => evaluate_simple_formula_ctx(s, ctx)?,
        _ => return None,
    };
    let unit_seconds = match p.get("durationUnit").and_then(serde_json::Value::as_str) {
        Some("hours") | None => SECS_PER_MUD_HOUR,
        Some("minutes") => 60,
        Some("rounds") => 4,
        // "seconds" or any unknown unit: treat the integer as seconds.
        Some(_) => 1,
    };
    Some(raw.saturating_mul(unit_seconds).max(1))
}

/// Evaluate a formula expression for ability amounts and durations.
/// Grammar:
///   expr    := term (('+' | '-') term)*
///   term    := factor (('*' | '/') factor)*
///   factor  := number | symbol | call | '(' expr ')' | '-' factor
///   symbol  := 'level' | 'skill'
///   call    := identifier '(' expr (',' expr)* ')'
/// Supported calls: `roll_dice(N, M)` — sum of N dice with M sides each.
/// Returns None on unknown symbols/calls, malformed input, or division
/// by zero so callers can fall through to the next fallback. Calls the
/// live RNG via `rand::random_range`; deterministic cases (no dice)
/// are reproducible.
/// Caster context passed to the formula evaluator. Holds the named
/// symbols the grammar can reference (`level`, `skill`,
/// `weapon_damage`, ...). Stack-allocated; expand with new fields as
/// the runtime grows the symbols it can resolve. Defaults are
/// 0-everywhere via `FormulaCtx::base(level, skill)` for legacy
/// callsites and tests that don't have weapon/stat context.
#[derive(Debug, Clone, Copy, Default)]
struct FormulaCtx {
    level: i32,
    skill: i32,
    weapon_damage: i32,
    str_bonus: i32,
    dex_bonus: i32,
    con_bonus: i32,
    int_bonus: i32,
    wis_bonus: i32,
    cha_bonus: i32,
    /// 1 when the caster has the `Stealth` marker, 0 otherwise.
    /// Used by rogue abilities (BACKSTAB's `bonusIfHidden`).
    hidden: i32,
}

impl FormulaCtx {
    /// Test/legacy helper: build a context with only `level` and
    /// `skill` set. Production callsites construct the struct
    /// directly so they can supply caster-derived symbols.
    #[cfg(test)]
    fn base(level: i32, skill: i32) -> Self {
        Self {
            level,
            skill,
            ..Self::default()
        }
    }

    fn lookup(self, name: &str) -> Option<i32> {
        match name {
            "level" => Some(self.level),
            "skill" => Some(self.skill),
            "weapon_damage" => Some(self.weapon_damage),
            "str_bonus" | "str" => Some(self.str_bonus),
            "dex_bonus" | "dex" => Some(self.dex_bonus),
            "con_bonus" | "con" => Some(self.con_bonus),
            "int_bonus" | "int" => Some(self.int_bonus),
            "wis_bonus" | "wis" => Some(self.wis_bonus),
            "cha_bonus" | "cha" => Some(self.cha_bonus),
            "hidden" => Some(self.hidden),
            _ => None,
        }
    }
}

/// Test/legacy entry point — production callsites take the full
/// `FormulaCtx` via `evaluate_simple_formula_ctx`.
#[cfg(test)]
fn evaluate_simple_formula(expr: &str, level: i32, skill: i32) -> Option<i32> {
    evaluate_simple_formula_ctx(expr, &FormulaCtx::base(level, skill))
}

/// Live-RNG entry point that takes the full `FormulaCtx` — used by
/// `invoke_ability` when caster-derived symbols (`weapon_damage` etc.)
/// matter.
fn evaluate_simple_formula_ctx(expr: &str, ctx: &FormulaCtx) -> Option<i32> {
    evaluate_formula(expr, ctx, &mut |name, a, b| match name {
        "roll_dice" => roll_dice(a, b),
        "random" if a <= b => rand::random_range(a..=b),
        _ => 0,
    })
}

/// Roll `num` dice with `sides` sides each and sum them. Both args
/// must be positive; non-positive inputs return 0.
fn roll_dice(num: i32, sides: i32) -> i32 {
    if num <= 0 || sides <= 0 {
        return 0;
    }
    let mut total: i32 = 0;
    for _ in 0..num {
        total = total.saturating_add(rand::random_range(1..=sides));
    }
    total
}

/// Same grammar as `evaluate_simple_formula`, but the dice-roll
/// callback is injectable so tests can pass a deterministic stub.
fn evaluate_formula(
    expr: &str,
    ctx: &FormulaCtx,
    rng_call: &mut dyn FnMut(&str, i32, i32) -> i32,
) -> Option<i32> {
    let tokens = tokenize_formula(expr)?;
    let mut p = FormulaParser { tokens: &tokens, idx: 0 };
    let v = p.parse_expr(ctx, rng_call)?;
    if p.idx != tokens.len() {
        return None;
    }
    Some(v)
}

#[derive(Debug, Clone, PartialEq)]
enum FormulaToken {
    Num(i32),
    /// Floating-point literal — only meaningful inside `pow(...)` as
    /// the exponent. The rest of the grammar stays integer; a Float
    /// outside pow returns None (caller falls through).
    Float(f64),
    Ident(String),
    LParen,
    RParen,
    Comma,
    Plus,
    Minus,
    Star,
    Slash,
}

fn tokenize_formula(expr: &str) -> Option<Vec<FormulaToken>> {
    let mut tokens = Vec::new();
    let mut chars = expr.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '(' => {
                chars.next();
                tokens.push(FormulaToken::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(FormulaToken::RParen);
            }
            ',' => {
                chars.next();
                tokens.push(FormulaToken::Comma);
            }
            '+' => {
                chars.next();
                tokens.push(FormulaToken::Plus);
            }
            '-' => {
                chars.next();
                tokens.push(FormulaToken::Minus);
            }
            '*' => {
                chars.next();
                tokens.push(FormulaToken::Star);
            }
            '/' => {
                chars.next();
                tokens.push(FormulaToken::Slash);
            }
            c if c.is_ascii_digit() => {
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() {
                        s.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                // Float literal: `123.45`. Only consume the `.` if a
                // digit follows — bare `.` could be from another grammar.
                let mut peek_clone = chars.clone();
                if peek_clone.next() == Some('.')
                    && peek_clone.peek().is_some_and(char::is_ascii_digit)
                {
                    s.push('.');
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        if c.is_ascii_digit() {
                            s.push(c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    tokens.push(FormulaToken::Float(s.parse().ok()?));
                } else {
                    tokens.push(FormulaToken::Num(s.parse().ok()?));
                }
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        s.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(FormulaToken::Ident(s));
            }
            _ => return None,
        }
    }
    Some(tokens)
}

struct FormulaParser<'a> {
    tokens: &'a [FormulaToken],
    idx: usize,
}

impl FormulaParser<'_> {
    fn peek(&self) -> Option<&FormulaToken> {
        self.tokens.get(self.idx)
    }
    fn advance(&mut self) -> Option<&FormulaToken> {
        let t = self.tokens.get(self.idx)?;
        self.idx += 1;
        Some(t)
    }
    fn parse_expr(
        &mut self,
        ctx: &FormulaCtx,
        rng_call: &mut dyn FnMut(&str, i32, i32) -> i32,
    ) -> Option<i32> {
        let mut lhs = self.parse_term(ctx, rng_call)?;
        loop {
            match self.peek() {
                Some(FormulaToken::Plus) => {
                    self.advance();
                    let rhs = self.parse_term(ctx, rng_call)?;
                    lhs = lhs.saturating_add(rhs);
                }
                Some(FormulaToken::Minus) => {
                    self.advance();
                    let rhs = self.parse_term(ctx, rng_call)?;
                    lhs = lhs.saturating_sub(rhs);
                }
                _ => break,
            }
        }
        Some(lhs)
    }
    fn parse_term(
        &mut self,
        ctx: &FormulaCtx,
        rng_call: &mut dyn FnMut(&str, i32, i32) -> i32,
    ) -> Option<i32> {
        let mut lhs = self.parse_factor(ctx, rng_call)?;
        loop {
            match self.peek() {
                Some(FormulaToken::Star) => {
                    self.advance();
                    let rhs = self.parse_factor(ctx, rng_call)?;
                    lhs = lhs.saturating_mul(rhs);
                }
                Some(FormulaToken::Slash) => {
                    self.advance();
                    let rhs = self.parse_factor(ctx, rng_call)?;
                    if rhs == 0 {
                        return None;
                    }
                    lhs /= rhs;
                }
                _ => break,
            }
        }
        Some(lhs)
    }
    fn parse_factor(
        &mut self,
        ctx: &FormulaCtx,
        rng_call: &mut dyn FnMut(&str, i32, i32) -> i32,
    ) -> Option<i32> {
        match self.advance()? {
            FormulaToken::Num(n) => Some(*n),
            FormulaToken::Minus => {
                let v = self.parse_factor(ctx, rng_call)?;
                Some(v.saturating_neg())
            }
            FormulaToken::LParen => {
                let v = self.parse_expr(ctx, rng_call)?;
                if !matches!(self.advance(), Some(FormulaToken::RParen)) {
                    return None;
                }
                Some(v)
            }
            FormulaToken::Ident(name) => {
                let n = name.clone();
                if matches!(self.peek(), Some(FormulaToken::LParen)) {
                    self.advance();
                    // Special-case `pow(base, exp)` so the exponent
                    // can be a Float literal — the rest of the grammar
                    // is integer-only.
                    if n == "pow" {
                        let base = self.parse_expr(ctx, rng_call)?;
                        if !matches!(self.advance(), Some(FormulaToken::Comma)) {
                            return None;
                        }
                        let exp = match self.advance()? {
                            FormulaToken::Float(f) => *f,
                            FormulaToken::Num(i) => f64::from(*i),
                            _ => return None,
                        };
                        if !matches!(self.advance(), Some(FormulaToken::RParen)) {
                            return None;
                        }
                        let result = f64::from(base).powf(exp);
                        // Round and clamp to i32 range. NaN / inf
                        // become None so the caller falls through.
                        if !result.is_finite() {
                            return None;
                        }
                        let rounded = result.round();
                        if rounded > f64::from(i32::MAX) || rounded < f64::from(i32::MIN) {
                            return None;
                        }
                        // Safe: bounded above.
                        #[allow(clippy::cast_possible_truncation)]
                        return Some(rounded as i32);
                    }
                    let mut args: Vec<i32> = Vec::new();
                    if !matches!(self.peek(), Some(FormulaToken::RParen)) {
                        args.push(self.parse_expr(ctx, rng_call)?);
                        while matches!(self.peek(), Some(FormulaToken::Comma)) {
                            self.advance();
                            args.push(self.parse_expr(ctx, rng_call)?);
                        }
                    }
                    if !matches!(self.advance(), Some(FormulaToken::RParen)) {
                        return None;
                    }
                    match (n.as_str(), args.as_slice()) {
                        ("roll_dice", [num, sides]) if *num > 0 && *sides > 0 => {
                            Some(rng_call("roll_dice", *num, *sides))
                        }
                        ("random", [lo, hi]) if lo <= hi => {
                            Some(rng_call("random", *lo, *hi))
                        }
                        _ => None,
                    }
                } else {
                    ctx.lookup(&n)
                }
            }
            _ => None,
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
    }
}

fn cmd_socials(world: &mut World, player: Entity, _args: &str) {
    let mut names: Vec<String> = world
        .resource::<SocialRegistry>()
        .by_name
        .keys()
        .cloned()
        .collect();
    names.sort_unstable();
    let mut out = format!("\r\n{} socials available:\r\n", names.len());
    let cols = 6usize;
    let col_width = 14usize;
    for (i, name) in names.iter().enumerate() {
        if i % cols == 0 {
            out.push_str("  ");
        }
        out.push_str(&format!("{name:<col_width$}"));
        if i % cols == cols - 1 {
            out.push_str("\r\n");
        }
    }
    if !names.len().is_multiple_of(cols) {
        out.push_str("\r\n");
    }
    send_to(world, player, out);
}

/// Try to dispatch `verb` as a social. Returns true if a matching social was
/// found (regardless of outcome — includes cases where target wasn't found).
fn try_dispatch_social(world: &mut World, player: Entity, verb: &str, args: &str) -> bool {
    let social = world
        .resource::<SocialRegistry>()
        .get(verb)
        .cloned();
    let Some(social) = social else {
        return false;
    };
    run_social(world, player, &social, args);
    true
}

fn run_social(world: &mut World, player: Entity, social: &SocialDef, args: &str) {
    let target_word = args.trim();
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;

    let actor_name = name_of(world, player);

    if target_word.is_empty() {
        // No-arg path.
        if let Some(line) = social.char_no_arg.as_ref() {
            let s = substitute(line, &actor_name, None);
            send_rendered(world, player, &format!("{s}\r\n"));
        }
        if let Some(line) = social.others_no_arg.as_ref() {
            let s = substitute(line, &actor_name, None);
            broadcast_room_except_rendered(world, room, &[player], &format!("{s}\r\n"));
        }
        return;
    }

    // Self-target?
    let self_target = matches_self(&actor_name, target_word);
    if self_target {
        if let Some(line) = social.char_auto.as_ref() {
            let s = substitute(line, &actor_name, Some(&actor_name));
            send_rendered(world, player, &format!("{s}\r\n"));
        }
        if let Some(line) = social.others_auto.as_ref() {
            let s = substitute(line, &actor_name, Some(&actor_name));
            broadcast_room_except_rendered(world, room, &[player], &format!("{s}\r\n"));
        }
        return;
    }

    // Try to find the target in the room.
    let target = find_actor_in_room(world, target_word, room, player);
    let Some(target) = target else {
        if let Some(line) = social.not_found.as_ref() {
            send_to(world, player, format!("{line}\r\n"));
        } else {
            send_to(world, player, format!("'{target_word}' isn't here.\r\n"));
        }
        return;
    };

    let target_name = name_of(world, target);

    if let Some(line) = social.char_found.as_ref() {
        let s = substitute(line, &actor_name, Some(&target_name));
        send_rendered(world, player, &format!("{s}\r\n"));
    }
    if let Some(line) = social.vict_found.as_ref() {
        let s = substitute(line, &actor_name, Some(&target_name));
        send_rendered(world, target, &format!("{s}\r\n"));
    }
    if let Some(line) = social.others_found.as_ref() {
        let s = substitute(line, &actor_name, Some(&target_name));
        broadcast_room_except_rendered(world, room, &[player, target], &format!("{s}\r\n"));
    }
}

fn matches_self(actor_name: &str, target_word: &str) -> bool {
    if target_word.eq_ignore_ascii_case("me") || target_word.eq_ignore_ascii_case("self") {
        return true;
    }
    actor_name
        .to_ascii_lowercase()
        .contains(&target_word.to_ascii_lowercase())
}

/// Replace social template placeholders. Genderless pronouns until we wire
/// per-character gender; "their" / "them" / "they" are the safe defaults.
fn substitute(template: &str, actor_name: &str, target_name: Option<&str>) -> String {
    let target = target_name.unwrap_or("someone");
    template
        .replace("{actor.name}", actor_name)
        .replace("{target.name}", target)
        .replace("{actor.pronoun.objective}", "them")
        .replace("{actor.pronoun.subjective}", "they")
        .replace("{actor.pronoun.possessive}", "their")
        .replace("{target.pronoun.objective}", "them")
        .replace("{target.pronoun.subjective}", "they")
        .replace("{target.pronoun.possessive}", "their")
}

/// Single-recipient companion to `broadcast_room_except_rendered`.
/// Renders the message's color tags with the recipient's `ColorMode`,
/// then sends. Use when a directed `send_to(world, t, format!(...))`
/// embeds a name that may carry XML-Lite tags.
pub(crate) fn send_rendered(world: &World, target: Entity, text: &str) {
    let mode = color_mode_for(world, target);
    send_to(world, target, render_color_tags(text, mode));
}

/// Read an entity's `Named.name` as an owned String. Empty when the
/// component is missing — matches the historical fallback at every
/// call site that wants a name for `format!`-ing.
pub(crate) fn name_of(world: &World, e: Entity) -> String {
    world
        .get::<Named>(e)
        .map_or_else(String::new, |n| n.name.clone())
}

/// Same shape as `name_of` but with a caller-chosen fallback string —
/// used by sites that prefer literal placeholders like `<unknown>`,
/// `<gone>`, or `<nowhere>` when the entity lacks a Named.
pub(crate) fn name_or(world: &World, e: Entity, fallback: &str) -> String {
    world
        .get::<Named>(e)
        .map_or_else(|| fallback.to_string(), |n| n.name.clone())
}

/// Insert (or replace) a component on an entity, silently no-op'ing if
/// the entity has been despawned. Mid-tick mutations frequently target
/// an entity that may have been removed earlier in the same tick — this
/// is the safe-by-default version of `world.entity_mut(e).insert(c)`.
pub(crate) fn try_insert<C: bevy_ecs::component::Component>(
    world: &mut World,
    e: Entity,
    c: C,
) {
    if let Ok(mut em) = world.get_entity_mut(e) {
        em.insert(c);
    }
}

/// Remove a component from an entity, silently no-op'ing if the entity
/// is gone. Companion to `try_insert`.
pub(crate) fn try_remove<C: bevy_ecs::component::Component>(world: &mut World, e: Entity) {
    if let Ok(mut em) = world.get_entity_mut(e) {
        em.remove::<C>();
    }
}

/// Send `raw_msg` to every entity in `room`, skipping any in `except`,
/// rendering color tags per-recipient — each player gets ANSI or
/// stripped output based on their own `COLOR_BLIND` flag. The default
/// "the room sees X happen" broadcast: every message in this codebase
/// embeds entity names that may carry XML-Lite tags, so we render once
/// per recipient rather than locking everyone into a single mode.
pub(crate) fn broadcast_room_except_rendered(
    world: &mut World,
    room: Entity,
    except: &[Entity],
    raw_msg: &str,
) {
    let targets: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located)>();
        q.iter(world)
            .filter(|(e, l)| l.0 == room && !except.contains(e))
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        let mode = color_mode_for(world, t);
        send_to(world, t, render_color_tags(raw_msg, mode));
    }
}

/// `broadcast_room_except_rendered`, with a `Player` filter on the
/// query. Used for messages that semantically don't apply to mobs
/// (whisper bystanders, posture announcements, social emotes, etc.) —
/// keeps the `PROMPT_RECIPIENTS` set narrow even though `send_to` is
/// already a no-op for actors without a `Connection`.
pub(crate) fn broadcast_room_except_players_rendered(
    world: &mut World,
    room: Entity,
    except: &[Entity],
    raw_msg: &str,
) {
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| l.0 == room && !except.contains(e))
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        let mode = color_mode_for(world, t);
        send_to(world, t, render_color_tags(raw_msg, mode));
    }
}

fn cmd_tell(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[1].trim().is_empty() {
        send_to(world, player, "Usage: tell <player> <message>\r\n");
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let target_name = parts[0].trim();
    let message = parts[1].trim();
    let target_lower = target_name.to_ascii_lowercase();

    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(target_name)
                || n.name.to_ascii_lowercase() == target_lower)
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_rendered(world, player, &format!("'{target_name}' isn't online.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "You mutter quietly to yourself.\r\n");
        return;
    }
    if has_flag(world, target, PlayerFlag::NoTell) {
        let actual = name_of(world, target);
        send_rendered(world, player, &format!("{actual} is not accepting tells right now.\r\n"),
        );
        return;
    }
    // Per-target ignore: if the receiver has us on their IgnoreList,
    // refuse with a generic message (don't leak which players ignore
    // them — the exact wording matches `NoTell` to keep social
    // friction low).
    let player_name_for_check = name_of(world, player);
    if world
        .get::<IgnoreList>(target)
        .is_some_and(|l| l.contains(&player_name_for_check))
    {
        let actual = name_of(world, target);
        send_rendered(
            world,
            player,
            &format!("{actual} is not accepting tells right now.\r\n"),
        );
        return;
    }

    let player_name = name_of(world, player);
    let target_name = name_of(world, target);

    send_rendered(world, player, &format!("You tell {target_name}, \"{message}\"\r\n"));
    if has_flag(world, target, PlayerFlag::Afk) {
        send_rendered(world, player, &format!("({target_name} is AFK and may not respond right away.)\r\n"),
        );
    }
    send_rendered(
        world,
        target,
        &format!("{player_name} tells you, \"{message}\"\r\n"),
    );

    // Stamp the receiver so they can `reply`.
    try_insert(world, target, LastTeller(player));
    // Append to the bounded history shown by `lasttells`. Created on
    // first inbound tell; subsequent pushes mutate in place.
    let player_name_owned = player_name.clone();
    if let Some(mut log) = world.get_mut::<TellLog>(target) {
        log.push(player_name_owned);
    } else {
        let mut log = TellLog::with_cap(10);
        log.push(player_name_owned);
        try_insert(world, target, log);
    }
}

fn cmd_reply(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Reply with what?\r\n");
        return;
    }
    let Some(LastTeller(last)) = world.get::<LastTeller>(player).copied() else {
        send_to(world, player, "Nobody has tell'd you recently.\r\n");
        return;
    };
    if world.get_entity(last).is_err() || world.get::<Online>(last).is_none() {
        send_to(world, player, "They're no longer online.\r\n");
        return;
    }
    let last_name = name_of(world, last);
    // Forward through cmd_tell so we get the LastTeller stamping for free.
    cmd_tell(world, player, &format!("{last_name} {message}"));
}

/// `ignore [<name> | -<name> | clear]`: manage a per-session list of
/// blocked tell senders. A no-arg call lists current entries.
fn cmd_ignore(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        let entries = world.get::<IgnoreList>(player).map(|l| l.0.clone()).unwrap_or_default();
        if entries.is_empty() {
            send_to(world, player, "You're ignoring nobody.\r\n");
        } else {
            let mut out = format!("\r\nIgnoring {} player(s):\r\n", entries.len());
            for n in &entries {
                out.push_str(&format!("  {n}\r\n"));
            }
            send_to(world, player, out);
        }
        return;
    }
    if arg.eq_ignore_ascii_case("clear") {
        if let Ok(mut e) = world.get_entity_mut(player) {
            e.remove::<IgnoreList>();
        }
        send_to(world, player, "Ignore list cleared.\r\n");
        return;
    }
    if let Some(name) = arg.strip_prefix('-') {
        let name = name.trim();
        if name.is_empty() {
            send_to(world, player, "Unignore whom?\r\n");
            return;
        }
        cmd_unignore(world, player, name);
        return;
    }
    // Add a name. `IgnoreList` is created on first use.
    let added = if let Some(mut list) = world.get_mut::<IgnoreList>(player) {
        list.add(arg)
    } else {
        let mut l = IgnoreList::default();
        let added = l.add(arg);
        try_insert(world, player, l);
        added
    };
    if added {
        send_to(world, player, format!("You will now ignore {arg}.\r\n"));
    } else {
        send_to(world, player, format!("You're already ignoring {arg}.\r\n"));
    }
}

fn cmd_unignore(world: &mut World, player: Entity, args: &str) {
    let name = args.trim();
    if name.is_empty() {
        send_to(world, player, "Unignore whom?\r\n");
        return;
    }
    let removed = world
        .get_mut::<IgnoreList>(player)
        .is_some_and(|mut l| l.remove(name));
    if removed {
        send_to(world, player, format!("You no longer ignore {name}.\r\n"));
    } else {
        send_to(world, player, format!("You aren't ignoring {name}.\r\n"));
    }
}

fn cmd_lasttells(world: &mut World, player: Entity, _args: &str) {
    let entries: Vec<(String, u64)> = world
        .get::<TellLog>(player)
        .map(|log| {
            log.entries
                .iter()
                .map(|(name, when)| (name.clone(), when.elapsed().as_secs()))
                .collect()
        })
        .unwrap_or_default();
    if entries.is_empty() {
        send_to(world, player, "No recent tells.\r\n");
        return;
    }
    let mut out = format!("\r\nRecent tells ({}):\r\n", entries.len());
    for (name, secs_ago) in &entries {
        out.push_str(&format!("  {:<20} ({} ago)\r\n", name, format_idle(*secs_ago)));
    }
    send_to(world, player, out);
}

/// Stub for the help/registry path — mail commands are intercepted by
/// the async pre-dispatch hook before this ever runs. If somehow it
/// does (a future refactor moves dispatch order around), bail loudly.
fn cmd_mail_stub(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        "Mail subsystem error: sync dispatch reached an async-only \
         command. Please report.\r\n",
    );
}

/// `mailbox`: list inbound non-deleted mail for the player's account,
/// newest first. Each line shows `# unread? sender — subject`.
/// `readmail <#>` reads the body and marks the row read.
pub(crate) async fn cmd_mailbox(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    let user_id = world.get::<Account>(player).map(|a| a.user_id.clone());
    let Some(user_id) = user_id else {
        send_to(world, player, "No account info; can't fetch mail.\r\n");
        return;
    };
    let rows = match mud_db::mail::inbox_for(pool, &user_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Mail fetch failed: {e}\r\n"));
            return;
        }
    };
    if rows.is_empty() {
        send_to(world, player, "\r\nYour mailbox is empty.\r\n");
        return;
    }
    let mut out = format!("\r\nMailbox ({} message(s)):\r\n", rows.len());
    for (i, row) in rows.iter().enumerate() {
        let unread = if row.read_at.is_some() { " " } else { "*" };
        let when = row.sent_at.format("%Y-%m-%d %H:%M");
        out.push_str(&format!(
            "  {:<3} {unread} {when}  {:<24} {}\r\n",
            i + 1,
            row.sender_display_name,
            row.subject,
        ));
    }
    out.push_str("\r\n* = unread.   Use `readmail <#>` to read, `delmail <#>` to delete.\r\n");
    send_to(world, player, out);
}

/// `readmail <#>`: print the body of the slot-numbered mail (1-based,
/// matching the `mailbox` listing). Marks the row read on success.
pub(crate) async fn cmd_readmail(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let arg = args.trim();
    let Ok(slot) = arg.parse::<usize>() else {
        send_to(
            world,
            player,
            "Read which mail? Pick a number from `mailbox`.\r\n",
        );
        return;
    };
    if slot == 0 {
        send_to(world, player, "Mail slots are 1-based.\r\n");
        return;
    }
    let user_id = world.get::<Account>(player).map(|a| a.user_id.clone());
    let Some(user_id) = user_id else {
        send_to(world, player, "No account info; can't fetch mail.\r\n");
        return;
    };
    let rows = match mud_db::mail::inbox_for(pool, &user_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Mail fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(row) = rows.get(slot - 1) else {
        send_to(world, player, format!("No mail at slot {slot}.\r\n"));
        return;
    };
    let mut out = String::from("\r\n");
    out.push_str(&format!("From:    {}\r\n", row.sender_display_name));
    out.push_str(&format!("Subject: {}\r\n", row.subject));
    out.push_str(&format!("Sent:    {}\r\n", row.sent_at.format("%Y-%m-%d %H:%M")));
    out.push_str("---\r\n");
    out.push_str(row.body.trim_end());
    out.push_str("\r\n---\r\n");
    send_to(world, player, out);
    if let Err(e) = mud_db::mail::mark_read(pool, row.id).await {
        tracing::warn!(error = %e, mail_id = row.id, "mark_read failed");
    }
}

/// `quests` / `qstat` / `qlist`: list quests the current character has
/// accepted, in-progress first then recently completed. Active rows
/// show the quest name + short description; completed rows show
/// completion count. Empty inbox = "no quests accepted."
pub(crate) async fn cmd_quests(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
) {
    let character_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let Some(character_id) = character_id else {
        send_to(world, player, "No account info; can't fetch quests.\r\n");
        return;
    };
    let rows = match mud_db::quests::list_for_character(pool, &character_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Quest fetch failed: {e}\r\n"));
            return;
        }
    };
    if rows.is_empty() {
        send_to(world, player, "\r\nYou have no active or completed quests.\r\n");
        return;
    }
    let active: Vec<&mud_db::quests::CharacterQuestRow> = rows
        .iter()
        .filter(|r| r.status == "IN_PROGRESS")
        .collect();
    let other: Vec<&mud_db::quests::CharacterQuestRow> = rows
        .iter()
        .filter(|r| r.status != "IN_PROGRESS")
        .collect();
    let mut out = String::from("\r\n");
    if !active.is_empty() {
        out.push_str(&format!("In progress ({}):\r\n", active.len()));
        for q in &active {
            out.push_str(&format!(
                "  ({}, {})  {}\r\n",
                q.quest_zone_id, q.quest_id, q.quest_name
            ));
            if let Some(desc) = &q.short_description
                && !desc.trim().is_empty()
            {
                out.push_str(&format!("        {}\r\n", desc.trim()));
            }
        }
        out.push_str("\r\n");
    }
    if !other.is_empty() {
        out.push_str(&format!("Other ({}):\r\n", other.len()));
        for q in &other {
            out.push_str(&format!(
                "  [{}] ({}, {})  {}",
                q.status, q.quest_zone_id, q.quest_id, q.quest_name,
            ));
            if q.completion_count > 1 {
                out.push_str(&format!(" ×{}", q.completion_count));
            }
            out.push_str("\r\n");
        }
    }
    send_to(world, player, out);
}

/// `qload <zone> <id>`: admin command — assign a quest to the caller's
/// character with status `IN_PROGRESS`. Skips if the row already
/// exists. Useful for testing the quest listing/abandon loop without
/// the full trigger-acceptance flow.
pub(crate) async fn cmd_qload(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let zone_raw = parts.next();
    let id_raw = parts.next();
    let (Some(zone_raw), Some(id_raw)) = (zone_raw, id_raw) else {
        send_to(world, player, "Usage: qload <zone> <quest-id>\r\n");
        return;
    };
    let Ok(zone) = zone_raw.parse::<i32>() else {
        send_to(world, player, "Zone must be an integer.\r\n");
        return;
    };
    let Ok(id) = id_raw.parse::<i32>() else {
        send_to(world, player, "Quest id must be an integer.\r\n");
        return;
    };
    let exists = match mud_db::quests::quest_exists(pool, zone, id).await {
        Ok(b) => b,
        Err(e) => {
            send_to(world, player, format!("Quest lookup failed: {e}\r\n"));
            return;
        }
    };
    if !exists {
        send_to(
            world,
            player,
            format!("No Quest defined at ({zone}, {id}).\r\n"),
        );
        return;
    }
    let character_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let Some(character_id) = character_id else {
        send_to(world, player, "No account info; can't assign.\r\n");
        return;
    };
    match mud_db::quests::admin_assign(pool, &character_id, zone, id).await {
        Ok(Some(_)) => send_to(
            world,
            player,
            format!("Assigned Quest ({zone}, {id}) to your character.\r\n"),
        ),
        Ok(None) => send_to(
            world,
            player,
            format!("Already have Quest ({zone}, {id}).\r\n"),
        ),
        Err(e) => send_to(world, player, format!("Assign failed: {e}\r\n")),
    }
}

/// `innate`: list the caller race's innate abilities (`RaceAbilities`).
pub(crate) async fn cmd_innate(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
) {
    let race = world.get::<Profile>(player).map(|p| p.race.clone());
    let Some(race) = race else {
        send_to(world, player, "You have no race assigned.\r\n");
        return;
    };
    let rows = match mud_db::race_abilities::list_for_race(pool, &race).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Innate fetch failed: {e}\r\n"));
            return;
        }
    };
    if rows.is_empty() {
        send_to(
            world,
            player,
            format!("\r\nThe {race} race has no innate abilities.\r\n"),
        );
        return;
    }
    let mut out = format!("\r\nInnate abilities for {race} ({}):\r\n", rows.len());
    for r in &rows {
        out.push_str(&format!(
            "  {name:<24} {cat:<10} bonus +{bonus:<3} cap {cap}\r\n",
            name = r.ability_name,
            cat = r.category,
            bonus = r.bonus,
            cap = r.proficiency_cap,
        ));
    }
    send_to(world, player, out);
}

/// `questinfo <zone> <id>`: read-only catalog view of one quest.
/// Reads `Quest` directly (not `CharacterQuest`), so the row doesn't
/// have to be assigned to anyone.
pub(crate) async fn cmd_questinfo(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let (Some(zone_raw), Some(id_raw)) = (parts.next(), parts.next()) else {
        send_to(world, player, "Usage: questinfo <zone> <id>\r\n");
        return;
    };
    let Ok(zone) = zone_raw.parse::<i32>() else {
        send_to(world, player, "Zone must be an integer.\r\n");
        return;
    };
    let Ok(id) = id_raw.parse::<i32>() else {
        send_to(world, player, "Id must be an integer.\r\n");
        return;
    };
    let row = match mud_db::quests::get_quest(pool, zone, id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Quest fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(row) = row else {
        send_to(
            world,
            player,
            format!("No Quest defined at ({zone}, {id}).\r\n"),
        );
        return;
    };
    let mut out = format!("\r\nQuest ({}, {}) — {}\r\n", row.zone_id, row.id, row.name);
    out.push_str(&format!(
        "Level range: {} to {}\r\n",
        row.min_level, row.max_level
    ));
    let mut flags: Vec<&'static str> = Vec::new();
    if row.repeatable {
        flags.push("repeatable");
    }
    if row.shareable {
        flags.push("shareable");
    }
    if row.hidden {
        flags.push("hidden");
    }
    if row.auto_accept {
        flags.push("auto-accept");
    }
    out.push_str(&format!(
        "Flags: {}\r\n",
        if flags.is_empty() {
            "none".to_string()
        } else {
            flags.join(", ")
        }
    ));
    if let Some(short) = row.short_description.as_deref()
        && !short.trim().is_empty()
    {
        out.push_str(&format!("\r\n{}\r\n", short.trim()));
    }
    if let Some(desc) = row.description.as_deref()
        && !desc.trim().is_empty()
    {
        out.push_str(&format!("\r\n{}\r\n", desc.trim()));
    }
    send_to(world, player, out);
}

/// `qgive <player> <zone> <quest-id>`: admin command — assign a
/// quest to another online player's character. Refuses if target
/// isn't online; offline assignment isn't wired today.
pub(crate) async fn cmd_qgive(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let target_word = parts.next();
    let zone_raw = parts.next();
    let id_raw = parts.next();
    let (Some(target_word), Some(zone_raw), Some(id_raw)) = (target_word, zone_raw, id_raw) else {
        send_to(world, player, "Usage: qgive <player> <zone> <quest-id>\r\n");
        return;
    };
    let Ok(zone) = zone_raw.parse::<i32>() else {
        send_to(world, player, "Zone must be an integer.\r\n");
        return;
    };
    let Ok(id) = id_raw.parse::<i32>() else {
        send_to(world, player, "Quest id must be an integer.\r\n");
        return;
    };
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(target_word))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{target_word}' isn't online.\r\n"));
        return;
    };
    let exists = match mud_db::quests::quest_exists(pool, zone, id).await {
        Ok(b) => b,
        Err(e) => {
            send_to(world, player, format!("Quest lookup failed: {e}\r\n"));
            return;
        }
    };
    if !exists {
        send_to(
            world,
            player,
            format!("No Quest defined at ({zone}, {id}).\r\n"),
        );
        return;
    }
    let target_char_id = world.get::<Account>(target).map(|a| a.character_id.clone());
    let Some(target_char_id) = target_char_id else {
        send_to(world, player, "Target has no account info.\r\n");
        return;
    };
    let target_name = name_of(world, target);
    match mud_db::quests::admin_assign(pool, &target_char_id, zone, id).await {
        Ok(Some(_)) => {
            send_to(
                world,
                player,
                format!("Assigned Quest ({zone}, {id}) to {target_name}.\r\n"),
            );
            send_to(
                world,
                target,
                format!(
                    "An immortal grants you a quest: ({zone}, {id}). Type `quests` to view.\r\n"
                ),
            );
        }
        Ok(None) => send_to(
            world,
            player,
            format!("{target_name} already has Quest ({zone}, {id}).\r\n"),
        ),
        Err(e) => send_to(world, player, format!("Assign failed: {e}\r\n")),
    }
}

/// `qcomplete <#>`: admin command — force-complete an in-progress
/// quest the caller's character has accepted. Slot is 1-based
/// against the `quests` in-progress section, same as `abandon`.
/// Useful for verifying reward / completion flow end-to-end without
/// the full objective-resolution pipeline.
pub(crate) async fn cmd_qcomplete(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let arg = args.trim();
    let Ok(slot) = arg.parse::<usize>() else {
        send_to(
            world,
            player,
            "Complete which quest? Pick a number from `quests`.\r\n",
        );
        return;
    };
    if slot == 0 {
        send_to(world, player, "Quest slots are 1-based.\r\n");
        return;
    }
    let character_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let Some(character_id) = character_id else {
        send_to(world, player, "No account info; can't fetch quests.\r\n");
        return;
    };
    let rows = match mud_db::quests::list_for_character(pool, &character_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Quest fetch failed: {e}\r\n"));
            return;
        }
    };
    let active: Vec<&mud_db::quests::CharacterQuestRow> = rows
        .iter()
        .filter(|r| r.status == "IN_PROGRESS")
        .collect();
    let Some(target) = active.get(slot - 1) else {
        send_to(
            world,
            player,
            format!("No in-progress quest at slot {slot}.\r\n"),
        );
        return;
    };
    match mud_db::quests::admin_complete(pool, &target.id).await {
        Ok(0) => {
            send_to(
                world,
                player,
                "That quest isn't in-progress; nothing to complete.\r\n",
            );
        }
        Ok(_) => {
            send_to(
                world,
                player,
                format!("Force-completed quest: {}.\r\n", target.quest_name),
            );
        }
        Err(e) => {
            send_to(world, player, format!("Complete failed: {e}\r\n"));
        }
    }
}

/// `abandon <#>`: drop an in-progress quest. Slot is 1-based against
/// the in-progress section of the `quests` listing. Marks the row
/// `ABANDONED` rather than deleting it, so the audit trail and
/// `(char, zone, id)` unique key are preserved.
pub(crate) async fn cmd_abandon(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let arg = args.trim();
    let Ok(slot) = arg.parse::<usize>() else {
        send_to(
            world,
            player,
            "Abandon which quest? Pick a number from `quests`.\r\n",
        );
        return;
    };
    if slot == 0 {
        send_to(world, player, "Quest slots are 1-based.\r\n");
        return;
    }
    let character_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let Some(character_id) = character_id else {
        send_to(world, player, "No account info; can't fetch quests.\r\n");
        return;
    };
    let rows = match mud_db::quests::list_for_character(pool, &character_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Quest fetch failed: {e}\r\n"));
            return;
        }
    };
    let active: Vec<&mud_db::quests::CharacterQuestRow> = rows
        .iter()
        .filter(|r| r.status == "IN_PROGRESS")
        .collect();
    let Some(target) = active.get(slot - 1) else {
        send_to(
            world,
            player,
            format!("No in-progress quest at slot {slot}.\r\n"),
        );
        return;
    };
    match mud_db::quests::abandon(pool, &target.id).await {
        Ok(0) => {
            send_to(
                world,
                player,
                "That quest isn't in-progress; nothing to abandon.\r\n",
            );
        }
        Ok(_) => {
            send_to(
                world,
                player,
                format!("Abandoned quest: {}.\r\n", target.quest_name),
            );
        }
        Err(e) => {
            send_to(world, player, format!("Abandon failed: {e}\r\n"));
        }
    }
}

/// `read <#>` while standing near a board: render that board's
/// message body. Routed here from the async pre-dispatch when the
/// argument is a positive integer and the player's room contains a
/// `BoardLink`-tagged item. Out-of-range / fetch errors fall back
/// to friendly messages without re-dispatching.
pub(crate) async fn cmd_read_board_msg(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    board_id: i32,
    args: &str,
) {
    let Ok(slot) = args.trim().parse::<usize>() else {
        return;
    };
    if slot == 0 {
        send_to(world, player, "Slots are 1-based.\r\n");
        return;
    }
    let summary = world
        .get_resource::<BoardCatalog>()
        .and_then(|c| c.by_id.get(&board_id))
        .cloned();
    let Some(summary) = summary else {
        send_to(world, player, "That board's catalog entry is missing.\r\n");
        return;
    };
    let messages = match mud_db::boards::messages_for_board(pool, board_id).await {
        Ok(m) => m,
        Err(e) => {
            send_to(world, player, format!("Message fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(msg) = messages.get(slot - 1) else {
        send_to(
            world,
            player,
            format!(
                "No message at slot {slot} on {} (it has {} message{}).\r\n",
                summary.title,
                messages.len(),
                if messages.len() == 1 { "" } else { "s" },
            ),
        );
        return;
    };
    let mut out = format!(
        "\r\n[{}] message {}/{}\r\n",
        summary.title, slot, messages.len()
    );
    out.push_str(&format!("From:    {} (level {})\r\n", msg.poster, msg.poster_level));
    out.push_str(&format!("Subject: {}\r\n", msg.subject));
    out.push_str(&format!("Posted:  {}\r\n", msg.posted_at.format("%Y-%m-%d %H:%M")));
    if msg.sticky {
        out.push_str("(sticky)\r\n");
    }
    out.push_str("---\r\n");
    out.push_str(msg.content.trim_end());
    out.push_str("\r\n---\r\n");
    send_to(world, player, out);
}

/// `look <board>` / `examine <board>`: render the board's message
/// listing inline. Routed here from the async pre-dispatch when the
/// argument matches a BOARD-tagged item in the player's room.
pub(crate) async fn cmd_look_board(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    board_id: i32,
) {
    let summary = world
        .get_resource::<BoardCatalog>()
        .and_then(|c| c.by_id.get(&board_id))
        .cloned();
    let Some(summary) = summary else {
        send_to(world, player, "That board's catalog entry is missing.\r\n");
        return;
    };
    let messages = match mud_db::boards::messages_for_board(pool, board_id).await {
        Ok(m) => m,
        Err(e) => {
            send_to(world, player, format!("Message fetch failed: {e}\r\n"));
            return;
        }
    };
    if messages.is_empty() {
        send_to(
            world,
            player,
            format!("\r\n{} has no messages.\r\n", summary.title),
        );
        return;
    }
    let mut out = format!(
        "\r\n{} ({} message{}):\r\n",
        summary.title,
        messages.len(),
        if messages.len() == 1 { "" } else { "s" },
    );
    for (i, msg) in messages.iter().enumerate() {
        let stickymark = if msg.sticky { "*" } else { " " };
        let when = msg.posted_at.format("%Y-%m-%d");
        out.push_str(&format!(
            "  {:<3} {} {when}  {:<20} {}\r\n",
            i + 1,
            stickymark,
            msg.poster,
            msg.subject,
        ));
    }
    out.push_str("\r\nUse `read <#>` to read a message, or `post` to add one.\r\n");
    send_to(world, player, out);
}

/// `boards`: list every available board with its alias and title.
/// Lock state is shown — locked boards refuse posts.
pub(crate) async fn cmd_boards(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    let rows = match mud_db::boards::list_boards(pool).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Board fetch failed: {e}\r\n"));
            return;
        }
    };
    if rows.is_empty() {
        send_to(world, player, "\r\nNo boards exist.\r\n");
        return;
    }
    let mut out = format!("\r\nBoards ({}):\r\n", rows.len());
    for b in &rows {
        let lock = if b.locked { "[locked]" } else { "        " };
        out.push_str(&format!("  {:<10} {} {}\r\n", b.alias, lock, b.title));
    }
    out.push_str("\r\nUse `board <alias>` to list messages, `board <alias> <#>` to read one.\r\n");
    send_to(world, player, out);
}

/// `board <alias> [#]`: list messages on a board, or read a specific
/// one if a slot number is appended. Sticky messages float to the top
/// of the listing and are flagged.
pub(crate) async fn cmd_board(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let Some(alias) = parts.next() else {
        send_to(world, player, "Usage: board <alias> [#]\r\n");
        return;
    };
    let slot = parts.next().and_then(|s| s.parse::<usize>().ok());
    let board = match mud_db::boards::find_board_by_alias(pool, alias).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            send_to(world, player, format!("No board called '{alias}'.\r\n"));
            return;
        }
        Err(e) => {
            send_to(world, player, format!("Board fetch failed: {e}\r\n"));
            return;
        }
    };
    let messages = match mud_db::boards::messages_for_board(pool, board.id).await {
        Ok(m) => m,
        Err(e) => {
            send_to(world, player, format!("Message fetch failed: {e}\r\n"));
            return;
        }
    };
    if let Some(slot) = slot {
        if slot == 0 {
            send_to(world, player, "Message slots are 1-based.\r\n");
            return;
        }
        let Some(msg) = messages.get(slot - 1) else {
            send_to(
                world,
                player,
                format!("No message at slot {slot} on '{alias}'.\r\n"),
            );
            return;
        };
        let mut out = format!("\r\n[{}] message {}/{}\r\n", board.title, slot, messages.len());
        out.push_str(&format!("From:    {} (level {})\r\n", msg.poster, msg.poster_level));
        out.push_str(&format!("Subject: {}\r\n", msg.subject));
        out.push_str(&format!("Posted:  {}\r\n", msg.posted_at.format("%Y-%m-%d %H:%M")));
        if msg.sticky {
            out.push_str("(sticky)\r\n");
        }
        out.push_str("---\r\n");
        out.push_str(msg.content.trim_end());
        out.push_str("\r\n---\r\n");
        send_to(world, player, out);
        return;
    }
    if messages.is_empty() {
        send_to(
            world,
            player,
            format!("\r\n{} has no messages.\r\n", board.title),
        );
        return;
    }
    let mut out = format!(
        "\r\n{} ({} message{}):\r\n",
        board.title,
        messages.len(),
        if messages.len() == 1 { "" } else { "s" },
    );
    for (i, msg) in messages.iter().enumerate() {
        let stickymark = if msg.sticky { "*" } else { " " };
        let when = msg.posted_at.format("%Y-%m-%d");
        out.push_str(&format!(
            "  {:<3} {} {when}  {:<20} {}\r\n",
            i + 1,
            stickymark,
            msg.poster,
            msg.subject,
        ));
    }
    out.push_str(&format!("\r\nUse `board {alias} <#>` to read a message.\r\n"));
    send_to(world, player, out);
}

/// `delmail <#>`: soft-delete the slot-numbered mail.
pub(crate) async fn cmd_delmail(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let arg = args.trim();
    let Ok(slot) = arg.parse::<usize>() else {
        send_to(
            world,
            player,
            "Delete which mail? Pick a number from `mailbox`.\r\n",
        );
        return;
    };
    if slot == 0 {
        send_to(world, player, "Mail slots are 1-based.\r\n");
        return;
    }
    let user_id = world.get::<Account>(player).map(|a| a.user_id.clone());
    let Some(user_id) = user_id else {
        send_to(world, player, "No account info; can't fetch mail.\r\n");
        return;
    };
    let rows = match mud_db::mail::inbox_for(pool, &user_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Mail fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(row) = rows.get(slot - 1) else {
        send_to(world, player, format!("No mail at slot {slot}.\r\n"));
        return;
    };
    if let Err(e) = mud_db::mail::soft_delete(pool, row.id).await {
        send_to(world, player, format!("Delete failed: {e}\r\n"));
        return;
    }
    send_to(
        world,
        player,
        format!("Deleted mail #{slot}: \"{}\".\r\n", row.subject),
    );
}

fn cmd_gossip(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Gossip what?\r\n");
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let player_name = name_of(world, player);

    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world).collect()
    };
    for t in targets {
        if t != player && has_flag(world, t, PlayerFlag::Deaf) {
            continue;
        }
        // Per-target ignore: receiver hides messages from senders on
        // their list. Sender sees their own message normally.
        if t != player
            && world
                .get::<IgnoreList>(t)
                .is_some_and(|l| l.contains(&player_name))
        {
            continue;
        }
        let line = if t == player {
            format!("You gossip, \"{message}\"\r\n")
        } else {
            format!("{player_name} gossips, \"{message}\"\r\n")
        };
        send_to(world, t, line);
    }
}

/// `music <message>`: global RP-flavored channel. Same broadcast
/// rules as gossip — every online player sees it unless they're
/// `Deaf` or have ignored the speaker.
fn cmd_music(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Sing what?\r\n");
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let player_name = name_of(world, player);
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world).collect()
    };
    for t in targets {
        if t != player && has_flag(world, t, PlayerFlag::Deaf) {
            continue;
        }
        if t != player
            && world
                .get::<IgnoreList>(t)
                .is_some_and(|l| l.contains(&player_name))
        {
            continue;
        }
        let line = if t == player {
            format!("You sing, \"{message}\"\r\n")
        } else {
            format!("{player_name} sings, \"{message}\"\r\n")
        };
        send_to(world, t, line);
    }
}

const INSULT_LINES: &[&str] = &[
    "You smell like a troll's armpit!",
    "Your mother was a bugbear!",
    "You fight like a dairy farmer!",
    "I've seen better-looking rust monsters!",
    "Even a gelatinous cube has more personality!",
    "Your sword is dull and your wits are duller!",
    "I've met kobolds with sharper tongues!",
    "Your aim is as bad as your cooking!",
];

/// `insult <target>`: random-line jab at another actor in the room.
/// Self-target collapses to "You feel insulted." per legacy.
fn cmd_insult(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "You feel insulted.\r\n");
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
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
        send_to(world, player, "You feel insulted.\r\n");
        return;
    }
    let line = INSULT_LINES[rand::random_range(0..INSULT_LINES.len())];
    let actor_name = name_of(world, player);
    let target_name = name_of(world, target);

    send_to(
        world,
        player,
        format!("You insult {target_name}: {line}\r\n"),
    );
    send_to(
        world,
        target,
        format!("{actor_name} insults you: {line}\r\n"),
    );

    let bystanders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| l.0 == located.0 && *e != player && *e != target)
            .map(|(e, _)| e)
            .collect()
    };
    let line_room = format!("{actor_name} insults {target_name}.\r\n");
    for e in bystanders {
        send_to(world, e, line_room.clone());
    }
}

/// `petition <message>`: one-way help channel. Anyone can send;
/// every online Immortal+ receives. Sender gets a confirmation echo.
fn cmd_petition(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(
            world,
            player,
            "Petition what? Use this to ask online immortals for help.\r\n",
        );
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let player_name = name_of(world, player);
    let immortals: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Account), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(_, a)| a.role.at_least(UserRole::Immortal))
            .map(|(e, _)| e)
            .collect()
    };
    let line = format!("[PETITION] {player_name}: {message}\r\n");
    for t in immortals {
        send_to(world, t, line.clone());
    }
    send_to(
        world,
        player,
        "Your petition has been sent to the immortals.\r\n",
    );
}

/// `wiznet <message>`: staff-only chat. Reaches every online
/// player whose Account.role is at least Immortal. Players never
/// see wiznet traffic.
fn cmd_wiznet(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Wiznet what?\r\n");
        return;
    }
    let player_name = name_of(world, player);
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Account), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(_, a)| a.role.at_least(UserRole::Immortal))
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        let line = if t == player {
            format!("[wiznet] You: {message}\r\n")
        } else {
            format!("[wiznet] {player_name}: {message}\r\n")
        };
        send_to(world, t, line);
    }
}

fn cmd_emote(world: &mut World, player: Entity, args: &str) {
    let action = args.trim();
    if action.is_empty() {
        send_to(world, player, "Emote what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let player_name = name_of(world, player);
    let line = format!("{player_name} {action}\r\n");

    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == located.0)
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        send_to(world, t, line.clone());
    }
}

fn cmd_shout(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Shout what?\r\n");
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let player_name = name_of(world, player);

    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world).collect()
    };
    for t in targets {
        if t != player && has_flag(world, t, PlayerFlag::Deaf) {
            continue;
        }
        if t != player
            && world
                .get::<IgnoreList>(t)
                .is_some_and(|l| l.contains(&player_name))
        {
            continue;
        }
        let line = if t == player {
            format!("You shout, \"{message}\"\r\n")
        } else {
            format!("{player_name} shouts, \"{message}\"\r\n")
        };
        send_to(world, t, line);
    }
}

// ---------------------------------------------------------------------------
// Combat handler
// ---------------------------------------------------------------------------

/// Combat-action stamina costs (one stop in scope so a balance pass can
/// retune them in one place).
const ATTACK_COST: i32 = 2;
const KICK_COST: i32 = 5;
const BASH_COST: i32 = 8;
const BANDAGE_COST: i32 = 4;
const LAYHANDS_COST: i32 = 12;
const RESCUE_COST: i32 = 6;
const DISARM_COST: i32 = 5;
const HITALL_COST: i32 = 10;
const DOORBASH_COST: i32 = 10;
const BACKSTAB_COST: i32 = 6;
const SPRINGLEAP_COST: i32 = 7;
const GOUGE_COST: i32 = 7;
const GOUGE_BLIND_SECS: i32 = 30;
const REND_COST: i32 = 7;
const REND_BLEED_SECS: i32 = 30;
const ROAR_COST: i32 = 8;
const ROAR_FEAR_SECS: i32 = 20;
const STOMP_COST: i32 = 6;
const TRIPUP_COST: i32 = 5;
const SWEEP_COST: i32 = 12;
const ROUNDHOUSE_COST: i32 = 7;
const THROATCUT_COST: i32 = 8;
const BERSERK_COST: i32 = 8;
const BERSERK_DURATION_SECS: i32 = 60;

/// Pre-flight stamina check. Returns false if the player has Stamina and
/// it's below `cost`; sends "You're too winded to <verb>." and the caller
/// should abort. Players without a Stamina component pass (mobs, etc.).
fn check_stamina(world: &World, player: Entity, cost: i32, verb: &str) -> bool {
    if let Some(s) = world.get::<Stamina>(player).copied()
        && s.current < cost
    {
        send_to(
            world,
            player,
            format!("You're too winded to {verb}.\r\n"),
        );
        return false;
    }
    true
}

/// Apply `amount` damage to `target`'s Health. Returns `(dead, threshold_msg)`
/// — `dead` is true if HP dropped to zero or below; `threshold_msg`, if Some,
/// is a one-time downward-crossing message ("hurt"/"badly hurt"/"near death")
/// that the caller should `send_to(target, ..)` after its hit-line so the
/// ordering reads naturally. None when no threshold was crossed, when the
/// target lacks Health, or when the blow was lethal (death message takes over).
/// Most-severe-wins: a single hit that crosses several thresholds emits only
/// the lowest-band message.
pub(crate) fn apply_damage(
    world: &mut World,
    target: Entity,
    amount: i32,
) -> (bool, Option<&'static str>) {
    let Some((old, max)) = world.get::<Health>(target).map(|h| (h.hp, h.max)) else {
        return (false, None);
    };
    let new_value = old - amount;
    if let Some(mut h) = world.get_mut::<Health>(target) {
        h.hp = new_value;
    }
    if new_value <= 0 {
        return (true, None);
    }
    let near = max / 10;
    let badly = max / 4;
    let hurt = max / 2;
    let msg = if old > near && new_value <= near {
        Some("You are near death!\r\n")
    } else if old > badly && new_value <= badly {
        Some("You are badly hurt!\r\n")
    } else if old > hurt && new_value <= hurt {
        Some("You are hurt.\r\n")
    } else {
        None
    };
    (false, msg)
}

/// Find every entity Fighting `target`, remove their Fighting component,
/// and send "Your target falls." to each. Used by both the natural
/// death path (`combat::handle_death`'s mob branch) and the admin `slay`
/// command — anywhere a target stops existing as a combatant and we
/// need everyone gunning for them to disengage cleanly.
pub(crate) fn disengage_attackers_of(world: &mut World, target: Entity) {
    let attackers: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Fighting)>();
        q.iter(world)
            .filter(|(_, f)| f.0 == target)
            .map(|(e, _)| e)
            .collect()
    };
    for a in attackers {
        try_remove::<Fighting>(world, a);
        send_to(world, a, "Your target falls.\r\n");
    }
}

/// Pay the stamina cost. Caps current at zero. Sends one-time messages
/// when crossing the "tired" (25% of max) and "exhausted" (0) thresholds
/// downward — never on the way back up (regen handles that silently).
pub(crate) fn drain_stamina(world: &mut World, player: Entity, cost: i32) {
    let Some((old, max)) = world.get::<Stamina>(player).map(|s| (s.current, s.max)) else {
        return;
    };
    let new_value = (old - cost).max(0);
    if let Some(mut s) = world.get_mut::<Stamina>(player) {
        s.current = new_value;
    }
    let tired_threshold = max / 4;
    if old > tired_threshold && new_value <= tired_threshold && new_value > 0 {
        send_to(world, player, "You're getting tired.\r\n");
    }
    if old > 0 && new_value == 0 {
        send_to(world, player, "You collapse, exhausted.\r\n");
    }
}

/// Refuse the action if the entity is sleeping; auto-rise from a sitting or
/// resting posture (with announcements). Returns false if the action should
/// be aborted.
fn require_alert_posture(world: &mut World, player: Entity, action: &str) -> bool {
    let posture = world.get::<Posture>(player).copied();
    match posture.map(|p| p.0) {
        Some(PostureKind::Sleeping) => {
            send_to(world, player, format!("You can't {action} while sleeping.\r\n"));
            false
        }
        Some(PostureKind::Sitting | PostureKind::Kneeling | PostureKind::Resting) => {
            // Auto-stand.
            try_insert(world, player, Posture(PostureKind::Standing));
            send_to(world, player, "You stand up.\r\n");
            if let Some(located) = world.get::<Located>(player).copied() {
                let mover_name = name_of(world, player);
                broadcast_room_except_players_rendered(
                    world,
                    located.0,
                    &[player],
                    &format!("{mover_name} stands up.\r\n"),
                );
            }
            true
        }
        _ => true,
    }
}

fn cmd_attack(world: &mut World, player: Entity, target_name: &str) {
    if !require_alert_posture(world, player, "attack") {
        return;
    }
    if !check_stamina(world, player, ATTACK_COST, "attack") {
        return;
    }
    let target_name = target_name.trim();
    if target_name.is_empty() {
        send_to(world, player, "Attack what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let target_lower = target_name.to_ascii_lowercase();

    let target = {
        let mut q = world.query::<(Entity, &Located, &Named)>();
        q.iter(world)
            .find(|(e, l, n)| {
                *e != player
                    && l.0 == located.0
                    && n.name.to_ascii_lowercase().contains(&target_lower)
            })
            .map(|(e, _, _)| e)
    };

    let Some(target) = target else {
        send_rendered(world, player, &format!("You don't see '{target_name}' here.\r\n"),
        );
        return;
    };

    let actual_name = name_of(world, target);
    let player_name = name_of(world, player);

    try_insert(world, player, Fighting(target));
    if world.get::<CombatStats>(target).is_some()
        && let Ok(mut e) = world.get_entity_mut(target)
    {
        e.insert(Fighting(player));
    }
    drain_stamina(world, player, ATTACK_COST);

    send_to(world, player, format!("You attack {actual_name}!\r\n"));
    send_rendered(world, target, &format!("{player_name} attacks you!\r\n"));
    broadcast_room_except_rendered(
        world,
        located.0,
        &[player, target],
        &format!("{player_name} attacks {actual_name}.\r\n"),
    );

    // Auto-assist: anyone following `target` with AUTO_ASSIST set, in
    // the same room, not already fighting — they engage `player`.
    auto_assist_followers_of(world, target, player, located.0);

    // Fire ATTACK trigger on the target. Bodies typically run
    // initial-aggression flavor or counter-attacks. `self` = target,
    // `actor` = attacker.
    crate::triggers::fire_event_with_actor(
        world,
        target,
        player,
        mud_world::TriggerEvent::Attack,
    );
}

/// When `defender` is attacked, find every entity with
/// `Follower(defender)` who has the `AUTO_ASSIST` flag, isn't already
/// fighting, and is in `room`, and engage `attacker`. Used as the
/// hook on the bottom of `cmd_attack`.
fn auto_assist_followers_of(
    world: &mut World,
    defender: Entity,
    attacker: Entity,
    room: Entity,
) {
    let helpers: Vec<Entity> = {
        let mut q = world
            .query_filtered::<(Entity, &Follower, &Located, Option<&PlayerFlags>, Option<&Fighting>), With<Player>>();
        q.iter(world)
            .filter(|(e, f, l, flags, fighting)| {
                *e != attacker
                    && *e != defender
                    && f.0 == defender
                    && l.0 == room
                    && fighting.is_none()
                    && flags.is_some_and(|pf| pf.has(PlayerFlag::AutoAssist))
            })
            .map(|(e, _, _, _, _)| e)
            .collect()
    };
    let attacker_name = name_or(world, attacker, "<unknown>");
    for helper in helpers {
        try_insert(world, helper, Fighting(attacker));
        let helper_name = name_or(world, helper, "<unknown>");
        send_rendered(
            world,
            helper,
            &format!(
                "You auto-assist and engage {attacker_name}!\r\n",
            ),
        );
        send_rendered(
            world,
            attacker,
            &format!(
                "{helper_name} auto-assists and joins the fight against you!\r\n",
            ),
        );
    }
}

fn cmd_consider(world: &mut World, player: Entity, target_word: &str) {
    let target_word = target_word.trim();
    if target_word.is_empty() {
        send_to(world, player, "Consider whom?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(target) = find_actor_in_room(world, target_word, located.0, player) else {
        send_rendered(world, player, &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    let target_name = name_of(world, target);

    let self_max_hp = world.get::<Health>(player).map_or(1, |h| h.max).max(1);
    let self_dmg = world.get::<CombatStats>(player).map_or(0, |c| c.dmg_roll);
    let target_max_hp = world.get::<Health>(target).map_or(0, |h| h.max);
    let target_dmg = world.get::<CombatStats>(target).map_or(0, |c| c.dmg_roll);

    if target_max_hp == 0 {
        send_rendered(world, player, &format!("{target_name} doesn't look like a fighter at all.\r\n"),
        );
        return;
    }

    // Score = max_hp scaled by damage output (1 + dmg/10). Compare ratio to
    // self. The cutoffs are chosen by feel — easy to retune later.
    let self_score = f64::from(self_max_hp) * (1.0 + f64::from(self_dmg) / 10.0);
    let target_score = f64::from(target_max_hp) * (1.0 + f64::from(target_dmg) / 10.0);
    let ratio = target_score / self_score.max(1.0);

    let verdict = if ratio < 0.30 {
        "is no match for you."
    } else if ratio < 0.70 {
        "looks like an easy fight."
    } else if ratio < 1.50 {
        "might give you a fight."
    } else if ratio < 3.00 {
        "looks tougher than you."
    } else {
        "would slaughter you. Don't try it."
    };

    send_rendered(world, player, &format!("{target_name} {verdict}\r\n"));
}

pub(crate) fn cmd_flee(world: &mut World, player: Entity, _args: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let from_room = located.0;

    // Collect open exits with valid targets.
    let candidates: Vec<(mud_db::enums::Direction, Entity)> = world
        .get::<Exits>(from_room)
        .map(|e| {
            e.0.iter()
                .filter_map(|(dir, ed)| {
                    if ed.state == mud_db::enums::ExitState::Open {
                        ed.to.map(|t| (*dir, t))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    if candidates.is_empty() {
        send_to(world, player, "There's nowhere to run!\r\n");
        return;
    }

    let pick = rand::random_range(0..candidates.len());
    let (dir, target) = candidates[pick];
    let dir_name = direction_name(dir);

    let mover_name = name_of(world, player);

    // Notify the source room you're fleeing.
    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[player],
        &format!("{mover_name} panics and flees {dir_name}!\r\n"),
    );

    // Drop our own Fighting; combat_tick auto-disengages attackers on
    // the next 1Hz pass via the room-mismatch check.
    try_remove::<Fighting>(world, player);

    // Move + announce arrival + auto-look.
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    }
    let arrival_dir = opposite(dir).map_or("nearby".to_string(), |d| {
        format!("the {}", direction_name(d))
    });
    broadcast_room_except_players_rendered(
        world,
        target,
        &[player],
        &format!("{mover_name} arrives, panting, from {arrival_dir}.\r\n"),
    );
    send_to(world, player, format!("You flee {dir_name}!\r\n"));
    cmd_look(world, player, "");
}

/// `kick` — Phase C migration: shimmed over the `KICK` data path
/// (damage effect, formula `level + dex_bonus + skill / 4` —
/// `dex_bonus` unmodeled, falls back to default `1d6`). Posture and
/// stamina gates stay; target/effect/messaging via `invoke_ability`.
/// Empty arg uses caster's current Fighting target.
fn cmd_kick(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "kick") {
        return;
    }
    let Some(fighting) = world.get::<Fighting>(player).copied() else {
        send_to(world, player, "You aren't fighting anyone.\r\n");
        return;
    };
    let target = fighting.0;
    if world.get_entity(target).is_err() {
        try_remove::<Fighting>(world, player);
        send_to(world, player, "Your target is gone.\r\n");
        return;
    }
    if !check_stamina(world, player, KICK_COST, "kick") {
        return;
    }
    drain_stamina(world, player, KICK_COST);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("kick {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// `berserk`: self-buff applying a `berserk` `EffectInstance` for 60s.
/// No combat-damage scaling consumer yet — visible state only. Same
/// dedup pattern as gouge via `has_effect_named`.
fn cmd_berserk(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "berserk") {
        return;
    }
    if has_effect_named(world, player, "berserk") {
        send_to(world, player, "You're already in a rage.\r\n");
        return;
    }
    if !check_stamina(world, player, BERSERK_COST, "berserk") {
        return;
    }
    drain_stamina(world, player, BERSERK_COST);
    world.spawn((
        EffectInstance {
            kind: 0,
            name: "berserk".to_string(),
            strength: 1,
            remaining_secs: BERSERK_DURATION_SECS,
            source: EffectSource::Other("berserk".to_string()),
            ability_id: None,
        },
        AppliedTo(player),
    ));
    send_to(world, player, "You go BERSERK!\r\n");
    let player_name = name_of(world, player);
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    broadcast_room_except_rendered(
        world,
        located.0,
        &[player],
        &format!("{player_name} goes BERSERK!\r\n"),
    );
}

/// `stomp [<target>]`: damage + knock target prone. Default target
/// is your current Fighting target. Refused on already-prone
/// targets.
fn cmd_stomp(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "stomp") {
        return;
    }
    if !check_stamina(world, player, STOMP_COST, "stomp") {
        return;
    }
    let arg = args.trim();
    let target = if arg.is_empty() {
        let Some(Fighting(t)) = world.get::<Fighting>(player).copied() else {
            send_to(world, player, "Stomp whom? You aren't fighting.\r\n");
            return;
        };
        t
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        let Some(t) = find_actor_in_room(world, arg, located.0, player) else {
            send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
            return;
        };
        t
    };
    if target == player {
        send_to(world, player, "You can't stomp yourself.\r\n");
        return;
    }
    let cur_posture = world.get::<Posture>(target).map(|p| p.0);
    if !matches!(cur_posture, Some(PostureKind::Standing)) {
        let target_name = name_or(world, target, "<unknown>");
        send_to(world, player, format!(
            "{target_name} is already on the ground.\r\n",
        ));
        return;
    }
    let Some(target_room) = world.get::<Located>(target).copied().map(|l| l.0) else {
        send_to(world, player, "Target is in limbo.\r\n");
        return;
    };

    let dmg = world.get::<CombatStats>(player).map_or(1, |c| (c.dmg_roll / 2).max(1));
    drain_stamina(world, player, STOMP_COST);

    let player_name = name_of(world, player);
    let target_name = name_or(world, target, "<unknown>");
    let (dead, _) = apply_damage(world, target, dmg);

    if !dead
        && let Ok(mut e) = world.get_entity_mut(target)
    {
        e.insert(Posture(PostureKind::Sitting));
    }

    send_to(world, player, format!(
        "You stomp on {target_name} for {dmg} damage; they go down!\r\n"
    ));
    if !dead {
        send_rendered(world, target, &format!(
            "{player_name} stomps you to the ground!\r\n"
        ));
    }
    broadcast_room_except_rendered(
        world,
        target_room,
        &[player, target],
        &format!("{player_name} stomps {target_name} to the ground!\r\n"),
    );

    if dead {
        crate::combat::handle_death(world, target, &target_name, target_room);
    }
}

/// `tripup` / `trip`: lighter version of stomp. Costs 5 stamina,
/// deals 1/4 `dmg_roll`, leaves target Resting (rather than Sitting).
/// `tripup` / `trip` — Phase C migration: shimmed over the `TRIP_UP`
/// data path (knockdown + damage effects, 8s cooldown). Posture and
/// stamina gates stay in the shim; target/effect/messaging flow
/// through `invoke_ability`. Empty arg falls back to the caster's
/// current `Fighting` target so legacy `trip` (no arg) keeps working.
fn cmd_tripup(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "tripup") {
        return;
    }
    let arg = args.trim();
    // Empty-arg shortcut: current Fighting target. The data path
    // doesn't synthesize this; we resolve it here and pass the name
    // through.
    let dispatched = if arg.is_empty() {
        let Some(Fighting(t)) = world.get::<Fighting>(player).copied() else {
            send_to(world, player, "Trip up whom? You aren't fighting.\r\n");
            return;
        };
        let target_name = name_of(world, t);
        format!("trip_up {target_name}")
    } else if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        // Targeting gate would also catch this, but refusing here
        // skips wasted stamina.
        send_to(world, player, "You can't trip yourself.\r\n");
        return;
    } else {
        format!("trip_up {arg}")
    };
    if !check_stamina(world, player, TRIPUP_COST, "tripup") {
        return;
    }
    drain_stamina(world, player, TRIPUP_COST);
    invoke_ability(
        world,
        player,
        &dispatched,
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// `sweep`: room-wide kick — knock every standing mob in the room
/// to Sitting and deal 1/4 `dmg_roll`. Players are filtered out.
fn cmd_sweep(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "sweep") {
        return;
    }
    if !check_stamina(world, player, SWEEP_COST, "sweep") {
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let dmg = world.get::<CombatStats>(player).map_or(1, |c| (c.dmg_roll / 4).max(1));
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located, Option<&Posture>, Option<&Health>), With<Mob>>();
        q.iter(world)
            .filter(|(_, l, p, h)| {
                l.0 == room
                    && h.is_some()
                    && matches!(p.map(|p| p.0), None | Some(PostureKind::Standing))
            })
            .map(|(e, _, _, _)| e)
            .collect()
    };
    if targets.is_empty() {
        send_to(world, player, "Nothing here to sweep.\r\n");
        return;
    }
    drain_stamina(world, player, SWEEP_COST);
    let player_name = name_of(world, player);
    let count = targets.len();
    for t in targets {
        let target_name = name_or(world, t, "<unknown>");
        let (dead, _) = apply_damage(world, t, dmg);
        if dead {
            crate::combat::handle_death(world, t, &target_name, room);
        } else if let Ok(mut e) = world.get_entity_mut(t) {
            e.insert(Posture(PostureKind::Sitting));
        }
    }
    send_to(world, player, format!(
        "You sweep your leg in a wide arc — {count} go down!\r\n"
    ));
    broadcast_room_except_rendered(
        world, room, &[player],
        &format!("{player_name} sweeps a wide kick across the room!\r\n"),
    );
}

/// `roundhouse` — Phase C migration: shimmed over the `ROUNDHOUSE`
/// data path (damage formula `"skill"`; falls back to default `1d6`
/// for untrained casters). Posture / stamina / fighting / dead-target
/// gates stay; messaging via `invoke_ability`.
fn cmd_roundhouse(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "roundhouse") {
        return;
    }
    let Some(Fighting(target)) = world.get::<Fighting>(player).copied() else {
        send_to(world, player, "You aren't fighting anyone.\r\n");
        return;
    };
    if world.get_entity(target).is_err() {
        try_remove::<Fighting>(world, player);
        send_to(world, player, "Your target is gone.\r\n");
        return;
    }
    if !check_stamina(world, player, ROUNDHOUSE_COST, "roundhouse") {
        return;
    }
    drain_stamina(world, player, ROUNDHOUSE_COST);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("roundhouse {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// `roar` / `howl`: room-wide fear application to mobs.
fn cmd_roar(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "roar") {
        return;
    }
    if !check_stamina(world, player, ROAR_COST, "roar") {
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;
    // Pre-compute the set of currently-feared mobs so we don't stack.
    let feared: std::collections::HashSet<Entity> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(e, _)| e.name.eq_ignore_ascii_case("fear"))
            .map(|(_, applied)| applied.0)
            .collect()
    };
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Mob>>();
        q.iter(world)
            .filter(|(e, l)| l.0 == room && !feared.contains(e))
            .map(|(e, _)| e)
            .collect()
    };
    drain_stamina(world, player, ROAR_COST);

    let player_name = name_of(world, player);
    let count = targets.len();
    for t in targets {
        world.spawn((
            EffectInstance {
                kind: 0,
                name: "fear".to_string(),
                strength: 1,
                remaining_secs: ROAR_FEAR_SECS,
                source: EffectSource::Other("roar".to_string()),
                ability_id: None,
            },
            AppliedTo(t),
        ));
    }
    send_to(world, player, format!(
        "You roar a primal challenge. ({count} mob(s) feared)\r\n"
    ));
    broadcast_room_except_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} roars a primal challenge!\r\n"),
    );
}

/// `rend [<target>]`: tearing attack — damage + temporary bleed
/// effect. Same shape as gouge but for the `bleed` debuff name.
fn cmd_rend(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "rend") {
        return;
    }
    if !check_stamina(world, player, REND_COST, "rend") {
        return;
    }
    let arg = args.trim();
    let target = if arg.is_empty() {
        let Some(Fighting(t)) = world.get::<Fighting>(player).copied() else {
            send_to(world, player, "Rend whom? You aren't fighting.\r\n");
            return;
        };
        t
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        let Some(t) = find_actor_in_room(world, arg, located.0, player) else {
            send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
            return;
        };
        t
    };
    if target == player {
        send_to(world, player, "You can't rend yourself.\r\n");
        return;
    }
    if has_effect_named(world, target, "bleed") {
        let target_name = name_or(world, target, "<unknown>");
        send_to(world, player, format!("{target_name} is already bleeding.\r\n"));
        return;
    }
    let Some(target_room) = world.get::<Located>(target).copied().map(|l| l.0) else {
        send_to(world, player, "Target is in limbo.\r\n");
        return;
    };

    let dmg = world.get::<CombatStats>(player).map_or(1, |c| c.dmg_roll);
    drain_stamina(world, player, REND_COST);

    let player_name = name_of(world, player);
    let target_name = name_or(world, target, "<unknown>");
    let (dead, _) = apply_damage(world, target, dmg);

    if !dead {
        world.spawn((
            EffectInstance {
                kind: 0,
                name: "bleed".to_string(),
                strength: 1,
                remaining_secs: REND_BLEED_SECS,
                source: EffectSource::Other("rend".to_string()),
                ability_id: None,
            },
            AppliedTo(target),
        ));
    }

    send_to(world, player, format!(
        "You rend {target_name} for {dmg} damage!\r\n"
    ));
    if !dead {
        send_rendered(world, target, &format!(
            "{player_name} tears into your flesh; you start to bleed!\r\n"
        ));
    }
    broadcast_room_except_rendered(
        world,
        target_room,
        &[player, target],
        &format!("{player_name} tears into {target_name}!\r\n"),
    );

    if dead {
        crate::combat::handle_death(world, target, &target_name, target_room);
    }
}

/// `gouge [<target>]`: damage + temporary blind effect. Default
/// target = current Fighting target.
fn cmd_gouge(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "gouge") {
        return;
    }
    if !check_stamina(world, player, GOUGE_COST, "gouge") {
        return;
    }
    let arg = args.trim();
    let target = if arg.is_empty() {
        let Some(Fighting(t)) = world.get::<Fighting>(player).copied() else {
            send_to(world, player, "Gouge whom? You aren't fighting.\r\n");
            return;
        };
        t
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        let Some(t) = find_actor_in_room(world, arg, located.0, player) else {
            send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
            return;
        };
        t
    };
    if target == player {
        send_to(world, player, "You can't gouge yourself.\r\n");
        return;
    }
    // Already blinded? Refuse.
    if has_effect_named(world, target, "blind") {
        let target_name = name_or(world, target, "<unknown>");
        send_to(world, player, format!("{target_name} is already blinded.\r\n"));
        return;
    }
    let Some(target_room) = world.get::<Located>(target).copied().map(|l| l.0) else {
        send_to(world, player, "Target is in limbo.\r\n");
        return;
    };

    let dmg = world.get::<CombatStats>(player).map_or(1, |c| c.dmg_roll);
    drain_stamina(world, player, GOUGE_COST);

    let player_name = name_of(world, player);
    let target_name = name_or(world, target, "<unknown>");
    let (dead, _) = apply_damage(world, target, dmg);

    if !dead {
        // Apply the blind effect.
        world.spawn((
            EffectInstance {
                kind: 0,
                name: "blind".to_string(),
                strength: 1,
                remaining_secs: GOUGE_BLIND_SECS,
                source: EffectSource::Other("gouge".to_string()),
                ability_id: None,
            },
            AppliedTo(target),
        ));
    }

    send_to(world, player, format!(
        "You gouge {target_name}'s eyes for {dmg} damage!\r\n"
    ));
    if !dead {
        send_rendered(world, target, &format!(
            "{player_name} gouges your eyes; you can't see!\r\n"
        ));
    }
    broadcast_room_except_rendered(
        world,
        target_room,
        &[player, target],
        &format!("{player_name} stabs at {target_name}'s eyes!\r\n"),
    );

    if dead {
        crate::combat::handle_death(world, target, &target_name, target_room);
    }
}

/// `springleap <target>` — Phase C migration: shimmed over the
/// `SPRINGLEAP` data path (damage formula `"skill"`). Out-of-combat
/// engagement opener: rejects when caster is already fighting or
/// when target is already engaged. After dispatching the data
/// effect, manually engages Fighting on both sides so subsequent
/// combat ticks fire (the data path doesn't auto-engage targets).
fn cmd_springleap(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "springleap") {
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't springleap while already fighting.\r\n");
        return;
    }
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Springleap whom?\r\n");
        return;
    }
    if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        send_to(world, player, "You can't springleap yourself.\r\n");
        return;
    }
    // Resolve the target up front so we can read its Fighting and
    // know the entity for the post-dispatch auto-engage.
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if world.get::<Fighting>(target).is_some() {
        send_to(world, player, "They're already fighting; no surprise.\r\n");
        return;
    }
    if !check_stamina(world, player, SPRINGLEAP_COST, "springleap") {
        return;
    }
    drain_stamina(world, player, SPRINGLEAP_COST);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("springleap {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
    // Auto-engage if the target survived. The data path doesn't model
    // engagement; springleap's gameplay contract is "open combat with
    // a leap kick".
    if world.get_entity(target).is_ok() {
        try_insert(world, player, Fighting(target));
        if world.get::<CombatStats>(target).is_some()
            && let Ok(mut e) = world.get_entity_mut(target)
        {
            e.insert(Fighting(player));
        }
    }
}

/// `throatcut <target>` — Phase C migration: shimmed over the
/// `THROATCUT` data path (damage formula `"skill"`). Out-of-combat
/// opener like springleap; auto-engages on success.
fn cmd_throatcut(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "throatcut") {
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "Your target is already aware of you.\r\n");
        return;
    }
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Throatcut whom?\r\n");
        return;
    }
    if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        send_to(world, player, "You can't throatcut yourself.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if world.get::<Fighting>(target).is_some() {
        send_to(world, player, "They're too alert.\r\n");
        return;
    }
    if !check_stamina(world, player, THROATCUT_COST, "throatcut") {
        return;
    }
    drain_stamina(world, player, THROATCUT_COST);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("throatcut {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
    if world.get_entity(target).is_ok() {
        try_insert(world, player, Fighting(target));
        if world.get::<CombatStats>(target).is_some()
            && let Ok(mut e) = world.get_entity_mut(target)
        {
            e.insert(Fighting(player));
        }
    }
}

/// `backstab <target>` — Phase C migration: shimmed over the
/// `BACKSTAB` data path. Damage formula
/// `weapon_damage * (2 + skill / 25)` resolves now that
/// `FormulaCtx.weapon_damage` is plumbed (b4e166e); needs a
/// piercing weapon equipped to pass the `weapon_type` restriction
/// (logged in SUGGESTIONS — runtime currently passes any rule it
/// can't evaluate). Out-of-combat opener with auto-engage.
fn cmd_backstab(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "backstab") {
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "Your target is already aware of you.\r\n");
        return;
    }
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Backstab whom?\r\n");
        return;
    }
    if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        send_to(world, player, "You can't backstab yourself.\r\n");
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
    if world.get::<Fighting>(target).is_some() {
        send_to(world, player, "They're too alert to backstab.\r\n");
        return;
    }
    if !check_stamina(world, player, BACKSTAB_COST, "backstab") {
        return;
    }
    drain_stamina(world, player, BACKSTAB_COST);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("backstab {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
    if world.get_entity(target).is_ok() {
        try_insert(world, player, Fighting(target));
        if world.get::<CombatStats>(target).is_some()
            && let Ok(mut e) = world.get_entity_mut(target)
        {
            e.insert(Fighting(player));
        }
    }
}

/// `hitall` / `tantrum`: one swing at every Mob in the room. Each
/// hit deals half the player's `dmg_roll`. Engages the first surviving
/// mob if not already fighting. Mobs without Health (the training
/// dummy) are skipped.
fn cmd_hitall(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "hitall") {
        return;
    }
    if !check_stamina(world, player, HITALL_COST, "hitall") {
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;

    let dmg = world
        .get::<CombatStats>(player)
        .map_or(1, |c| (c.dmg_roll / 2).max(1));
    let mob_targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located, Option<&Health>), With<Mob>>();
        q.iter(world)
            .filter(|(_, l, h)| l.0 == room && h.is_some())
            .map(|(e, _, _)| e)
            .collect()
    };
    if mob_targets.is_empty() {
        send_to(world, player, "Nothing here to swing at.\r\n");
        return;
    }
    drain_stamina(world, player, HITALL_COST);

    let player_name = name_of(world, player);
    let already_fighting = world.get::<Fighting>(player).is_some();
    let mut first_alive: Option<Entity> = None;
    let mut hits: Vec<(String, bool)> = Vec::with_capacity(mob_targets.len());
    for target in &mob_targets {
        let target_name = name_or(world, *target, "<unknown>");
        let (dead, _msg) = apply_damage(world, *target, dmg);
        hits.push((target_name.clone(), dead));
        if dead {
            crate::combat::handle_death(world, *target, &target_name, room);
        } else if first_alive.is_none() {
            first_alive = Some(*target);
        }
    }

    // Engage the first survivor if we weren't already fighting.
    if !already_fighting
        && let Some(first) = first_alive
    {
        try_insert(world, player, Fighting(first));
        if world.get::<CombatStats>(first).is_some()
            && let Ok(mut e) = world.get_entity_mut(first)
        {
            e.insert(Fighting(player));
        }
    }

    let total_hits = hits.len();
    let kills = hits.iter().filter(|(_, dead)| *dead).count();
    send_to(
        world,
        player,
        format!(
            "You swing wildly: {total_hits} hit(s), {kills} kill(s) for {dmg} damage each.\r\n",
        ),
    );
    broadcast_room_except_rendered(
        world,
        room,
        &[player],
        &format!(
            "{player_name} swings wildly at everyone here.\r\n",
        ),
    );
}

/// `disarm [<target>]`: remove the target's wielded weapon. Default
/// target is the current Fighting target; arg-form accepts any mob/
/// player in the room.
fn cmd_disarm(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "disarm") {
        return;
    }
    if !check_stamina(world, player, DISARM_COST, "disarm") {
        return;
    }
    let arg = args.trim();
    let target = if arg.is_empty() {
        let Some(Fighting(t)) = world.get::<Fighting>(player).copied() else {
            send_to(world, player, "Disarm whom? You aren't fighting.\r\n");
            return;
        };
        t
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        let Some(t) = find_actor_in_room(world, arg, located.0, player) else {
            send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
            return;
        };
        t
    };
    if target == player {
        send_to(world, player, "You can't disarm yourself.\r\n");
        return;
    }

    // Find the target's wielded item.
    let weapon: Option<Entity> = {
        let mut q = world
            .query_filtered::<(Entity, &Located, &EquippedSlot), With<Item>>();
        q.iter(world)
            .find(|(_, l, eq)| l.0 == target && eq.0 == Slot::Wield)
            .map(|(e, _, _)| e)
    };
    let Some(weapon) = weapon else {
        let target_name = name_or(world, target, "<unknown>");
        send_to(world, player, format!("{target_name} isn't wielding anything.\r\n"));
        return;
    };
    let Some(target_room) = world
        .get::<Located>(target)
        .copied()
        .map(|l| l.0)
    else {
        send_to(world, player, "Target is in limbo; can't disarm.\r\n");
        return;
    };
    drain_stamina(world, player, DISARM_COST);

    // Drop weapon: remove EquippedSlot, re-Located to the room.
    if let Ok(mut e) = world.get_entity_mut(weapon) {
        e.remove::<EquippedSlot>();
        e.insert(Located(target_room));
    }
    let weapon_name = name_or(world, weapon, "<weapon>");
    let target_name = name_or(world, target, "<unknown>");
    let player_name = name_of(world, player);
    send_to(world, player, format!(
        "You disarm {target_name}; {weapon_name} clatters to the ground.\r\n"
    ));
    if target != player {
        send_rendered(world, target, &format!(
            "{player_name} disarms you! {weapon_name} clatters to the ground.\r\n"
        ));
    }
    broadcast_room_except_rendered(
        world,
        target_room,
        &[player, target],
        &format!("{player_name} disarms {target_name}; {weapon_name} drops.\r\n"),
    );
}

/// `rescue <player>` — Phase C migration: shimmed over the RESCUE
/// data path. Posture, "already fighting" and stamina-cost gates
/// stay here (the data path doesn't model them yet); target
/// resolution + Fighting-swap + room broadcast all flow through
/// `invoke_ability` (redirect effect-type with `aggro=true`).
/// `guard <player|off>`: insert a `Guarding(target)` component on
/// the player so combat-tick redirects swings against the target
/// onto the guard. Bare `guard` reports the current target.
fn cmd_guard(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        if let Some(g) = world.get::<mud_world::Guarding>(player) {
            let n = name_of(world, g.0);
            send_to(world, player, format!("You are guarding {n}.\r\n"));
        } else {
            send_to(world, player, "You aren't guarding anyone.\r\n");
        }
        return;
    }
    if arg.eq_ignore_ascii_case("off") || arg.eq_ignore_ascii_case("none") {
        let had = world.get::<mud_world::Guarding>(player).is_some();
        try_remove::<mud_world::Guarding>(world, player);
        if had {
            send_to(world, player, "You stop guarding.\r\n");
        } else {
            send_to(world, player, "You aren't guarding anyone.\r\n");
        }
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_rendered(world, player, &format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "You can't guard yourself.\r\n");
        return;
    }
    world
        .entity_mut(player)
        .insert(mud_world::Guarding(target));
    let n = name_of(world, target);
    send_to(world, player, format!("You begin guarding {n}.\r\n"));
    send_rendered(
        world,
        target,
        &format!(
            "{} stands ready to defend you.\r\n",
            name_of(world, player)
        ),
    );
}

fn cmd_rescue(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "rescue") {
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You're already fighting.\r\n");
        return;
    }
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Rescue whom?\r\n");
        return;
    }
    // Self-target shortcut: refuse before draining stamina (the
    // redirect arm in invoke_ability also refuses, but we'd waste
    // the cost otherwise).
    if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        send_to(world, player, "You can't rescue yourself.\r\n");
        return;
    }
    if !check_stamina(world, player, RESCUE_COST, "rescue") {
        return;
    }
    drain_stamina(world, player, RESCUE_COST);
    invoke_ability(
        world,
        player,
        &format!("rescue {arg}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// `assist <player>`: engage whatever your teammate is fighting.
/// Resolves the teammate's `Fighting` target, then forwards to
/// `cmd_attack` with the target's name so we get all the standard
/// engagement bookkeeping (stamina, posture gate, broadcast).
fn cmd_assist(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Assist whom?\r\n");
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You're already fighting.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(ally) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    let Some(Fighting(ally_target)) = world.get::<Fighting>(ally).copied() else {
        let ally_name = name_or(world, ally, "<unknown>");
        send_to(world, player, format!("{ally_name} isn't fighting anyone.\r\n"));
        return;
    };
    if world.get_entity(ally_target).is_err() {
        send_to(world, player, "Their target is already gone.\r\n");
        return;
    }
    let target_name = name_or(world, ally_target, "<unknown>");
    cmd_attack(world, player, &target_name);
}

/// `retreat <direction>`: directional flee. Same combat-disengage
/// + arrival broadcast as `flee`, but you pick the exit. Refused
///   when the direction has no open exit.
fn cmd_retreat(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let Some(dir) = parse_direction(arg) else {
        send_to(world, player, "Retreat which way?\r\n");
        return;
    };
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let from_room = located.0;
    let Some(exits) = world.get::<Exits>(from_room).cloned() else {
        send_to(world, player, "No exits here.\r\n");
        return;
    };
    let Some(ed) = exits.0.get(&dir).copied() else {
        send_to(world, player, format!("No exit {}.\r\n", direction_name(dir)));
        return;
    };
    if ed.state != ExitState::Open {
        send_to(world, player, format!("The exit {} is closed.\r\n", direction_name(dir)));
        return;
    }
    let Some(target) = ed.to else {
        send_to(world, player, "That exit goes nowhere.\r\n");
        return;
    };

    let dir_name = direction_name(dir);
    let mover_name = name_of(world, player);

    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[player],
        &format!("{mover_name} retreats {dir_name}!\r\n"),
    );
    try_remove::<Fighting>(world, player);
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    }
    let arrival_dir = opposite(dir).map_or("nearby".to_string(), |d| {
        format!("the {}", direction_name(d))
    });
    broadcast_room_except_players_rendered(
        world,
        target,
        &[player],
        &format!("{mover_name} retreats here from {arrival_dir}.\r\n"),
    );
    send_to(world, player, format!("You retreat {dir_name}.\r\n"));
    cmd_look(world, player, "");
}

/// `layhands [<target>]`: in-combat self/ally heal (30 HP, 12 stam).
/// Same shape as bandage but works while fighting and heals more.
/// `layhands` / `lay` aliases — Phase C migration: the actual heal
/// logic now lives in the data path (`Ability.LAY_HANDS` →
/// `AbilityEffect` heal effect with formula `level * 2`). This shim
/// preserves the stamina cost and the legacy command names; the rest
/// flows through `invoke_ability`.
fn cmd_layhands(world: &mut World, player: Entity, args: &str) {
    if !check_stamina(world, player, LAYHANDS_COST, "lay hands") {
        return;
    }
    drain_stamina(world, player, LAYHANDS_COST);
    let arg = args.trim();
    let dispatched = if arg.is_empty() {
        String::from("lay_hands")
    } else {
        format!("lay_hands {arg}")
    };
    invoke_ability(
        world,
        player,
        &dispatched,
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// `bandage` — Phase C migration: shimmed over the `BANDAGE`
/// data path. Out-of-combat first aid; staunches bleeds in the
/// shim until the BANDAGE row gets a second `AbilityEffect`
/// mapping for `cleanse(condition=["bleed"])` — see
/// `SUGGESTIONS.md`.
/// Heal amount comes from the data formula `skill / 5`; admin
/// gets 0 HP from that until the formula gains a baseline (also
/// in `SUGGESTIONS.md`) — until then the staunch is the only
/// visible effect for an untrained caster.
/// `tame <target>`: animal-control skill shim. Drains stamina,
/// dispatches via the data path. Combat is NOT engaged on tame —
/// the charmed effect's runtime installs `Follower(player)` on the
/// mob so it joins the caster's group instead.
fn cmd_tame(world: &mut World, player: Entity, args: &str) {
    const TAME_COST: i32 = 4;
    if !require_alert_posture(world, player, "tame") {
        return;
    }
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Tame what?\r\n");
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
    if world.get::<Mob>(target).is_none() {
        send_to(world, player, "You can only tame animals.\r\n");
        return;
    }
    if !check_stamina(world, player, TAME_COST, "tame") {
        return;
    }
    drain_stamina(world, player, TAME_COST);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("tame {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// `drag`: self-cast DRAG skill. Applies the schema's drag effect
/// (`speedPenalty: 0.5`) which the movement code already reads to
/// double stamina cost. v1 is self-target — corpse-dragging needs
/// a corpse system first.
fn cmd_drag(world: &mut World, player: Entity, _args: &str) {
    const DRAG_COST: i32 = 3;
    if !require_alert_posture(world, player, "drag") {
        return;
    }
    if !check_stamina(world, player, DRAG_COST, "drag") {
        return;
    }
    drain_stamina(world, player, DRAG_COST);
    invoke_ability(
        world,
        player,
        "drag",
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// `buck <target>`: dispatches BUCK. Same engage-skill shim shape
/// as `lure`/`corner`. Combat is engaged on use because the
/// knockdown sub-effect is hostile.
fn cmd_buck(world: &mut World, player: Entity, args: &str) {
    engage_skill_shim(world, player, args, "buck", 5);
}

/// `breathe [<target>]`: dragonborn breath weapon shim. Looks up
/// the player's race in a static DRAGONBORN_* → ability-name map;
/// non-dragonborn races refuse with a flavor line. Drains 6 stamina
/// and dispatches via the data path.
fn cmd_breathe(world: &mut World, player: Entity, args: &str) {
    const BREATHE_COST: i32 = 6;
    let race = world
        .get::<Profile>(player)
        .map(|p| p.race.clone())
        .unwrap_or_default();
    let ability_name = match race.as_str() {
        "DRAGONBORN_FIRE" => "breathe_fire",
        "DRAGONBORN_FROST" => "breathe_frost",
        "DRAGONBORN_ACID" => "breathe_acid",
        "DRAGONBORN_GAS" => "breathe_gas",
        "DRAGONBORN_LIGHTNING" => "breathe_lightning",
        _ => {
            send_to(world, player, "You have no breath weapon.\r\n");
            return;
        }
    };
    if !require_alert_posture(world, player, "breathe") {
        return;
    }
    if !check_stamina(world, player, BREATHE_COST, "breathe") {
        return;
    }
    drain_stamina(world, player, BREATHE_COST);
    let arg = args.trim();
    let dispatched = if arg.is_empty() {
        ability_name.to_string()
    } else {
        format!("{ability_name} {arg}")
    };
    invoke_ability(
        world,
        player,
        &dispatched,
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// `lure <target>` / `corner <target>` shared shim: target arg
/// resolves to an actor in the room, drains stamina, dispatches the
/// named skill via the data path, and engages combat (mutual
/// `Fighting`). Used by `cmd_lure` and `cmd_corner` since the only
/// per-skill difference is the ability name.
fn engage_skill_shim(
    world: &mut World,
    player: Entity,
    args: &str,
    skill: &str,
    cost: i32,
) {
    if !require_alert_posture(world, player, skill) {
        return;
    }
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, format!("{} whom?\r\n", capitalize(skill)));
        return;
    }
    if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        send_to(world, player, format!("You can't {skill} yourself.\r\n"));
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
    if !check_stamina(world, player, cost, skill) {
        return;
    }
    drain_stamina(world, player, cost);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("{skill} {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
    if world.get_entity(target).is_ok() {
        try_insert(world, player, Fighting(target));
        if world.get::<CombatStats>(target).is_some()
            && let Ok(mut e) = world.get_entity_mut(target)
        {
            e.insert(Fighting(player));
        }
    }
}

fn cmd_lure(world: &mut World, player: Entity, args: &str) {
    engage_skill_shim(world, player, args, "lure", 4);
}

fn cmd_corner(world: &mut World, player: Entity, args: &str) {
    engage_skill_shim(world, player, args, "corner", 4);
}

/// `sneak`: data-path SNEAK skill shim. Stealth marker installation
/// happens in the status effect-type arm via the runtime wired in
/// 404fa6c.
fn cmd_sneak(world: &mut World, player: Entity, _args: &str) {
    const SNEAK_COST: i32 = 3;
    if !check_stamina(world, player, SNEAK_COST, "sneak") {
        return;
    }
    drain_stamina(world, player, SNEAK_COST);
    invoke_ability(
        world,
        player,
        "sneak",
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// `conceal`: data-path CONCEAL skill shim. Same pattern as sneak;
/// catalog separates the two only by proficiency curve / duration.
fn cmd_conceal(world: &mut World, player: Entity, _args: &str) {
    const CONCEAL_COST: i32 = 4;
    if !check_stamina(world, player, CONCEAL_COST, "conceal") {
        return;
    }
    drain_stamina(world, player, CONCEAL_COST);
    invoke_ability(
        world,
        player,
        "conceal",
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// `firstaid [<target>]`: rogue/scout heal-self skill. Same pattern
/// as `bandage` but dispatches `FIRST_AID` instead of `BANDAGE`; no
/// hardcoded bleed-staunch (`first_aid` only carries the heal
/// effect in the schema).
fn cmd_firstaid(world: &mut World, player: Entity, args: &str) {
    const FIRSTAID_COST: i32 = 4;
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't apply first aid in combat.\r\n");
        return;
    }
    if !check_stamina(world, player, FIRSTAID_COST, "firstaid") {
        return;
    }
    drain_stamina(world, player, FIRSTAID_COST);
    let arg = args.trim();
    let dispatched = if arg.is_empty() {
        String::from("first_aid")
    } else {
        format!("first_aid {arg}")
    };
    invoke_ability(
        world,
        player,
        &dispatched,
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

fn cmd_bandage(world: &mut World, player: Entity, args: &str) {
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't bandage in combat.\r\n");
        return;
    }
    if !check_stamina(world, player, BANDAGE_COST, "bandage") {
        return;
    }
    drain_stamina(world, player, BANDAGE_COST);
    // Resolve target (for the bleed staunch — invoke_ability also
    // resolves it but we need access to call remove_effect_named).
    let arg = args.trim();
    let target = if arg.is_empty()
        || arg.eq_ignore_ascii_case("me")
        || arg.eq_ignore_ascii_case("self")
    {
        Some(player)
    } else if let Some(located) = world.get::<Located>(player).copied() {
        find_actor_in_room(world, arg, located.0, player)
    } else {
        None
    };
    if let Some(t) = target {
        let staunched = remove_effect_named(world, t, "bleed") > 0;
        if staunched {
            send_to(world, player, "Bleeding stops.\r\n");
            if t != player {
                send_rendered(world, t, "Your bleeding stops.\r\n");
            }
        }
    }
    let dispatched = if arg.is_empty() {
        String::from("bandage")
    } else {
        format!("bandage {arg}")
    };
    invoke_ability(
        world,
        player,
        &dispatched,
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// `invite <player>`: send a group invite. Recipient gets a
/// `GroupInvite` component carrying the inviter's entity; their
/// `accept` will install Follower(self) for the sender.
fn cmd_invite(world: &mut World, player: Entity, args: &str) {
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
fn cmd_accept(world: &mut World, player: Entity, _args: &str) {
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
fn cmd_decline(world: &mut World, player: Entity, _args: &str) {
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

/// Find the root of a follow chain — walks `Follower` upward until
/// it hits an entity with no `Follower` component. Returns `start`
/// itself if it's already a root.
pub(crate) fn group_root(world: &World, start: Entity) -> Entity {
    let mut current = start;
    let mut steps = 0;
    while let Some(f) = world.get::<Follower>(current) {
        // Cycle guard — `cmd_follow` rejects cycles, but defend in
        // case data drifts.
        if steps > 32 {
            return start;
        }
        current = f.0;
        steps += 1;
    }
    current
}

/// Walk every entity transitively following `root` (directly or via
/// chain). Includes `root` itself in the returned vec. The order is
/// breadth-first; the leader is always position 0.
pub(crate) fn group_members(world: &mut World, root: Entity) -> Vec<Entity> {
    let mut group = vec![root];
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        let children: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Follower), With<Player>>();
            q.iter(world)
                .filter(|(e, f)| f.0 == parent && !group.contains(e))
                .map(|(e, _)| e)
                .collect()
        };
        for c in &children {
            group.push(*c);
            frontier.push(*c);
        }
    }
    group
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
fn cmd_group(world: &mut World, player: Entity, args: &str) {
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
    let mut out = format!("\r\nGroup ({} members):\r\n", members.len());
    for (i, m) in members.iter().enumerate() {
        let name = name_of(world, *m);
        let role = if i == 0 { "leader" } else { "member" };
        let hp = world
            .get::<Health>(*m)
            .map(|h| format!("HP {}/{}", h.hp, h.max))
            .unwrap_or_default();
        let here = if let (Some(my_room), Some(their_room)) = (
            world.get::<Located>(player).map(|l| l.0),
            world.get::<Located>(*m).map(|l| l.0),
        ) {
            if my_room == their_room { "here" } else { "elsewhere" }
        } else {
            "elsewhere"
        };
        out.push_str(&format!("  [{role:<6}] {name:<20} {hp:<14} ({here})\r\n"));
    }
    send_to(world, player, out);
}

/// Remove one direct follower by name. Used by `group dismiss`. The
/// named player must currently be following `dismisser` (Follower
/// component pointing at them); deeper-chain members can't be
/// dismissed without their direct leader's cooperation.
fn group_dismiss_one(world: &mut World, dismisser: Entity, target_name: &str) {
    if target_name.is_empty() {
        send_to(world, dismisser, "Dismiss whom?\r\n");
        return;
    }
    let needle = target_name.to_ascii_lowercase();
    let target: Option<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Follower, &Named), With<Player>>();
        q.iter(world)
            .find(|(_, f, n)| f.0 == dismisser && n.name.to_ascii_lowercase().contains(&needle))
            .map(|(e, _, _)| e)
    };
    let Some(target) = target else {
        send_to(
            world,
            dismisser,
            format!(
                "Nobody named '{target_name}' is following you. \
                 Use `group` to see who is.\r\n"
            ),
        );
        return;
    };
    let target_name_canonical = name_of(world, target);
    let dismisser_name = name_of(world, dismisser);
    try_remove::<Follower>(world, target);
    send_rendered(
        world,
        dismisser,
        &format!("You dismiss {target_name_canonical} from the group.\r\n"),
    );
    send_rendered(
        world,
        target,
        &format!("{dismisser_name} dismisses you from the group.\r\n"),
    );
}

/// `order <follower|all> <command>`: forwards a command to a mob
/// follower of the caller. Resolves the named mob (must be in the
/// same room and pointing `Follower(player)` at the caller); `all`
/// reaches every same-room mob follower. The mob runs the command
/// via the normal dispatcher — admin gates still apply (mobs only
/// reach Player-level commands).
fn cmd_order(world: &mut World, player: Entity, args: &str) {
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
fn cmd_dismiss(world: &mut World, player: Entity, args: &str) {
    group_dismiss_one(world, player, args.trim());
}

/// `split <amount>`: pull `<amount>` from the caller's `Wealth` and
/// distribute it evenly across every group member currently in the
/// same room (including the caller). Remainder stays with the caller.
fn cmd_split(world: &mut World, player: Entity, args: &str) {
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

/// `gsay <msg>` / `gtell` / `gecho`: broadcast a message to every
/// member of the player's group, regardless of what room they're in.
/// Players outside the group don't see it. Empty group = no-op with
/// helpful message.
fn cmd_gsay(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Group-say what?\r\n");
        return;
    }
    let root = group_root(world, player);
    let members = group_members(world, root);
    if members.len() <= 1 {
        send_to(
            world,
            player,
            "You're not in a group — nobody to say that to.\r\n",
        );
        return;
    }
    let speaker = name_of(world, player);
    for m in members {
        let line = if m == player {
            format!("You group-say, \"{message}\"\r\n")
        } else {
            format!("({speaker} group-says) \"{message}\"\r\n")
        };
        send_rendered(world, m, &line);
    }
}

/// `disband`: clear every direct `Follower(self)` link, breaking the
/// group apart. Members deeper in the chain stay connected to each
/// other unless they too disband. Self has no Follower component to
/// touch — only entities pointing at self.
fn cmd_disband(world: &mut World, player: Entity, _args: &str) {
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

fn cmd_follow(world: &mut World, player: Entity, args: &str) {
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

fn cmd_unfollow(world: &mut World, player: Entity, _args: &str) {
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

/// Walk the Follower chain from `start`. Return true if `end` is reachable
/// (would create a cycle if `end` then started following `start`).
fn would_create_cycle(world: &mut World, start: Entity, end: Entity) -> bool {
    let mut current = start;
    let mut hops = 0;
    while let Some(Follower(next)) = world.get::<Follower>(current).copied() {
        if next == end {
            return true;
        }
        current = next;
        hops += 1;
        if hops > 64 {
            // Defensive: existing cycle somewhere; treat as cycle.
            return true;
        }
    }
    false
}

fn cmd_bash(world: &mut World, player: Entity, target_word: &str) {
    if !require_alert_posture(world, player, "bash") {
        return;
    }
    if !check_stamina(world, player, BASH_COST, "bash") {
        return;
    }
    let target_word = target_word.trim();
    if target_word.is_empty() {
        send_to(world, player, "Bash what?\r\n");
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

    // Engage if not already.
    let already_fighting = world.get::<Fighting>(player).is_some();
    if !already_fighting
        && let Ok(mut e) = world.get_entity_mut(player)
    {
        e.insert(Fighting(target));
    }
    if world.get::<CombatStats>(target).is_some()
        && let Ok(mut e) = world.get_entity_mut(target)
    {
        e.insert(Fighting(player));
    }

    let dmg_roll = world
        .get::<CombatStats>(player)
        .map_or(1, |cs| cs.dmg_roll);
    let damage = (dmg_roll + 3).max(1);
    drain_stamina(world, player, BASH_COST);

    let target_name = name_of(world, target);
    let player_name = name_of(world, player);

    let (dead, threshold_msg) = apply_damage(world, target, damage);

    // Knockdown — set target to Sitting.
    if !dead && let Ok(mut e) = world.get_entity_mut(target) {
        e.insert(Posture(PostureKind::Sitting));
    }

    send_rendered(world, player, &format!("You bash {target_name} for {damage} damage, knocking them down!\r\n"),
    );
    send_rendered(world, target, &format!("{player_name} bashes you for {damage} damage, knocking you down!\r\n"),
    );
    if let Some(m) = threshold_msg {
        send_to(world, target, m);
    }
    broadcast_room_except_rendered(
        world,
        located.0,
        &[player, target],
        &format!("{player_name} bashes {target_name}, knocking them down.\r\n"),
    );

    if dead {
        crate::combat::handle_death(world, target, &target_name, located.0);
    }
}

fn cmd_disengage(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Fighting>(player).is_none() {
        send_to(world, player, "You aren't fighting anyone.\r\n");
        return;
    }
    try_remove::<Fighting>(world, player);
    send_to(world, player, "You stop fighting.\r\n");
}

// ---------------------------------------------------------------------------
// Movement (12 directions, all delegate to cmd_move)
// ---------------------------------------------------------------------------

macro_rules! mv {
    ($name:ident, $dir:ident) => {
        fn $name(world: &mut World, player: Entity, _args: &str) {
            cmd_move(world, player, Direction::$dir);
        }
    };
}
mv!(cmd_north, North);
mv!(cmd_south, South);
mv!(cmd_east, East);
mv!(cmd_west, West);
mv!(cmd_up, Up);
mv!(cmd_down, Down);
mv!(cmd_northeast, Northeast);
mv!(cmd_northwest, Northwest);
mv!(cmd_southeast, Southeast);
mv!(cmd_southwest, Southwest);
mv!(cmd_in, In);
mv!(cmd_out, Out);

// Walk + follower cascade + per-mover notifications + auto-look + stamina
// drain — naturally a long sequence; splitting into helpers would just
// shuffle the order.
#[allow(clippy::too_many_lines)]
fn cmd_move(world: &mut World, player: Entity, dir: Direction) {
    if !require_alert_posture(world, player, "move") {
        return;
    }
    if effect_prevents(world, player, Prevent::Movement) {
        send_to(world, player, "You can't move right now.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let from_room = located.0;

    let exit = world
        .get::<Exits>(from_room)
        .and_then(|e| e.0.get(&dir).copied());
    let Some(exit) = exit else {
        send_to(world, player, "You can't go that way.\r\n");
        return;
    };
    if exit.state != ExitState::Open {
        send_to(world, player, "The way is closed.\r\n");
        return;
    }
    let Some(target) = exit.to else {
        send_to(world, player, "That exit leads nowhere.\r\n");
        return;
    };

    // Stamina pre-flight: cost depends on the target room's sector.
    // Followers along for the ride aren't checked — they go where the leader
    // goes; the leader pays the cost. `Flying` flattens sector cost to
    // 1 but adds a +1 wing-flap charge on top.
    let target_sector = world
        .get::<RoomSector>(target)
        .map_or(Sector::Field, |s| s.0);
    let is_flying = world.get::<mud_world::Flying>(player).is_some();
    let mut stamina_cost = if is_flying {
        1 + 1
    } else {
        sector_movement_cost(target_sector)
    };
    // Drag-effect penalty: doubles movement cost. The schema's
    // `speedPenalty` is 0.5 (half speed = double cost). Spawned by
    // the DRAG skill; effect name is "drag" via the spec.name flow.
    if has_effect_named(world, player, "drag") {
        stamina_cost = stamina_cost.saturating_mul(2);
    }
    if let Some(s) = world.get::<Stamina>(player).copied()
        && s.current < stamina_cost
    {
        send_to(world, player, "You're too exhausted to move.\r\n");
        return;
    }

    // Walk the follower graph rooted at `player`, but only enroll followers
    // who are currently in the same source room — followers in other rooms
    // shouldn't teleport.
    let mut movers: Vec<Entity> = Vec::with_capacity(4);
    movers.push(player);
    let mut idx = 0;
    while idx < movers.len() {
        let leader = movers[idx];
        idx += 1;
        let new_followers: Vec<Entity> = {
            let mut q = world.query::<(Entity, &Located, &Follower)>();
            q.iter(world)
                .filter(|(e, l, f)| {
                    f.0 == leader && l.0 == from_room && !movers.contains(e)
                })
                .map(|(e, _, _)| e)
                .collect()
        };
        for f in new_followers {
            movers.push(f);
        }
    }

    let dir_name = direction_name(dir);
    let arrival_dir = opposite(dir).map_or("nearby".to_string(), |d| {
        format!("the {}", direction_name(d))
    });

    // Notify the source room of each mover departing (in chain order).
    for &mover in &movers {
        let mover_name = name_of(world, mover);
        broadcast_room_except_players_rendered(
            world,
            from_room,
            &movers,
            &format!("{mover_name} leaves {dir_name}.\r\n"),
        );
    }

    // Fire PREENTRY triggers on the destination room before any
    // movers' Located is updated. Bodies can read `actor` to inspect
    // the entering player and emit flavor / gating text.
    for &mover in &movers {
        crate::triggers::fire_room_entry(
            world,
            target,
            mover,
            mud_world::TriggerEvent::Preentry,
        );
    }

    // Move everyone — and any mounts they're riding go with them.
    let mounts: Vec<Entity> = movers
        .iter()
        .filter_map(|m| world.get::<mud_world::Mounted>(*m).map(|x| x.0))
        .collect();
    for &mover in &movers {
        if let Some(mut l) = world.get_mut::<Located>(mover) {
            l.0 = target;
        }
    }
    for mount in mounts {
        if let Some(mut l) = world.get_mut::<Located>(mount) {
            l.0 = target;
        }
    }

    // Drain the leader's stamina by the target sector's cost. Followers
    // don't pay the cost — they're being led.
    if let Some(mut s) = world.get_mut::<Stamina>(player) {
        s.current = (s.current - stamina_cost).max(0);
    }

    // Notify the destination room of arrivals.
    for &mover in &movers {
        let mover_name = name_of(world, mover);
        broadcast_room_except_players_rendered(
            world,
            target,
            &movers,
            &format!("{mover_name} arrives from {arrival_dir}.\r\n"),
        );
    }

    // Each mover sees the new room. Followers also get a "You follow." line
    // before the look.
    for (i, &mover) in movers.iter().enumerate() {
        if i > 0 {
            send_to(world, mover, "You follow.\r\n");
        }
        cmd_look(world, mover, "");
    }

    // Fire GREET / GREET_ALL triggers for every entity in the
    // destination room. Each mover triggers GREET on every existing
    // entity. Done after look so the player sees the room before
    // any scripted reaction text.
    for &mover in &movers {
        crate::triggers::fire_greet_in_room(world, mover, target);
    }

    // Fire POSTENTRY triggers attached to the destination room.
    // `self` = room, `actor` = mover. Bodies typically run delayed
    // flavor (the WORLD-trigger equivalent of "as you arrive...").
    for &mover in &movers {
        crate::triggers::fire_room_entry(
            world,
            target,
            mover,
            mud_world::TriggerEvent::Postentry,
        );
    }
}

// ---------------------------------------------------------------------------
// Admin handlers
// ---------------------------------------------------------------------------

fn cmd_slay(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: slay <mob>\r\n");
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
    if world.get::<Player>(target).is_some() {
        send_to(
            world,
            player,
            "Slaying players is not allowed. Use `restore` if they're in trouble.\r\n",
        );
        return;
    }
    let target_name = name_or(world, target, "<unknown>");

    // Notify the room before death.
    let admin_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        located.0,
        &[player],
        &format!("{admin_name} extends a hand and {target_name} crumbles to dust.\r\n"),
    );
    send_rendered(world, player, &format!("{target_name} crumbles to dust at your gesture.\r\n"),
    );

    // Briefly point the admin at the target so the kill payout's
    // first-Player-attacker walk credits them. handle_death sweeps
    // the Fighting component on the way out.
    try_insert(world, player, Fighting(target));
    crate::combat::handle_death(world, target, &target_name, located.0);
}

/// Builder+ room cleanup. Despawns every Mob and every non-equipped
/// Item directly Located in the player's room (and nested items
/// Located on those mobs go with them implicitly — when a parent is
/// despawned, the orphan handler... well actually `bevy_ecs` doesn't
/// auto-cascade, so we walk and despawn explicitly). Players are
/// never touched.
/// `dumpworld [<path>]`: write a JSON checkpoint of live world state.
/// Doesn't pause the tick — entity values are read in a single pass
/// and serialized into a `serde_json::Value` tree before being
/// written to disk. Default path is `/tmp/world_dump_<unix>.json`.
#[allow(clippy::too_many_lines)]
fn cmd_dumpworld(world: &mut World, player: Entity, args: &str) {
    let path = args.trim();
    let path = if path.is_empty() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("/tmp/world_dump_{stamp}.json")
    } else {
        path.to_string()
    };

    let tick = world.resource::<TickCount>().0;
    let clock = world.resource::<mud_world::MudClock>().clone();

    // Online players roster.
    let players: Vec<serde_json::Value> = {
        let mut q = world.query_filtered::<
            (
                &Named,
                &Account,
                Option<&Profile>,
                &Located,
                Option<&Health>,
                Option<&Stamina>,
                Option<&Wealth>,
            ),
            (With<Player>, With<Online>),
        >();
        q.iter(world)
            .map(|(name, acct, prof, loc, hp, st, wealth)| {
                let room_name = name_or(world, loc.0, "<unknown>");
                let room_key = world
                    .get::<WorldKey>(loc.0)
                    .map_or((-1, -1), |wk| (wk.zone, wk.id));
                serde_json::json!({
                    "name": name.name,
                    "role": acct.role.label(),
                    "level": prof.map_or(0, |p| p.level),
                    "race": prof.map(|p| p.race.clone()).unwrap_or_default(),
                    "room_name": room_name,
                    "room_zone": room_key.0,
                    "room_id": room_key.1,
                    "hp": hp.map_or(0, |h| h.hp),
                    "hp_max": hp.map_or(0, |h| h.max),
                    "stamina": st.map_or(0, |s| s.current),
                    "stamina_max": st.map_or(0, |s| s.max),
                    "wealth_copper": wealth.map_or(0, |w| w.0),
                })
            })
            .collect()
    };

    // Entity counts.
    let mob_count = {
        let mut q = world.query_filtered::<Entity, (With<Mob>, Without<Player>)>();
        q.iter(world).count()
    };
    let item_count = {
        let mut q = world.query::<&Item>();
        q.iter(world).count()
    };
    let effect_count = {
        let mut q = world.query::<&EffectInstance>();
        q.iter(world).count()
    };

    let trigger_catalog = world.resource::<mud_world::TriggerCatalog>();
    let triggers = serde_json::json!({
        "rows": trigger_catalog.by_key.len(),
        "mob_attachments": trigger_catalog.mob_attachments.len(),
        "object_attachments": trigger_catalog.object_attachments.len(),
        "room_attachments": trigger_catalog.room_attachments.len(),
    });

    let payload = serde_json::json!({
        "schema_version": 1,
        "captured_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        "tick": tick,
        "clock": {
            "year": clock.year,
            "month": clock.month,
            "day": clock.day,
            "hour": clock.hour,
            "stamp": clock.stamp,
        },
        "counts": {
            "online_players": players.len(),
            "mobs": mob_count,
            "items": item_count,
            "effect_instances": effect_count,
        },
        "players": players,
        "triggers": triggers,
    });

    let serialized = match serde_json::to_string_pretty(&payload) {
        Ok(s) => s,
        Err(e) => {
            send_to(world, player, format!("Serialization failed: {e}\r\n"));
            return;
        }
    };

    if let Err(e) = std::fs::write(&path, &serialized) {
        send_to(world, player, format!("Write failed ({path}): {e}\r\n"));
        return;
    }

    let bytes = serialized.len();
    let player_count = payload["counts"]["online_players"].as_u64().unwrap_or(0);
    send_to(
        world,
        player,
        format!(
            "World dumped to {path} ({bytes} bytes, {player_count} player(s)).\r\n"
        ),
    );
    info!(path = %path, bytes, "dumpworld checkpoint written");
}

fn cmd_purge(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;

    if !arg.is_empty() {
        // Single-target form: try mobs/items in the room (no players).
        let target = find_actor_in_room(world, arg, room, player)
            .filter(|e| world.get::<Player>(*e).is_none())
            .or_else(|| {
                let mut q = world
                    .query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
                q.iter(world)
                    .find(|(_, l, n, kw)| l.0 == room && matches(&arg.to_ascii_lowercase(), n, *kw))
                    .map(|(e, _, _, _)| e)
            });
        let Some(target) = target else {
            send_to(world, player, format!("No purge-able '{arg}' here.\r\n"));
            return;
        };
        let target_name = name_or(world, target, "<unknown>");
        // Cascade-despawn: anything Located on the target (mob's gear /
        // container contents) goes too.
        let nested: Vec<Entity> = {
            let mut q = world.query::<(Entity, &Located)>();
            q.iter(world).filter(|(_, l)| l.0 == target).map(|(e, _)| e).collect()
        };
        for n in nested {
            if let Ok(e) = world.get_entity_mut(n) {
                e.despawn();
            }
        }
        if let Ok(e) = world.get_entity_mut(target) {
            e.despawn();
        }
        send_rendered(world, player, &format!("You purge {target_name}.\r\n"));
        return;
    }

    // No-arg form: every mob + every item in the room.
    let mobs: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Mob>>();
        q.iter(world).filter(|(_, l)| l.0 == room).map(|(e, _)| e).collect()
    };
    let items: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
        q.iter(world).filter(|(_, l)| l.0 == room).map(|(e, _)| e).collect()
    };
    let mob_count = mobs.len();
    let item_count = items.len();
    // Despawn nested children of mobs first (gear, contents).
    let nested_of_mobs: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located)>();
        q.iter(world)
            .filter(|(_, l)| mobs.contains(&l.0))
            .map(|(e, _)| e)
            .collect()
    };
    let nested_count = nested_of_mobs.len();
    for e in nested_of_mobs.into_iter().chain(mobs.into_iter()).chain(items.into_iter()) {
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
    send_to(
        world,
        player,
        format!(
            "Purged {mob_count} mob(s), {item_count} item(s), and {nested_count} nested.\r\n"
        ),
    );
}

fn cmd_restore(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let target = if arg.is_empty() || arg.eq_ignore_ascii_case("me")
        || arg.eq_ignore_ascii_case("self")
    {
        player
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        let Some(found) = find_actor_in_room(world, arg, located.0, player) else {
            send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
            return;
        };
        found
    };
    if let Some(mut h) = world.get_mut::<Health>(target) {
        h.hp = h.max;
    }
    if let Some(mut s) = world.get_mut::<Stamina>(target) {
        s.current = s.max;
    }
    let target_name = name_or(world, target, "<unknown>");
    if target == player {
        send_to(world, player, "You feel completely refreshed.\r\n");
        return;
    }
    let admin_name = name_of(world, player);
    send_rendered(world, player, &format!("You restore {target_name}.\r\n"));
    send_rendered(world, target, &format!("{admin_name} restores you. You feel completely refreshed.\r\n"),
    );
}

fn cmd_apply(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 || parts.len() > 3 {
        send_to(
            world,
            player,
            "Usage: apply <effect_name> <target> [seconds]\r\n",
        );
        return;
    }
    let effect_name = parts[0];
    let target_word = parts[1];
    let duration_s: i32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);

    let effect_def = world
        .resource::<EffectCatalog>()
        .find_by_name(effect_name)
        .cloned();
    let Some(effect_def) = effect_def else {
        send_to(world, player, format!("Unknown effect: {effect_name}\r\n"));
        return;
    };

    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let target = if target_word.eq_ignore_ascii_case("me")
        || target_word.eq_ignore_ascii_case("self")
    {
        Some(player)
    } else {
        let target_lower = target_word.to_ascii_lowercase();
        let mut q = world.query::<(Entity, &Located, &Named)>();
        q.iter(world)
            .find(|(e, l, n)| {
                *e != player
                    && l.0 == located.0
                    && n.name.to_ascii_lowercase().contains(&target_lower)
            })
            .map(|(e, _, _)| e)
    };
    let Some(target) = target else {
        send_rendered(world, player, &format!("No '{target_word}' here.\r\n"),
        );
        return;
    };

    world.spawn((
        EffectInstance {
            kind: effect_def.id,
            name: effect_def.name.clone(),
            strength: 1,
            remaining_secs: duration_s,
            source: EffectSource::Admin,
            ability_id: None,
        },
        AppliedTo(target),
    ));

    let target_name = name_or(world, target, "<unknown>");
    let dur_label = if duration_s < 0 {
        "permanently".to_string()
    } else {
        format!("for {duration_s}s")
    };
    send_to(
        world,
        player,
        format!(
            "Applied '{}' to {target_name} {dur_label}.\r\n",
            effect_def.name
        ),
    );
    if target != player {
        send_to(
            world,
            target,
            format!("You feel the effect of {}.\r\n", effect_def.name),
        );
    }
}

fn cmd_lua(world: &mut World, player: Entity, args: &str) {
    let code = args.trim();
    if code.is_empty() {
        send_to(world, player, "Usage: lua <code>\r\n");
        return;
    }
    // Take the LuaHost out of the world temporarily so we can borrow
    // both &LuaHost and &mut World at once.
    let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
        host.exec_for_actor(world, player, code)
    });
    drain_lua_outbox(world);
    match result {
        Ok(out) => {
            if out.is_empty() {
                send_to(world, player, "(no output)\r\n");
            } else {
                send_to(world, player, out);
            }
        }
        Err(e) => {
            send_to(world, player, format!("{e}\r\n"));
        }
    }
}

/// `triggers [here|<keyword>]`: list Lua triggers attached to entities
/// in the current room (default) or to a single keyword-resolved
/// target. Read-only diagnostic — does not fire anything.
#[allow(clippy::too_many_lines)]
fn cmd_triggers(world: &mut World, player: Entity, args: &str) {
    use mud_world::TriggerEvent;

    let arg = args.trim();
    let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
        send_to(world, player, "You're nowhere.\r\n");
        return;
    };

    // Targets: room itself + every mob/item/player whose Located == room,
    // unless the user named a specific keyword.
    let mut targets: Vec<Entity> = Vec::new();
    if arg.is_empty() || arg.eq_ignore_ascii_case("here") {
        targets.push(room);
        let mut q = world.query::<(Entity, &Located)>();
        for (e, l) in q.iter(world) {
            if l.0 == room {
                targets.push(e);
            }
        }
    } else if let Some(e) = find_in_room(world, arg, room)
        .or_else(|| find_actor_in_room(world, arg, room, player))
    {
        targets.push(e);
    } else {
        send_to(world, player, format!("No '{arg}' here.\r\n"));
        return;
    }

    let render_event = |ev: &TriggerEvent| match ev {
        TriggerEvent::Global => "GLOBAL",
        TriggerEvent::Random => "RANDOM",
        TriggerEvent::Command => "COMMAND",
        TriggerEvent::Load => "LOAD",
        TriggerEvent::Cast => "CAST",
        TriggerEvent::Leave => "LEAVE",
        TriggerEvent::Time => "TIME",
        TriggerEvent::Speech => "SPEECH",
        TriggerEvent::Act => "ACT",
        TriggerEvent::Death => "DEATH",
        TriggerEvent::Greet => "GREET",
        TriggerEvent::GreetAll => "GREET_ALL",
        TriggerEvent::Entry => "ENTRY",
        TriggerEvent::Receive => "RECEIVE",
        TriggerEvent::Fight => "FIGHT",
        TriggerEvent::HitPercent => "HIT_PERCENT",
        TriggerEvent::Bribe => "BRIBE",
        TriggerEvent::Memory => "MEMORY",
        TriggerEvent::Door => "DOOR",
        TriggerEvent::SpeechTo => "SPEECH_TO",
        TriggerEvent::Look => "LOOK",
        TriggerEvent::Auto => "AUTO",
        TriggerEvent::Attack => "ATTACK",
        TriggerEvent::Defend => "DEFEND",
        TriggerEvent::Timer => "TIMER",
        TriggerEvent::Get => "GET",
        TriggerEvent::Drop => "DROP",
        TriggerEvent::Give => "GIVE",
        TriggerEvent::Wear => "WEAR",
        TriggerEvent::Remove => "REMOVE",
        TriggerEvent::Use => "USE",
        TriggerEvent::Consume => "CONSUME",
        TriggerEvent::Reset => "RESET",
        TriggerEvent::Preentry => "PREENTRY",
        TriggerEvent::Postentry => "POSTENTRY",
    };

    let mut out = String::new();
    let mut total = 0usize;
    for &e in &targets {
        let Some(at) = world.get::<AttachedTriggers>(e) else {
            continue;
        };
        if at.0.is_empty() {
            continue;
        }
        let label = world.get::<Named>(e).map_or("(unnamed)", |n| n.name.as_str());
        let kind = if e == room {
            "room"
        } else if world.get::<Mob>(e).is_some() {
            "mob"
        } else if world.get::<Item>(e).is_some() {
            "item"
        } else if world.get::<Player>(e).is_some() {
            "player"
        } else {
            "entity"
        };
        out.push_str(&format!("{label} [{kind}]:\r\n"));
        let keys = at.0.clone();
        let catalog = world.resource::<TriggerCatalog>();
        for (zone, id) in keys {
            total += 1;
            if let Some(def) = catalog.by_key.get(&(zone, id)) {
                let flags: Vec<&'static str> = def.flags.iter().map(render_event).collect();
                let flag_str = if flags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", flags.join(" "))
                };
                out.push_str(&format!("  ({zone}, {id}) {}{flag_str}\r\n", def.name));
            } else {
                out.push_str(&format!("  ({zone}, {id}) <missing>\r\n"));
            }
        }
    }

    if total == 0 {
        send_to(world, player, "No triggers attached.\r\n");
    } else {
        out.push_str(&format!("{total} trigger(s).\r\n"));
        send_to(world, player, out);
    }
}

/// `firetrig <zone> <id> [<keyword>]`: manually fire a Lua trigger
/// body against an actor. The actor defaults to the player; with a
/// keyword, resolves a mob/item in the current room. Bound via
/// `self` / `actor` in the executed snippet.
fn cmd_firetrig(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        send_to(world, player, "Usage: firetrig <zone> <id> [<keyword>]\r\n");
        return;
    }
    let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
        return;
    };
    let body = world
        .resource::<TriggerCatalog>()
        .by_key
        .get(&(zone, id))
        .map(|d| d.commands.clone());
    let Some(code) = body else {
        send_to(world, player, format!("No trigger ({zone}, {id}) in catalog.\r\n"));
        return;
    };

    let actor = if parts.len() >= 3 {
        let needle = parts[2..].join(" ");
        let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
            send_to(world, player, "You're nowhere.\r\n");
            return;
        };
        let Some(target) = find_in_room(world, &needle, room)
            .or_else(|| find_actor_in_room(world, &needle, room, player))
        else {
            send_to(world, player, format!("No '{needle}' here.\r\n"));
            return;
        };
        target
    } else {
        player
    };

    let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
        host.exec_for_actor(world, actor, &code)
    });
    drain_lua_outbox(world);
    match result {
        Ok(out) => {
            if out.is_empty() {
                send_to(world, player, "(trigger ran, no output)\r\n");
            } else {
                send_to(world, player, out);
            }
        }
        Err(e) => send_to(world, player, format!("{e}\r\n")),
    }
}

/// `zstat [<id>]`: dump zone-level state. Resolves the zone via the
/// player's current room (no arg) or by direct id (one arg).
fn cmd_zstat(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let zone_id = if parts.is_empty() {
        // Resolve via player's room WorldKey.
        let Some(zone) = world
            .get::<Located>(player)
            .and_then(|l| world.get::<WorldKey>(l.0))
            .map(|wk| wk.zone)
        else {
            send_to(world, player, "Can't find your zone.\r\n");
            return;
        };
        zone
    } else if parts.len() == 1 {
        let Ok(id) = parts[0].parse::<i32>() else {
            send_to(world, player, "Usage: zstat [<zone_id>]\r\n");
            return;
        };
        id
    } else {
        send_to(world, player, "Usage: zstat [<zone_id>]\r\n");
        return;
    };

    let Some(zone_entity) = world
        .resource::<WorldKeyIndex>()
        .zones
        .get(&zone_id)
        .copied()
    else {
        send_to(world, player, format!("No zone {zone_id} loaded.\r\n"));
        return;
    };
    let zone_name = name_of(world, zone_entity);
    let room_count = world
        .query_filtered::<&Located, With<mud_world::Room>>()
        .iter(world)
        .filter(|l| l.0 == zone_entity)
        .count();
    let mob_proto_count = world
        .resource::<MobPrototypes>()
        .by_key
        .keys()
        .filter(|(z, _)| *z == zone_id)
        .count();
    let obj_proto_count = world
        .resource::<ObjectPrototypes>()
        .by_key
        .keys()
        .filter(|(z, _)| *z == zone_id)
        .count();
    let live_mobs = world
        .query_filtered::<&WorldKey, With<Mob>>()
        .iter(world)
        .filter(|wk| wk.zone == zone_id)
        .count();
    let live_items = world
        .query_filtered::<&WorldKey, With<Item>>()
        .iter(world)
        .filter(|wk| wk.zone == zone_id)
        .count();

    let mut out = String::from("\r\n");
    out.push_str(&format!("entity:        {zone_entity:?}\r\n"));
    out.push_str(&format!("name:          {zone_name}\r\n"));
    out.push_str(&format!("zone_id:       {zone_id}\r\n"));
    out.push_str(&format!("rooms:         {room_count}\r\n"));
    out.push_str(&format!("mob_protos:    {mob_proto_count}\r\n"));
    out.push_str(&format!("obj_protos:    {obj_proto_count}\r\n"));
    out.push_str(&format!("live_mobs:     {live_mobs}\r\n"));
    out.push_str(&format!("live_items:    {live_items}\r\n"));
    send_to(world, player, out);
}

/// `mstat <zone> <id>`: dump mob prototype + live count.
fn cmd_mstat(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: mstat <zone> <id>\r\n");
        return;
    }
    let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
        return;
    };
    let proto = world
        .resource::<MobPrototypes>()
        .by_key
        .get(&(zone, id))
        .cloned();
    let Some(p) = proto else {
        send_to(world, player, format!("No mob proto ({zone}, {id}).\r\n"));
        return;
    };
    let live = world
        .query_filtered::<&WorldKey, With<Mob>>()
        .iter(world)
        .filter(|wk| wk.zone == zone && wk.id == id)
        .count();
    let trig_count = world
        .resource::<mud_world::TriggerCatalog>()
        .mob_attachments
        .get(&(zone, id))
        .map_or(0, Vec::len);

    let mut out = String::from("\r\n");
    out.push_str(&format!("(zone, id):    ({zone}, {id})\r\n"));
    out.push_str(&format!("name:          {}\r\n", p.name));
    out.push_str(&format!("keywords:      {}\r\n", p.keywords.join(", ")));
    out.push_str(&format!("room_desc:     {}\r\n", p.room_description));
    out.push_str(&format!("level:         {}\r\n", p.level));
    out.push_str(&format!("alignment:     {}\r\n", p.alignment));
    out.push_str(&format!("role:          {:?}\r\n", p.role));
    out.push_str(&format!(
        "hp dice:       {}d{}+{}\r\n",
        p.hp_dice_num, p.hp_dice_size, p.hp_dice_bonus
    ));
    out.push_str(&format!(
        "damage dice:   {}d{}+{}\r\n",
        p.damage_dice_num, p.damage_dice_size, p.damage_dice_bonus
    ));
    out.push_str(&format!("hit_roll:      {}\r\n", p.hit_roll));
    out.push_str(&format!("armor_class:   {}\r\n", p.armor_class));
    out.push_str(&format!("wealth:        {} cp\r\n", p.wealth));
    out.push_str(&format!("class_id:      {:?}\r\n", p.class_id));
    out.push_str(&format!("triggers:      {trig_count}\r\n"));
    out.push_str(&format!("live count:    {live}\r\n"));
    send_to(world, player, out);
}

/// `ostat <zone> <id>`: dump object prototype + live count.
fn cmd_ostat(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: ostat <zone> <id>\r\n");
        return;
    }
    let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
        return;
    };
    let proto = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(zone, id))
        .cloned();
    let Some(p) = proto else {
        send_to(world, player, format!("No object proto ({zone}, {id}).\r\n"));
        return;
    };
    let live = world
        .query_filtered::<&WorldKey, With<Item>>()
        .iter(world)
        .filter(|wk| wk.zone == zone && wk.id == id)
        .count();
    let trig_count = world
        .resource::<mud_world::TriggerCatalog>()
        .object_attachments
        .get(&(zone, id))
        .map_or(0, Vec::len);

    let mut out = String::from("\r\n");
    out.push_str(&format!("(zone, id):    ({zone}, {id})\r\n"));
    out.push_str(&format!("name:          {}\r\n", p.name));
    out.push_str(&format!("keywords:      {}\r\n", p.keywords.join(", ")));
    if let Some(desc) = &p.examine_description {
        out.push_str(&format!("examine:       {desc}\r\n"));
    }
    out.push_str(&format!("type:          {:?}\r\n", p.r#type));
    out.push_str(&format!("wear_flags:    {:?}\r\n", p.wear_flags));
    if let Some(b) = p.board_id {
        out.push_str(&format!("board_id:      {b}\r\n"));
    }
    if let Some(liq) = &p.liquid {
        out.push_str(&format!(
            "liquid:        {} ({}/{}, poisoned={})\r\n",
            liq.liquid, liq.remaining, liq.capacity, liq.poisoned
        ));
    }
    out.push_str(&format!("triggers:      {trig_count}\r\n"));
    out.push_str(&format!("live count:    {live}\r\n"));
    send_to(world, player, out);
}

/// `setweather <climate> [<zone_id>]`: override the climate for a
/// zone. Without a zone arg, mutates the player's current zone.
fn cmd_setweather(world: &mut World, player: Entity, args: &str) {
    use mud_db::enums::Climate;
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        send_to(world, player, "Usage: setweather <climate> [<zone_id>]\r\n");
        return;
    }
    let climate = match parts[0].to_ascii_lowercase().as_str() {
        "none" => Climate::None,
        "semiarid" => Climate::Semiarid,
        "arid" => Climate::Arid,
        "oceanic" => Climate::Oceanic,
        "temperate" => Climate::Temperate,
        "subtropical" => Climate::Subtropical,
        "tropical" => Climate::Tropical,
        "subarctic" => Climate::Subarctic,
        "arctic" => Climate::Arctic,
        "alpine" => Climate::Alpine,
        other => {
            send_to(
                world,
                player,
                format!(
                    "Unknown climate '{other}'. Try: none, semiarid, arid, oceanic, temperate, subtropical, tropical, subarctic, arctic, alpine.\r\n"
                ),
            );
            return;
        }
    };
    let zone_id = if parts.len() >= 2 {
        let Ok(z) = parts[1].parse::<i32>() else {
            send_to(world, player, "zone_id must be an integer.\r\n");
            return;
        };
        z
    } else {
        let Some(zone) = world
            .get::<Located>(player)
            .and_then(|l| world.get::<WorldKey>(l.0))
            .map(|wk| wk.zone)
        else {
            send_to(world, player, "Can't find your zone.\r\n");
            return;
        };
        zone
    };
    let Some(zone_entity) = world
        .resource::<WorldKeyIndex>()
        .zones
        .get(&zone_id)
        .copied()
    else {
        send_to(world, player, format!("No zone {zone_id} loaded.\r\n"));
        return;
    };
    if let Some(mut zc) = world.get_mut::<ZoneClimate>(zone_entity) {
        zc.0 = climate;
    } else {
        world
            .entity_mut(zone_entity)
            .insert(ZoneClimate(climate));
    }
    let zone_name = name_of(world, zone_entity);
    send_to(
        world,
        player,
        format!("Set climate of zone {zone_id} ({zone_name}) to {climate:?}.\r\n"),
    );
}

/// `identify <item>`: dump proto + runtime state for a carried item.
#[allow(clippy::too_many_lines)]
fn cmd_identify(world: &mut World, player: Entity, args: &str) {
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
    let mut out = String::from("\r\n");
    out.push_str(&format!(
        "  Item:      {}\r\n",
        render_color_tags(&p.name, mode)
    ));
    out.push_str(&format!("  Type:      {:?}\r\n", p.r#type));
    out.push_str(&format!("  Weight:    {:.1}\r\n", p.weight));
    out.push_str(&format!("  Level:     {}\r\n", p.level));
    if p.cost > 0
        && let Some(coin) = format_wealth(i64::from(p.cost))
    {
        out.push_str(&format!("  Value:     {coin}\r\n"));
    }
    if !p.wear_flags.is_empty() {
        let labels: Vec<String> = p.wear_flags.iter().map(|f| format!("{f:?}")).collect();
        out.push_str(&format!("  Wear:      {}\r\n", labels.join(", ")));
    }
    if p.weapon_dice_num > 0 {
        out.push_str(&format!(
            "  Damage:    {}d{}+{}\r\n",
            p.weapon_dice_num, p.weapon_dice_size, p.weapon_dice_bonus
        ));
    }
    if let Some(liq) = &p.liquid {
        let state = world.get::<mud_world::LiquidContainer>(item).cloned();
        let (remaining, capacity) =
            state.as_ref().map_or((liq.remaining, liq.capacity), |s| (s.remaining, s.capacity));
        out.push_str(&format!(
            "  Liquid:    {} ({}/{}){}\r\n",
            liq.liquid,
            remaining,
            capacity,
            if state.as_ref().is_some_and(|s| s.poisoned) {
                " — POISONED"
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
        out.push_str("  Bound abilities:\r\n");
        let abilities = world.resource::<AbilityCatalog>();
        for b in bindings {
            let name = abilities
                .by_name
                .values()
                .find(|d| d.id == b.ability_id)
                .map_or_else(|| format!("ability {}", b.ability_id), |d| d.plain_name.clone());
            let charges = b
                .charges
                .map_or_else(|| "unlimited".to_string(), |c| format!("{c} charges"));
            out.push_str(&format!(
                "    - {name} (level {}, {charges})\r\n",
                b.level
            ));
        }
    }
    if let Some(c) = world.get::<mud_world::Charges>(item) {
        out.push_str(&format!("  Charges remaining: {}\r\n", c.0));
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
        out.push_str(&format!("  Effects:   {}\r\n", item_effects.join(", ")));
    }

    send_rendered(world, player, &out);
}

/// `set <target> <field> <value>`: admin mutation of a numeric
/// character stat. v1 supports level / xp / hp / maxhp / stamina /
/// maxstamina / gold / alignment. Session-only — the existing
/// disconnect save handles a subset; full persistence (across all
/// fields) follows when each field's column round-trips.
#[allow(clippy::too_many_lines)]
fn cmd_set(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(3, char::is_whitespace).collect();
    if parts.len() != 3 || parts[1].trim().is_empty() || parts[2].trim().is_empty() {
        send_to(world, player, "Usage: set <target|me> <field> <value>\r\n");
        return;
    }
    let target_word = parts[0].trim();
    let field = parts[1].trim().to_ascii_lowercase();
    let value_word = parts[2].trim();

    let target = if target_word.eq_ignore_ascii_case("me") || target_word == "self" {
        player
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You're nowhere.\r\n");
            return;
        };
        let Some(t) = find_actor_in_room(world, target_word, located.0, player) else {
            send_to(world, player, format!("No '{target_word}' here.\r\n"));
            return;
        };
        t
    };
    let target_name = name_of(world, target);

    // All supported fields are integer-typed for now.
    let Ok(value_i32) = value_word.parse::<i32>() else {
        send_to(world, player, "Value must be an integer.\r\n");
        return;
    };
    let value_i64 = i64::from(value_i32);

    let applied = match field.as_str() {
        "level" => world
            .get_mut::<Profile>(target)
            .map(|mut p| p.level = value_i32.max(1))
            .is_some(),
        "xp" | "exp" | "experience" => world
            .get_mut::<Profile>(target)
            .map(|mut p| p.experience = value_i32.max(0))
            .is_some(),
        "hp" => world
            .get_mut::<Health>(target)
            .map(|mut h| h.hp = value_i32.max(0).min(h.max))
            .is_some(),
        "maxhp" => {
            world
                .get_mut::<Health>(target)
                .map(|mut h| {
                    h.max = value_i32.max(1);
                    h.hp = h.hp.min(h.max);
                })
                .is_some()
        }
        "stamina" | "stam" => world
            .get_mut::<Stamina>(target)
            .map(|mut s| s.current = value_i32.max(0).min(s.max))
            .is_some(),
        "maxstamina" | "maxstam" => {
            world
                .get_mut::<Stamina>(target)
                .map(|mut s| {
                    s.max = value_i32.max(1);
                    s.current = s.current.min(s.max);
                })
                .is_some()
        }
        "gold" | "copper" | "wealth" => {
            if let Some(mut w) = world.get_mut::<Wealth>(target) {
                w.0 = value_i64.max(0);
                true
            } else {
                world.entity_mut(target).insert(Wealth(value_i64.max(0)));
                true
            }
        }
        "alignment" | "align" => world
            .get_mut::<CombatStats>(target)
            .map(|mut c| c.alignment = value_i32)
            .is_some(),
        other => {
            send_to(
                world,
                player,
                format!("Unknown field '{other}'. Try: level, xp, hp, maxhp, stamina, maxstamina, gold, alignment.\r\n"),
            );
            return;
        }
    };
    if applied {
        send_to(
            world,
            player,
            format!("Set {target_name}.{field} = {value_word}.\r\n"),
        );
    } else {
        send_to(
            world,
            player,
            format!("{target_name} has no component for {field}.\r\n"),
        );
    }
}

/// `show <subsystem>`: diagnostic dumps. Builder+ admin tool —
/// the long-form alternative to the per-kind `*stat` commands
/// when the goal is "summarize everything in this category."
#[allow(clippy::too_many_lines)]
fn cmd_show(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim().to_ascii_lowercase();
    let mut out = String::from("\r\n");
    match arg.as_str() {
        "" => {
            out.push_str("Usage: show <subsystem>. Available:\r\n");
            out.push_str("  players   online list with role/level/room\r\n");
            out.push_str("  triggers  catalog totals and per-event tally\r\n");
            out.push_str("  effects   active EffectInstance counts\r\n");
            out.push_str("  clock     MudClock + TickCount\r\n");
            out.push_str("  resets    mob/object reset catalog counts\r\n");
        }
        "players" => {
            let mut rows: Vec<(String, String, i32, String)> = {
                let mut q = world
                    .query_filtered::<(&Named, &Account, Option<&Profile>, &Located), (With<Player>, With<Online>)>();
                q.iter(world)
                    .map(|(n, acct, prof, loc)| {
                        let role = acct.role.label().to_string();
                        let level = prof.map_or(0, |p| p.level);
                        let room = name_or(world, loc.0, "<unknown>");
                        (n.name.clone(), role, level, room)
                    })
                    .collect()
            };
            rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
            out.push_str(&format!("{} player(s) online:\r\n", rows.len()));
            for (name, role, level, room) in &rows {
                out.push_str(&format!(
                    "  {name:<24} L{level:>3} {role:<12} @ {room}\r\n"
                ));
            }
        }
        "triggers" => {
            use mud_world::TriggerEvent;
            let cat = world.resource::<TriggerCatalog>();
            out.push_str(&format!("Trigger catalog: {} rows\r\n", cat.by_key.len()));
            out.push_str(&format!(
                "  mob attachments:    {}\r\n",
                cat.mob_attachments.len()
            ));
            out.push_str(&format!(
                "  object attachments: {}\r\n",
                cat.object_attachments.len()
            ));
            out.push_str(&format!(
                "  room attachments:   {}\r\n",
                cat.room_attachments.len()
            ));
            let mut tally: HashMap<&'static str, usize> = HashMap::new();
            let label = |e: &TriggerEvent| match e {
                TriggerEvent::Global => "GLOBAL",
                TriggerEvent::Random => "RANDOM",
                TriggerEvent::Command => "COMMAND",
                TriggerEvent::Load => "LOAD",
                TriggerEvent::Cast => "CAST",
                TriggerEvent::Leave => "LEAVE",
                TriggerEvent::Time => "TIME",
                TriggerEvent::Speech => "SPEECH",
                TriggerEvent::Act => "ACT",
                TriggerEvent::Death => "DEATH",
                TriggerEvent::Greet => "GREET",
                TriggerEvent::GreetAll => "GREET_ALL",
                TriggerEvent::Entry => "ENTRY",
                TriggerEvent::Receive => "RECEIVE",
                TriggerEvent::Fight => "FIGHT",
                TriggerEvent::HitPercent => "HIT_PERCENT",
                TriggerEvent::Bribe => "BRIBE",
                TriggerEvent::Memory => "MEMORY",
                TriggerEvent::Door => "DOOR",
                TriggerEvent::SpeechTo => "SPEECH_TO",
                TriggerEvent::Look => "LOOK",
                TriggerEvent::Auto => "AUTO",
                TriggerEvent::Attack => "ATTACK",
                TriggerEvent::Defend => "DEFEND",
                TriggerEvent::Timer => "TIMER",
                TriggerEvent::Get => "GET",
                TriggerEvent::Drop => "DROP",
                TriggerEvent::Give => "GIVE",
                TriggerEvent::Wear => "WEAR",
                TriggerEvent::Remove => "REMOVE",
                TriggerEvent::Use => "USE",
                TriggerEvent::Consume => "CONSUME",
                TriggerEvent::Reset => "RESET",
                TriggerEvent::Preentry => "PREENTRY",
                TriggerEvent::Postentry => "POSTENTRY",
            };
            for def in cat.by_key.values() {
                for f in &def.flags {
                    *tally.entry(label(f)).or_insert(0) += 1;
                }
            }
            let mut entries: Vec<(&str, usize)> = tally.into_iter().collect();
            entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            out.push_str("  per-event tally:\r\n");
            for (name, n) in entries {
                out.push_str(&format!("    {name:<14} {n:>5}\r\n"));
            }
        }
        "effects" => {
            let mut tally: HashMap<String, i32> = HashMap::new();
            let mut q = world.query::<&EffectInstance>();
            for e in q.iter(world) {
                *tally.entry(e.name.clone()).or_insert(0) += 1;
            }
            let total: i32 = tally.values().sum();
            out.push_str(&format!("{total} active EffectInstance(s):\r\n"));
            let mut rows: Vec<(String, i32)> = tally.into_iter().collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            for (name, n) in rows.iter().take(20) {
                out.push_str(&format!("  {name:<24} {n:>5}\r\n"));
            }
            if rows.len() > 20 {
                out.push_str(&format!("  ... ({} more)\r\n", rows.len() - 20));
            }
        }
        "clock" => {
            let tick = world.resource::<TickCount>().0;
            let clock = world.resource::<mud_world::MudClock>().clone();
            out.push_str(&format!("Tick:   {tick}\r\n"));
            out.push_str(&format!(
                "Clock:  year {}  month {}  day {}  hour {}\r\n",
                clock.year, clock.month, clock.day, clock.hour
            ));
            out.push_str(&format!("Stamp:  {} (Unix epoch seconds)\r\n", clock.stamp));
        }
        "resets" => {
            use mud_world::{MobResetCatalog, ObjectResetCatalog};
            let mob_count = world.resource::<MobResetCatalog>().entries.len();
            let obj_count = world.resource::<ObjectResetCatalog>().entries.len();
            out.push_str(&format!("Mob reset rows:    {mob_count}\r\n"));
            out.push_str(&format!("Object reset rows: {obj_count}\r\n"));
        }
        other => {
            out.push_str(&format!(
                "Unknown subsystem '{other}'. Try `show` for the list.\r\n"
            ));
        }
    }
    send_to(world, player, out);
}

/// `scripterrors [<n>]`: list the most recent Lua trigger fire
/// failures from the in-memory `ScriptErrorLog`.
fn cmd_scripterrors(world: &mut World, player: Entity, args: &str) {
    use mud_world::ScriptErrorLog;
    let n: usize = args
        .trim()
        .parse()
        .ok()
        .filter(|x: &usize| *x > 0)
        .unwrap_or(20);
    if !world.contains_resource::<ScriptErrorLog>() {
        send_to(world, player, "No trigger errors recorded yet.\r\n");
        return;
    }
    let log = world.resource::<ScriptErrorLog>();
    if log.entries.is_empty() {
        send_to(world, player, "No trigger errors recorded yet.\r\n");
        return;
    }
    let total = log.entries.len();
    let mut out = format!("\r\nLast {} of {total} trigger error(s):\r\n", n.min(total));
    for entry in log.entries.iter().rev().take(n) {
        let secs_ago = std::time::SystemTime::now()
            .duration_since(entry.at)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push_str(&format!(
            "  {ago:>4}s ago  ({zone}, {id}) [{event}] {name}\r\n         {msg}\r\n",
            ago = secs_ago,
            zone = entry.trigger_zone,
            id = entry.trigger_id,
            event = entry.event,
            name = entry.trigger_name,
            msg = entry.message,
        ));
    }
    send_to(world, player, out);
}

/// `syslog [<count>] [<filter>]`: list the most recent tracing log
/// lines captured into the in-memory ring buffer.
fn cmd_syslog(world: &mut World, player: Entity, args: &str) {
    let mut tokens = args.split_whitespace();
    let count: usize = tokens
        .next()
        .and_then(|s| s.parse().ok())
        .map_or(30, |n: usize| n.clamp(1, 500));
    let filter = tokens.next().map(str::to_ascii_uppercase);

    let entries = crate::syslog::snapshot();
    if entries.is_empty() {
        send_to(world, player, "Syslog buffer is empty.\r\n");
        return;
    }

    let matches = |e: &crate::syslog::SyslogEntry| -> bool {
        let Some(f) = filter.as_deref() else { return true };
        e.level.as_str().eq_ignore_ascii_case(f)
            || e.target.to_ascii_uppercase().contains(f)
            || e.message.to_ascii_uppercase().contains(f)
    };

    let mut picked: Vec<&crate::syslog::SyslogEntry> = Vec::new();
    for entry in entries.iter().rev() {
        if matches(entry) {
            picked.push(entry);
            if picked.len() >= count {
                break;
            }
        }
    }
    let total = entries.len();
    let shown = picked.len();
    picked.reverse();

    let mut out = format!("\r\nSyslog: showing {shown} of {total} entry(s)");
    if let Some(f) = filter.as_deref() {
        out.push_str(&format!(" matching '{f}'"));
    }
    out.push_str(":\r\n");
    let now = std::time::SystemTime::now();
    for e in &picked {
        let secs_ago = now.duration_since(e.at).map(|d| d.as_secs()).unwrap_or(0);
        out.push_str(&format!(
            "  {ago:>5}s  {lvl:<5}  {target:<24}  {msg}\r\n",
            ago = secs_ago,
            lvl = e.level.as_str(),
            target = e.target,
            msg = e.message,
        ));
    }
    send_to(world, player, out);
}

/// `astat [<target>]`: detailed effect listing for any character.
fn cmd_astat(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let target = if arg.is_empty() {
        player
    } else {
        let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
            send_to(world, player, "You're nowhere.\r\n");
            return;
        };
        let Some(t) = find_actor_in_room(world, arg, room, player) else {
            send_to(world, player, format!("No '{arg}' here.\r\n"));
            return;
        };
        t
    };
    let target_name = name_of(world, target);
    let active: Vec<(String, i32, i32, mud_world::EffectSource, Option<i32>)> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, a)| a.0 == target)
            .map(|(inst, _)| {
                (
                    inst.name.clone(),
                    inst.remaining_secs,
                    inst.strength,
                    inst.source.clone(),
                    inst.ability_id,
                )
            })
            .collect()
    };
    let mut out = format!("\r\nEffects on {target_name}:\r\n");
    if active.is_empty() {
        out.push_str("  (none)\r\n");
        send_to(world, player, out);
        return;
    }
    let catalog = world.resource::<AbilityCatalog>();
    for (name, remaining, strength, source, ability_id) in active {
        let from = ability_id.and_then(|id| {
            catalog
                .by_name
                .values()
                .find(|d| d.id == id)
                .map(|d| d.plain_name.clone())
        });
        let from_str = from.as_deref().map_or(String::new(), |n| format!(" from {n}"));
        let dur = if remaining < 0 {
            "permanent".to_string()
        } else {
            format!("{remaining}s left")
        };
        out.push_str(&format!(
            "  {name:<20} strength={strength:<3} {dur} source={source:?}{from_str}\r\n"
        ));
    }
    send_to(world, player, out);
}

/// `sstat <zone> <id>`: dump a Shop catalog row.
fn cmd_sstat(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: sstat <zone> <id>\r\n");
        return;
    }
    let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
        return;
    };
    let shop = world.resource::<ShopCatalog>().by_key.get(&(zone, id)).cloned();
    let Some(s) = shop else {
        send_to(world, player, format!("No shop ({zone}, {id}).\r\n"));
        return;
    };
    let mut out = String::from("\r\n");
    out.push_str(&format!("(zone, id):    ({zone}, {id})\r\n"));
    out.push_str(&format!(
        "keeper:        ({}, {})\r\n",
        s.keeper_zone_id, s.keeper_id
    ));
    out.push_str(&format!("buy_profit:    {:.2}\r\n", s.buy_profit));
    out.push_str(&format!("sell_profit:   {:.2}\r\n", s.sell_profit));
    out.push_str(&format!("items:         {}\r\n", s.items.len()));
    for it in s.items.iter().take(20) {
        out.push_str(&format!(
            "  ({}, {}) amount={} price={}\r\n",
            it.object_zone_id, it.object_id, it.amount, it.price
        ));
    }
    if s.items.len() > 20 {
        out.push_str(&format!("  ... ({} more)\r\n", s.items.len() - 20));
    }
    out.push_str(&format!("accepts rules: {}\r\n", s.accepts.len()));
    out.push_str(&format!("pets:          {}\r\n", s.pets.len()));
    send_to(world, player, out);
}

/// `tstat <zone> <id>`: dump a Lua trigger row.
fn cmd_tstat(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: tstat <zone> <id>\r\n");
        return;
    }
    let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
        return;
    };
    let def = world
        .resource::<mud_world::TriggerCatalog>()
        .by_key
        .get(&(zone, id))
        .cloned();
    let Some(d) = def else {
        send_to(world, player, format!("No trigger ({zone}, {id}).\r\n"));
        return;
    };
    let flag_strs: Vec<String> = d.flags.iter().map(|f| format!("{f:?}")).collect();
    let mut out = String::from("\r\n");
    out.push_str(&format!("(zone, id):    ({zone}, {id})\r\n"));
    out.push_str(&format!("name:          {}\r\n", d.name));
    out.push_str(&format!("attach:        {:?}\r\n", d.attach_type));
    out.push_str(&format!("flags:         [{}]\r\n", flag_strs.join(", ")));
    if !d.arg_list.is_empty() {
        out.push_str(&format!("arg_list:      [{}]\r\n", d.arg_list.join(", ")));
    }
    out.push_str(&format!("num_args:      {}\r\n", d.num_args));
    out.push_str("commands:\r\n");
    for line in d.commands.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push_str("\r\n");
    }
    send_to(world, player, out);
}

/// `rstat [zone id]`: dump room components and occupant counts.
/// No-arg form uses the player's current room; two-int form looks
/// the room up via `WorldKeyIndex`. Useful for verifying loader
/// state and catching dangling references.
fn cmd_rstat(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let room = if parts.is_empty() {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        located.0
    } else if parts.len() == 2 {
        let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
            send_to(world, player, "Usage: rstat [<zone_id> <room_id>]\r\n");
            return;
        };
        let Some(found) = world
            .resource::<WorldKeyIndex>()
            .rooms
            .get(&(zone, id))
            .copied()
        else {
            send_to(world, player, format!("No room ({zone}, {id}) loaded.\r\n"));
            return;
        };
        found
    } else {
        send_to(world, player, "Usage: rstat [<zone_id> <room_id>]\r\n");
        return;
    };

    let mut out = String::from("\r\n");
    out.push_str(&format!("entity:        {room:?}\r\n"));
    out.push_str(&format!("name:          {}\r\n", name_of(world, room)));
    if let Some(wk) = world.get::<WorldKey>(room) {
        out.push_str(&format!("world_key:     ({}, {})\r\n", wk.zone, wk.id));
    }
    if let Some(s) = world.get::<RoomSector>(room) {
        out.push_str(&format!("sector:        {:?}\r\n", s.0));
    }
    if let Some(exits) = world.get::<Exits>(room).cloned() {
        if exits.0.is_empty() {
            out.push_str("exits:         <none>\r\n");
        } else {
            out.push_str(&format!("exits:         {} populated\r\n", exits.0.len()));
            for (dir, ed) in &exits.0 {
                let (target_name, target_label) = match ed.to {
                    Some(t) => (name_or(world, t, "<unknown>"), format!("{t:?}")),
                    None => ("<dangling>".to_string(), "None".to_string()),
                };
                out.push_str(&format!(
                    "               {:>9} -> {} ({})\r\n",
                    direction_name(*dir),
                    target_label,
                    target_name,
                ));
            }
        }
    }
    // Occupants: mobs, players, items directly Located in this room.
    let mob_count = world
        .query_filtered::<&Located, With<Mob>>()
        .iter(world)
        .filter(|l| l.0 == room)
        .count();
    let player_count = world
        .query_filtered::<&Located, With<Player>>()
        .iter(world)
        .filter(|l| l.0 == room)
        .count();
    let item_count = world
        .query_filtered::<&Located, With<Item>>()
        .iter(world)
        .filter(|l| l.0 == room)
        .count();
    out.push_str(&format!(
        "occupants:     {mob_count} mob(s), {player_count} player(s), {item_count} item(s)\r\n",
    ));
    send_to(world, player, out);
}

/// `stat <target>`: dump components on a single entity for diagnosis.
/// Resolves the target via the same in-room finder `cmd_examine` uses
/// (or self when arg empty / "me" / "self"). Output is intentionally
/// dense — a Debug-style readout, not for player consumption.
#[allow(clippy::too_many_lines)]
fn cmd_stat(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let target = if arg.is_empty() || arg.eq_ignore_ascii_case("me")
        || arg.eq_ignore_ascii_case("self")
    {
        player
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        // Try actor (mob/player) first, then item (room or carried).
        let needle = arg.to_ascii_lowercase();
        let actor = find_actor_in_room(world, arg, located.0, player);
        let item = actor.or_else(|| {
            let mut q = world
                .query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
            q.iter(world)
                .find(|(_, l, n, kw)| {
                    (l.0 == located.0 || l.0 == player) && matches(&needle, n, *kw)
                })
                .map(|(e, _, _, _)| e)
        });
        let Some(found) = item else {
            send_to(world, player, format!("No '{arg}' here.\r\n"));
            return;
        };
        found
    };

    let mut out = String::from("\r\n");
    out.push_str(&format!("entity:        {target:?}\r\n"));
    out.push_str(&format!("name:          {}\r\n", name_of(world, target)));
    if let Some(wk) = world.get::<WorldKey>(target) {
        out.push_str(&format!("world_key:     ({}, {})\r\n", wk.zone, wk.id));
    }
    if let Some(located) = world.get::<Located>(target) {
        let in_name = name_or(world, located.0, "<unknown>");
        out.push_str(&format!("located_in:    {:?} ({})\r\n", located.0, in_name));
    }
    if let Some(kw) = world.get::<Keywords>(target) {
        out.push_str(&format!("keywords:      {:?}\r\n", kw.0));
    }
    if world.get::<Player>(target).is_some() {
        out.push_str("kind:          Player\r\n");
    } else if world.get::<Mob>(target).is_some() {
        out.push_str("kind:          Mob\r\n");
    } else if world.get::<Item>(target).is_some() {
        out.push_str("kind:          Item\r\n");
        // Resolve through the prototype catalog for weight / level /
        // type. Synthetic seed items lack a WorldKey and so fall
        // through silently.
        if let Some(wk) = world.get::<WorldKey>(target).copied() {
            if let Some(proto) = world
                .resource::<ObjectPrototypes>()
                .by_key
                .get(&(wk.zone, wk.id))
                .cloned()
            {
                out.push_str(&format!(
                    "proto:         weight {:.1}, level {}, type {:?}\r\n",
                    proto.weight, proto.level, proto.r#type,
                ));
            }
            // Bound abilities (scrolls / wands / staves).
            if let Some(abilities) = world
                .resource::<mud_world::ObjectAbilityCatalog>()
                .by_key
                .get(&(wk.zone, wk.id))
                .cloned()
            {
                let catalog = world.resource::<AbilityCatalog>();
                for b in &abilities {
                    let name = catalog
                        .by_name
                        .values()
                        .find(|d| d.id == b.ability_id)
                        .map_or_else(
                            || format!("(id {})", b.ability_id),
                            |d| d.plain_name.clone(),
                        );
                    let ch = b.charges.map_or_else(|| "∞".to_string(), |c| c.to_string());
                    out.push_str(&format!(
                        "ability:       {name} (level {}, charges {ch})\r\n",
                        b.level,
                    ));
                }
            }
        }
    } else {
        out.push_str("kind:          (other)\r\n");
    }
    if let Some(h) = world.get::<Health>(target) {
        out.push_str(&format!("health:        {}/{}\r\n", h.hp, h.max));
    }
    if let Some(s) = world.get::<Stamina>(target) {
        out.push_str(&format!("stamina:       {}/{}\r\n", s.current, s.max));
    }
    if let Some(p) = world.get::<Posture>(target) {
        out.push_str(&format!("posture:       {}\r\n", p.0.label()));
    }
    if let Some(cs) = world.get::<CombatStats>(target) {
        out.push_str(&format!(
            "combat:        hit {} / dmg {} / ac {} / align {}\r\n",
            cs.hit_roll, cs.dmg_roll, cs.ac, cs.alignment
        ));
    }
    if let Some(prof) = world.get::<Profile>(target) {
        let class_label = prof
            .class_id
            .and_then(|id| {
                world
                    .get_resource::<ClassCatalog>()
                    .and_then(|c| c.by_id.get(&id).map(|d| d.plain_name.clone()))
            })
            .unwrap_or_else(|| String::from("(none)"));
        out.push_str(&format!(
            "profile:       L{} {} ({}), xp {}\r\n",
            prof.level, prof.race, class_label, prof.experience,
        ));
    }
    if let Some(f) = world.get::<Fighting>(target) {
        let n = name_or(world, f.0, "<gone>");
        out.push_str(&format!("fighting:      {:?} ({n})\r\n", f.0));
    }
    if let Some(eq) = world.get::<EquippedSlot>(target) {
        out.push_str(&format!("equipped_slot: {}\r\n", eq.0.db_label()));
    }
    if let Some(account) = world.get::<Account>(target) {
        out.push_str(&format!(
            "account:       role={} char_id={}\r\n",
            account.role.label(),
            account.character_id,
        ));
    }
    if let Some(fl) = world.get::<PlayerFlags>(target) {
        let labels: Vec<&'static str> = fl.0.iter().map(|f| f.label()).collect();
        if labels.is_empty() {
            out.push_str("flags:         <none>\r\n");
        } else {
            out.push_str(&format!("flags:         {}\r\n", labels.join(", ")));
        }
    }
    // EffectInstances applied to this entity.
    let effects: Vec<(String, i32)> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, applied)| applied.0 == target)
            .map(|(eff, _)| (eff.name.clone(), eff.remaining_secs))
            .collect()
    };
    if effects.is_empty() {
        out.push_str("effects:       <none>\r\n");
    } else {
        out.push_str(&format!("effects:       {} active\r\n", effects.len()));
        for (name, secs) in &effects {
            out.push_str(&format!("               {name} ({secs}s)\r\n"));
        }
    }
    send_to(world, player, out);
}

/// `loadobj <zone> <id>`: object counterpart to `summon`. Resolves
/// the prototype, spawns a fresh Item in the player's room with the
/// same component bundle the loader's reset pass produces (Item /
/// Named / Keywords / `WorldKey` / `Located`, plus Description when
/// the proto has an examine line).
/// `load <obj|mob> <zone> <id>`: front-end for `loadobj` / `summon`.
/// Splits the type token and forwards the rest of the args verbatim
/// to the existing handlers.
fn cmd_load(world: &mut World, player: Entity, args: &str) {
    let trimmed = args.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let kind = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    if rest.is_empty() {
        send_to(
            world,
            player,
            "Usage: load <obj|mob> <zone> <id>\r\n",
        );
        return;
    }
    match kind.to_ascii_lowercase().as_str() {
        "obj" | "object" | "item" => cmd_loadobj(world, player, rest),
        "mob" | "mobile" | "npc" | "creature" => cmd_summon(world, player, rest),
        other => send_to(
            world,
            player,
            format!("Unknown load type '{other}'. Use obj or mob.\r\n"),
        ),
    }
}

fn cmd_loadobj(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: loadobj <zone_id> <obj_id>\r\n");
        return;
    }
    let Ok(zone) = parts[0].parse::<i32>() else {
        send_to(world, player, "Invalid zone id.\r\n");
        return;
    };
    let Ok(obj_id) = parts[1].parse::<i32>() else {
        send_to(world, player, "Invalid object id.\r\n");
        return;
    };

    let proto = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(zone, obj_id))
        .cloned();
    let Some(proto) = proto else {
        send_to(
            world,
            player,
            format!("No object prototype ({zone}, {obj_id}).\r\n"),
        );
        return;
    };

    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; can't load.\r\n");
        return;
    };
    let room = located.0;
    let proto_name = proto.name.clone();
    let proto_keywords = proto.keywords.clone();
    let examine = proto.examine_description.clone();

    let primary_slot = mud_world::wear_flags_primary_slot(&proto.wear_flags);
    let mut bundle = world.spawn((
        Item,
        Named { name: proto_name.clone() },
        Keywords(proto_keywords),
        WorldKey { zone: proto.zone_id, id: proto.id },
        Located(room),
    ));
    if let Some(desc) = examine {
        bundle.insert(Description(desc));
    }
    if let Some(s) = primary_slot {
        bundle.insert(WearableIn(s));
    }
    if let Some(board_id) = proto.board_id {
        bundle.insert(mud_world::BoardLink(board_id));
    }
    if let Some(liq) = proto.liquid.clone() {
        bundle.insert(mud_world::LiquidContainer {
            liquid: liq.liquid,
            capacity: liq.capacity,
            remaining: liq.remaining,
            poisoned: liq.poisoned,
        });
    }
    let item = bundle.id();
    // Populate Charges from the first ObjectAbilities binding
    // (wands and staves carry finite-use charges in the schema's
    // `charges` column). Items without a binding or without
    // charges set get no Charges component → treated as unlimited.
    if let Some(charges) = world
        .resource::<mud_world::ObjectAbilityCatalog>()
        .by_key
        .get(&(proto.zone_id, proto.id))
        .and_then(|v| v.first().and_then(|b| b.charges))
    {
        crate::commands::try_insert(world, item, mud_world::Charges(charges));
    }

    send_to(
        world,
        player,
        format!(
            "Loaded {proto_name} (entity {item:?}) at your feet.\r\n"
        ),
    );
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} produces {proto_name} from thin air.\r\n"),
    );
}

fn cmd_summon(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: summon <zone_id> <mob_id>\r\n");
        return;
    }
    let Ok(zone) = parts[0].parse::<i32>() else {
        send_to(world, player, "Invalid zone id.\r\n");
        return;
    };
    let Ok(mob_id) = parts[1].parse::<i32>() else {
        send_to(world, player, "Invalid mob id.\r\n");
        return;
    };

    let proto = world
        .resource::<MobPrototypes>()
        .by_key
        .get(&(zone, mob_id))
        .cloned();
    let Some(proto) = proto else {
        send_rendered(world, player, &format!("No mob prototype ({zone}, {mob_id}).\r\n"),
        );
        return;
    };

    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; can't summon.\r\n");
        return;
    };
    let room = located.0;

    let hp = proto.rolled_hp();
    let dmg = proto.avg_damage();
    let proto_name = proto.name.clone();
    let proto_keywords = proto.keywords.clone();
    let proto_room_desc = proto.room_description.clone();
    let proto_alignment = proto.alignment;
    let proto_hit_roll = proto.hit_roll;
    let proto_armor_class = proto.armor_class;

    let mob_entity = world
        .spawn((
            Mob,
            Named { name: proto_name.clone() },
            Keywords(proto_keywords),
            Description(proto_room_desc),
            Located(room),
            Health { hp, max: hp },
            CombatStats {
                hit_roll: proto_hit_roll,
                dmg_roll: dmg,
                ac: proto_armor_class,
                alignment: proto_alignment,
            },
            Posture(PostureKind::Standing),
        ))
        .id();

    send_to(
        world,
        player,
        format!(
            "Summoned {proto_name} (entity {mob_entity:?}) — HP {hp}, dmg {dmg}.\r\n"
        ),
    );
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} summons {proto_name} from thin air.\r\n"),
    );
}

/// `enter <portal>`: walk into a Portal-typed Item in the room. Reads
/// the portal's `Destination` legacy vnum, resolves to a (zone, id) →
/// room entity via `WorldKeyIndex.legacy_vnums`, and teleports. No
/// stamina cost (matches `recall`); refuses while fighting.
#[allow(clippy::too_many_lines)]
fn cmd_enter(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Enter what?\r\n");
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't slip into a portal mid-fight.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let from_room = located.0;
    let lc = needle.to_ascii_lowercase();
    let portal_match = {
        let mut q =
            world.query_filtered::<(Entity, &Located, &Named, Option<&Keywords>, &WorldKey), With<Item>>();
        q.iter(world)
            .find(|(_, l, n, kw, _)| l.0 == from_room && matches(&lc, n, *kw))
            .map(|(e, _, _, _, k)| (e, *k))
    };
    let Some((portal, key)) = portal_match else {
        send_rendered(
            world,
            player,
            &format!("There's no portal called '{needle}' here.\r\n"),
        );
        return;
    };
    let proto = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(key.zone, key.id))
        .cloned();
    let Some(proto) = proto else {
        send_to(world, player, "That portal's prototype is missing.\r\n");
        return;
    };
    if !matches!(proto.r#type, mud_db::enums::ObjectType::Portal) {
        let portal_name = name_of(world, portal);
        send_rendered(
            world,
            player,
            &format!("{portal_name} isn't something you can enter.\r\n"),
        );
        return;
    }
    let Some(vnum) = proto.portal_destination_vnum else {
        send_rendered(
            world,
            player,
            &format!("{} leads nowhere right now.\r\n", proto.name),
        );
        return;
    };
    let dest_key = world
        .resource::<WorldKeyIndex>()
        .legacy_vnums
        .get(&vnum)
        .copied();
    let dest_room = dest_key.and_then(|k| {
        world
            .resource::<WorldKeyIndex>()
            .rooms
            .get(&k)
            .copied()
    });
    let Some(dest) = dest_room else {
        send_rendered(
            world,
            player,
            &format!("{} shimmers, but the destination is gone.\r\n", proto.name),
        );
        return;
    };
    if dest == from_room {
        send_to(world, player, "It would just spit you back out where you are.\r\n");
        return;
    }
    let mover_name = name_of(world, player);
    let portal_name = proto.name.clone();
    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[player],
        &format!("{mover_name} steps into {portal_name} and vanishes.\r\n"),
    );
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = dest;
    }
    broadcast_room_except_players_rendered(
        world,
        dest,
        &[player],
        &format!("{mover_name} steps out of a swirling portal.\r\n"),
    );
    send_rendered(
        world,
        player,
        &format!("You step into {portal_name}...\r\n"),
    );
    cmd_look(world, player, "");
}

fn cmd_recall(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't recall while fighting!\r\n");
        return;
    }
    let Some(target) = world.get::<RecallPoint>(player).map(|r| r.0) else {
        send_to(
            world,
            player,
            "You have no recall point set. Use `setrecall` somewhere to bind one.\r\n",
        );
        return;
    };
    if world.get_entity(target).is_err() {
        send_to(world, player, "Your recall point has vanished.\r\n");
        try_remove::<RecallPoint>(world, player);
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; can't recall.\r\n");
        return;
    };
    let from_room = located.0;
    if from_room == target {
        send_to(world, player, "You're already at your recall point.\r\n");
        return;
    }

    let mover_name = name_of(world, player);

    // Notify source room.
    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[player],
        &format!("{mover_name} fades away in a flash of light.\r\n"),
    );

    let mount = world.get::<mud_world::Mounted>(player).map(|m| m.0);
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    }
    if let Some(mount) = mount
        && let Some(mut l) = world.get_mut::<Located>(mount)
    {
        l.0 = target;
    }

    // Notify destination room.
    broadcast_room_except_players_rendered(
        world,
        target,
        &[player],
        &format!("{mover_name} appears in a flash of light.\r\n"),
    );

    send_to(world, player, "The world swirls around you...\r\n");
    cmd_look(world, player, "");
}

fn cmd_setrecall(world: &mut World, player: Entity, _args: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; can't bind a recall point.\r\n");
        return;
    };
    try_insert(world, player, RecallPoint(located.0));
    let room_name = name_or(world, located.0, "<unknown>");
    send_to(
        world,
        player,
        format!("Recall point bound: {room_name}.\r\n"),
    );
}

fn cmd_freeze(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: freeze <player>\r\n");
        return;
    }
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(arg))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{arg}' isn't online.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "Freezing yourself would be unwise.\r\n");
        return;
    }
    let admin_name = name_of(world, player);
    let target_name = name_of(world, target);
    let was_frozen = world.get::<Frozen>(target).is_some();
    if was_frozen {
        try_remove::<Frozen>(world, target);
        send_rendered(world, player, &format!("You thaw {target_name}.\r\n"));
        send_rendered(
            world,
            target,
            &format!("{admin_name} thaws you. You can move again.\r\n"),
        );
        info!(admin = %admin_name, target = %target_name, action = "thaw", "freeze toggle");
    } else {
        try_insert(world, target, Frozen);
        send_rendered(world, player, &format!("You freeze {target_name}.\r\n"));
        send_to(
            world,
            target,
            format!(
                "{admin_name} freezes you in place. You cannot act until thawed.\r\n"
            ),
        );
        info!(admin = %admin_name, target = %target_name, action = "freeze", "freeze toggle");
    }
}

fn cmd_force(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[1].trim().is_empty() {
        send_to(world, player, "Usage: force <player> <command>\r\n");
        return;
    }
    let target_word = parts[0].trim();
    let cmd_text = parts[1].trim();
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(target_word))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{target_word}' isn't online.\r\n"));
        return;
    };
    let admin_name = name_of(world, player);
    let target_name = name_of(world, target);

    send_rendered(world, player, &format!("You force {target_name} to: {cmd_text}\r\n"),
    );
    send_rendered(world, target, &format!("{admin_name} forces you to: {cmd_text}\r\n"),
    );
    info!(
        admin = %admin_name,
        target = %target_name,
        command = %cmd_text,
        "force"
    );
    dispatch(world, target, cmd_text);
}

fn cmd_transfer(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: transfer <player>\r\n");
        return;
    }
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(arg))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{arg}' isn't online.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "You're already with yourself.\r\n");
        return;
    }
    let Some(dest_loc) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere — can't transfer here.\r\n");
        return;
    };
    let Some(src_loc) = world.get::<Located>(target).copied() else {
        send_to(world, player, "They're nowhere; nothing to transfer from.\r\n");
        return;
    };
    if src_loc.0 == dest_loc.0 {
        send_to(world, player, "They're already in your room.\r\n");
        return;
    }

    let admin_name = name_of(world, player);
    let target_name = name_of(world, target);

    // Source-room bystanders (everyone but the target).
    let src_bystanders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != target && l.0 == src_loc.0)
            .map(|(e, _)| e)
            .collect()
    };
    for b in src_bystanders {
        send_rendered(world, b, &format!("{target_name} vanishes in a puff of smoke.\r\n"),
        );
    }

    // Move the target.
    if let Some(mut l) = world.get_mut::<Located>(target) {
        l.0 = dest_loc.0;
    }

    // Destination-room bystanders (everyone but admin and the just-arrived target).
    let dest_bystanders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != player && *e != target && l.0 == dest_loc.0)
            .map(|(e, _)| e)
            .collect()
    };
    for b in dest_bystanders {
        send_rendered(world, b, &format!("{target_name} appears, summoned by {admin_name}.\r\n"),
        );
    }

    send_rendered(world, player, &format!("You summon {target_name}.\r\n"));
    send_rendered(world, target, &format!("{admin_name} summons you.\r\n"),
    );
    cmd_look(world, target, "");
}

/// `teleport <player> <zone> <room>`: send target to a room. Inverse
/// of `transfer` (target → me) and `goto` (me → room).
fn cmd_teleport(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 3 {
        send_to(
            world,
            player,
            "Usage: teleport <player> <zone> <room>\r\n",
        );
        return;
    }
    let target_word = parts[0];
    let Ok(zone) = parts[1].parse::<i32>() else {
        send_to(world, player, "Zone must be an integer.\r\n");
        return;
    };
    let Ok(room_id) = parts[2].parse::<i32>() else {
        send_to(world, player, "Room id must be an integer.\r\n");
        return;
    };
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(target_word))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{target_word}' isn't online.\r\n"));
        return;
    };
    let dest = world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(zone, room_id))
        .copied();
    let Some(dest) = dest else {
        send_to(world, player, format!("No room ({zone}, {room_id}).\r\n"));
        return;
    };
    let admin_name = name_of(world, player);
    let target_name = name_of(world, target);
    let Some(src_loc) = world.get::<Located>(target).copied() else {
        send_to(world, player, "Target is nowhere.\r\n");
        return;
    };
    if src_loc.0 == dest {
        send_to(world, player, "They're already there.\r\n");
        return;
    }
    let mount = world.get::<mud_world::Mounted>(target).map(|m| m.0);

    let src_bystanders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != target && l.0 == src_loc.0)
            .map(|(e, _)| e)
            .collect()
    };
    for b in src_bystanders {
        send_rendered(
            world,
            b,
            &format!("{target_name} vanishes in a puff of smoke.\r\n"),
        );
    }

    if let Some(mut l) = world.get_mut::<Located>(target) {
        l.0 = dest;
    }
    if let Some(mount) = mount
        && let Some(mut l) = world.get_mut::<Located>(mount)
    {
        l.0 = dest;
    }

    let dest_bystanders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != target && l.0 == dest)
            .map(|(e, _)| e)
            .collect()
    };
    for b in dest_bystanders {
        send_rendered(world, b, &format!("{target_name} arrives in a swirl of light.\r\n"));
    }

    send_rendered(
        world,
        player,
        &format!("You teleport {target_name} to ({zone}, {room_id}).\r\n"),
    );
    send_rendered(
        world,
        target,
        &format!("{admin_name} teleports you elsewhere.\r\n"),
    );
    cmd_look(world, target, "");
}

fn cmd_goto(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: goto <zone_id> <room_id>\r\n");
        return;
    }
    let Ok(zone) = parts[0].parse::<i32>() else {
        send_to(world, player, "Invalid zone id.\r\n");
        return;
    };
    let Ok(room_id) = parts[1].parse::<i32>() else {
        send_to(world, player, "Invalid room id.\r\n");
        return;
    };
    let target = world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(zone, room_id))
        .copied();
    let Some(target) = target else {
        send_to(world, player, format!("No room ({zone}, {room_id}).\r\n"));
        return;
    };
    let mount = world.get::<mud_world::Mounted>(player).map(|m| m.0);
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    }
    // Bring the mount along on goto / recall — otherwise the mount
    // is orphaned in the old room with a stale RiddenBy link.
    if let Some(mount) = mount
        && let Some(mut l) = world.get_mut::<Located>(mount)
    {
        l.0 = target;
    }
    cmd_look(world, player, "");
}

// ---------------------------------------------------------------------------
// Direction name helpers
// ---------------------------------------------------------------------------

/// Stamina drained when moving INTO a room of this sector. The mapping
/// roughly tracks classic CircleMUD/FieryMUD: paved/easy = 1, normal
/// terrain = 2, water/swamp = 3-4, magical/floating planes = 1 (you're
/// not really walking).
fn sector_movement_cost(s: Sector) -> i32 {
    match s {
        // Easy terrain: paved, indoors, level grass; OR magical/floating
        // planes where you're not really walking.
        Sector::Structure
        | Sector::City
        | Sector::Road
        | Sector::Field
        | Sector::Grasslands
        | Sector::Beach
        | Sector::Air
        | Sector::Astralplane
        | Sector::Etherealplane
        | Sector::Airplane
        | Sector::Fireplane
        | Sector::Earthplane
        | Sector::Avernus => 1,
        // Standard wilderness.
        Sector::Forest | Sector::Hills | Sector::Cave | Sector::Ruins | Sector::Underdark => 2,
        // Slogging / difficult.
        Sector::Mountain | Sector::Shallows | Sector::Swamp => 3,
        // Swimming.
        Sector::Water => 4,
        Sector::Underwater => 6,
    }
}

fn direction_name(d: Direction) -> &'static str {
    use Direction::{
        Down, East, In, North, Northeast, Northwest, Out, Portal, South, Southeast, Southwest, Up,
        West,
    };
    match d {
        North => "north",
        South => "south",
        East => "east",
        West => "west",
        Up => "up",
        Down => "down",
        Northeast => "northeast",
        Northwest => "northwest",
        Southeast => "southeast",
        Southwest => "southwest",
        In => "in",
        Out => "out",
        Portal => "portal",
        Direction::None => "<none>",
    }
}

fn opposite(d: Direction) -> Option<Direction> {
    use Direction::{
        Down, East, In, North, Northeast, Northwest, Out, South, Southeast, Southwest, Up, West,
    };
    Some(match d {
        North => South,
        South => North,
        East => West,
        West => East,
        Up => Down,
        Down => Up,
        Northeast => Southwest,
        Southwest => Northeast,
        Northwest => Southeast,
        Southeast => Northwest,
        In => Out,
        Out => In,
        _ => return None,
    })
}
