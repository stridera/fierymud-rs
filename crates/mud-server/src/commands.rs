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
use mud_db::enums::{Direction, ExitState, Permission, UserRole};
use mud_net::Outbound;
use mud_world::{
    Account, AppliedTo, CombatStats, EffectCatalog, EffectInstance, EffectSource, EquippedSlot,
    Exits, Fighting, Health, Item, Keywords, Located, Named, Online, Player, Slot, WearableIn,
    WorldKeyIndex,
};
use tracing::info_span;

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

    let (cmd, n_consumed) = match longest_prefix_match(&tokens) {
        Some(m) => m,
        None => {
            send_to(
                world,
                player,
                format!("Unknown command: {}\r\n", tokens[0]),
            );
            return;
        }
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
}

// ---------------------------------------------------------------------------
// Info handlers
// ---------------------------------------------------------------------------

fn cmd_help(world: &mut World, player: Entity, args: &str) {
    let (role, perms) = world
        .get::<Account>(player)
        .map(|a| (a.role, a.perms.clone()))
        .unwrap_or((UserRole::Player, Vec::new()));

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

fn cmd_look(world: &mut World, player: Entity, _args: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;

    let room_name = world
        .get::<Named>(room)
        .map_or_else(|| "<nowhere>".to_string(), |n| n.name.clone());
    let exits: Vec<Direction> = world
        .get::<Exits>(room)
        .map(|e| e.0.keys().copied().collect())
        .unwrap_or_default();

    // Mobs/players in the room (anything in this room that isn't an Item).
    let others: Vec<String> = {
        let mut q = world.query::<(Entity, &Located, &Named, Option<&Item>)>();
        q.iter(world)
            .filter(|(e, l, _, item)| *e != player && l.0 == room && item.is_none())
            .map(|(_, _, n, _)| n.name.clone())
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
    if !others.is_empty() {
        out.push_str(&format!("Also here: {}\r\n", others.join(", ")));
    }
    if !items.is_empty() {
        out.push_str(&format!("On the ground: {}\r\n", items.join(", ")));
    }
    if exits.is_empty() {
        out.push_str("Exits: none\r\n");
    } else {
        let names: Vec<&str> = exits.iter().map(|d| direction_name(*d)).collect();
        out.push_str(&format!("Exits: {}\r\n", names.join(", ")));
    }
    send_to(world, player, out);
}

fn cmd_who(world: &mut World, player: Entity, _args: &str) {
    let names: Vec<String> = {
        let mut q = world.query_filtered::<&Named, (With<Player>, With<Online>)>();
        q.iter(world).map(|n| n.name.clone()).collect()
    };
    let mut out = format!("\r\n{} online:\r\n", names.len());
    for name in &names {
        out.push_str(&format!("  {name}\r\n"));
    }
    send_to(world, player, out);
}

fn cmd_score(world: &mut World, player: Entity, _args: &str) {
    let name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());
    let hp = world.get::<Health>(player).copied();
    let cs = world.get::<CombatStats>(player).copied();
    let fighting = world.get::<Fighting>(player).copied();

    let mut out = format!("\r\n{name}\r\n");
    if let Some(hp) = hp {
        out.push_str(&format!("  HP: {} / {}\r\n", hp.hp, hp.max));
    }
    if let Some(cs) = cs {
        out.push_str(&format!(
            "  Hit roll: {}    Damage roll: {}    AC: {}    Alignment: {}\r\n",
            cs.hit_roll, cs.dmg_roll, cs.ac, cs.alignment
        ));
    }
    if let Some(f) = fighting {
        let target_name = world
            .get::<Named>(f.0)
            .map_or_else(|| "<gone>".to_string(), |n| n.name.clone());
        out.push_str(&format!("  Fighting: {target_name}\r\n"));
    }
    send_to(world, player, out);
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

fn cmd_where(world: &mut World, player: Entity, _args: &str) {
    let mut rows: Vec<(String, String)> = {
        let mut q = world
            .query_filtered::<(&Named, &Located), (With<Player>, With<Online>)>();
        q.iter(world)
            .map(|(n, l)| {
                let room_name = world
                    .get::<Named>(l.0)
                    .map(|n| n.name.clone())
                    .unwrap_or_else(|| "<unknown>".to_string());
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

    let item_name = world
        .get::<Named>(item)
        .map_or_else(String::new, |n| n.name.clone());
    let player_name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());

    if let Some(mut l) = world.get_mut::<Located>(item) {
        l.0 = player;
    }

    send_to(world, player, format!("You pick up {item_name}.\r\n"));
    let bystanders: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located)>();
        q.iter(world)
            .filter(|(e, l)| *e != player && l.0 == room)
            .map(|(e, _)| e)
            .collect()
    };
    for b in bystanders {
        send_to(
            world,
            b,
            format!("{player_name} picks up {item_name}.\r\n"),
        );
    }
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

    let item_name = world
        .get::<Named>(item)
        .map_or_else(String::new, |n| n.name.clone());
    let player_name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());

    if let Some(mut l) = world.get_mut::<Located>(item) {
        l.0 = room;
    }

    send_to(world, player, format!("You drop {item_name}.\r\n"));
    let bystanders: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located)>();
        q.iter(world)
            .filter(|(e, l)| *e != player && l.0 == room)
            .map(|(e, _)| e)
            .collect()
    };
    for b in bystanders {
        send_to(world, b, format!("{player_name} drops {item_name}.\r\n"));
    }
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

    let item_name = world
        .get::<Named>(item)
        .map_or_else(String::new, |n| n.name.clone());
    let target_name = world
        .get::<Named>(target)
        .map_or_else(String::new, |n| n.name.clone());
    let player_name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());

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
    let bystanders: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located)>();
        q.iter(world)
            .filter(|(e, l)| *e != player && *e != target && l.0 == room)
            .map(|(e, _)| e)
            .collect()
    };
    for b in bystanders {
        send_to(
            world,
            b,
            format!("{player_name} gives {item_name} to {target_name}.\r\n"),
        );
    }
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

    let item_name = world
        .get::<Named>(item)
        .map_or_else(String::new, |n| n.name.clone());

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
    let item_name = world
        .get::<Named>(item)
        .map_or_else(String::new, |n| n.name.clone());
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
    let speaker = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());

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

    let player_name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());
    let target_name = world
        .get::<Named>(target)
        .map_or_else(String::new, |n| n.name.clone());

    send_to(world, player, format!("You tell {target_name}, \"{message}\"\r\n"));
    send_to(world, target, format!("{player_name} tells you, \"{message}\"\r\n"));
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
    let player_name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());
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
    let player_name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());

    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world).collect()
    };
    for t in targets {
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

fn cmd_attack(world: &mut World, player: Entity, target_name: &str) {
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

    let actual_name = world
        .get::<Named>(target)
        .map_or_else(String::new, |n| n.name.clone());
    let player_name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());

    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(Fighting(target));
    }
    if world.get::<CombatStats>(target).is_some()
        && let Ok(mut e) = world.get_entity_mut(target)
    {
        e.insert(Fighting(player));
    }

    send_to(world, player, format!("You attack {actual_name}!\r\n"));
    send_to(world, target, format!("{player_name} attacks you!\r\n"));
    let bystanders: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located)>();
        q.iter(world)
            .filter(|(e, l)| *e != player && *e != target && l.0 == located.0)
            .map(|(e, _)| e)
            .collect()
    };
    for b in bystanders {
        send_to(world, b, format!("{player_name} attacks {actual_name}.\r\n"));
    }
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

fn cmd_move(world: &mut World, player: Entity, dir: Direction) {
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

    let mover_name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());
    let dir_name = direction_name(dir);

    let from_others: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != player && l.0 == from_room)
            .map(|(e, _)| e)
            .collect()
    };
    for o in from_others {
        send_to(world, o, format!("{mover_name} leaves {dir_name}.\r\n"));
    }

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
            format!("{mover_name} arrives from {arrival_dir}.\r\n"),
        );
    }

    cmd_look(world, player, "");
}

// ---------------------------------------------------------------------------
// Admin handlers
// ---------------------------------------------------------------------------

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

    let target_name = world
        .get::<Named>(target)
        .map_or_else(|| "<unknown>".to_string(), |n| n.name.clone());
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
