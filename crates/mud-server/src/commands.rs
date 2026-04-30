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
    AbilityCatalog, Account, AccountSummary, AppliedTo, ClassCatalog, CombatStats, Cooldowns,
    CoreStats, Description, EffectCatalog, EffectInstance, EffectSource, EquippedSlot, ExitData, Exits, Fighting, Follower, Frozen,
    Health, IgnoreList, Item, Keywords, KnownAbilities, LastInputAt, LastTeller, Located, LoggedInAt, Mob,
    MobPrototypes, Named, ObjectPrototypes, Online, Player, PlayerFlags, Posture, PostureKind, Profile, Prompt,
    RecallPoint, RoomSector, Slot, SocialDef, SocialRegistry, Stamina, Stunned, TellLog, Title, UiStyle,
    WearableIn, WorldKey, WorldKeyIndex,
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
            usage: "get <item>",
            summary: "Pick up an item from the room.",
            long: "Match is by case-insensitive substring on the item's \
                   keywords (or its name). The item moves into your \
                   inventory; everyone else in the room sees you pick \
                   it up.",
        },
        run: cmd_get,
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
        names: &["quaff", "drink"],
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
        names: &["world", "users"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "world",
            summary: "Live entity counts: zones, rooms, mobs, items, players.",
            long: "Snapshot of the world state right now: how many zones \
                   and rooms loaded, how many live mobs and items \
                   spawned, how many players online, server tick + \
                   uptime. The aliases `users` mirrors `world` since \
                   player count is the most-asked piece.",
        },
        run: cmd_world,
    },
    Command {
        names: &["time", "uptime"],
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
        names: &["prompt"],
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
        names: &["effects", "affects"],
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
        names: &["rest"],
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

    // Permission gate
    let allowed = world.get::<Account>(player).is_some_and(|a| {
        a.role.at_least(cmd.min_role)
            && cmd.required_perm.is_none_or(|p| a.perms.contains(&p))
    });
    if !allowed {
        send_to(world, player, "You can't do that.\r\n");
        return;
    }

    let span = info_span!("cmd", name = cmd.names[0]);
    let _g = span.enter();
    let args = skip_n_tokens(trimmed, n_consumed);
    (cmd.run)(world, player, args);
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
        let _ = conn.0.send(text.into());
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
        assert_eq!(render_prompt("<%h/%H>", hp, st, name, room), "<42/100> ");
        assert_eq!(render_prompt("<%v/%V mv>", hp, st, name, room), "<7/50 mv> ");
        assert_eq!(
            render_prompt("<%h/%H %v/%V>", hp, st, name, room),
            "<42/100 7/50> "
        );
        // Trailing space already present — don't double-add.
        assert_eq!(render_prompt("<%h> ", hp, st, name, room), "<42> ");
        // Literal percent.
        assert_eq!(render_prompt("100%%", hp, st, name, room), "100% ");
        // Name substitution.
        assert_eq!(render_prompt("[%n]", hp, st, name, room), "[Strider] ");
        // Room substitution.
        assert_eq!(render_prompt("[%r]", hp, st, name, room), "[The Void] ");
        // Unknown variable: pass through literally so the player sees they
        // typed something we don't implement.
        assert_eq!(render_prompt("[%z]", hp, st, name, room), "[%z] ");
        // Missing Health: question marks.
        assert_eq!(render_prompt("<%h/%H>", None, st, name, room), "<?/?> ");
        // Missing Stamina: question marks for v/V.
        assert_eq!(render_prompt("<%v/%V>", hp, None, name, room), "<?/?> ");
        // Missing name: question mark.
        assert_eq!(render_prompt("[%n]", hp, st, None, room), "[?] ");
        // Missing room: question mark.
        assert_eq!(render_prompt("[%r]", hp, st, name, None), "[?] ");
        // Empty template still gets a trailing space.
        assert_eq!(render_prompt("", hp, st, name, room), " ");
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
        // Unrecognized kinds get a free pass — DRAG, PALM, RESURRECT,
        // BUCK shouldn't be blocked while their target categories are
        // unmodeled.
        let valid: Vec<String> = vec!["CORPSE".to_string()];
        assert_eq!(check_target_type(&mut world, caster, target, &valid), None);
        let valid: Vec<String> = vec!["RIDER".to_string(), "OBJECT_INV".to_string()];
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
    let rendered = render_prompt(template, hp, stamina, name, room);
    // Prompts can carry color tags both directly in the template
    // (`prompt <red>%h</>`) and indirectly via %r / %n (room and player
    // names that may have embedded tags). render_color_tags handles
    // both — and is_tag_shaped lets the default `<%h/%H>` survive
    // since `<42/100>` isn't tag-shaped after %-substitution.
    let mode = color_mode_for(world, target);
    let _ = conn.0.send(render_color_tags(&rendered, mode));
}

