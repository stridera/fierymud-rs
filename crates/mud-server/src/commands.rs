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
    Account, AppliedTo, CombatStats, Description, EffectCatalog, EffectInstance, EffectSource,
    EquippedSlot, Exits, Fighting, Follower, Frozen, Health, Item, Keywords, LastInputAt,
    LastTeller, Located, LoggedInAt, Mob,
    MobPrototypes, Named, Online, Player, PlayerFlags, Posture, PostureKind, Prompt, RecallPoint,
    RoomSector, Slot, SocialDef, SocialRegistry, Stamina, WearableIn, WorldKey, WorldKeyIndex,
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
        names: &["attack", "kill", "k"],
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
        names: &["bash"],
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
        names: &["follow"],
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
        names: &["recall"],
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
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(LastInputAt(std::time::Instant::now()));
    }
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

/// Strip `FieryMUD` color/markup tags (`<b:yellow>`, `</>`, `<r>`, etc.) so
/// the raw text is readable. Future work: translate to ANSI when the
/// player's client supports it.
pub(crate) fn strip_color_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '<' {
            for c2 in chars.by_ref() {
                if c2 == '>' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        apply_damage, direction_name, format_idle, parse_direction, render_prompt,
        sector_movement_cost, strip_color_tags,
    };
    use bevy_ecs::prelude::*;
    use mud_db::enums::Sector;
    use mud_world::{Health, Stamina};

    #[test]
    fn strip_color_tags_handles_common_patterns() {
        // No tags: identity.
        assert_eq!(strip_color_tags("plain text"), "plain text");
        // Single tag pair.
        assert_eq!(strip_color_tags("<r>red</>"), "red");
        // Nested-looking: just sequential.
        assert_eq!(
            strip_color_tags("<b:yellow>warning:</> watch out"),
            "warning: watch out"
        );
        // Unterminated tag: drains rest of string (acceptable for malformed
        // input).
        assert_eq!(strip_color_tags("hello <b:yellow"), "hello ");
        // Empty tags.
        assert_eq!(strip_color_tags("<>x<>y"), "xy");
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
    let _ = conn.0.send(rendered);
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
        send_to(
            world,
            player,
            format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };

    let name = name_of(world, target);
    let description = world
        .get::<Description>(target)
        .map(|d| d.0.clone())
        .unwrap_or_default();
    let posture = world.get::<Posture>(target).map(|p| p.0);

    let mut out = format!("\r\nYou look at {name}.\r\n");
    if !description.trim().is_empty() {
        out.push_str(&format!("{}\r\n", strip_color_tags(description.trim_end())));
    }
    if let Some(p) = posture
        && p != PostureKind::Standing
    {
        out.push_str(&format!("{name} is {} here.\r\n", p.label()));
    }
    if let Some(hp) = world.get::<Health>(target).copied() {
        let pct = if hp.max > 0 {
            (hp.hp * 100) / hp.max
        } else {
            0
        };
        let condition = match pct {
            i32::MIN..=0 => "is dying",
            1..=15 => "is mortally wounded",
            16..=35 => "is badly hurt",
            36..=60 => "is bleeding",
            61..=85 => "has some scrapes",
            _ => "is in excellent shape",
        };
        out.push_str(&format!("{name} {condition}.\r\n"));
    }
    send_to(world, player, out);
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

    let mut out = String::new();
    out.push_str(&format!("\r\n{room_name}\r\n"));
    // BRIEF flag suppresses the description — name/occupants/exits only.
    // CircleMUD-standard "brief mode".
    if !has_flag(world, player, PlayerFlag::Brief) && !room_desc.trim().is_empty() {
        out.push_str(&format!("{}\r\n", strip_color_tags(room_desc.trim_end())));
    }
    for line in &mob_lines {
        out.push_str(&format!("{}\r\n", strip_color_tags(line)));
    }
    if !other_players.is_empty() {
        out.push_str(&format!("Also here: {}\r\n", other_players.join(", ")));
    }
    if !items.is_empty() {
        out.push_str(&format!("On the ground: {}\r\n", items.join(", ")));
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
    let rows: Vec<(String, bool, Option<u64>)> = {
        let mut q = world.query_filtered::<(
            &Named,
            Option<&PlayerFlags>,
            Option<&LastInputAt>,
        ), (With<Player>, With<Online>)>();
        q.iter(world)
            .map(|(n, f, last)| {
                let afk = f.is_some_and(|pf| pf.has(PlayerFlag::Afk));
                let idle = last.map(|l| l.0.elapsed().as_secs());
                (n.name.clone(), afk, idle)
            })
            .collect()
    };
    let mut out = format!("\r\n{} online:\r\n", rows.len());
    for (name, afk, idle) in &rows {
        out.push_str("  ");
        out.push_str(name);
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

fn cmd_score(world: &mut World, player: Entity, _args: &str) {
    let name = name_of(world, player);
    let hp = world.get::<Health>(player).copied();
    let stamina = world.get::<Stamina>(player).copied();
    let cs = world.get::<CombatStats>(player).copied();
    let fighting = world.get::<Fighting>(player).copied();
    let posture = world.get::<Posture>(player).copied();
    let logged_in = world.get::<LoggedInAt>(player).copied();

    let mut out = format!("\r\n{name}\r\n");
    if let Some(hp) = hp {
        out.push_str(&format!("  HP: {} / {}\r\n", hp.hp, hp.max));
    }
    if let Some(stamina) = stamina {
        out.push_str(&format!("  Stamina: {} / {}\r\n", stamina.current, stamina.max));
    }
    if let Some(cs) = cs {
        out.push_str(&format!(
            "  Hit roll: {}    Damage roll: {}    AC: {}    Alignment: {}\r\n",
            cs.hit_roll, cs.dmg_roll, cs.ac, cs.alignment
        ));
    }
    if let Some(p) = posture {
        out.push_str(&format!("  Posture: {}\r\n", p.0.label()));
    }
    if let Some(l) = logged_in {
        out.push_str(&format!("  Online for: {}\r\n", format_idle(l.0.elapsed().as_secs())));
    }
    if let Some(f) = fighting {
        let target_name = name_or(world, f.0, "<gone>");
        out.push_str(&format!("  Fighting: {target_name}\r\n"));
    }
    let flags: Vec<&'static str> = world
        .get::<PlayerFlags>(player)
        .map(|f| f.0.iter().map(|fl| fl.label()).collect())
        .unwrap_or_default();
    if !flags.is_empty() {
        out.push_str(&format!("  Flags: {}\r\n", flags.join(", ")));
    }
    send_to(world, player, out);
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
        send_to(world, player, format!("{target_name} is already awake.\r\n"));
        return;
    }
    if let Ok(mut e) = world.get_entity_mut(target) {
        e.insert(Posture(PostureKind::Standing));
    }
    let player_name = name_of(world, player);
    send_to(world, player, format!("You wake {target_name}.\r\n"));
    send_to(
        world,
        target,
        format!("{player_name} wakes you up.\r\n"),
    );
    broadcast_room_except_players(
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
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(Posture(new));
    }
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
    broadcast_room_except_players(
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
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(Prompt(template.to_string()));
    }
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
    let desc = world
        .get::<Description>(target_room)
        .map(|d| strip_color_tags(&d.0))
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
    let mut out = if items.is_empty() {
        "\r\nYou are carrying nothing.\r\n".to_string()
    } else {
        format!("\r\nYou are carrying {} item(s):\r\n", items.len())
    };
    for name in &items {
        out.push_str(&format!("  {name}\r\n"));
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

    send_to(world, player, format!("You pick up {item_name}.\r\n"));
    broadcast_room_except(
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
        send_to(
            world,
            player,
            format!("You aren't carrying '{target_word}'.\r\n"),
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

    send_to(world, player, format!("You drop {item_name}.\r\n"));
    broadcast_room_except(
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
        send_to(
            world,
            player,
            format!("You aren't carrying '{item_word}'.\r\n"),
        );
        return;
    };
    let target = find_actor_in_room(world, target_word, room, player);
    let Some(target) = target else {
        send_to(
            world,
            player,
            format!("You don't see '{target_word}' here.\r\n"),
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
    broadcast_room_except(
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
        send_to(world, player, format!("{item_name} can't be worn.\r\n"));
        return;
    };

    if let Some(forced) = force_slot
        && forced != slot
    {
        send_to(
            world,
            player,
            format!("{item_name} can't be wielded.\r\n"),
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
        send_to(
            world,
            player,
            format!("Your {} is already occupied.\r\n", slot.label()),
        );
        return;
    }

    if let Ok(mut e) = world.get_entity_mut(item) {
        e.insert(EquippedSlot(slot));
    }

    let verb = if slot == Slot::Wield { "wield" } else { "wear" };
    send_to(world, player, format!("You {verb} {item_name}.\r\n"));
}

fn cmd_remove(world: &mut World, player: Entity, args: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, "Remove what?\r\n");
        return;
    }
    let item = find_carried_by(world, target_word, player, EquipFilter::Equipped);
    let Some(item) = item else {
        send_to(
            world,
            player,
            format!("You aren't wearing '{target_word}'.\r\n"),
        );
        return;
    };
    let item_name = name_of(world, item);
    if let Ok(mut e) = world.get_entity_mut(item) {
        e.remove::<EquippedSlot>();
    }
    send_to(world, player, format!("You remove {item_name}.\r\n"));
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
    let mut out = String::from("\r\nEquipment:\r\n");
    for (slot, name) in &by_slot {
        out.push_str(&format!("  {:>14}: {}\r\n", slot.label(), name));
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
        send_to(world, target, line);
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
        send_to(
            world,
            player,
            format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    let speaker = name_of(world, player);
    let target_name = name_of(world, target);

    send_to(
        world,
        player,
        format!("You whisper to {target_name}, \"{message}\"\r\n"),
    );
    send_to(
        world,
        target,
        format!("{speaker} whispers to you, \"{message}\"\r\n"),
    );
    broadcast_room_except_players(
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
            send_to(world, player, format!("{s}\r\n"));
        }
        if let Some(line) = social.others_no_arg.as_ref() {
            let s = substitute(line, &actor_name, None);
            broadcast_room_except(world, room, &[player], &format!("{s}\r\n"));
        }
        return;
    }

    // Self-target?
    let self_target = matches_self(&actor_name, target_word);
    if self_target {
        if let Some(line) = social.char_auto.as_ref() {
            let s = substitute(line, &actor_name, Some(&actor_name));
            send_to(world, player, format!("{s}\r\n"));
        }
        if let Some(line) = social.others_auto.as_ref() {
            let s = substitute(line, &actor_name, Some(&actor_name));
            broadcast_room_except(world, room, &[player], &format!("{s}\r\n"));
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
        send_to(world, player, format!("{s}\r\n"));
    }
    if let Some(line) = social.vict_found.as_ref() {
        let s = substitute(line, &actor_name, Some(&target_name));
        send_to(world, target, format!("{s}\r\n"));
    }
    if let Some(line) = social.others_found.as_ref() {
        let s = substitute(line, &actor_name, Some(&target_name));
        broadcast_room_except(world, room, &[player, target], &format!("{s}\r\n"));
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

/// Send `msg` to every entity in `room`, skipping any in `except`. Used by
/// say/whisper/transfer/combat-broadcast etc. — anywhere a "the room sees
/// X happen" line needs to fan out without including the actor(s) who saw
/// the first-person variant.
pub(crate) fn broadcast_room_except(
    world: &mut World,
    room: Entity,
    except: &[Entity],
    msg: &str,
) {
    let targets: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located)>();
        q.iter(world)
            .filter(|(e, l)| l.0 == room && !except.contains(e))
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        send_to(world, t, msg);
    }
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

/// Like `broadcast_room_except`, but only fans out to entities that have
/// the `Player` marker. Used for messages that semantically don't apply
/// to mobs (whisper bystanders, posture announcements, social emotes, etc.)
/// — even though I/O is a no-op for mobs since they have no Connection,
/// keeping the filter narrow keeps `PROMPT_RECIPIENTS` clean.
pub(crate) fn broadcast_room_except_players(
    world: &mut World,
    room: Entity,
    except: &[Entity],
    msg: &str,
) {
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| l.0 == room && !except.contains(e))
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        send_to(world, t, msg);
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
        send_to(world, player, format!("'{target_name}' isn't online.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "You mutter quietly to yourself.\r\n");
        return;
    }
    if has_flag(world, target, PlayerFlag::NoTell) {
        let actual = name_of(world, target);
        send_to(
            world,
            player,
            format!("{actual} is not accepting tells right now.\r\n"),
        );
        return;
    }

    let player_name = name_of(world, player);
    let target_name = name_of(world, target);

    send_to(world, player, format!("You tell {target_name}, \"{message}\"\r\n"));
    if has_flag(world, target, PlayerFlag::Afk) {
        send_to(
            world,
            player,
            format!("({target_name} is AFK and may not respond right away.)\r\n"),
        );
    }
    send_to(world, target, format!("{player_name} tells you, \"{message}\"\r\n"));

    // Stamp the receiver so they can `reply`.
    if let Ok(mut e) = world.get_entity_mut(target) {
        e.insert(LastTeller(player));
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
        if let Ok(mut e) = world.get_entity_mut(a) {
            e.remove::<Fighting>();
        }
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
            if let Ok(mut e) = world.get_entity_mut(player) {
                e.insert(Posture(PostureKind::Standing));
            }
            send_to(world, player, "You stand up.\r\n");
            if let Some(located) = world.get::<Located>(player).copied() {
                let mover_name = name_of(world, player);
                broadcast_room_except_players(
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
        send_to(
            world,
            player,
            format!("You don't see '{target_name}' here.\r\n"),
        );
        return;
    };

    let actual_name = name_of(world, target);
    let player_name = name_of(world, player);

    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(Fighting(target));
    }
    if world.get::<CombatStats>(target).is_some()
        && let Ok(mut e) = world.get_entity_mut(target)
    {
        e.insert(Fighting(player));
    }
    drain_stamina(world, player, ATTACK_COST);

    send_to(world, player, format!("You attack {actual_name}!\r\n"));
    send_to(world, target, format!("{player_name} attacks you!\r\n"));
    broadcast_room_except(
        world,
        located.0,
        &[player, target],
        &format!("{player_name} attacks {actual_name}.\r\n"),
    );
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
        send_to(
            world,
            player,
            format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    let target_name = name_of(world, target);

    let self_max_hp = world.get::<Health>(player).map_or(1, |h| h.max).max(1);
    let self_dmg = world.get::<CombatStats>(player).map_or(0, |c| c.dmg_roll);
    let target_max_hp = world.get::<Health>(target).map_or(0, |h| h.max);
    let target_dmg = world.get::<CombatStats>(target).map_or(0, |c| c.dmg_roll);

    if target_max_hp == 0 {
        send_to(
            world,
            player,
            format!("{target_name} doesn't look like a fighter at all.\r\n"),
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

    send_to(world, player, format!("{target_name} {verdict}\r\n"));
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

    // Notify the room you're fleeing.
    let from_others: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != player && l.0 == from_room)
            .map(|(e, _)| e)
            .collect()
    };
    for o in from_others {
        send_to(world, o, format!("{mover_name} panics and flees {dir_name}!\r\n"));
    }

    // Drop our own Fighting; combat_tick auto-disengages attackers on
    // the next 1Hz pass via the room-mismatch check.
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.remove::<Fighting>();
    }

    // Move + announce arrival + auto-look.
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    }
    let to_others: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != player && l.0 == target)
            .map(|(e, _)| e)
            .collect()
    };
    let arrival_dir = opposite(dir).map_or("nearby".to_string(), |d| {
        format!("the {}", direction_name(d))
    });
    for o in to_others {
        send_to(
            world,
            o,
            format!("{mover_name} arrives, panting, from {arrival_dir}.\r\n"),
        );
    }
    send_to(world, player, format!("You flee {dir_name}!\r\n"));
    cmd_look(world, player, "");
}

fn cmd_kick(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "kick") {
        return;
    }
    if !check_stamina(world, player, KICK_COST, "kick") {
        return;
    }
    let Some(fighting) = world.get::<Fighting>(player).copied() else {
        send_to(world, player, "You aren't fighting anyone.\r\n");
        return;
    };
    let target = fighting.0;
    if world.get_entity(target).is_err() {
        // Target has been despawned; clean up our stale Fighting.
        if let Ok(mut e) = world.get_entity_mut(player) {
            e.remove::<Fighting>();
        }
        send_to(world, player, "Your target is gone.\r\n");
        return;
    }
    let Some(player_room) = world.get::<Located>(player).map(|l| l.0) else {
        return;
    };
    let Some(target_room) = world.get::<Located>(target).map(|l| l.0) else {
        return;
    };
    if player_room != target_room {
        send_to(world, player, "Your target isn't here.\r\n");
        return;
    }

    let dmg_roll = world
        .get::<CombatStats>(player)
        .map_or(1, |cs| cs.dmg_roll);
    let damage = (dmg_roll + 4).max(1);
    drain_stamina(world, player, KICK_COST);

    let target_name = name_of(world, target);
    let player_name = name_of(world, player);

    let (dead, threshold_msg) = apply_damage(world, target, damage);

    send_to(world, player, format!("You kick {target_name} for {damage} damage!\r\n"));
    send_to(world, target, format!("{player_name} kicks you for {damage} damage!\r\n"));
    if let Some(m) = threshold_msg {
        send_to(world, target, m);
    }
    broadcast_room_except(
        world,
        player_room,
        &[player, target],
        &format!("{player_name} kicks {target_name}.\r\n"),
    );

    if dead {
        // Defer to combat::handle_death? It's pub(crate) only inside combat.rs.
        // Simplest: just clear Fighting on attacker; the next combat tick
        // will sweep the orphan. For now just despawn here.
        let is_player = world.get::<Player>(target).is_some();
        if is_player {
            // Revive
            if let Some(mut hp) = world.get_mut::<Health>(target) {
                hp.hp = hp.max;
            }
            if let Ok(mut e) = world.get_entity_mut(target) {
                e.remove::<Fighting>();
            }
            send_to(world, target, "You collapse, then gasp back to life with full health.\r\n");
        } else {
            send_to(world, player, "Your target falls.\r\n");
            // Mob death: let combat_tick clean up via orphan logic on next pass.
            if let Ok(e) = world.get_entity_mut(target) {
                e.despawn();
            }
            // Clear our own Fighting so combat doesn't re-target a despawned entity.
            if let Ok(mut e) = world.get_entity_mut(player) {
                e.remove::<Fighting>();
            }
        }
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

    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(Follower(target));
    }
    let target_name = name_of(world, target);
    let player_name = name_of(world, player);
    send_to(world, player, format!("You start following {target_name}.\r\n"));
    send_to(world, target, format!("{player_name} starts following you.\r\n"));
}

fn cmd_unfollow(world: &mut World, player: Entity, _args: &str) {
    let prev = world.get::<Follower>(player).copied();
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.remove::<Follower>();
    }
    if let Some(Follower(prev_target)) = prev {
        let target_name = name_of(world, prev_target);
        send_to(world, player, format!("You stop following {target_name}.\r\n"));
        let player_name = name_of(world, player);
        send_to(world, prev_target, format!("{player_name} stops following you.\r\n"));
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

    send_to(
        world,
        player,
        format!("You bash {target_name} for {damage} damage, knocking them down!\r\n"),
    );
    send_to(
        world,
        target,
        format!("{player_name} bashes you for {damage} damage, knocking you down!\r\n"),
    );
    if let Some(m) = threshold_msg {
        send_to(world, target, m);
    }
    broadcast_room_except(
        world,
        located.0,
        &[player, target],
        &format!("{player_name} bashes {target_name}, knocking them down.\r\n"),
    );

    if dead {
        let is_player = world.get::<Player>(target).is_some();
        if is_player {
            if let Some(mut hp) = world.get_mut::<Health>(target) {
                hp.hp = hp.max;
            }
            if let Ok(mut e) = world.get_entity_mut(target) {
                e.remove::<Fighting>();
            }
            send_to(world, target, "You collapse, then gasp back to life with full health.\r\n");
        } else {
            send_to(world, player, "Your target falls.\r\n");
            if let Ok(e) = world.get_entity_mut(target) {
                e.despawn();
            }
            if let Ok(mut e) = world.get_entity_mut(player) {
                e.remove::<Fighting>();
            }
        }
    }
}

fn cmd_disengage(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Fighting>(player).is_none() {
        send_to(world, player, "You aren't fighting anyone.\r\n");
        return;
    }
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.remove::<Fighting>();
    }
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
        let from_others: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
            q.iter(world)
                .filter(|(e, l)| !movers.contains(e) && l.0 == from_room)
                .map(|(e, _)| e)
                .collect()
        };
        for o in from_others {
            send_to(world, o, format!("{mover_name} leaves {dir_name}.\r\n"));
        }
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
        let to_others: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
            q.iter(world)
                .filter(|(e, l)| !movers.contains(e) && l.0 == target)
                .map(|(e, _)| e)
                .collect()
        };
        for o in to_others {
            send_to(
                world,
                o,
                format!("{mover_name} arrives from {arrival_dir}.\r\n"),
            );
        }
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
    broadcast_room_except_players(
        world,
        located.0,
        &[player],
        &format!("{admin_name} extends a hand and {target_name} crumbles to dust.\r\n"),
    );
    send_to(
        world,
        player,
        format!("{target_name} crumbles to dust at your gesture.\r\n"),
    );

    if let Ok(e) = world.get_entity_mut(target) {
        e.despawn();
    }
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
    send_to(world, player, format!("You restore {target_name}.\r\n"));
    send_to(
        world,
        target,
        format!("{admin_name} restores you. You feel completely refreshed.\r\n"),
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
        send_to(
            world,
            player,
            format!("No '{target_word}' here.\r\n"),
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
        send_to(
            world,
            player,
            format!("No mob prototype ({zone}, {mob_id}).\r\n"),
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
    broadcast_room_except_players(
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
        if let Ok(mut e) = world.get_entity_mut(player) {
            e.remove::<RecallPoint>();
        }
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
    let from_others: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != player && l.0 == from_room)
            .map(|(e, _)| e)
            .collect()
    };
    for o in from_others {
        send_to(world, o, format!("{mover_name} fades away in a flash of light.\r\n"));
    }

    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    }

    // Notify destination room.
    let to_others: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != player && l.0 == target)
            .map(|(e, _)| e)
            .collect()
    };
    for o in to_others {
        send_to(
            world,
            o,
            format!("{mover_name} appears in a flash of light.\r\n"),
        );
    }

    send_to(world, player, "The world swirls around you...\r\n");
    cmd_look(world, player, "");
}

fn cmd_setrecall(world: &mut World, player: Entity, _args: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; can't bind a recall point.\r\n");
        return;
    };
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(RecallPoint(located.0));
    }
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
        if let Ok(mut e) = world.get_entity_mut(target) {
            e.remove::<Frozen>();
        }
        send_to(world, player, format!("You thaw {target_name}.\r\n"));
        send_to(world, target, format!("{admin_name} thaws you. You can move again.\r\n"));
        info!(admin = %admin_name, target = %target_name, action = "thaw", "freeze toggle");
    } else {
        if let Ok(mut e) = world.get_entity_mut(target) {
            e.insert(Frozen);
        }
        send_to(world, player, format!("You freeze {target_name}.\r\n"));
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

    send_to(
        world,
        player,
        format!("You force {target_name} to: {cmd_text}\r\n"),
    );
    send_to(
        world,
        target,
        format!("{admin_name} forces you to: {cmd_text}\r\n"),
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
        send_to(
            world,
            b,
            format!("{target_name} vanishes in a puff of smoke.\r\n"),
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
        send_to(
            world,
            b,
            format!("{target_name} appears, summoned by {admin_name}.\r\n"),
        );
    }

    send_to(world, player, format!("You summon {target_name}.\r\n"));
    send_to(
        world,
        target,
        format!("{admin_name} summons you.\r\n"),
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