fn render_prompt(
    template: &str,
    hp: Option<Health>,
    stamina: Option<Stamina>,
    name: Option<&str>,
    room: Option<&str>,
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

    // Self-target.
    if needle == "me" || needle == "self" {
        let name = name_of(world, player);
        send_to(world, player, format!("\r\nYou look at yourself: {name}.\r\n"));
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

fn cmd_who(world: &mut World, player: Entity, _args: &str) {
    let rows: Vec<(String, Option<String>, bool, Option<u64>)> = {
        let mut q = world.query_filtered::<(
            &Named,
            Option<&Title>,
            Option<&PlayerFlags>,
            Option<&LastInputAt>,
        ), (With<Player>, With<Online>)>();
        q.iter(world)
            .map(|(n, t, f, last)| {
                let afk = f.is_some_and(|pf| pf.has(PlayerFlag::Afk));
                let idle = last.map(|l| l.0.elapsed().as_secs());
                (n.name.clone(), t.map(|t| t.0.clone()), afk, idle)
            })
            .collect()
    };
    let mut out = format!("\r\n{} online:\r\n", rows.len());
    for (name, title, afk, idle) in &rows {
        out.push_str("  ");
        out.push_str(name);
        if let Some(t) = title {
            out.push(' ');
            out.push_str(t);
        }
        if *afk {
            out.push_str(" [AFK]");
        }
        if let Some(secs) = idle
            && *secs >= 60
        {
            out.push_str(&format!(" [idle {}]", format_idle(*secs)));
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
                 %V max stamina, %% literal %.\r\n"
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

    let mut out = String::from("\r\n");
    out.push_str(&format!("  Server time: {}\r\n", now.format("%Y-%m-%d %H:%M:%S UTC")));
    out.push_str(&format!("  Uptime:      {h}h {m}m {s}s\r\n"));
    out.push_str(&format!("  World tick:  {tick}\r\n"));
    send_to(world, player, out);
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
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Get what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;

    let item = find_in_room(world, target_word, room);
    let Some(item) = item else {
        send_to(world, player, format!("You don't see '{target_word}' here.\r\n"));
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
    if let Ok(e) = world.get_entity_mut(item) {
        e.despawn();
    }
}

fn cmd_eat(world: &mut World, player: Entity, args: &str) {
    consume_item(world, player, args, mud_db::enums::ObjectType::Food, "eat");
}

fn cmd_quaff(world: &mut World, player: Entity, args: &str) {
    consume_item(world, player, args, mud_db::enums::ObjectType::Potion, "quaff");
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

fn cmd_effects(world: &mut World, player: Entity, _args: &str) {
    let active: Vec<(String, i32)> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, a)| a.0 == player)
            .map(|(inst, _)| (inst.name.clone(), inst.remaining_secs))
            .collect()
    };
    let mut out = if active.is_empty() {
        "\r\nYou have no active effects.\r\n".to_string()
    } else {
        format!("\r\n{} active effect(s):\r\n", active.len())
    };
    for (name, remaining) in active {
        if remaining < 0 {
            out.push_str(&format!("  {name} (permanent)\r\n"));
        } else {
            out.push_str(&format!("  {name} ({remaining}s remaining)\r\n"));
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
                effect_catalog.by_id.get(id).map(|e| EffectSpec {
                    id: *id,
                    name: e.name.clone(),
                    effect_type: e.effect_type.clone(),
                    override_params: override_params.clone(),
                    default_params: e.default_params.clone(),
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
    let mut applied_msgs: Vec<String> = Vec::with_capacity(effect_specs.len());
    let mut spawn_count: usize = 0;
    for spec in &effect_specs {
        match spec.effect_type.as_str() {
            "damage" => {
                // Resolve `amount` from override → default params and
                // apply via `apply_damage`. Most schema formulas use
                // unsupported tokens (`pow`, `weapon_damage`,
                // `str_bonus`, ...) and return None — those casts
                // do 0 damage today and a follow-up extends the
                // formula evaluator. No EffectInstance spawned.
                let amount = resolve_effect_amount(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                    &formula_ctx,
                )
                .unwrap_or(0);
                if amount > 0 {
                    let (dead, _) =
                        crate::commands::apply_damage(world, target_entity, amount);
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
                let dur_secs = resolve_effect_duration(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                    &formula_ctx,
                );
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
                let dur_secs = resolve_effect_duration(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                    &formula_ctx,
                );
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
                let dur_secs = resolve_effect_duration(
                    spec.override_params.as_ref(),
                    Some(&spec.default_params),
                    &formula_ctx,
                );
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

fn cmd_gossip(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Gossip what?\r\n");
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
const BANDAGE_HEAL: i32 = 10;
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
        Some(PostureKind::Sitting | PostureKind::Resting) => {
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

/// `bandage [<target>]`: apply first aid for a small heal. Out-of-
/// combat only. Self-target by default; arg-form finds someone in
/// the same room.
fn cmd_bandage(world: &mut World, player: Entity, args: &str) {
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't bandage in combat.\r\n");
        return;
    }
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
        let Some(t) = find_actor_in_room(world, arg, located.0, player) else {
            send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
            return;
        };
        t
    };
    if !check_stamina(world, player, BANDAGE_COST, "bandage") {
        return;
    }
    let Some(target_hp) = world.get::<Health>(target).copied() else {
        send_to(world, player, "There's nothing to bandage there.\r\n");
        return;
    };
    if target_hp.hp >= target_hp.max {
        let target_name = name_or(world, target, "<unknown>");
        send_to(
            world,
            player,
            if target == player {
                "You're not hurt.\r\n".to_string()
            } else {
                format!("{target_name} isn't hurt.\r\n")
            },
        );
        return;
    }
    drain_stamina(world, player, BANDAGE_COST);
    let new_hp = (target_hp.hp + BANDAGE_HEAL).min(target_hp.max);
    if let Some(mut h) = world.get_mut::<Health>(target) {
        h.hp = new_hp;
    }
    let healed = new_hp - target_hp.hp;
    // Bandage staunches active bleeding as part of the same action.
    let staunched = remove_effect_named(world, target, "bleed") > 0;
    let target_name = name_or(world, target, "<unknown>");
    let bleed_suffix = if staunched { " Bleeding stops." } else { "" };
    if target == player {
        send_to(
            world,
            player,
            format!("You bandage your wounds. (+{healed} HP){bleed_suffix}\r\n"),
        );
    } else {
        send_to(
            world,
            player,
            format!("You bandage {target_name}. (+{healed} HP){bleed_suffix}\r\n"),
        );
        send_rendered(
            world,
            target,
            &format!(
                "{} bandages your wounds. (+{healed} HP){bleed_suffix}\r\n",
                name_of(world, player),
            ),
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
    // goes; the leader pays the cost.
    let target_sector = world
        .get::<RoomSector>(target)
        .map_or(Sector::Field, |s| s.0);
    let stamina_cost = sector_movement_cost(target_sector);
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

    // Move everyone.
    for &mover in &movers {
        if let Some(mut l) = world.get_mut::<Located>(mover) {
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

    // End any combat against this mob — attackers stop swinging.
    disengage_attackers_of(world, target);

    // Notify the room before despawn.
    let admin_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        located.0,
        &[player],
        &format!("{admin_name} extends a hand and {target_name} crumbles to dust.\r\n"),
    );
    send_rendered(world, player, &format!("{target_name} crumbles to dust at your gesture.\r\n"),
    );

    if let Ok(e) = world.get_entity_mut(target) {
        e.despawn();
    }
}

/// Builder+ room cleanup. Despawns every Mob and every non-equipped
/// Item directly Located in the player's room (and nested items
/// Located on those mobs go with them implicitly — when a parent is
/// despawned, the orphan handler... well actually `bevy_ecs` doesn't
/// auto-cascade, so we walk and despawn explicitly). Players are
/// never touched.
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
    let result = world.resource_scope::<mud_script::LuaHost, _>(|world, host| {
        host.exec_for_actor(world, player, code)
    });
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
    let item = bundle.id();

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

    if let Some(mut l) = world.get_mut::<Located>(player) {
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
    if let Some(mut l) = world.get_mut::<Located>(player) {
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
