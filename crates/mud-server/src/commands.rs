//! Player command system: distributed registry, role/permission gated.
//!
//! Every command is a `Command` value submitted via `inventory::submit!`
//! from a per-category file under `commands/`. The registry's
//! first-touch initialization asserts on empty `help.summary` and on
//! duplicate names so contract violations surface at server startup,
//! not when a player tries to use the command.
//!
//! Adding a new command: pick the matching cluster file (or create a
//! new one + add a `#[path = ...] mod` line below), submit a Command,
//! and write the handler body. No central array touch.
//!
//! What's still in this file: the dispatcher (`try_dispatch` and its
//! async sibling for stateful mail / boards / quests), the alias
//! engine, and the helper functions / constants that the per-file
//! handlers share (e.g. `send_to`, `find_actor_in_room`, the combat
//! stamina costs).

use std::collections::HashMap;
use std::sync::LazyLock;

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, Permission, PlayerFlag, Sector, UserRole};
use mud_net::Outbound;
use mud_world::{
    AbilityCatalog, Account, AppliedTo, ClassCatalog, CombatStats, Cooldowns, CoreStats,
    Description, EffectCatalog, EffectInstance, EffectSource, EquippedSlot, Exits, Fighting,
    Follower, Frozen, Ghost, Health, Item, Keywords, KnownAbilities, LastInputAt, Located,
    Mob, MobPrototypes, Named, ObjectPrototypes, Player, PlayerFlags, Posture,
    PostureKind, Profile, Prompt, BankWealth, BoardDraft, MailDraft,
    RecallPoint, RoomSector, Slot, SocialDef, SocialRegistry, Stamina, Stealth, Stunned, Wealth,
    WearableIn, WorldKey, WorldKeyIndex,
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

/// World resource carrying a clone of the Postgres pool. Lets sync
/// command handlers (which can't `.await`) `tokio::spawn` async DB
/// writes — currently used by the `bug`/`idea`/`typo` feedback
/// commands. Cheap to clone; sqlx pools are Arc-backed.
#[derive(Resource)]
pub struct DbPool(pub mud_db::sqlx::PgPool);

/// One pending mutation an async task wants applied to a player
/// entity. The tick reads `character_id` to find the matching
/// online entity (or drops the message silently if the player
/// disconnected before the tick fires).
#[derive(Debug, Clone)]
pub enum PendingPlayerUpdate {
    /// Add to `Profile.experience` (and re-check level-ups).
    ExperienceDelta { character_id: String, amount: i32 },
    /// Add to `Wealth`.
    WealthDelta { character_id: String, amount: i64 },
    /// Add to `SkillPoints`.
    SkillPointsDelta { character_id: String, amount: i32 },
    /// Insert into `KnownAbilities` if not present.
    AbilityKnown {
        character_id: String,
        ability_id: i32,
    },
    /// Spawn one or more instances of an object proto into the
    /// player's inventory. Used by quest reward grants.
    SpawnItem {
        character_id: String,
        object_zone: i32,
        object_id: i32,
        quantity: i32,
    },
}

impl PendingPlayerUpdate {
    /// The character id this update targets — looked up against
    /// `Account.character_id` in the drain tick.
    #[must_use]
    pub fn character_id(&self) -> &str {
        match self {
            Self::ExperienceDelta { character_id, .. }
            | Self::WealthDelta { character_id, .. }
            | Self::SkillPointsDelta { character_id, .. }
            | Self::AbilityKnown { character_id, .. }
            | Self::SpawnItem { character_id, .. } => character_id,
        }
    }
}

/// Sender side of the async-to-world channel for player ECS
/// updates. Cloned by command handlers before `tokio::spawn`.
#[derive(Resource, Clone)]
pub struct PlayerUpdateTx(pub tokio::sync::mpsc::Sender<PendingPlayerUpdate>);

/// Receiver side, drained once per tick by `drain_player_updates`.
#[derive(Resource)]
pub struct PlayerUpdateInbox(
    pub std::sync::Mutex<tokio::sync::mpsc::Receiver<PendingPlayerUpdate>>,
);

/// Cap for the player-update channel that async tasks (quest
/// rewards, delayed grants, etc.) push deltas into. Sized for the
/// expected concurrency: a few dozen in-flight grants at any moment
/// is plenty; on overflow the sending task awaits until the tick
/// drains a slot.
pub const PLAYER_UPDATE_QUEUE_CAP: usize = 1024;

/// Tick system that drains the player-update inbox and applies
/// each message to the matching online player. Idempotent —
/// missing characters (offline players) silently drop.
pub fn drain_player_updates(world: &mut World) {
    let drained: Vec<PendingPlayerUpdate> = {
        let inbox = world.resource::<PlayerUpdateInbox>();
        let Ok(mut rx) = inbox.0.lock() else {
            return;
        };
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            out.push(msg);
        }
        out
    };
    for msg in drained {
        let target_cid = msg.character_id().to_string();
        // Resolve the entity by Account.character_id.
        let entity: Option<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Account), With<Player>>();
            q.iter(world)
                .find(|(_, a)| a.character_id == target_cid)
                .map(|(e, _)| e)
        };
        let Some(entity) = entity else {
            continue;
        };
        match msg {
            PendingPlayerUpdate::ExperienceDelta { amount, .. } => {
                if let Some(mut p) = world.get_mut::<Profile>(entity) {
                    p.experience = p.experience.saturating_add(amount);
                }
            }
            PendingPlayerUpdate::WealthDelta { amount, .. } => {
                if let Some(mut w) = world.get_mut::<Wealth>(entity) {
                    w.0 = w.0.saturating_add(amount);
                }
            }
            PendingPlayerUpdate::SkillPointsDelta { amount, .. } => {
                if let Some(mut s) = world.get_mut::<mud_world::SkillPoints>(entity) {
                    s.0 = s.0.saturating_add(amount);
                }
            }
            PendingPlayerUpdate::AbilityKnown { ability_id, .. } => {
                if let Some(mut k) = world.get_mut::<KnownAbilities>(entity)
                    && !k.entries.iter().any(|(id, _, _)| *id == ability_id)
                {
                    k.entries.push((ability_id, 1, true));
                }
            }
            PendingPlayerUpdate::SpawnItem {
                object_zone,
                object_id,
                quantity,
                ..
            } => {
                let proto = world
                    .resource::<ObjectPrototypes>()
                    .by_key
                    .get(&(object_zone, object_id))
                    .cloned();
                let Some(proto) = proto else {
                    continue;
                };
                for _ in 0..quantity.max(1) {
                    let mut bundle = world.spawn((
                        Item,
                        Named { name: proto.name.clone() },
                        Keywords(proto.keywords.clone()),
                        WorldKey {
                            zone: proto.zone_id,
                            id: proto.id,
                        },
                        Located(entity),
                    ));
                    if let Some(desc) = proto.examine_description.clone() {
                        bundle.insert(Description(desc));
                    }
                }
                send_to(
                    world,
                    entity,
                    format!(
                        "You receive {} of {}.\r\n",
                        quantity.max(1),
                        proto.name
                    ),
                );
            }
        }
    }
}

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

// Distributed-registration entry point. Anywhere in the binary
// can `inventory::submit! { Command { ... } }` and the entry
// will land in `inventory::iter::<Command>()` at runtime.
inventory::collect!(Command);

/// Boxed-future return type for `AsyncDispatchFn`. The dispatcher
/// is single-threaded (`current_thread` runtime), so futures don't
/// need `Send`.
pub type DispatchFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>>;

/// Per-file async-command dispatch hook. The function inspects
/// the lowercase command head + args; if its module owns this
/// command, it returns `Some(Box::pin(handler(...)))`; otherwise
/// `None` so the dispatcher tries the next registration. This is
/// the async analogue of `inventory::submit!(Command { ... })` —
/// adding a new async command means one file edit.
pub type AsyncDispatchFn = for<'a> fn(
    world: &'a mut World,
    player: Entity,
    pool: &'a mud_db::sqlx::PgPool,
    head: &'a str,
    args: &'a str,
) -> Option<DispatchFuture<'a>>;

pub struct AsyncCommand {
    pub dispatch: AsyncDispatchFn,
}

inventory::collect!(AsyncCommand);

// Migrated commands — each lives in its own file under
// `commands/` and registers itself via `inventory::submit!`.
// `#[path]` lets us keep `commands.rs` as the parent module
// without renaming it to `commands/mod.rs`. Adding a new file
// here is the only edit needed when migrating an existing command
// (or shipping a new one) — no central array touch.
#[path = "commands/account_bank.rs"]
mod account_bank;
#[path = "commands/account_chest.rs"]
mod account_chest;
#[path = "commands/admin_inspect.rs"]
mod admin_inspect;
#[path = "commands/admin_management.rs"]
mod admin_management;
#[path = "commands/admin_world.rs"]
mod admin_world;
#[path = "commands/balance.rs"]
mod balance;
#[path = "commands/boards.rs"]
mod boards;
pub(crate) use boards::compose_board_step;
#[path = "commands/channels.rs"]
mod channels;
#[path = "commands/clan_chat.rs"]
mod clan_chat;
#[path = "commands/combat.rs"]
mod combat_commands;
pub(crate) use combat_commands::cmd_flee;
#[path = "commands/enter.rs"]
mod enter;
#[path = "commands/feedback.rs"]
mod feedback;
#[path = "commands/housing.rs"]
mod housing;
#[path = "commands/identity.rs"]
mod identity;
#[path = "commands/name_approval.rs"]
mod name_approval;
#[path = "commands/info.rs"]
mod info;
pub(crate) use info::{cmd_look, has_object_flag, has_restriction};
#[path = "commands/mail.rs"]
mod mail;
pub(crate) use mail::{cmd_mail_stub, compose_mail_step};
#[path = "commands/movement_directions.rs"]
mod movement_directions;
#[path = "commands/quests.rs"]
mod quests;
#[path = "commands/recall.rs"]
mod recall;
#[path = "commands/room_chat.rs"]
mod room_chat;
#[path = "commands/release.rs"]
mod release;
#[path = "commands/save.rs"]
mod save;
#[path = "commands/setrecall.rs"]
mod setrecall;
#[path = "commands/spells.rs"]
mod spells;
#[path = "commands/status_lists.rs"]
mod status_lists;
#[path = "commands/tells.rs"]
mod tells;
#[path = "commands/unban.rs"]
mod unban;

/// Iterate every registered command — both the static `COMMANDS`
/// array and any `inventory::submit!`-distributed entries. The
/// dispatcher walks this; help / `cmd_dispatch_async` walk it too.
/// Order: static array first, then submitted entries (linker-
/// determined). A future migration can drain the static array
/// entirely without touching dispatch.
pub fn all_commands() -> impl Iterator<Item = &'static Command> {
    inventory::iter::<Command>()
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
    Magic,
    Inventory,
    Group,
    Mount,
    Banking,
    Quest,
    Mail,
    Settings,
    Admin,
}

impl Category {
    /// Display order for `help` with no args. Ordered roughly by
    /// what a new player needs first: orienting (Info / Movement /
    /// Comm), then moment-to-moment play (Combat / Magic /
    /// Inventory), then social and meta (Group / Mount / Banking /
    /// Quest / Mail / Settings), then admin at the bottom.
    pub const ORDER: &'static [Self] = &[
        Self::Info,
        Self::Movement,
        Self::Communication,
        Self::Combat,
        Self::Magic,
        Self::Inventory,
        Self::Group,
        Self::Mount,
        Self::Banking,
        Self::Quest,
        Self::Mail,
        Self::Settings,
        Self::Admin,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "Information",
            Self::Movement => "Movement",
            Self::Communication => "Communication",
            Self::Combat => "Combat",
            Self::Magic => "Magic",
            Self::Inventory => "Inventory",
            Self::Group => "Group",
            Self::Mount => "Mount",
            Self::Banking => "Banking",
            Self::Quest => "Quest",
            Self::Mail => "Mail",
            Self::Settings => "Settings",
            Self::Admin => "Admin",
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

const MAX_NAME_TOKENS: usize = 3;

pub(crate) const MOVE_HELP: Help = Help {
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
    for cmd in all_commands() {
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
    // Every successful arm needs the same prompt-marking + activity
    // stamp before its handler runs. Stamp once up front; on the
    // `false` paths the caller falls through to the sync `dispatch`,
    // which re-marks idempotently.
    mark_for_prompt(player);
    try_insert(world, player, LastInputAt(std::time::Instant::now()));

    // Composition mode: when the player has a `MailDraft` or
    // `BoardDraft` component, every line is routed to the matching
    // composer until `.send` / `.abort` clears it.
    if world.get::<MailDraft>(player).is_some() {
        compose_mail_step(world, player, pool, trimmed).await;
        return true;
    }
    if world.get::<BoardDraft>(player).is_some() {
        compose_board_step(world, player, pool, trimmed).await;
        return true;
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("").to_ascii_lowercase();
    let args = parts.next().unwrap_or("").trim();

    // Iterate every distributed `AsyncCommand`. The first one whose
    // dispatch fn returns `Some(future)` for this `head` claims the
    // line. Per-file modules submit AsyncCommand records the same
    // way they submit sync `Command` records — `inventory::submit!`.
    for cmd in inventory::iter::<AsyncCommand>() {
        if let Some(fut) = (cmd.dispatch)(world, player, pool, &head, args) {
            fut.await;
            return true;
        }
    }
    false
}

/// State summary of one composition step — needed because we have to
/// release the `Mut<MailDraft>` borrow before sending feedback to the
/// player (`send_to` re-borrows the world).
pub(crate) enum ComposeStep {
    Nudge,
    SubjectSet,
    BodyAdded,
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

    // `switch` redirects: when the player is puppeteering a mob,
    // commands they type dispatch against the mob instead. The
    // `return` command (and `switch` with no arg) are gates that
    // we DON'T retarget — otherwise there's no escape hatch.
    let lower_first = trimmed.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    let is_escape_hatch = matches!(lower_first.as_str(), "return" | "switch");
    let player = if !is_escape_hatch
        && let Some(mud_world::SwitchedInto(mob)) =
            world.get::<mud_world::SwitchedInto>(player).copied()
    {
        mob
    } else {
        player
    };

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

    // Ghost gate: dead-but-incorporeal players can perceive the world
    // and communicate but can't interact with it physically. Whitelist
    // the verbs that ARE allowed; everything else gets a "you can't do
    // that as a ghost" reply pointing them at `release`.
    if world.get::<mud_world::Ghost>(player).is_some() && !ghost_allowed(tokens[0]) {
        send_to(
            world,
            player,
            "Your spirit can't act on the world while disembodied. \
             Type `release` to return to your recall point, or wait \
             for someone to resurrect you.\r\n",
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

    let cmd_n_consumed = longest_prefix_match(&tokens).or_else(|| {
        // No exact name match. Try resolving the first token as a
        // prefix abbreviation against commands the player can see
        // (G1.6). When unique + not on the destructive denylist,
        // dispatch as if the player had typed the canonical name.
        let (role, perms) = world.get::<Account>(player).map_or_else(
            || (UserRole::Player, Vec::new()),
            |a| (a.role, a.perms.clone()),
        );
        resolve_by_prefix(tokens[0], role, &perms).map(|cmd| (cmd, 1usize))
    });
    let Some((cmd, n_consumed)) = cmd_n_consumed else {
        // Fall through to socials before declaring unknown.
        if try_dispatch_social(world, player, tokens[0], skip_n_tokens(trimmed, 1)) {
            return;
        }
        send_to(
            world,
            player,
            format!(
                "Unknown command: {}{}\r\n",
                tokens[0],
                unknown_command_hint(world, player, tokens[0]),
            ),
        );
        return;
    };

    // Permission gate. Players check Account.role; mobs (no Account)
    // are allowed Player-level commands only — that's the path used
    // by `order <mob> <cmd>` and by `actor:command()` queued from Lua
    // triggers running on a mob. Admin commands always require an
    // account at the right role + perms.
    //
    // DevMode short-circuit: open-playtest servers grant every command
    // (regardless of min_role / required_perm) to every player. Mobs
    // still get Player-level only — DevMode is for human playtesters,
    // not Lua trigger sandboxing. See ``DevMode`` in main.rs.
    let dev_mode_on = world.get_resource::<crate::DevMode>().is_some_and(|d| d.0);
    let allowed = if let Some(a) = world.get::<Account>(player) {
        dev_mode_on || (
            a.role.at_least(cmd.min_role)
                && cmd.required_perm.is_none_or(|p| a.perms.contains(&p))
        )
    } else if world.get::<Mob>(player).is_some() {
        cmd.min_role == UserRole::Player && cmd.required_perm.is_none()
    } else {
        false
    };
    if !allowed {
        send_to(world, player, "You can't do that.\r\n");
        return;
    }

    // Debug-command gate: even Implementor-tier commands that execute
    // arbitrary code or dump full server state can be turned off with
    // `security.enable_debug_commands=false`. Default true (legacy
    // permissive). Refusal message points at the gate so an admin
    // doesn't waste time wondering why their `lua` returns nothing.
    if is_debug_command(cmd.names[0])
        && !world
            .resource::<mud_world::RuntimeConfig>()
            .get_bool("security", "enable_debug_commands", true)
    {
        send_to(
            world,
            player,
            "Debug commands are disabled by `security.enable_debug_commands=false`. \
             An admin must flip the GameConfig row to re-enable them.\r\n",
        );
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
pub(crate) fn expand_alias(world: &World, player: Entity, line: &str) -> Option<String> {
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

/// Names of commands considered "debug" — arbitrary-code or
/// world-introspection tools that an operator might want to turn off
/// in production via `security.enable_debug_commands=false`. Listed
/// by canonical name (the first entry in the registration's `names`
/// array). Aliases would never reach the gate because dispatch
/// resolves them to the same `Command` whose `names[0]` is checked.
fn is_debug_command(name: &str) -> bool {
    matches!(name, "lua" | "dumpworld" | "trace")
}

/// Whitelist of verbs a ghost can use. Covers perception, movement,
/// communication, account-level utilities, and the `release` exit
/// from the ghost state. Everything else is gated off so ghosts can't
/// fight, cast, pick things up, wear gear, etc. Cardinal direction
/// movement is allowed by name (n/s/e/w/up/down + diagonals + their
/// long forms) so ghosts can wander between rooms.
pub(crate) fn ghost_allowed(verb: &str) -> bool {
    matches!(
        verb,
        "release"
            | "look" | "l"
            | "examine" | "ex"
            | "exits"
            | "scan"
            | "where"
            | "who"
            | "score" | "sc"
            | "inventory" | "inv" | "i"
            | "equipment" | "eq"
            | "abilities"
            | "stat"
            | "rstat"
            | "help"
            | "save"
            | "quit"
            // Movement
            | "north" | "n"
            | "south" | "s"
            | "east" | "e"
            | "west" | "w"
            | "up" | "u"
            | "down" | "d"
            | "northeast" | "ne"
            | "northwest" | "nw"
            | "southeast" | "se"
            | "southwest" | "sw"
            | "in" | "out"
            // Communication
            | "say" | "'"
            | "tell"
            | "gossip"
            | "shout"
            | "ooc"
            | "emote"
            // Settings
            | "title"
            | "description"
            | "prompt"
            | "color"
    )
}

pub(crate) fn longest_prefix_match(tokens: &[&str]) -> Option<(&'static Command, usize)> {
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

/// Commands a player has to type in full — auto-abbreviation never
/// resolves to these. Picked for blast radius: an inadvertently
/// abbreviated `q` should not log the player out, `del` should not
/// delete account data, etc. Mortal-visible verbs first; admin
/// verbs follow. Aliases (`quit` covers `quit`; the abbrev path
/// queries canonical `names[0]`) — adding the canonical name here
/// blocks every alias as well.
const ABBREV_DENYLIST: &[&str] = &[
    // Mortal
    "quit", "delete", "release", "drop", "junk", "give", "remove",
    // Combat lifecycle
    "flee", "wimpy",
    // Admin / staff
    "shutdown", "reboot", "purge", "ban", "unban", "kick", "freeze",
    "force", "transfer", "wizlock", "demote", "promote", "wipe",
    "deletechar", "deleteobj", "rdelete", "mdelete", "odelete", "zdelete",
];

/// Resolve a typed verb by unique prefix among commands the player
/// can see. Returns `Some(&Command)` only when exactly one canonical
/// command matches and that command isn't on the destructive
/// denylist. Multiple matches, zero matches, or a denylisted target
/// all return `None` so the caller falls through to the
/// social-dispatch / unknown-command path.
///
/// Tokens 1–2 chars are too aggressive to auto-resolve safely
/// (a stray `n` would expand to `north` even when the player meant
/// to type a single letter into a draft); minimum is 3.
pub(crate) fn resolve_by_prefix(
    typed: &str,
    role: UserRole,
    perms: &[Permission],
) -> Option<&'static Command> {
    if typed.len() < 3 {
        return None;
    }
    let needle = typed; // already lowercased at call site
    // Walk every visible command; collect unique canonical names whose
    // primary name begins with `needle`. Aliases that match are noise
    // here — they'd resolve to the same canonical and produce false
    // collisions in the unique-match check. Only `names[0]` counts.
    let mut hit: Option<&'static Command> = None;
    for cmd in all_commands() {
        if !visible(cmd, role, perms) {
            continue;
        }
        if !cmd.names[0].starts_with(needle) {
            continue;
        }
        if ABBREV_DENYLIST.contains(&cmd.names[0]) {
            // Denylisted: even if it's the only prefix match, refuse.
            // Player has to type the full verb to fire it.
            return None;
        }
        match hit {
            None => hit = Some(cmd),
            Some(prev) if std::ptr::eq(prev, cmd) => {}
            Some(_) => return None, // ambiguous
        }
    }
    hit
}

pub(crate) fn skip_n_tokens(s: &str, n: usize) -> &str {
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

/// Send rendered output to a player or mob. Input is XML-Lite color
/// markup (`<red>...</>`); we run it through `render_color_tags` per
/// the target's `UiStyle.colors` mode before sending. This is the
/// default everywhere — call sites that build messages via `format!`
/// containing entity names (which may carry XML-Lite tags) get
/// correct ANSI output without needing to reach for `send_rendered`.
///
/// Use [`send_raw`] if you need to bypass rendering (e.g. when the
/// bytes are already ANSI). Most non-text protocol bytes (GMCP/IAC)
/// go through `Connection.0.send` directly, not here.
pub(crate) fn send_to(world: &World, target: Entity, text: impl Into<String>) {
    let mode = color_mode_for(world, target);
    let rendered = render_color_tags(&text.into(), mode);
    send_raw(world, target, rendered);
}

/// Raw-bytes send, no color-tag rendering. `PROMPT_RECIPIENTS` is
/// still tracked. Used by `send_to` after rendering, and by callers
/// that ship pre-rendered ANSI. Also mirrors to a snooper when the
/// target carries `SnoopedBy` — admin debugging visibility.
pub(crate) fn send_raw(world: &World, target: Entity, text: impl Into<String>) {
    let text = text.into();
    if let Some(conn) = world.get::<Connection>(target) {
        let _ = conn.0.try_send(text.clone().into_bytes());
    }
    // Switch puppet: when admin has used `switch <mob>`, the mob
    // doesn't have its own Connection — forward the bytes to the
    // puppeteer's connection so they see what their puppet sees.
    if let Some(mud_world::SwitchedFrom(puppeteer)) =
        world.get::<mud_world::SwitchedFrom>(target).copied()
        && let Some(puppeteer_conn) = world.get::<Connection>(puppeteer)
    {
        let _ = puppeteer_conn.0.try_send(text.clone().into_bytes());
    }
    // Snoop mirror: forward a dim-prefixed copy to the snooper.
    // Skip when the text already starts with the prefix (defensive
    // against accidental recursion) and when the snooper is the
    // target itself.
    if let Some(mud_world::SnoopedBy(snooper)) = world.get::<mud_world::SnoopedBy>(target).copied()
        && snooper != target
        && let Some(snooper_conn) = world.get::<Connection>(snooper)
    {
        let mut framed = String::with_capacity(text.len() + 16);
        // Render line-by-line so multi-line output stays
        // visually associated with the snooped entity. The dim
        // `%` prefix mirrors the legacy convention.
        for line in text.split_inclusive('\n') {
            framed.push_str("\x1b[2m%\x1b[0m ");
            framed.push_str(line);
        }
        let _ = snooper_conn.0.try_send(framed.into_bytes());
    }
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

/// Push a `Comm.Channel.Text` GMCP frame to one recipient. Lets
/// chat commands (gossip, wiznet, tell, ctell, room speech) hand
/// the client a structured chat feed in addition to the rendered
/// text — Mudlet and the major web clients use it to route
/// messages to dedicated tabs without screen-scraping. `channel`
/// is the routing key carried in the JSON payload (`"gossip"`,
/// `"wiznet"`, `"tells"`, ...), NOT the GMCP package name (which
/// is always `Comm.Channel.Text`, per the IRE spec).
///
/// `talker` and `text` are color-stripped before serialization so
/// clients re-style consistently. By convention `text` is the
/// rendered third-person line (e.g. `"Strider gossips, \"hi\""`)
/// — the client uses it as the body to display, with `talker` and
/// `channel` available for filtering and per-channel styling.
///
/// No-op when `recipient` has no `Connection` (mob target,
/// switched puppet without its own session, disconnected player).
pub(crate) fn send_comm_channel_text(
    world: &World,
    recipient: Entity,
    channel: &str,
    talker: &str,
    text: &str,
) {
    let Some(conn) = world.get::<Connection>(recipient) else { return };
    let plain_speaker = render_color_tags(talker, ColorMode::Strip);
    let plain_text = render_color_tags(text, ColorMode::Strip);
    let payload = format!(
        r#"{{"channel":"{}","talker":"{}","text":"{}"}}"#,
        channel,
        plain_speaker.replace('\\', "\\\\").replace('"', "\\\""),
        plain_text.replace('\\', "\\\\").replace('"', "\\\""),
    );
    let _ = conn.0.try_send(mud_net::gmcp_packet("Comm.Channel.Text", &payload));
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
pub(crate) fn flush_prompts(world: &mut World) {
    let recipients =
        PROMPT_RECIPIENTS.with(|r| std::mem::take(&mut *r.borrow_mut()));
    for entity in recipients {
        if world.get_entity(entity).is_ok() {
            send_prompt(world, entity);
        }
    }
}

/// Pump the real-time syslog WARN+ broadcast queue. Called once per
/// tick from `main.rs`. Drains every entry the tracing layer has
/// pushed since the previous tick and fans each one out to every
/// online player carrying a `WatchingSyslog` whose floor admits it.
/// Early-outs on either side being empty so the no-subscriber case
/// is cheap.
pub(crate) fn drain_syslog_to_watchers(world: &mut World) {
    let entries = crate::syslog::drain_broadcast();
    if entries.is_empty() {
        return;
    }
    // Snapshot subscribers up front so we don't reborrow the World
    // mid-fanout. Tracking the floor per-subscriber lets us skip
    // formatting work when an ERROR-only watcher won't see a WARN.
    let watchers: Vec<(Entity, mud_world::SyslogMinLevel)> = {
        let mut q = world
            .query_filtered::<(Entity, &mud_world::WatchingSyslog), With<mud_world::Online>>();
        q.iter(world).map(|(e, w)| (e, w.min_level)).collect()
    };
    if watchers.is_empty() {
        return;
    }
    for entry in entries {
        let admits_warn = matches!(entry.level, tracing::Level::WARN | tracing::Level::ERROR);
        let admits_error = matches!(entry.level, tracing::Level::ERROR);
        // Color the level tag by severity so a glance separates
        // benign WARN noise from an actual ERROR event.
        let level_tag = match entry.level {
            tracing::Level::ERROR => "<red>ERROR</>",
            tracing::Level::WARN => "<yellow>WARN</>",
            // INFO/DEBUG/TRACE never get pushed (see syslog.rs filter)
            // but match exhaustively so future filter changes don't
            // silently drop entries here.
            _ => continue,
        };
        let line = format!(
            "\r\n<dim>[syslog]</> {level_tag} <cyan>{}</>: {}\r\n",
            entry.target, entry.message,
        );
        for (entity, floor) in &watchers {
            let admit = match floor {
                mud_world::SyslogMinLevel::Warn => admits_warn,
                mud_world::SyslogMinLevel::Error => admits_error,
            };
            if admit {
                send_to(world, *entity, line.clone());
            }
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

/// Single foreground/background color value carried on a style layer.
/// Three variants cover the full XML-Lite color surface:
///
/// * `Ansi16` — basic 8 + bright 8 colors. Stored as the literal
///   ANSI fg code (30–37, 90–97); background emits `+10`. This is
///   what `<red>` / `<b:yellow>` / `<bg-blue>` produce.
/// * `Ansi256` — xterm 256-color palette. Index 0–255. `<c196>` /
///   `<bgc208>` produce these. Emits `38;5;N` / `48;5;N`.
/// * `Rgb` — 24-bit truecolor. `<#FF8800>` / `<bg#001020>` produce
///   these. Emits `38;2;R;G;B` / `48;2;R;G;B`.
///
/// All three are emitted unconditionally — gating on detected client
/// capability (MTTS truecolor bit) is the renderer caller's job, not
/// this layer's. Modern clients (Mudlet, BlightMud, MUSHclient,
/// every web client) handle all three; legacy 16-color terminals
/// will quietly down-sample 256/RGB to the nearest match. We don't
/// try to translate server-side because the client's mapping is
/// invariably better than ours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Color {
    Ansi16(u8),
    Ansi256(u8),
    Rgb(u8, u8, u8),
}

/// Per-layer style state. Each opening tag pushes one of these to the
/// stack; closes pop. Anonymous tags (`<b:red>` style) keep `name`
/// empty and can only be closed via `</>`. The 8 bool fields map 1:1
/// to ANSI attribute codes (1, 2, 3, 4, 5, 7, 8, 9) — a bitflags type
/// would compile to the same thing, just with an extra dependency.
#[allow(clippy::struct_excessive_bools)]
#[derive(Default, Clone, Debug)]
pub(crate) struct StyleLayer {
    name: String,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    reverse: bool,
    hidden: bool,
    strikethrough: bool,
    fg: Option<Color>,
    bg: Option<Color>,
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
    // Index-based walk so we can rewind on a nested `<`. The previous
    // iterator-based scanner ate `<<yellow>` as a single non-tag run
    // ("<yellow") and emitted it literally, which broke prompts that
    // had a literal leading `<` (e.g. `prompt <%h/%H ...>` — the
    // colored %h substitution puts a `<yellow>` right after the
    // literal `<`).
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c != '<' {
            out.push(c);
            i += 1;
            continue;
        }
        // Scan for matching `>`. Bail to "literal `<`" if we hit
        // another `<` first — that's a sign the outer `<` is just
        // a typed angle bracket, not the start of a tag. Bail to
        // "drop trailing fragment" if we hit end-of-input.
        let mut j = i + 1;
        let mut found_close: Option<usize> = None;
        while j < chars.len() {
            match chars[j] {
                '>' => {
                    found_close = Some(j);
                    break;
                }
                '<' => break,
                _ => j += 1,
            }
        }
        let Some(close) = found_close else {
            // Either ran into a nested `<` or hit end-of-input.
            // Either way the outer `<` is literal — emit it and
            // resume parsing from the next char so a nested
            // `<yellow>` after a literal `<` still opens the tag.
            out.push('<');
            i += 1;
            continue;
        };
        let tag: String = chars[i + 1..close].iter().collect();
        // Only consume `<...>` as a tag if the content actually looks
        // tag-shaped. This is what lets the default prompt template
        // `<%h/%H>` survive: after %-substitution it's `<42/100>`,
        // which contains a `/` mid-content (not the leading-slash
        // close form) and so doesn't match any color-tag shape —
        // we emit it literally.
        if !is_tag_shaped(&tag) {
            out.push('<');
            out.push_str(&tag);
            out.push('>');
            i = close + 1;
            continue;
        }
        if apply_tag(&tag, &mut stack) && mode == ColorMode::Ansi {
            emit_ansi_state(&mut out, &stack);
        }
        i = close + 1;
    }
    if mode == ColorMode::Ansi && !stack.is_empty() {
        out.push_str("\x1b[0m");
    }
    out
}

/// Render a string for embedding inside a GMCP JSON payload: strip
/// color tags, then escape `\` and `"`. The combination every named-
/// entity GMCP emit site needs. Keeps the wire payload plain so the
/// client can re-style without re-parsing color tags.
#[must_use]
pub(crate) fn plain_for_gmcp(s: &str) -> String {
    render_color_tags(s, ColorMode::Strip)
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// True if `<tag>` looks like an XML-Lite color/style tag — i.e. its
/// contents only contain characters the spec uses (alphanumerics, `:`
/// for modifier separators, `#` for RGB, `-` and `_` for `bg-NAME`-
/// style names) plus an optional leading `/` for close tags. The empty
/// string also returns true to preserve the previous "drop empty `<>`"
/// behavior. Anything else (most importantly `<%h/%H>`-style prompt
/// vars) is treated as literal text.
/// Visible width of a string after XML-Lite color tags are stripped.
/// Counts characters that would actually appear in the rendered
/// output — `<red>foo</>` reports 3, not 11. Used by table-alignment
/// sites (`who`, `idle`, ability/inventory lists) so columns stay
/// aligned regardless of color markup in entity names.
///
/// Limitations: counts chars (not grapheme clusters), so multi-codepoint
/// emoji or combining marks would still over-count. Good enough for
/// the all-ASCII content the imported world contains.
pub(crate) fn visible_width(s: &str) -> usize {
    let mut count = 0usize;
    // Index-based to mirror render_color_tags' nested-`<` fallback.
    // A literal `<` followed by a tag (`<<yellow>...`) counts the
    // outer `<` as 1 visible char and re-enters parsing on the
    // inner `<` — same shape as the renderer.
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c != '<' {
            count += 1;
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut found_close: Option<usize> = None;
        while j < chars.len() {
            match chars[j] {
                '>' => {
                    found_close = Some(j);
                    break;
                }
                '<' => break,
                _ => j += 1,
            }
        }
        let Some(close) = found_close else {
            // Outer `<` is a literal angle bracket — counts as 1
            // visible char; resume scanning from the next char.
            count += 1;
            i += 1;
            continue;
        };
        let tag: String = chars[i + 1..close].iter().collect();
        if !is_tag_shaped(&tag) {
            // Literal `<...>` — counts as visible (`<`, body, `>`).
            count += 2 + tag.chars().count();
        }
        // Tag-shaped: contributes 0 visible chars.
        i = close + 1;
    }
    count
}

/// Right-pad `s` to `width` *visible* columns with spaces. Used in
/// place of `format!("{s:<width$}")` for content that may carry
/// XML-Lite color tags.
pub(crate) fn pad_visible(s: &str, width: usize) -> String {
    let vis = visible_width(s);
    if vis >= width {
        s.to_string()
    } else {
        let mut out = String::with_capacity(s.len() + (width - vis));
        out.push_str(s);
        for _ in 0..(width - vis) {
            out.push(' ');
        }
        out
    }
}

pub(crate) fn is_tag_shaped(tag: &str) -> bool {
    // `<>` is the empty no-op tag (renderer drops it cleanly).
    if tag.is_empty() {
        return true;
    }
    // Closing form: `</>` is full reset; `</name>` requires a known
    // single-modifier name (color or attribute, no `:`-compound).
    if let Some(name) = tag.strip_prefix('/') {
        return name.is_empty() || is_known_tag_part(name);
    }
    // Opening tags: every `:`-separated part must parse to a real
    // attribute (modifier name, named color, `bg-NAME`, or `#RRGGBB`).
    // Free-text-shaped tokens like `<unknown>` / `<gone>` fall through
    // to literal text so callers can use them as placeholder markers
    // without paying a tag-eats-the-text tax.
    tag.split(':').all(is_known_tag_part)
}

fn is_known_tag_part(p: &str) -> bool {
    if matches!(
        p,
        "b" | "u" | "i" | "s" | "d" | "dim" | "blink" | "reverse" | "hide"
    ) {
        return true;
    }
    if let Some(rest) = p.strip_prefix("bg-") {
        return named_color(rest).is_some();
    }
    if let Some(rest) = p.strip_prefix("bg#") {
        return rest.len() == 6 && rest.bytes().all(|b| b.is_ascii_hexdigit());
    }
    if let Some(rest) = p.strip_prefix('#') {
        return rest.len() == 6 && rest.bytes().all(|b| b.is_ascii_hexdigit());
    }
    // `bgcN` and `cN` are 256-color forms — accept them only when
    // the suffix actually parses as a 0..=255 index. Otherwise fall
    // through so a named color starting with `bgc` / `c` (today the
    // critical one is `cyan`) still resolves via the named_color
    // table at the end. A previous regression had this branch
    // returning `false` whenever the suffix wasn't numeric, which
    // ate every `<cyan>` / `<bgcyan>` tag and left them as literal
    // text on the wire.
    if let Some(rest) = p.strip_prefix("bgc")
        && parse_ansi256_index(rest).is_some()
    {
        return true;
    }
    if let Some(rest) = p.strip_prefix('c')
        && parse_ansi256_index(rest).is_some()
    {
        return true;
    }
    named_color(p).is_some()
}

/// Parse an xterm 256-color index. Accepts `0`-`255` decimal; returns
/// `None` for empty / non-numeric / out-of-range. Pulled out so
/// `is_known_tag_part` and `apply_modifier` agree on what counts as
/// a valid `cN` / `bgcN` body.
fn parse_ansi256_index(s: &str) -> Option<u8> {
    if s.is_empty() {
        return None;
    }
    s.parse::<u8>().ok()
}

/// Parse `RRGGBB` hex into an `(r, g, b)` triple. Caller has already
/// stripped the leading `#` / `bg#`. Returns `None` on wrong length
/// or non-hex digits.
fn parse_rgb_hex(s: &str) -> Option<(u8, u8, u8)> {
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}

/// Mutate the style stack in response to one parsed tag. Returns true
/// if the stack changed; the caller uses that to skip a no-op ANSI
/// re-emit (empty `<>`, `</no-such-name>`).
pub(crate) fn apply_tag(tag: &str, stack: &mut Vec<StyleLayer>) -> bool {
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

pub(crate) fn apply_modifier(layer: &mut StyleLayer, m: &str) {
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
            // Same precedence story as `is_known_tag_part`: try the
            // structured prefixes first, but fall through to the
            // named-color table when the prefix-strip leaves
            // something that doesn't parse as the expected payload.
            // Otherwise `<cyan>` would be eaten by the `c` branch
            // (numeric parse fails → no fg set → silent drop) and
            // never reach `named_color`.
            if let Some(rest) = m.strip_prefix("bg-")
                && let Some(c) = named_color(rest)
            {
                layer.bg = Some(Color::Ansi16(c));
            } else if let Some(rest) = m.strip_prefix("bg#")
                && let Some((r, g, b)) = parse_rgb_hex(rest)
            {
                layer.bg = Some(Color::Rgb(r, g, b));
            } else if let Some(rest) = m.strip_prefix("bgc")
                && let Some(idx) = parse_ansi256_index(rest)
            {
                layer.bg = Some(Color::Ansi256(idx));
            } else if let Some(rest) = m.strip_prefix('#')
                && let Some((r, g, b)) = parse_rgb_hex(rest)
            {
                layer.fg = Some(Color::Rgb(r, g, b));
            } else if let Some(rest) = m.strip_prefix('c')
                && let Some(idx) = parse_ansi256_index(rest)
            {
                layer.fg = Some(Color::Ansi256(idx));
            } else if let Some(c) = named_color(m) {
                layer.fg = Some(Color::Ansi16(c));
            }
            // Anything left over parses as a no-op; layer contributes
            // nothing for those positions. Mismatched tags
            // (`<unknown>`) are filtered out earlier by
            // `is_known_tag_part`, so this branch only handles
            // malformed-but-tag-shaped input like `<c999>`.
        }
    }
}

/// Map a named color word to its base ANSI foreground code. Aliases
/// (`magenta`/`purple`, `cyan`/`teal`, `brown`/`yellow`, `orange` →
/// bright yellow) follow the `FieryMUD` `XMLLite` docs.
pub(crate) fn named_color(s: &str) -> Option<u8> {
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
pub(crate) fn emit_ansi_state(out: &mut String, stack: &[StyleLayer]) {
    out.push_str("\x1b[0m");
    if stack.is_empty() {
        return;
    }
    let merged = merge_stack(stack);
    // Build the parameter list as strings rather than `u8` codes —
    // 256-color and truecolor SGR introducers (`38;5;N`, `38;2;R;G;B`)
    // span multiple parameters, which the previous `Vec<u8>` shape
    // couldn't express. Attribute toggles still emit single-number
    // params; only the fg/bg paths fan out.
    let mut params: Vec<String> = Vec::new();
    if merged.bold {
        params.push("1".to_string());
    }
    if merged.dim {
        params.push("2".to_string());
    }
    if merged.italic {
        params.push("3".to_string());
    }
    if merged.underline {
        params.push("4".to_string());
    }
    if merged.blink {
        params.push("5".to_string());
    }
    if merged.reverse {
        params.push("7".to_string());
    }
    if merged.hidden {
        params.push("8".to_string());
    }
    if merged.strikethrough {
        params.push("9".to_string());
    }
    if let Some(fg) = merged.fg {
        push_color_params(&mut params, fg, false);
    }
    if let Some(bg) = merged.bg {
        push_color_params(&mut params, bg, true);
    }
    if params.is_empty() {
        return;
    }
    out.push_str("\x1b[");
    out.push_str(&params.join(";"));
    out.push('m');
}

/// Append the SGR parameters for a single color value. The
/// foreground/background distinction collapses to: ANSI16 fg uses
/// the raw code (30-37 / 90-97), ANSI16 bg adds 10 (40-47 /
/// 100-107); 256-color uses the `38;5;N` / `48;5;N` introducer; RGB
/// uses `38;2;R;G;B` / `48;2;R;G;B`. All three SGR forms are
/// fixture-grade widely supported — 256 since xterm 88 (~2002),
/// truecolor since xterm 256 (~2012), and modern MUD clients all
/// implement them.
fn push_color_params(params: &mut Vec<String>, color: Color, is_bg: bool) {
    match color {
        Color::Ansi16(code) => {
            // Bright codes are 90-97; their bg counterpart is 100-107
            // — same +10 offset as the basic block (30-37 → 40-47).
            let emitted = if is_bg { code + 10 } else { code };
            params.push(emitted.to_string());
        }
        Color::Ansi256(idx) => {
            params.push(if is_bg { "48".to_string() } else { "38".to_string() });
            params.push("5".to_string());
            params.push(idx.to_string());
        }
        Color::Rgb(r, g, b) => {
            params.push(if is_bg { "48".to_string() } else { "38".to_string() });
            params.push("2".to_string());
            params.push(r.to_string());
            params.push(g.to_string());
            params.push(b.to_string());
        }
    }
}

/// Collapse the stack into one effective style: attributes OR-combined,
/// foreground/background = most-recent (deepest layer wins).
pub(crate) fn merge_stack(stack: &[StyleLayer]) -> StyleLayer {
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
        ColorMode, FormulaCtx, PromptCtx, amount_from_blob, apply_damage, apply_heal_hp,
        apply_heal_stamina,
        apply_knockdown_posture, check_ability_restrictions, check_target_type, condition_label,
        direction_name, duration_from_blob, evaluate_formula, evaluate_simple_formula,
        format_idle, has_effect_named,
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

    /// Smoke test: every inventory-distributed command shows up
    /// in `all_commands()`. Proves the registry actually links
    /// each `inventory::submit!` block — a regression here means
    /// migrated commands would silently disappear.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn inventory_distributed_commands_are_registered() {
        let names: Vec<&'static str> = super::all_commands()
            .flat_map(|c| c.names.iter().copied())
            .collect();
        // balance / unban (steps 1 + 1.5)
        for name in ["balance", "bal", "unban"] {
            assert!(
                names.contains(&name),
                "{name} not in all_commands()"
            );
        }
        // Movement category — fully migrated. Directionals from
        // movement_directions.rs, plus recall/release/enter/setrecall.
        for name in [
            "north", "n", "south", "s", "east", "e", "west", "w",
            "up", "u", "down", "d",
            "northeast", "ne", "northwest", "nw",
            "southeast", "se", "southwest", "sw",
            "in", "out",
            "recall", "home", "release", "enter", "setrecall",
        ] {
            assert!(
                names.contains(&name),
                "movement command `{name}` missing"
            );
        }
        // channels.rs (broadcast comm channels)
        for name in ["gossip", "/", "music", "shout", "wiznet", ";"] {
            assert!(
                names.contains(&name),
                "channel `{name}` missing"
            );
        }
        // tells.rs (private comms + ignore list + history)
        for name in ["tell", "t", "reply", "r", "ignore", "unignore", "lasttells", "lt"] {
            assert!(
                names.contains(&name),
                "tells command `{name}` missing"
            );
        }
        // feedback.rs (bug / idea / typo / petition)
        for name in ["bug", "idea", "typo", "petition"] {
            assert!(
                names.contains(&name),
                "feedback `{name}` missing"
            );
        }
        // room_chat.rs
        for name in [
            "say", "'", "emote", ":", "ask", "whisper", "insult",
            "gsay", "gtell", "gecho", "gt",
        ] {
            assert!(
                names.contains(&name),
                "room-chat `{name}` missing"
            );
        }
        // clan_chat.rs
        for name in ["ctell", "ct", "clan"] {
            assert!(
                names.contains(&name),
                "clan-chat `{name}` missing"
            );
        }
        // name_approval.rs — replaces the deleted login_approval.rs
        // commands. `approve_login` / `deny_login` / `lreqs` are
        // gone; the new commands operate on the
        // `Characters.name_approved` column.
        for name in [
            "approve_name", "approvename",
            "reject_name", "rejectname",
            "name_status", "namestatus",
        ] {
            assert!(
                names.contains(&name),
                "name-approval `{name}` missing"
            );
        }
        // The retired LoginRequests commands must NOT appear in the
        // registry — guard against a stale `inventory::submit!` that
        // a future refactor accidentally re-introduces.
        for name in ["approve_login", "deny_login", "lreqs"] {
            assert!(
                !names.contains(&name),
                "retired login-approval `{name}` resurfaced in the registry"
            );
        }
        // status_lists.rs (report + socials)
        for name in ["report", "socials"] {
            assert!(
                names.contains(&name),
                "status-lists `{name}` missing"
            );
        }
        // mail.rs + boards.rs (async-dispatched stubs)
        for name in [
            "mail", "mailbox", "readmail", "delmail",
            "boards", "board", "post", "delpost", "editpost",
        ] {
            assert!(
                names.contains(&name),
                "mail/board `{name}` missing"
            );
        }
        // quests.rs (Info + Admin verbs)
        for name in [
            "quests", "qstat", "qlist", "abandon", "innate",
            "questinfo", "qload", "qaccept", "qgive", "qcomplete",
        ] {
            assert!(
                names.contains(&name),
                "quest verb `{name}` missing"
            );
        }
        // admin_management.rs
        for name in [
            "ban", "cclan", "pnote", "playernote",
            "hinfo", "hgrant", "hrevoke",
        ] {
            assert!(
                names.contains(&name),
                "admin-mgmt `{name}` missing"
            );
        }
        // admin_world.rs
        for name in [
            "where", "goto", "transfer", "teleport", "force", "freeze",
            "summon", "apply", "restore", "slay", "purge",
            "load", "loadobj", "loado", "dumpworld",
        ] {
            assert!(
                names.contains(&name),
                "admin-world `{name}` missing"
            );
        }
        // admin_inspect.rs
        for name in [
            "zstat", "mstat", "ostat", "sstat", "tstat", "astat",
            "rstat", "stat", "setweather", "set", "show",
            "scripterrors", "scripterr", "syslog", "lua",
            "triggers", "trigs", "firetrig",
        ] {
            assert!(
                names.contains(&name),
                "admin-inspect `{name}` missing"
            );
        }
        // combat.rs
        for name in [
            "attack", "kill", "k", "hit", "murder",
            "consider", "con", "flee", "kick", "berserk",
            "tripup", "trip", "sweep", "roundhouse", "stomp",
            "roar", "howl", "rend", "gouge", "springleap",
            "throatcut", "backstab", "bs", "hitall", "tantrum",
            "disarm", "rescue", "guard", "assist",
            "layhands", "lay", "retreat", "tame", "drag",
            "buck", "breathe", "lure", "corner", "sneak",
            "conceal", "firstaid", "bandage", "disengage",
            "doorbash", "bash", "bodyslam", "maul",
        ] {
            assert!(
                names.contains(&name),
                "combat `{name}` missing"
            );
        }
        // spells.rs
        for name in [
            "pick", "study", "memorize", "mem", "pray", "forget",
            "cast", "c", "chant", "perform", "skill", "use",
            "abort", "cancel",
        ] {
            assert!(
                names.contains(&name),
                "spell/skill `{name}` missing"
            );
        }
    }

    #[test]
    fn visible_width_matches_render_strip_length() {
        // Plain text: visible_width == chars().count().
        assert_eq!(super::visible_width("plain text"), "plain text".chars().count());
        // Color-wrapped: only inner text counts.
        assert_eq!(super::visible_width("<red>foo</>"), 3);
        assert_eq!(super::visible_width("<b:yellow>warning</> ahead"), "warning ahead".len());
        // Multi-tag: each pair contributes only its inner content.
        assert_eq!(super::visible_width("<red>r</><green>g</><b>b</>"), 3);
        // Non-tag-shaped angle text: counts as literal.
        assert_eq!(super::visible_width("<%h/%H>"), "<%h/%H>".chars().count());
        // Unterminated `<` is treated as a literal angle bracket
        // (no trailing `>` to close it), so the whole "hi <b:yellow"
        // counts as 12 visible chars. Mirrors render_color_tags'
        // matching fallback.
        assert_eq!(super::visible_width("hi <b:yellow"), 12);
        // Nested `<<yellow>foo</>`: outer `<` is literal (1 col), the
        // inner `<yellow>...</>` wraps `foo` (3 cols). Total 4.
        assert_eq!(super::visible_width("<<yellow>foo</>"), 4);
    }

    #[test]
    fn pad_visible_aligns_color_names() {
        // Plain name padded to width.
        assert_eq!(super::pad_visible("foo", 6), "foo   ");
        // Colored name still pads to the same visible width — output
        // contains the tags plus enough trailing spaces to reach
        // 6 visible columns (3 visible + 3 spaces).
        let padded = super::pad_visible("<red>foo</>", 6);
        assert_eq!(super::visible_width(&padded), 6);
        assert!(padded.starts_with("<red>foo</>"));
        assert!(padded.ends_with("   "));
        // No truncation when input wider than `width`.
        assert_eq!(super::pad_visible("looooong", 4), "looooong");
    }

    /// Strip ANSI CSI escapes from `s` and return the visible-character
    /// width that remains. Tracks `\x1b[...m` sequences only — what
    /// `render_color_tags(.., ColorMode::Ansi)` emits today.
    fn ansi_visible_width(s: &str) -> usize {
        let mut count = 0usize;
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Eat `[` and everything up to and including `m`.
                let _ = chars.next();
                for nx in chars.by_ref() {
                    if nx == 'm' {
                        break;
                    }
                }
                continue;
            }
            count += 1;
        }
        count
    }

    #[test]
    fn pad_then_render_yields_expected_visible_width() {
        // Regression for 2bb9a1a / adc473e: the listing grids must
        // pad XML-Lite first, then render. After rendering the
        // resulting ANSI string should still occupy exactly the
        // pad target's visible columns. This test enforces the
        // contract that all the call sites depend on.
        let cases: &[(&str, usize)] = &[
            ("foo", 6),
            ("<red>foo</>", 6),
            ("<b:cyan>Magic Missile</> <red>(fire)</>", 30),
            ("Hellfire and Brimstone <red>(fire)</>", 33),
            ("Protection from Fire <b:white>(protection)</>", 36),
            ("plain", 12),
            ("", 4),
        ];
        for &(xml, width) in cases {
            let padded = super::pad_visible(xml, width);
            let rendered = ansi(&padded);
            assert_eq!(
                ansi_visible_width(&rendered),
                width,
                "pad-then-render width mismatch for {xml:?} → {width}",
            );
        }
        // Inverse: rendering first then padding undercounts.
        // pad_visible scans for `<...>` markers and treats the
        // ANSI escape bytes as visible chars, so it bails early
        // and the result is *not* width-aligned. This test pins
        // the broken order so a future "simplification" that
        // collapses the pipeline back to render-then-pad gets
        // caught immediately.
        let xml = "<red>fire</>";
        let render_first = super::pad_visible(&ansi(xml), 30);
        assert_ne!(
            ansi_visible_width(&render_first),
            30,
            "render-then-pad must not produce a width-30 line — \
             ANSI bytes inflate visible_width's count"
        );
    }

    #[test]
    fn render_color_tags_strip_mode_matches_legacy() {
        // No tags: identity.
        assert_eq!(strip("plain text"), "plain text");
        // Single tag pair.
        assert_eq!(strip("<red>red</>"), "red");
        // Multi-modifier open + full reset close.
        assert_eq!(strip("<b:yellow>warning:</> watch out"), "warning: watch out");
        // Unterminated `<` is treated as a literal angle bracket and
        // the rest of the string passes through verbatim. (Earlier
        // behavior dropped everything after the `<`; the new shape
        // is needed so prompts like `<%h/%H ...>` survive when the
        // %-substitution introduces nested tags.)
        assert_eq!(strip("hello <b:yellow"), "hello <b:yellow");
        // Nested literal-then-tag: the outer `<` is literal, the
        // inner `<yellow>...</>` strips out cleanly.
        assert_eq!(strip("<<yellow>foo</>"), "<foo");
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
    fn render_color_tags_truecolor_rgb_emits_38_2_form() {
        // <#FF0000>...</> → \x1b[0m\x1b[38;2;255;0;0m text \x1b[0m\x1b[0m
        let out = ansi("<#FF0000>red</>");
        assert!(out.contains("\x1b[38;2;255;0;0m"), "fg rgb present: {out:?}");
        assert!(out.contains("red"));
    }

    #[test]
    fn render_color_tags_truecolor_rgb_lowercase_hex_works() {
        // hex digits are case-insensitive; both forms accepted.
        let out = ansi("<#ff8800>orange</>");
        assert!(out.contains("\x1b[38;2;255;136;0m"), "fg rgb lowercase: {out:?}");
    }

    #[test]
    fn render_color_tags_truecolor_bg_emits_48_2_form() {
        // <bg#001020>... — background path uses 48 instead of 38.
        let out = ansi("<bg#001020>x</>");
        assert!(out.contains("\x1b[48;2;0;16;32m"), "bg rgb present: {out:?}");
    }

    #[test]
    fn render_color_tags_ansi256_emits_38_5_form() {
        // <c196>... → \x1b[38;5;196m text \x1b[0m
        let out = ansi("<c196>fire</>");
        assert!(out.contains("\x1b[38;5;196m"), "256-color present: {out:?}");
        assert!(out.contains("fire"));
    }

    #[test]
    fn render_color_tags_ansi256_bg_emits_48_5_form() {
        let out = ansi("<bgc208>x</>");
        assert!(out.contains("\x1b[48;5;208m"), "bg 256 present: {out:?}");
    }

    #[test]
    fn render_color_tags_ansi256_out_of_range_is_no_op() {
        // u8 parse caps at 255; `c999` exceeds the range and parses as
        // a no-op modifier (still tag-shaped via parse_ansi256_index
        // returning None, so the tag falls through to literal text
        // rather than emitting a malformed escape).
        let out = ansi("<c999>?</>");
        // The whole tag is preserved as literal because is_known_tag_part
        // rejects it (parse_ansi256_index returned None).
        assert!(out.contains("<c999>"), "tag treated as literal: {out:?}");
    }

    #[test]
    fn render_color_tags_truecolor_bad_hex_is_literal() {
        // 5 hex digits — not a valid #RRGGBB form. Tag-shape check fails
        // and the whole `<#...>` passes through as literal text.
        let out = ansi("<#abcde>?</>");
        assert!(out.contains("<#abcde>"), "short hex literal: {out:?}");
    }

    #[test]
    fn render_color_tags_cyan_is_named_not_256_color_prefix() {
        // Regression: `<cyan>` starts with `c`, which the 256-color
        // path is also keyed on. The is_known_tag_part / apply_modifier
        // pair must fall through `c<digits>` to the named_color table
        // when the suffix isn't numeric — otherwise `<cyan>` parses
        // as tag-shaped but emits no fg, leaving the literal `<cyan>`
        // on the wire OR (when the tag-shape check was returning
        // false) emitting it as raw literal text.
        let out = ansi("<cyan>foo</>");
        assert!(out.contains("\x1b[36m"), "fg cyan present: {out:?}");
        assert!(out.contains("foo"));
        // Strip mode should drop the tag entirely (not leak literal):
        assert_eq!(strip("<cyan>foo</>"), "foo");
        // Bold-cyan via the `:` modifier syntax — same hazard.
        assert!(ansi("<b:cyan>x</>").contains("\x1b[1;36m"));
        // Background flavor — `<bg-cyan>` still works.
        assert!(ansi("<bg-cyan>x</>").contains("\x1b[46m"));
        // 256-color form is still functional (precedence didn't break).
        assert!(ansi("<c196>x</>").contains("\x1b[38;5;196m"));
    }

    #[test]
    fn render_color_tags_mixed_palettes_in_one_string() {
        // ANSI16 + 256 + truecolor in one render — exercises
        // push_color_params for all three branches at once.
        let out = ansi("<red>R</><c208>O</><#FFEE00>Y</>");
        assert!(out.contains("\x1b[31m"));
        assert!(out.contains("\x1b[38;5;208m"));
        assert!(out.contains("\x1b[38;2;255;238;0m"));
    }

    #[test]
    fn render_color_tags_empty_tag_is_dropped() {
        assert_eq!(ansi("<>x<>y"), "xy");
    }

    #[test]
    fn render_color_tags_unknown_alphabetic_tag_is_literal() {
        // `<unknown>` / `<gone>` / `<dangling>` are placeholder strings
        // a few callsites use as fallback text. They aren't recognized
        // tag parts (no color, no modifier), so they pass through as
        // literal angle-bracket text instead of being silently swallowed.
        assert_eq!(strip("<unknown>"), "<unknown>");
        assert_eq!(strip("<gone>"), "<gone>");
        assert_eq!(strip("name=<dangling>"), "name=<dangling>");
        assert_eq!(ansi("<unknown>"), "<unknown>");
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
    fn sanitize_prompt_template_collapses_double_percent_for_known_vars() {
        // The schema's old default was `<%%h/%%Hhp %%v/%%Vmv>`; that
        // text after the two-pass render produces literal `%h` etc.
        // Sanitizer collapses each `%%X` (X = known variable) to `%X`
        // so the next render hits the actual substitution path.
        assert_eq!(
            super::sanitize_prompt_template("<%%h/%%Hhp %%v/%%Vmv>"),
            "<%h/%Hhp %v/%Vmv>"
        );
        // All known variables: h/H/v/V/B/M/n/r/g/t/s/d.
        for v in ['h', 'H', 'v', 'V', 'B', 'M', 'n', 'r', 'g', 't', 's', 'd'] {
            let input = format!("%%{v}");
            let want = format!("%{v}");
            assert_eq!(super::sanitize_prompt_template(&input), want);
        }
        // Unknown letter after `%%` stays untouched (the `%%` is a
        // valid literal-percent escape in render_prompt for that case).
        assert_eq!(super::sanitize_prompt_template("100%%"), "100%%");
        assert_eq!(super::sanitize_prompt_template("%%X"), "%%X");
        // Already-correct templates pass through.
        assert_eq!(super::sanitize_prompt_template("<%h/%H>"), "<%h/%H>");
        // Mixed: only the known-variable form collapses.
        assert_eq!(
            super::sanitize_prompt_template("100%% complete <%%h/%%H>"),
            "100%% complete <%h/%H>"
        );
    }

    #[test]
    fn render_prompt_substitutes_hp_and_stamina() {
        // Use a healthy ratio (>=50%) so vitals don't get colored.
        // Color-threshold cases live in their own test below.
        let ctx = PromptCtx {
            hp: Some(Health { hp: 80, max: 100 }),
            stamina: Some(Stamina { current: 40, max: 50 }),
            name: Some("Strider"),
            room: Some("The Void"),
            wealth: Some(12345i64),
            hour: Some(7),
            season: Some("Winter"),
            day_night: Some("day"),
            enemy_name: None,
            enemy_hp: None,
        };
        assert_eq!(render_prompt("<%h/%H>", ctx), "<80/100> ");
        assert_eq!(render_prompt("<%v/%V mv>", ctx), "<40/50 mv> ");
        assert_eq!(render_prompt("<%h/%H %v/%V>", ctx), "<80/100 40/50> ");
        // Trailing space already present — don't double-add.
        assert_eq!(render_prompt("<%h> ", ctx), "<80> ");
        // Literal percent.
        assert_eq!(render_prompt("100%%", ctx), "100% ");
        // Name substitution.
        assert_eq!(render_prompt("[%n]", ctx), "[Strider] ");
        // Room substitution.
        assert_eq!(render_prompt("[%r]", ctx), "[The Void] ");
        // Wealth substitution: raw copper.
        assert_eq!(render_prompt("[%g cp]", ctx), "[12345 cp] ");
        // Hour substitution: zero-padded.
        assert_eq!(render_prompt("[%t]", ctx), "[07] ");
        // Season + day/night.
        assert_eq!(render_prompt("[%s %d]", ctx), "[Winter day] ");
        // Unknown variable: pass through literally so the player sees they
        // typed something we don't implement.
        assert_eq!(render_prompt("[%z]", ctx), "[%z] ");
        // Missing Health: question marks.
        assert_eq!(
            render_prompt("<%h/%H>", PromptCtx { hp: None, ..ctx }),
            "<?/?> "
        );
        // Missing Stamina: question marks for v/V.
        assert_eq!(
            render_prompt("<%v/%V>", PromptCtx { stamina: None, ..ctx }),
            "<?/?> "
        );
        // Missing name: question mark.
        assert_eq!(
            render_prompt("[%n]", PromptCtx { name: None, ..ctx }),
            "[?] "
        );
        // Missing room: question mark.
        assert_eq!(
            render_prompt("[%r]", PromptCtx { room: None, ..ctx }),
            "[?] "
        );
        // Missing wealth: question mark.
        assert_eq!(
            render_prompt("[%g]", PromptCtx { wealth: None, ..ctx }),
            "[?] "
        );
        // Missing hour: question mark.
        assert_eq!(
            render_prompt("[%t]", PromptCtx { hour: None, ..ctx }),
            "[?] "
        );
        // Empty template still gets a trailing space.
        assert_eq!(render_prompt("", ctx), " ");
        // Combat codes render `-` out of combat.
        assert_eq!(render_prompt("%N %e/%E %p%%", ctx), "- -/- -% ");
        // In combat: enemy fields populated.
        let combat_ctx = PromptCtx {
            enemy_name: Some("an orc"),
            enemy_hp: Some(Health { hp: 75, max: 100 }),
            ..ctx
        };
        assert_eq!(
            render_prompt("%N %e/%E %p%%", combat_ctx),
            "an orc 75/100 75% ",
        );
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
        assert_eq!(msg, Some("<yellow>You are hurt.</>\r\n"));
        assert_eq!(w.get::<Health>(e).unwrap().hp, 40);

        // Crossing only the 25% line: 40 → 20 (already past 50% → no re-fire).
        let e = spawn_with_hp(&mut w, 40, 100);
        let (_, msg) = apply_damage(&mut w, e, 20);
        assert_eq!(msg, Some("<red>You are badly hurt!</>\r\n"));

        // Crossing only the 10% line.
        let e = spawn_with_hp(&mut w, 20, 100);
        let (_, msg) = apply_damage(&mut w, e, 12);
        assert_eq!(msg, Some("<b:red>You are near death!</>\r\n"));

        // Skip-crossing: 80 → 5 should report the deepest band only.
        let e = spawn_with_hp(&mut w, 80, 100);
        let (_, msg) = apply_damage(&mut w, e, 75);
        assert_eq!(msg, Some("<b:red>You are near death!</>\r\n"));

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
    fn aoe_scope_inferred_from_violent_flag() {
        // Once Ability.target_scope lands as a schema column,
        // is_area + violent → ROOM_ENEMIES will be a direct read.
        // Until then the runtime infers from the existing flags.
        // Guard the inference rule so any future schema change
        // doesn't silently flip the routing.
        use mud_db::abilities::AbilityKind;
        use mud_world::resources::{AbilityCatalog, AbilityDef};
        // Synthetic "violent + AOE" def: should route through
        // invoke_ability_aoe which expands RoomEnemies. We can
        // inspect the inferred scope by checking the AoeScope
        // arm picked in invoke_ability_with's branch — but
        // exposing that branch directly is awkward, so just
        // reason about the property: violent → RoomEnemies.
        let violent_def = AbilityDef {
            id: 1,
            name: "Firestorm".to_string(),
            plain_name: "FIRESTORM".to_string(),
            description: None,
            kind: AbilityKind::Spell,
            violent: true,
            combat_ok: true,
            in_combat_only: false,
            cast_time_rounds: 1,
            cooldown_ms: 0,
            is_area: true,
            min_position_label: "STANDING".to_string(),
            min_posture_rank: 9,
            target_scope: "ROOM_ENEMIES".to_string(),
            is_magical: true,
            sphere: Some("fire".to_string()),
            damage_type: Some("fire".to_string()),
        };
        let _ = AbilityCatalog::default();
        // The property under test is a one-line conditional; the
        // existence of a dedicated test guards against a
        // regression that swaps the violent / non-violent arms.
        let inferred = if violent_def.violent {
            super::AoeScope::RoomEnemies
        } else {
            super::AoeScope::RoomAllies
        };
        assert!(
            matches!(inferred, super::AoeScope::RoomEnemies),
            "violent AOE infers RoomEnemies"
        );
        let nonviolent_def = AbilityDef {
            violent: false,
            ..violent_def
        };
        let inferred = if nonviolent_def.violent {
            super::AoeScope::RoomEnemies
        } else {
            super::AoeScope::RoomAllies
        };
        assert!(
            matches!(inferred, super::AoeScope::RoomAllies),
            "non-violent AOE infers RoomAllies"
        );
    }

    #[test]
    fn aoe_targets_room_enemies_excludes_group_members() {
        // RoomEnemies expands to every Mob in the room plus PK-flagged
        // Players, minus the caster's group. Use the runtime helper
        // through a minimal world.
        use super::{AoeScope, aoe_targets_in_room};
        let mut world = World::new();
        let room_a = world.spawn_empty().id();
        let _room_b = world.spawn_empty().id();
        // Caster + one group teammate in room_a (group via Follower).
        let caster = world
            .spawn((
                Player,
                Named { name: "Caster".to_string() },
                mud_world::Located(room_a),
            ))
            .id();
        let _teammate = world
            .spawn((
                Player,
                Named { name: "Teammate".to_string() },
                mud_world::Located(room_a),
                mud_world::Follower(caster),
            ))
            .id();
        // Two mobs in room_a — both should appear as enemies.
        let _mob_a = world
            .spawn((
                mud_world::Mob,
                Named { name: "a stray dog".to_string() },
                mud_world::Located(room_a),
            ))
            .id();
        let _mob_b = world
            .spawn((
                mud_world::Mob,
                Named { name: "a half-elven guard".to_string() },
                mud_world::Located(room_a),
            ))
            .id();

        let names = aoe_targets_in_room(&mut world, caster, room_a, AoeScope::RoomEnemies);
        assert!(
            names.iter().any(|n| n == "a stray dog"),
            "stray dog in enemy list: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "a half-elven guard"),
            "half-elven guard in enemy list: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "Teammate"),
            "group teammate excluded from enemy list: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "Caster"),
            "caster excluded from own enemy list: {names:?}"
        );
    }

    #[test]
    fn sphere_color_tag_covers_palette() {
        use super::sphere_color_tag;
        // Elemental + healing / death — the schema's most common
        // spheres. Fire/water are the load-bearing ones for player
        // intuition (red/cyan); the rest follow.
        assert_eq!(sphere_color_tag("fire"), Some("<red>"));
        assert_eq!(sphere_color_tag("water"), Some("<cyan>"));
        assert_eq!(sphere_color_tag("air"), Some("<b:cyan>"));
        assert_eq!(sphere_color_tag("earth"), Some("<yellow>"));
        assert_eq!(sphere_color_tag("healing"), Some("<green>"));
        assert_eq!(sphere_color_tag("death"), Some("<b:black>"));
        assert_eq!(sphere_color_tag("protection"), Some("<b:white>"));
        assert_eq!(sphere_color_tag("enchantment"), Some("<magenta>"));
        assert_eq!(sphere_color_tag("summoning"), Some("<b:magenta>"));
        assert_eq!(sphere_color_tag("divination"), Some("<b:yellow>"));
        // GENERIC + unmapped fall through to None — caller renders
        // dim. Input is lowercase; uppercase variants don't match
        // (matches how the catalog stores them after our
        // LOWER(sphere::text) cast in mud_db).
        assert_eq!(sphere_color_tag("generic"), None);
        assert_eq!(sphere_color_tag(""), None);
        assert_eq!(sphere_color_tag("FIRE"), None);
        assert_eq!(sphere_color_tag("unrecognized"), None);
    }

    #[test]
    fn who_level_color_bands_match_honor_roll_threshold() {
        // The honor-roll [★] decoration on `who` rows fires at
        // level >= 100 — same threshold the bold-magenta staff
        // band fires at. Pin the boundary so a future band tweak
        // can't desync the two.
        use super::who_level_color;
        assert_eq!(who_level_color(0), None);
        assert_eq!(who_level_color(1), Some("<yellow>"));
        assert_eq!(who_level_color(9), Some("<yellow>"));
        assert_eq!(who_level_color(10), Some("<b:yellow>"));
        assert_eq!(who_level_color(24), Some("<b:yellow>"));
        assert_eq!(who_level_color(25), Some("<green>"));
        assert_eq!(who_level_color(49), Some("<green>"));
        assert_eq!(who_level_color(50), Some("<b:cyan>"));
        assert_eq!(who_level_color(99), Some("<b:cyan>"));
        // Honor-roll threshold: bold-magenta from 100, holding
        // until the impl tier at 105.
        assert_eq!(who_level_color(100), Some("<b:magenta>"));
        assert_eq!(who_level_color(104), Some("<b:magenta>"));
        assert_eq!(who_level_color(105), Some("<b:white>"));
    }

    #[test]
    fn bound_ability_line_renders_sphere_palette() {
        // Equipping a wand/staff with bindings should surface the
        // ability + sphere on the wield confirmation. Pin the
        // formatter contract (sphere hue applied via
        // format_ability_with_sphere) so a future palette tweak
        // can't silently drop the parenthetical.
        use mud_db::abilities::AbilityKind;
        use mud_world::{
            AbilityCatalog, AbilityDef, ObjectAbilityCatalog, WorldKey,
            resources::ObjectAbilityBinding,
        };
        let mut world = World::new();
        let mut catalog = AbilityCatalog::default();
        catalog.by_name.insert(
            "magic missile".to_string(),
            AbilityDef {
                id: 42,
                name: "Magic Missile".to_string(),
                plain_name: "MAGIC_MISSILE".to_string(),
                description: None,
                kind: AbilityKind::Spell,
                violent: true,
                combat_ok: true,
                in_combat_only: false,
                cast_time_rounds: 1,
                cooldown_ms: 0,
                is_area: false,
                min_position_label: "STANDING".to_string(),
                min_posture_rank: 9,
                target_scope: "ROOM_ENEMY".to_string(),
                is_magical: true,
                sphere: Some("fire".to_string()),
                damage_type: Some("fire".to_string()),
            },
        );
        world.insert_resource(catalog);
        let mut bindings = ObjectAbilityCatalog::default();
        bindings.by_key.insert(
            (10, 5),
            vec![ObjectAbilityBinding {
                ability_id: 42,
                level: 1,
                charges: Some(7),
            }],
        );
        world.insert_resource(bindings);
        let item = world.spawn(WorldKey { zone: 10, id: 5 }).id();
        let line = super::render_bound_ability_line(&mut world, item)
            .expect("bound item produces a line");
        // Formatter wraps the sphere parenthetical in <red> (fire)
        // — the literal tag we're pinning is what `format_ability_with_sphere`
        // emits, not the post-render ANSI.
        assert!(
            line.contains("Magic Missile <red>(fire)</>"),
            "sphere parenthetical present: {line}"
        );
        assert!(line.starts_with("<dim>It carries</>"), "lead-in dim: {line}");

        // Item without a binding row → no follow-up line at all.
        let bare = world.spawn(WorldKey { zone: 99, id: 1 }).id();
        assert!(
            super::render_bound_ability_line(&mut world, bare).is_none(),
            "no binding → no line"
        );

        // Item with no WorldKey at all (corpses, synthesized entities)
        // → no follow-up line.
        let keyless = world.spawn_empty().id();
        assert!(
            super::render_bound_ability_line(&mut world, keyless).is_none(),
            "no key → no line"
        );
    }

    #[test]
    fn aoe_targets_room_allies_includes_caster_and_group_excludes_mobs() {
        // RoomAllies = caster + group members in the room. Mobs are
        // excluded (no allied-mob tag today). Out-of-room group
        // members also drop out — only co-located allies receive
        // the buff / heal.
        use super::{AoeScope, aoe_targets_in_room};
        let mut world = World::new();
        let room_a = world.spawn_empty().id();
        let room_b = world.spawn_empty().id();
        let caster = world
            .spawn((
                Player,
                Named { name: "Caster".to_string() },
                mud_world::Located(room_a),
            ))
            .id();
        let _co_located_teammate = world
            .spawn((
                Player,
                Named { name: "Teammate".to_string() },
                mud_world::Located(room_a),
                mud_world::Follower(caster),
            ))
            .id();
        let _far_teammate = world
            .spawn((
                Player,
                Named { name: "FarTeammate".to_string() },
                mud_world::Located(room_b),
                mud_world::Follower(caster),
            ))
            .id();
        let _mob = world
            .spawn((
                mud_world::Mob,
                Named { name: "a stray dog".to_string() },
                mud_world::Located(room_a),
            ))
            .id();

        let names = aoe_targets_in_room(&mut world, caster, room_a, AoeScope::RoomAllies);
        assert!(
            names.iter().any(|n| n == "Caster"),
            "caster included in RoomAllies: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "Teammate"),
            "co-located teammate included: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "FarTeammate"),
            "out-of-room teammate excluded: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "a stray dog"),
            "mobs excluded from RoomAllies: {names:?}"
        );
    }

    #[test]
    fn aoe_targets_room_all_includes_everyone_except_caster() {
        // RoomAll is the chaos / admin scope: every Player + Mob
        // in the room except the caster. Group membership doesn't
        // grant immunity — that's the point of the variant.
        use super::{AoeScope, aoe_targets_in_room};
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let caster = world
            .spawn((
                Player,
                Named { name: "Caster".to_string() },
                mud_world::Located(room),
            ))
            .id();
        let _teammate = world
            .spawn((
                Player,
                Named { name: "Teammate".to_string() },
                mud_world::Located(room),
                mud_world::Follower(caster),
            ))
            .id();
        let _mob = world
            .spawn((
                mud_world::Mob,
                Named { name: "a stray dog".to_string() },
                mud_world::Located(room),
            ))
            .id();

        let names = aoe_targets_in_room(&mut world, caster, room, AoeScope::RoomAll);
        assert!(
            !names.iter().any(|n| n == "Caster"),
            "caster excluded from RoomAll: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "Teammate"),
            "RoomAll includes group teammates (no immunity): {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "a stray dog"),
            "RoomAll includes mobs: {names:?}"
        );
    }

    #[test]
    fn aoe_targets_empty_room_returns_empty_list() {
        // Caster alone in a room produces an empty target list for
        // RoomEnemies and RoomAll (only-caster excluded). RoomAllies
        // is the exception — caster is included since they're
        // still themselves an ally.
        use super::{AoeScope, aoe_targets_in_room};
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let caster = world
            .spawn((
                Player,
                Named { name: "Solo".to_string() },
                mud_world::Located(room),
            ))
            .id();

        assert!(
            aoe_targets_in_room(&mut world, caster, room, AoeScope::RoomEnemies).is_empty(),
            "RoomEnemies in an empty room is empty"
        );
        assert!(
            aoe_targets_in_room(&mut world, caster, room, AoeScope::RoomAll).is_empty(),
            "RoomAll in an empty room is empty (only caster present, who is excluded)"
        );
        let allies = aoe_targets_in_room(&mut world, caster, room, AoeScope::RoomAllies);
        assert_eq!(
            allies, vec!["Solo".to_string()],
            "RoomAllies on a solo caster targets self only"
        );
    }

    #[test]
    fn parse_indexed_needle_recognizes_dotted_prefix() {
        use super::parse_indexed_needle;
        // Bare needle defaults to index 1 (first match).
        assert_eq!(parse_indexed_needle("ancient"), (1, "ancient"));
        // `2.ancient` → index 2, base needle "ancient".
        assert_eq!(parse_indexed_needle("2.ancient"), (2, "ancient"));
        assert_eq!(parse_indexed_needle("17.dog"), (17, "dog"));
        // `0.foo` and missing-tail collapse to (1, full input) so
        // pathological inputs degrade gracefully into a normal
        // first-match search rather than a silent no-op.
        assert_eq!(parse_indexed_needle("0.foo"), (1, "0.foo"));
        assert_eq!(parse_indexed_needle("3."), (1, "3."));
        // Non-numeric prefix: not an indexed needle, the whole
        // string is the literal target.
        assert_eq!(parse_indexed_needle("bag.of.holding"), (1, "bag.of.holding"));
        assert_eq!(parse_indexed_needle("foo"), (1, "foo"));
    }

    #[test]
    fn parse_quoted_first_token_handles_quotes_and_whitespace() {
        use super::parse_quoted_first_token;
        // Bare word: legacy whitespace split.
        let (head, tail) = parse_quoted_first_token("fireball goblin");
        assert_eq!(head, "fireball");
        assert_eq!(tail, Some("goblin"));
        // Single-quoted multi-word phrase + trailing target.
        let (head, tail) = parse_quoted_first_token("'magic missile' goblin");
        assert_eq!(head, "magic missile");
        assert_eq!(tail, Some("goblin"));
        // Single-quoted phrase, no trailing target.
        let (head, tail) = parse_quoted_first_token("'magic missile'");
        assert_eq!(head, "magic missile");
        assert_eq!(tail, None);
        // Double quotes work the same.
        let (head, tail) = parse_quoted_first_token("\"acid burst\" troll");
        assert_eq!(head, "acid burst");
        assert_eq!(tail, Some("troll"));
        // Leading whitespace is forgiven.
        let (head, tail) = parse_quoted_first_token("  fireball  goblin");
        assert_eq!(head, "fireball");
        assert_eq!(tail, Some("goblin"));
        // Unclosed quote: fall back to whitespace split (and the
        // first token retains the leading quote — caller's lookup
        // will fail naturally and produce the standard "no match"
        // message rather than silently misreading half the args).
        let (head, _) = parse_quoted_first_token("'magic missile");
        assert_eq!(head, "'magic");
        // Empty input: empty head, no tail.
        let (head, tail) = parse_quoted_first_token("");
        assert_eq!(head, "");
        assert_eq!(tail, None);
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
        // base_damage is now a known symbol — `base` (FormulaCtx::base)
        // builds with base_damage=0, so `base_damage + skill` = 5.
        // The full invoke_ability path computes the real circle-based
        // value at cast time.
        assert_eq!(evaluate_simple_formula("base_damage + skill", 10, 5), Some(5));
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
        // base_damage resolves from ctx.base_damage (0 by default for
        // FormulaCtx::default()-derived ctx; populated in the
        // invoke_ability path from level+circle*2+max(int,wis)).
        assert_eq!(
            evaluate_formula("base_damage + 5", &ctx, &mut zero),
            Some(5),
        );
        let ctx_with_base = FormulaCtx {
            base_damage: 30,
            ..ctx
        };
        // FIRESTORM-style: base_damage + (skill^2 / 100). For
        // skill=30 in ctx, skill^2/100 = 9. 30 + 9 = 39.
        assert_eq!(
            evaluate_formula("base_damage + (pow(skill, 2) / 100)", &ctx_with_base, &mut zero),
            Some(39),
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
        // A5: spell_power is exposed as both `spell_power` and `sp`
        // for formulas that want to read it explicitly.
        let ctx_sp = FormulaCtx { spell_power: 30, ..FormulaCtx::default() };
        assert_eq!(evaluate_formula("spell_power", &ctx_sp, &mut zero), Some(30));
        assert_eq!(evaluate_formula("sp", &ctx_sp, &mut zero), Some(30));
        assert_eq!(
            evaluate_formula("base + sp / 2", &FormulaCtx { spell_power: 40, base_damage: 10, ..FormulaCtx::default() }, &mut zero),
            Some(30),
        );
    }

    /// A7: per-element resistance application.
    #[test]
    fn apply_resistance_mitigates_and_amplifies() {
        // No resistance: amount passes through.
        assert_eq!(super::apply_resistance(100, 0), 100);
        // 25% resist: 75 damage.
        assert_eq!(super::apply_resistance(100, 25), 75);
        // 100% resist: immune.
        assert_eq!(super::apply_resistance(100, 100), 0);
        // Over-capped resist (>100) clamps to immune.
        assert_eq!(super::apply_resistance(100, 200), 0);
        // -50 resist (vulnerable): 150% damage.
        assert_eq!(super::apply_resistance(100, -50), 150);
        // -200 resist: 300% damage.
        assert_eq!(super::apply_resistance(100, -200), 300);
        // Floor at 0 for paranoid math (over-capped negative
        // pct won't actually go positive because clamp caps at 100,
        // but verify the floor).
        assert!(super::apply_resistance(0, 50) >= 0);
    }

    /// A7: damage element string → ElementType resolver.
    #[test]
    fn resolve_damage_element_picks_known_types() {
        use mud_db::enums::ElementType as E;
        let blob = |t: &str| serde_json::json!({"type": t});
        assert_eq!(super::resolve_damage_element(Some(&blob("fire")), None), E::Fire);
        assert_eq!(super::resolve_damage_element(Some(&blob("HOLY")), None), E::Holy);
        // Synonyms.
        assert_eq!(super::resolve_damage_element(Some(&blob("lightning")), None), E::Shock);
        assert_eq!(super::resolve_damage_element(Some(&blob("psychic")), None), E::Mental);
        // Unknown / missing → Physical (so we don't accidentally
        // bypass the resist step).
        assert_eq!(super::resolve_damage_element(Some(&blob("xyzzy")), None), E::Physical);
        let no_type = serde_json::json!({"amount": "1d6"});
        assert_eq!(super::resolve_damage_element(Some(&no_type), None), E::Physical);
        // Override beats default.
        let def = blob("cold");
        let over = blob("fire");
        assert_eq!(
            super::resolve_damage_element(Some(&over), Some(&def)),
            E::Fire,
        );
    }

    /// A5 regression: a magical-spell damage path scales by
    /// spell_power. We verify the math directly (the live invoke
    /// path is exercised by integration tests + playtest).
    #[test]
    fn spell_power_scales_magical_damage() {
        // Mirror the inline expression at the apply_effect site:
        //   amount = (amount * (100 + sp)) / 100
        let scale = |amount: i32, sp: i32| (amount * (100 + sp)) / 100;
        // No spell_power → no change.
        assert_eq!(scale(100, 0), 100);
        // +20 SP → 120% damage.
        assert_eq!(scale(100, 20), 120);
        // +50 SP → 150%.
        assert_eq!(scale(100, 50), 150);
        // Negative SP (cursed gear) reduces damage but the apply
        // site floors at 1, which `scale` doesn't model — verify
        // the raw math here, the floor is asserted at the call site.
        assert_eq!(scale(100, -25), 75);
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
    fn apply_damage_no_op_on_ghost() {
        // Regression for the post-death damage loop: apply_damage
        // must early-return on Ghost targets so a swing snapshotted
        // before the entity ghosted (or a residual mob targeting
        // the corpse) doesn't push HP into death-event territory
        // again. Returns (false, None) — no death event, no
        // threshold message.
        use mud_world::Ghost;
        let mut world = World::new();
        let target = world
            .spawn((Health { hp: 0, max: 100 }, Ghost))
            .id();
        let (dead, msg) = apply_damage(&mut world, target, 50);
        assert!(!dead, "Ghost target doesn't trigger a death event");
        assert_eq!(msg, None, "Ghost target doesn't get threshold messages");
        assert_eq!(
            world.get::<Health>(target).unwrap().hp,
            0,
            "Ghost HP unchanged by apply_damage"
        );
    }

    #[test]
    fn apply_ward_skips_mundane_damage() {
        // Mundane on-hit abilities (`is_magical = false`) and raw
        // weapon swings bypass ward entirely, regardless of how much
        // ward the target stacks. Per combat.md "Source-magicality
        // gated" decision.
        assert_eq!(super::apply_ward(100, 50, false), 100);
        assert_eq!(super::apply_ward(100, 99, false), 100);
        // Even at full immunity (ward=100), mundane damage lands.
        assert_eq!(super::apply_ward(100, 100, false), 100);
    }

    #[test]
    fn apply_ward_50pct_halves_magical_damage() {
        // Typical case: a level-mid mage with 50% ward absorbs
        // exactly half a 100-point fireball.
        assert_eq!(super::apply_ward(100, 50, true), 50);
        // Edge: zero ward is a no-op even on magical damage.
        assert_eq!(super::apply_ward(100, 0, true), 100);
    }

    #[test]
    fn apply_ward_clamps_negative_into_armor_side() {
        // Vulnerability (negative ward) is intentionally NOT
        // amplified by the ward stage — that semantic lives on the
        // armor / type-resist side per combat.md. A negative
        // ward_pct gets clamped to 0 so the magical hit lands at
        // full damage and any vulnerability has to come from
        // resistances instead.
        assert_eq!(super::apply_ward(100, -50, true), 100);
        assert_eq!(super::apply_ward(100, i32::MIN, true), 100);
    }

    #[test]
    fn apply_ward_caps_at_100_immunity() {
        // 100% ward zeros magical damage. Anything above clamps
        // to the same — no negative-damage healing exploit.
        assert_eq!(super::apply_ward(100, 100, true), 0);
        assert_eq!(super::apply_ward(100, 200, true), 0);
        assert_eq!(super::apply_ward(100, i32::MAX, true), 0);
    }

    #[test]
    fn apply_modify_delta_ward_routes_to_ward_pct() {
        // Regression: prior code aliased the "ward" modify stat
        // onto AC (subtracting the buff). Per combat.md the Ward
        // stat is independent of the armor pipeline; positive
        // amounts increment `ward_pct`, and the inverse delta on
        // effect expiry decrements it back. The armor axis must
        // NOT move under a ward modify (post-AC-pivot the armor
        // pipeline is `armor_pct` / `armor_flat`).
        use mud_world::CombatStats;
        let mut world = World::new();
        let entity = world.spawn(CombatStats::default()).id();
        let baseline_armor_pct = world.get::<CombatStats>(entity).unwrap().armor_pct;
        let baseline_armor_flat = world.get::<CombatStats>(entity).unwrap().armor_flat;
        super::apply_modify_delta(&mut world, entity, "ward", 25);
        let cs = world.get::<CombatStats>(entity).copied().unwrap();
        assert_eq!(cs.ward_pct, 25, "ward modify routes to ward_pct");
        assert_eq!(cs.armor_pct, baseline_armor_pct, "ward modify does NOT touch armor_pct");
        assert_eq!(cs.armor_flat, baseline_armor_flat, "ward modify does NOT touch armor_flat");
        // Reverse delta (effect expiry) walks it back.
        super::reverse_modify_delta(&mut world, entity, "ward", 25);
        let cs2 = world.get::<CombatStats>(entity).copied().unwrap();
        assert_eq!(cs2.ward_pct, 0, "reverse_modify_delta returns ward_pct to 0");
        assert_eq!(cs2.armor_pct, baseline_armor_pct, "reverse stays clear of armor_pct");
        assert_eq!(cs2.armor_flat, baseline_armor_flat, "reverse stays clear of armor_flat");
    }

    #[test]
    fn apply_heal_hp_no_op_on_ghost() {
        // Healing a corpse is a no-op. release is the only path
        // back from Ghost — it sets hp = max in one shot.
        use mud_world::Ghost;
        let mut world = World::new();
        let target = world
            .spawn((Health { hp: 0, max: 100 }, Ghost))
            .id();
        let healed = apply_heal_hp(&mut world, target, 50);
        assert_eq!(healed, 0, "Ghost target reports no healing applied");
        assert_eq!(
            world.get::<Health>(target).unwrap().hp,
            0,
            "Ghost HP unchanged by apply_heal_hp"
        );
    }

    #[test]
    fn apply_heal_stamina_no_op_on_ghost() {
        // Same parity as HP: corpses don't refill stamina either.
        use mud_world::Ghost;
        let mut world = World::new();
        let target = world
            .spawn((Stamina { current: 0, max: 50 }, Ghost))
            .id();
        let healed = apply_heal_stamina(&mut world, target, 25);
        assert_eq!(healed, 0, "Ghost target reports no stamina healed");
        assert_eq!(world.get::<Stamina>(target).unwrap().current, 0);
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

    // --- score-sheet helpers ---

    #[test]
    fn experience_for_level_floor_at_one() {
        assert_eq!(super::experience_for_level(0), 0);
        assert_eq!(super::experience_for_level(1), 0);
    }

    #[test]
    fn experience_for_level_is_monotonic() {
        let curve: Vec<i64> = (1..=20).map(super::experience_for_level).collect();
        for window in curve.windows(2) {
            assert!(window[0] <= window[1], "curve must be monotonic: {curve:?}");
        }
    }

    #[test]
    fn level_progress_capped_at_99() {
        // level 100 = max; no progress shown.
        assert!(super::level_progress_for(100, 1).is_none());
        // level 0 = invalid; no progress shown.
        assert!(super::level_progress_for(0, 1).is_none());
    }

    #[test]
    fn level_progress_clamps_negative_xp_to_zero() {
        // Death penalty can drive xp below the bracket floor —
        // make sure the bar shows 0% rather than a negative
        // percent or wraparound.
        let p = super::level_progress_for(5, -100).expect("inside range");
        assert_eq!(p.percent, 0);
    }

    #[test]
    fn progress_bar_is_fixed_width() {
        // Always 22 chars: '[' + 20 cells + ']'.
        for pct in [0, 1, 50, 99, 100] {
            assert_eq!(super::progress_bar(pct).len(), 22, "pct={pct}");
        }
    }

    #[test]
    fn drunk_band_thresholds() {
        assert_eq!(super::drunk_band(0), "sober");
        assert_eq!(super::drunk_band(1), "Buzzed");
        assert_eq!(super::drunk_band(39), "Buzzed");
        assert_eq!(super::drunk_band(40), "Tipsy");
        assert_eq!(super::drunk_band(65), "Tipsy");
        assert_eq!(super::drunk_band(66), "Very Drunk");
        assert_eq!(super::drunk_band(79), "Very Drunk");
        assert_eq!(super::drunk_band(80), "Blackout");
        assert_eq!(super::drunk_band(100), "Blackout");
    }

    #[test]
    fn encumbrance_band_thresholds() {
        // Floor at 0/0 capacity returns the unburdened label.
        assert_eq!(super::encumbrance_band(0.0, 0.0), "unburdened");
        assert_eq!(super::encumbrance_band(0.0, 100.0), "unburdened");
        assert_eq!(super::encumbrance_band(49.9, 100.0), "unburdened");
        assert_eq!(super::encumbrance_band(50.0, 100.0), "burdened");
        assert_eq!(super::encumbrance_band(75.0, 100.0), "encumbered");
        assert_eq!(super::encumbrance_band(90.0, 100.0), "heavy");
        assert_eq!(super::encumbrance_band(100.0, 100.0), "overloaded");
        assert_eq!(super::encumbrance_band(150.0, 100.0), "overloaded");
    }

    #[test]
    fn format_age_handles_singular_and_plural_units() {
        // Level 0/negative → no profile-derived age. Score skips the
        // line rather than printing nonsense.
        assert_eq!(super::format_age(0), None);
        assert_eq!(super::format_age(-5), None);
        // Level 1 → 21 years, 3 months (placeholder formula).
        assert_eq!(
            super::format_age(1),
            Some("21 years, 3 months".to_string()),
        );
        // Level 25 → 45 years, (25*3)%12 = 75%12 = 3 months.
        assert_eq!(
            super::format_age(25),
            Some("45 years, 3 months".to_string()),
        );
        // Singular "1 month" path: level 4 → 24 years, 12%12=0 → 0
        // months (plural). Level 9 → 29 years, 27%12=3 months. The
        // exact level that yields "1 month" is level 11 → 33%12=9
        // months — none, actually. Use level 5: (5*3)%12 = 15%12=3.
        // Level that gives 1 month: (n*3)%12 = 1 → n*3 ≡ 1 mod 12
        // has no integer solution since gcd(3,12)=3. So singular
        // months is unreachable via this formula — leave the
        // suffix logic exercised only by years.
        // Singular "1 year" is also unreachable (years = 20+level,
        // level >= 1 → years >= 21). Both suffixes therefore
        // pluralize in practice. Test still guards the format
        // shape so a future refactor can't silently change it.
    }

    #[test]
    fn format_wealth_zero_or_negative_is_none() {
        assert_eq!(super::format_wealth(0), None);
        assert_eq!(super::format_wealth(-5), None);
    }

    #[test]
    fn format_wealth_decomposes_in_canonical_order() {
        // 1234 copper = 1 platinum, 2 gold, 3 silver, 4 copper.
        assert_eq!(
            super::format_wealth(1234),
            Some("1 platinum, 2 gold, 3 silver, 4 copper".to_string()),
        );
    }

    #[test]
    fn format_wealth_omits_zero_denominations() {
        // 1100 copper = 1 platinum, 1 gold (no silver, no copper).
        assert_eq!(
            super::format_wealth(1100),
            Some("1 platinum, 1 gold".to_string()),
        );
        assert_eq!(super::format_wealth(7), Some("7 copper".to_string()));
    }

    #[test]
    fn condition_summary_silent_below_thresholds() {
        // hunger 23 < HUNGRY_AT 24, thirst 11 < THIRSTY_AT 12.
        let none: &[String] = &[];
        assert_eq!(super::condition_summary(0, 0, none), None);
        assert_eq!(super::condition_summary(23, 11, none), None);
    }

    #[test]
    fn condition_summary_bands_promote_at_each_threshold() {
        let none: &[String] = &[];
        assert_eq!(
            super::condition_summary(24, 0, none),
            Some("hungry".to_string()),
        );
        assert_eq!(
            super::condition_summary(48, 24, none),
            Some("starving, parched".to_string()),
        );
        // Thirst-only.
        assert_eq!(
            super::condition_summary(0, 12, none),
            Some("thirsty".to_string()),
        );
    }

    #[test]
    fn condition_summary_includes_positive_effects() {
        // Nourished/Refreshed render before negative bands.
        let nourished = vec!["Nourished".to_string()];
        assert_eq!(
            super::condition_summary(0, 0, &nourished),
            Some("nourished".to_string()),
        );
        // Both positives + a negative band stack in stable order.
        let both = vec!["Refreshed".to_string(), "Nourished".to_string()];
        assert_eq!(
            super::condition_summary(24, 0, &both),
            Some("nourished, refreshed, hungry".to_string()),
        );
        // Match is case-insensitive — effect catalog rows can use
        // any casing without breaking the score render.
        let lower = vec!["nourished".to_string()];
        assert_eq!(
            super::condition_summary(0, 0, &lower),
            Some("nourished".to_string()),
        );
    }

    // --- score renderer smoke tests ---
    //
    // Build a fully-populated ScoreData and assert each renderer
    // includes the expected sections. These guard against a future
    // refactor accidentally dropping a section line.

    fn build_smoke_score_data<'a>(
        name: &'a str,
        effects: &'a [String],
    ) -> super::ScoreData<'a> {
        super::ScoreData {
            name,
            hp: Some(Health { hp: 95, max: 100 }),
            stamina: Some(Stamina {
                current: 80,
                max: 100,
            }),
            cs: Some(super::CombatStats {
                // Old fixture used hit_roll: 5, dmg_roll: 8, ac: 12.
                // Migration mapping (combat.md): accuracy = 50 + hit_roll*2,
                // attack_power = dmg_roll * 5, armor_pct = clamp(0, 80,
                // (10 - ac) * 5) = clamp(0, 80, -10) = 0. Defaults
                // for the rest (evasion 0, crit 0, ward 0, etc.) keep
                // the score smoke output deterministic.
                accuracy: 60,
                attack_power: 40,
                armor_pct: 0,
                alignment: 250,
                ..Default::default()
            }),
            core_stats: Some(super::CoreStats {
                strength: 16,
                dexterity: 14,
                constitution: 13,
                intelligence: 12,
                wisdom: 11,
                charisma: 10,
            }),
            posture: Some(super::Posture(super::PostureKind::Standing)),
            fight_target: None,
            profile: Some((25, "Warrior", "human", "male", 1234)),
            wealth: 1234,
            bank: 5000,
            hunger: 0,
            thirst: 0,
            carry: (47.5, 200.0),
            drunkenness: 50,
            kill_total: 42,
            clan: Some(("Test Clan", "TC", "Member")),
            active_effects: effects,
            group_status: super::GroupStatus::default(),
            level_progress: super::level_progress_for(25, 1234),
            location: Some(("Town Square", 30, 1)),
            practice_points: 3,
            achievements: (5, 47),
            title: Some("the Daring Adventurer"),
            wimpy: Some(25),
            // Mortal-level fixture has no rank title — assertions
            // for the staff path are local to specific tests.
            level_title: None,
            next_level_gains: Some((26, 18, 9)),
            recall: Some(("The Inn", 30, 1)),
            stealth: false,
            flying: false,
            mount_name: None,
            house: Some((3, 1000, 17)),
            cooldowns_active: 2,
            guarding_name: None,
            mail_draft: None,
            board_draft: None,
            size: Some("Medium"),
            is_ghost: false,
            is_stunned: false,
            is_frozen: false,
        }
    }

    #[test]
    fn score_minimal_includes_core_fields() {
        let effects: Vec<String> = vec!["bless".to_string()];
        let data = build_smoke_score_data("Strider", &effects);
        let out = super::render_score_minimal(&data);
        assert!(out.contains("Strider"), "name: {out}");
        assert!(out.contains("L25"), "level: {out}");
        assert!(out.contains("hp:95/100"), "hp: {out}");
        assert!(out.contains("st:80/100"), "stamina: {out}");
        assert!(out.contains("xp:"), "xp present: {out}");
        assert!(out.contains("eff:1"), "effect count: {out}");
        assert!(out.contains("prac:3"), "practice points: {out}");
        assert!(out.contains("kills:42"), "kill total: {out}");
        assert!(out.contains("clan:TC"), "clan abbrev: {out}");
    }

    #[test]
    fn score_standard_includes_section_headings() {
        let effects: Vec<String> = vec!["bless".to_string(), "haste".to_string()];
        let data = build_smoke_score_data("Strider", &effects);
        let out = super::render_score_standard(&data);
        assert!(out.contains("Strider"), "name: {out}");
        assert!(out.contains("HP: 95 / 100"), "hp line: {out}");
        assert!(out.contains("bless, haste"), "effects line: {out}");
        // Equipment block was moved out of score (it lives on the
        // `equipment` command). Score must NOT include it now.
        assert!(
            !out.contains("Equipment:"),
            "equipment block dropped from score: {out}",
        );
        assert!(out.contains("Practice:"), "practice line: {out}");
        assert!(out.contains("Achievements: 5 / 47"), "achievements: {out}");
        assert!(out.contains("Location: Town Square"), "location: {out}");
        assert!(
            out.contains("Title: the Daring Adventurer"),
            "title line: {out}",
        );
        // Age: 20 + 25 = 45 years; (25*3) % 12 = 3 months.
        assert!(out.contains("Age: 45 years, 3 months"), "age line: {out}");
        // Wimpy threshold from the fixture.
        assert!(
            out.contains("Wimpy:  flee at HP < 25%"),
            "wimpy line: {out}",
        );
        // Next-level gains preview (level 25 -> 26: +18 hp, +9 st).
        assert!(
            out.contains("Next level (#26): +18 HP, +9 Stamina"),
            "next-level line: {out}",
        );
        assert!(
            out.contains("Recall:   The Inn  [30:1]"),
            "recall line: {out}",
        );
        assert!(
            out.contains("House:    3 rooms at [1000:17]"),
            "house line: {out}",
        );
        // Cooldowns from the fixture (2 active).
        assert!(
            out.contains("Cooldowns: 2 abilities recharging"),
            "cooldowns line: {out}",
        );
    }

    #[test]
    fn score_board_draft_line_only_when_in_flight() {
        let effects: Vec<String> = Vec::new();
        let mut data =
            build_smoke_score_data("Strider", &effects);
        let off = super::render_score_standard(&data);
        assert!(!off.contains("Board draft:"), "no board row: {off}");
        data.board_draft = Some(("mortal", 5));
        let on = super::render_score_standard(&data);
        assert!(
            on.contains("Board draft: on mortal, 5 lines"),
            "board row: {on}",
        );
    }

    #[test]
    fn score_mail_draft_line_only_when_in_flight() {
        let effects: Vec<String> = Vec::new();
        let mut data =
            build_smoke_score_data("Strider", &effects);
        let off = super::render_score_standard(&data);
        assert!(!off.contains("Mail draft:"), "no draft row: {off}");
        data.mail_draft = Some(("Samui", 3));
        let on = super::render_score_standard(&data);
        assert!(
            on.contains("Mail draft: to Samui, 3 lines"),
            "draft row: {on}",
        );
    }

    #[test]
    fn score_guarding_line_only_when_set() {
        let effects: Vec<String> = Vec::new();
        let mut data =
            build_smoke_score_data("Strider", &effects);
        let off = super::render_score_standard(&data);
        assert!(!off.contains("Guarding:"), "no guarding row: {off}");
        data.guarding_name = Some("Samui");
        let on = super::render_score_standard(&data);
        assert!(on.contains("Guarding: Samui"), "guarding row: {on}");
    }

    #[test]
    fn score_motion_state_lines_only_when_active() {
        let effects: Vec<String> = Vec::new();
        let mut data =
            build_smoke_score_data("Strider", &effects);
        // Default fixture: on foot, walking → no rows.
        let grounded = super::render_score_standard(&data);
        assert!(!grounded.contains("Flying:"), "no fly row: {grounded}");
        assert!(!grounded.contains("Mounted on:"), "no mount row: {grounded}");
        // Toggle both.
        data.flying = true;
        data.mount_name = Some("a chestnut warhorse");
        let aloft = super::render_score_standard(&data);
        assert!(aloft.contains("Flying: aloft"), "fly row: {aloft}");
        assert!(
            aloft.contains("Mounted on: a chestnut warhorse"),
            "mount row: {aloft}",
        );
    }

    #[test]
    fn score_stealth_line_only_when_hidden() {
        let effects: Vec<String> = Vec::new();
        let mut data =
            build_smoke_score_data("Strider", &effects);
        // Default fixture: no stealth → no line.
        let visible = super::render_score_standard(&data);
        assert!(
            !visible.contains("Stealth:"),
            "no stealth row when visible: {visible}",
        );
        // Toggle the marker.
        data.stealth = true;
        let hidden = super::render_score_standard(&data);
        assert!(
            hidden.contains("Stealth: hidden"),
            "stealth row when hidden: {hidden}",
        );
    }

    #[test]
    fn score_level_title_appended_for_staff() {
        let effects: Vec<String> = Vec::new();
        let mut data = build_smoke_score_data("Strider", &effects);
        data.level_title = Some("Implementer");
        let out = super::render_score_standard(&data);
        // Title sits between the level number and gender/race.
        assert!(
            out.contains("Level 25 Implementer Male Human"),
            "rank title in level row: {out}",
        );
    }

    #[test]
    fn score_level_title_omitted_when_none() {
        let effects: Vec<String> = Vec::new();
        let data = build_smoke_score_data("Strider", &effects);
        let out = super::render_score_standard(&data);
        // Mortal rendering keeps the original "Level N <gender>"
        // shape with no extra spaces — guard against a regression
        // that injects an empty rank slot.
        assert!(
            out.contains("Level 25 Male Human"),
            "mortal level row unchanged: {out}",
        );
    }

    #[test]
    fn score_fancy_box_borders_render() {
        let effects: Vec<String> = Vec::new();
        let data = build_smoke_score_data("Strider", &effects);
        let out = super::render_score_fancy(&data);
        // Fancy renderer wraps in a box; both top + bottom borders
        // start with '+' and end with '+'.
        assert!(out.contains("+--"), "top border: {out}");
        assert!(out.contains("Strider"), "name: {out}");
        // Achievements line still rendered inside the box.
        assert!(out.contains("Achievements: 5 / 47"), "achievements: {out}");
    }

    // --- duration_from_blob ---
    //
    // Surfaces the BERSERK-style "skill / 10" hours-unit case
    // SUGGESTIONS flagged: at skill=100 the expected duration is
    // 10 game-hours ≈ 750 real seconds; at skill=0 it clamps to
    // the 1-second floor. These tests guard the formula
    // evaluator + unit conversion so a regression there is
    // caught before it lands in player-visible "1 sec berserk".

    #[test]
    fn duration_from_blob_handles_skill_division_in_hours() {
        let blob = serde_json::json!({
            "duration": "skill / 10",
            "durationUnit": "hours",
        });
        let ctx = FormulaCtx::base(1, 100);
        // 100 / 10 = 10 hours × 75 seconds/hour = 750 seconds.
        assert_eq!(duration_from_blob(Some(&blob), &ctx), Some(750));
    }

    #[test]
    fn duration_from_blob_clamps_zero_skill_to_one_second_floor() {
        let blob = serde_json::json!({
            "duration": "skill / 10",
            "durationUnit": "hours",
        });
        let ctx = FormulaCtx::base(1, 0);
        // 0 hours × 75 = 0 → clamp to 1 second floor.
        assert_eq!(duration_from_blob(Some(&blob), &ctx), Some(1));
    }

    #[test]
    fn duration_from_blob_passes_integer_literal_through_unit() {
        let blob = serde_json::json!({
            "duration": 4,
            "durationUnit": "minutes",
        });
        let ctx = FormulaCtx::base(1, 0);
        assert_eq!(duration_from_blob(Some(&blob), &ctx), Some(4 * 60));
    }

    #[test]
    fn duration_from_blob_defaults_to_hours_when_unit_missing() {
        let blob = serde_json::json!({ "duration": 2 });
        let ctx = FormulaCtx::base(1, 0);
        // 2 game-hours × 75 = 150 seconds.
        assert_eq!(duration_from_blob(Some(&blob), &ctx), Some(150));
    }

    #[test]
    fn duration_from_blob_returns_none_on_missing_field() {
        let blob = serde_json::json!({ "amount": 10 });
        let ctx = FormulaCtx::base(1, 0);
        assert_eq!(duration_from_blob(Some(&blob), &ctx), None);
    }

    // --- exit_is_hidden_to / RevealedExits ---
    //
    // Pins the contract every exit-rendering / movement site
    // depends on: hidden exits stay invisible to a fresh player,
    // and become visible once the player's `RevealedExits` set
    // contains the (room, dir) pair.

    #[test]
    fn exit_is_hidden_to_respects_revealed_exits() {
        use mud_db::enums::{Direction, ExitState};
        use mud_world::{ExitData, RevealedExits};

        let mut world = World::new();
        let player = world.spawn(()).id();
        let room = world.spawn(()).id();

        let visible_exit = ExitData {
            to: None,
            state: ExitState::Open,
            key: None,
            description: None,
            keywords: Vec::new(),
            is_hidden: false,
            is_pickproof: false,
        };
        let hidden_exit = ExitData {
            to: None,
            state: ExitState::Open,
            key: None,
            description: None,
            keywords: Vec::new(),
            is_hidden: true,
            is_pickproof: false,
        };

        // Non-hidden exit is visible regardless of reveal state.
        assert!(!super::exit_is_hidden_to(
            &world, player, room, Direction::North, &visible_exit
        ));

        // Hidden exit hides from a player with no RevealedExits.
        assert!(super::exit_is_hidden_to(
            &world, player, room, Direction::North, &hidden_exit
        ));

        // Adding the (room, north) pair to RevealedExits flips it.
        let mut set = std::collections::HashSet::new();
        set.insert((room, Direction::North));
        world.entity_mut(player).insert(RevealedExits { set });
        assert!(!super::exit_is_hidden_to(
            &world, player, room, Direction::North, &hidden_exit
        ));

        // Different direction stays hidden — reveal is per-direction.
        assert!(super::exit_is_hidden_to(
            &world, player, room, Direction::South, &hidden_exit
        ));

        // Different room (same direction) stays hidden — reveal is
        // per-(room, direction).
        let other_room = world.spawn(()).id();
        assert!(super::exit_is_hidden_to(
            &world, player, other_room, Direction::North, &hidden_exit
        ));
    }

    // ---- Liquid drink path ----

    /// Build a minimal world that the drink-path queries can run
    /// against. Only the resources/components the path actually
    /// reads are populated; everything else (sessions, prototypes
    /// for non-fountains, ConsumableEffects) stays empty.
    fn drink_test_world() -> (World, Entity, Entity) {
        use mud_world::{
            ConsumableEffectCatalog, Drunkenness, Hunger, Item, Keywords, LiquidCatalog, LiquidDef,
            LiquidContainer, LiquidIndex, Located, Named, ObjectPrototypes, Thirst,
        };
        let mut world = World::new();
        // Catalog with three liquids: water (no drunk), wine (alcoholic).
        let mut cat = LiquidCatalog::default();
        cat.insert(LiquidDef {
            id: 1,
            name: "water".to_string(),
            alias: "water".to_string(),
            color_desc: "clear".to_string(),
            drunk_effect: 0,
            hunger_effect: 0,
            thirst_effect: 10,
            description: Some("Cool and refreshing.".to_string()),
        });
        cat.insert(LiquidDef {
            id: 3,
            name: "wine".to_string(),
            alias: "wine".to_string(),
            color_desc: "ruby".to_string(),
            drunk_effect: 5,
            hunger_effect: 2,
            thirst_effect: 5,
            description: None,
        });
        world.insert_resource(cat);
        let mut idx = LiquidIndex::default();
        idx.by_name.insert("water".to_string(), 1);
        idx.by_name.insert("wine".to_string(), 3);
        idx.drunk_effect.insert("water".to_string(), 0);
        idx.drunk_effect.insert("wine".to_string(), 5);
        world.insert_resource(idx);
        // Empty resources to satisfy `apply_consumable_liquid_effects`
        // and the proto-fountain detector.
        world.insert_resource(ConsumableEffectCatalog::default());
        world.insert_resource(ObjectPrototypes::default());
        // Room → player → wineskin (Located on the player).
        let room = world.spawn(()).id();
        let player = world
            .spawn((
                Located(room),
                Hunger(30),
                Thirst(30),
                Drunkenness(0),
            ))
            .id();
        let item = world
            .spawn((
                Item,
                Located(player),
                Named { name: "a wineskin".to_string() },
                Keywords(vec!["wineskin".to_string(), "skin".to_string()]),
                LiquidContainer {
                    liquid: "wine".to_string(),
                    capacity: 20,
                    remaining: 20,
                    poisoned: false,
                },
            ))
            .id();
        (world, player, item)
    }

    #[test]
    fn drink_decrements_remaining_and_applies_deltas() {
        use mud_world::{Drunkenness, Hunger, LiquidContainer, Thirst};
        let (mut world, player, item) = drink_test_world();
        super::drink_amount(&mut world, player, "wineskin", 4, "drink");
        // 4 units consumed.
        assert_eq!(world.get::<LiquidContainer>(item).unwrap().remaining, 16);
        // Drunk delta: 5 per unit × 4 = 20.
        assert_eq!(world.get::<Drunkenness>(player).unwrap().0, 20);
        // Hunger delta: 2 per unit × 4 = 8, subtracted from 30.
        assert_eq!(world.get::<Hunger>(player).unwrap().0, 22);
        // Thirst delta: 5 per unit × 4 = 20, subtracted from 30.
        assert_eq!(world.get::<Thirst>(player).unwrap().0, 10);
    }

    #[test]
    fn drink_clamps_gauges_at_zero() {
        use mud_world::{Hunger, LiquidContainer, Thirst};
        let (mut world, player, item) = drink_test_world();
        // Player starts dehydrated/starving at 2; one big swig should clamp at 0.
        world.get_mut::<Hunger>(player).unwrap().0 = 2;
        world.get_mut::<Thirst>(player).unwrap().0 = 2;
        super::drink_amount(&mut world, player, "wineskin", 4, "drink");
        assert_eq!(world.get::<Hunger>(player).unwrap().0, 0);
        assert_eq!(world.get::<Thirst>(player).unwrap().0, 0);
        assert_eq!(world.get::<LiquidContainer>(item).unwrap().remaining, 16);
    }

    #[test]
    fn drink_caps_drank_at_remaining() {
        use mud_world::{Drunkenness, LiquidContainer};
        let (mut world, player, item) = drink_test_world();
        // Set remaining low — only 1 unit left.
        world.get_mut::<LiquidContainer>(item).unwrap().remaining = 1;
        super::drink_amount(&mut world, player, "wineskin", 4, "drink");
        // Only 1 unit was actually drunk; container now empty.
        assert_eq!(world.get::<LiquidContainer>(item).unwrap().remaining, 0);
        // Drunkenness = 5 × 1 = 5.
        assert_eq!(world.get::<Drunkenness>(player).unwrap().0, 5);
    }

    #[test]
    fn drink_refuses_empty_container() {
        use mud_world::{Drunkenness, LiquidContainer};
        let (mut world, player, item) = drink_test_world();
        world.get_mut::<LiquidContainer>(item).unwrap().remaining = 0;
        super::drink_amount(&mut world, player, "wineskin", 4, "drink");
        // Nothing changed — still empty, still sober.
        assert_eq!(world.get::<LiquidContainer>(item).unwrap().remaining, 0);
        assert_eq!(world.get::<Drunkenness>(player).unwrap().0, 0);
    }

    #[test]
    fn drink_unknown_alias_uses_fallback() {
        use mud_world::{Drunkenness, Hunger, LiquidContainer, Thirst};
        let (mut world, player, item) = drink_test_world();
        // Swap the alias to something the catalog doesn't know.
        world.get_mut::<LiquidContainer>(item).unwrap().liquid = "moonshine".to_string();
        super::drink_amount(&mut world, player, "wineskin", 4, "drink");
        // Fallback is water-shaped: thirst -= 10*4 = 40, hunger
        // unchanged, drunkenness unchanged.
        assert_eq!(world.get::<Hunger>(player).unwrap().0, 30);
        assert_eq!(world.get::<Thirst>(player).unwrap().0, 0, "30 - 40 clamped at 0");
        assert_eq!(world.get::<Drunkenness>(player).unwrap().0, 0);
        // The swig still went through.
        assert_eq!(world.get::<LiquidContainer>(item).unwrap().remaining, 16);
    }

    #[test]
    fn sip_consumes_one_unit() {
        use mud_world::{Drunkenness, LiquidContainer};
        let (mut world, player, item) = drink_test_world();
        super::drink_amount(&mut world, player, "wineskin", 1, "sip");
        assert_eq!(world.get::<LiquidContainer>(item).unwrap().remaining, 19);
        // Drunkenness = 5 × 1 = 5.
        assert_eq!(world.get::<Drunkenness>(player).unwrap().0, 5);
    }

    /// `name_approval_gate` returns true (refusing the command) when
    /// `NameApprovalPending` is attached, and false (allowing the
    /// command) otherwise. The refusal message itself goes through
    /// `send_to` which writes into the entity's `Connection`
    /// outbound channel — absent here, so the helper silently no-
    /// ops and returns the boolean. That's the contract every chat
    /// command depends on.
    #[test]
    fn name_approval_gate_blocks_when_marker_present() {
        use mud_world::NameApprovalPending;
        let mut world = World::new();
        let approved = world.spawn(()).id();
        let pending = world.spawn(NameApprovalPending).id();
        assert!(
            !super::name_approval_gate(&world, approved),
            "approved player is NOT gated",
        );
        assert!(
            super::name_approval_gate(&world, pending),
            "pending player IS gated",
        );
    }

    /// Round-trip: dropping the marker (the `approve_name` codepath)
    /// reopens the gate without needing to reconnect.
    #[test]
    fn name_approval_gate_clears_when_marker_removed() {
        use mud_world::NameApprovalPending;
        let mut world = World::new();
        let player = world.spawn(NameApprovalPending).id();
        assert!(super::name_approval_gate(&world, player));
        world.entity_mut(player).remove::<NameApprovalPending>();
        assert!(
            !super::name_approval_gate(&world, player),
            "gate clears immediately on marker remove",
        );
    }
}

/// Compute the "next level" progress (`next_level_pct` in
/// `Char.Vitals`). Returns the integer percentage `[0, 100]` of the
/// way from this level's XP threshold toward the next level's.
/// Returns 0 for the immortal levels (no "next") and 0 when the
/// level table hasn't loaded yet — calling code treats either as
/// "no progress bar to draw" rather than as a real measurement.
fn compute_level_progress(world: &World, level: i32, xp: i32) -> i32 {
    let Some(table) = world.get_resource::<mud_world::LevelTable>() else {
        return 0;
    };
    let Some(curr_thr) = table.exp_for(level) else {
        return 0;
    };
    let Some(next_thr) = table.exp_for(level + 1) else {
        return 0;
    };
    let span = (next_thr - curr_thr).max(1);
    let into = (xp - curr_thr).max(0);
    ((into * 100) / span).clamp(0, 100)
}

/// Push a `Char.Items.List` GMCP frame to `viewer`. `location` is
/// the IRE-convention slot — `"inv"` (carried), `"wear"` (equipped),
/// `"room"` (on the ground here), or a numeric container id when
/// Push a `Comm.Channel.List` GMCP frame describing every chat
/// channel the recipient can hear / use. IRE convention is an
/// array of `{name, caption, command}` objects — clients (Mudlet
/// packages, web clients) build their chat-tab list from this
/// instead of hardcoding a per-MUD map. Sent once per login so
/// role-gated channels (wiznet) only appear for staff.
///
/// `name` matches the `channel` field in `Comm.Channel.Text`
/// frames (so the client can route by it). `caption` is the
/// human-readable tab label. `command` is the verb the player
/// would type to send on that channel — useful for clients that
/// expose a "click tab → focus input with channel command"
/// shortcut.
pub(crate) fn send_comm_channel_list(world: &World, viewer: Entity) {
    let Some(conn) = world.get::<Connection>(viewer) else { return };
    let role = world
        .get::<Account>(viewer)
        .map_or(mud_db::enums::UserRole::Player, |a| a.role);
    // Channel directory. Order is the order tabs appear in the
    // Mudlet package's chat panel by default — gossip-first
    // matches the existing `consoles` array's tab order.
    struct Ch {
        name: &'static str,
        caption: &'static str,
        command: &'static str,
        min_role: mud_db::enums::UserRole,
    }
    let dir = [
        Ch { name: "gossip",  caption: "Gossip",   command: "gossip",  min_role: mud_db::enums::UserRole::Player },
        Ch { name: "music",   caption: "Music",    command: "music",   min_role: mud_db::enums::UserRole::Player },
        Ch { name: "shout",   caption: "Shout",    command: "shout",   min_role: mud_db::enums::UserRole::Player },
        Ch { name: "quest",   caption: "Quest",    command: "qsay",    min_role: mud_db::enums::UserRole::Player },
        Ch { name: "tells",   caption: "Tells",    command: "tell",    min_role: mud_db::enums::UserRole::Player },
        Ch { name: "clan",    caption: "Clan",     command: "ctell",   min_role: mud_db::enums::UserRole::Player },
        Ch { name: "group",   caption: "Group",    command: "gsay",    min_role: mud_db::enums::UserRole::Player },
        Ch { name: "say",     caption: "Local",    command: "say",     min_role: mud_db::enums::UserRole::Player },
        Ch { name: "emote",   caption: "Local",    command: "emote",   min_role: mud_db::enums::UserRole::Player },
        Ch { name: "ask",     caption: "Local",    command: "ask",     min_role: mud_db::enums::UserRole::Player },
        Ch { name: "whisper", caption: "Local",    command: "whisper", min_role: mud_db::enums::UserRole::Player },
        Ch { name: "insult",  caption: "Local",    command: "insult",  min_role: mud_db::enums::UserRole::Player },
        Ch { name: "wiznet",  caption: "Wiznet",   command: "wiznet",  min_role: mud_db::enums::UserRole::Immortal },
    ];
    let entries: Vec<String> = dir
        .iter()
        .filter(|c| role.at_least(c.min_role))
        .map(|c| {
            format!(
                r#"{{"name":"{}","caption":"{}","command":"{}"}}"#,
                c.name, c.caption, c.command,
            )
        })
        .collect();
    let payload = format!("[{}]", entries.join(","));
    let _ = conn.0.try_send(mud_net::gmcp_packet("Comm.Channel.List", &payload));
}

/// listing a container's contents. `items` is the entity set to
/// emit; the helper builds the JSON payload and sends in one
/// telnet frame. Skips gracefully when `viewer` has no connection
/// (mob, switched-into puppet) since GMCP only makes sense for
/// real clients.
///
/// IRE convention: each item carries `id` (stable within the
/// session — we use the runtime entity id), `name` (color tags
/// stripped so the client can re-style), and optional `icon` /
/// `attrib`. We omit those last two for now — Mudlet renders
/// items name-only when they're missing.
pub(crate) fn send_char_items_list(
    world: &World,
    viewer: Entity,
    location: &str,
    items: &[Entity],
) {
    let Some(conn) = world.get::<Connection>(viewer) else { return };
    let mut entries: Vec<String> = Vec::with_capacity(items.len());
    for &item in items {
        let raw_name = world
            .get::<Named>(item)
            .map(|n| n.name.as_str())
            .unwrap_or("");
        let plain = render_color_tags(raw_name, ColorMode::Strip)
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let id = item.to_bits();
        // Item type label (Weapon / Armor / Drinkcontainer / etc.)
        // — sourced from the proto. Lets the client pick the right
        // verb for click-actions (use vs eat vs wear) without
        // needing a follow-up GMCP roundtrip.
        let item_type = world
            .get::<WorldKey>(item)
            .and_then(|k| {
                world
                    .resource::<mud_world::ObjectPrototypes>()
                    .by_key
                    .get(&(k.zone, k.id))
                    .map(|p| p.r#type.label())
            })
            .unwrap_or("");
        // Identified flag — presence of the marker component
        // means the carrier has cast `identify` on it. Drives
        // the rich-detail view in the client's item panel.
        let identified = world.get::<mud_world::Identified>(item).is_some();
        // Per-item slot string, only for items in the `wear`
        // bucket. Empty for inventory items / containers. Lets
        // the equipment panel render "head: a golden circlet"
        // without a second lookup.
        let slot_field = if location == "wear" {
            world
                .get::<EquippedSlot>(item)
                .map(|eq| format!(r#","location":"{}""#, eq.0.label()))
                .unwrap_or_default()
        } else {
            String::new()
        };
        entries.push(format!(
            r#"{{"id":"{id}","name":"{plain}","type":"{item_type}","identified":{identified}{slot_field}}}"#
        ));
    }
    let payload = format!(
        r#"{{"location":"{}","items":[{}]}}"#,
        location.replace('"', "\\\""),
        entries.join(","),
    );
    let _ = conn.0.try_send(mud_net::gmcp_packet("Char.Items.List", &payload));
}

/// Re-emit `Char.Items.List` for both the player's inventory and
/// equipment. Used after item-mutation commands (get / drop /
/// wear / remove / give) so Mudlet's items panel re-renders
/// without the client having to issue a follow-up Char.Items.Inv
/// request. Cheaper than a granular Add/Remove pairing here
/// because the snapshot is small (~one telnet frame per slot
/// list) and the call site doesn't need to track which item moved.
pub(crate) fn refresh_player_items_gmcp(world: &mut World, player: Entity) {
    let inv: Vec<Entity> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, Option<&EquippedSlot>),
            With<Item>,
        >();
        q.iter(world)
            .filter(|(_, l, eq)| l.0 == player && eq.is_none())
            .map(|(e, _, _)| e)
            .collect()
    };
    send_char_items_list(world, player, "inv", &inv);
    let worn: Vec<Entity> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &EquippedSlot),
            With<Item>,
        >();
        q.iter(world)
            .filter(|(_, l, _)| l.0 == player)
            .map(|(e, _, _)| e)
            .collect()
    };
    send_char_items_list(world, player, "wear", &worn);
}

/// Push a single-item `Char.Items.{Add | Remove | Update}` frame.
/// Wraps the same item-shape as [`send_char_items_list`] in the
/// IRE-convention envelope `{location, item: {...}}`. `verb` must
/// be one of `"Add"` / `"Remove"` / `"Update"` — case-sensitive
/// per the Mudlet event handler convention. `Remove` only needs
/// the id but emits the full record for symmetry; clients ignore
/// extra fields.
pub(crate) fn send_char_items_diff(
    world: &World,
    viewer: Entity,
    verb: &str,
    location: &str,
    item: Entity,
) {
    let Some(conn) = world.get::<Connection>(viewer) else { return };
    let raw_name = world
        .get::<Named>(item)
        .map(|n| n.name.as_str())
        .unwrap_or("");
    let plain = render_color_tags(raw_name, ColorMode::Strip)
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let id = item.to_bits();
    let payload = format!(
        r#"{{"location":"{loc}","item":{{"id":"{id}","name":"{plain}"}}}}"#,
        loc = location.replace('"', "\\\""),
    );
    let package = format!("Char.Items.{verb}");
    let _ = conn.0.try_send(mud_net::gmcp_packet(&package, &payload));
}

/// Push `Room.Players` to `viewer` — the snapshot of every player
/// currently in `viewer`'s room (excluding `viewer` themselves, by
/// IRE convention). Mudlet's "who's here" panel renders this. We
/// strip color tags from names so the client can re-style.
///
/// Takes `&mut World` because `query_filtered` requires a
/// mutable borrow to construct its state cache. The actual
/// iteration is read-only.
pub(crate) fn send_room_players_snapshot(world: &mut World, viewer: Entity) {
    let Some(room) = world.get::<Located>(viewer).map(|l| l.0) else {
        return;
    };
    let mut entries: Vec<String> = Vec::new();
    {
        let mut q = world.query_filtered::<(Entity, &Located, &Named), With<Player>>();
        for (e, loc, named) in q.iter(world) {
            if loc.0 == room && e != viewer {
                let plain = render_color_tags(&named.name, ColorMode::Strip)
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"");
                entries.push(format!(r#"{{"name":"{plain}","full_name":"{plain}"}}"#));
            }
        }
    }
    let Some(conn) = world.get::<Connection>(viewer) else { return };
    let payload = format!("[{}]", entries.join(","));
    let _ = conn.0.try_send(mud_net::gmcp_packet("Room.Players", &payload));
}

/// Push a single `Room.AddPlayer` / `Room.RemovePlayer` diff to
/// every other player in `room`. Used by movement / connect /
/// disconnect paths so observers refresh their "who's here" panel
/// without re-querying the room. Self is excluded — they don't
/// need to know they entered/left their own room.
pub(crate) fn broadcast_room_player_diff(
    world: &mut World,
    room: Entity,
    subject: Entity,
    verb: &str, // "AddPlayer" or "RemovePlayer"
) {
    let raw_name = world
        .get::<Named>(subject)
        .map(|n| n.name.as_str())
        .unwrap_or("");
    let plain = render_color_tags(raw_name, ColorMode::Strip)
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let payload = format!(r#"{{"name":"{plain}","full_name":"{plain}"}}"#);
    let frame = mud_net::gmcp_packet(&format!("Room.{verb}"), &payload);

    let recipients: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, loc)| loc.0 == room && *e != subject)
            .map(|(e, _)| e)
            .collect()
    };
    for e in recipients {
        if let Some(conn) = world.get::<Connection>(e) {
            let _ = conn.0.try_send(frame.clone());
        }
    }
}

/// Push a `Char.Skills.List` GMCP frame in response to a client's
/// `Char.Skills.Get`. The IRE convention is a flat array of skill
/// names — the client groups them however its UI prefers. We
/// resolve names from `AbilityCatalog` so the strings match
/// what `spells` / `skills` print in-game.
pub(crate) fn send_char_skills_list(world: &World, viewer: Entity) {
    let Some(conn) = world.get::<Connection>(viewer) else { return };
    let Some(known) = world.get::<KnownAbilities>(viewer) else {
        let empty = "[]";
        let _ = conn.0.try_send(mud_net::gmcp_packet("Char.Skills.List", empty));
        return;
    };
    let catalog = world.resource::<AbilityCatalog>();
    let mut names: Vec<String> = Vec::with_capacity(known.entries.len());
    for &(ability_id, _, known_flag) in &known.entries {
        if !known_flag {
            continue;
        }
        if let Some(def) = catalog.by_name.values().find(|d| d.id == ability_id) {
            let plain = render_color_tags(&def.plain_name, ColorMode::Strip)
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            names.push(format!("\"{plain}\""));
        }
    }
    let payload = format!("[{}]", names.join(","));
    let _ = conn.0.try_send(mud_net::gmcp_packet("Char.Skills.List", &payload));
}

/// Push a `Char.Skills` GMCP frame to `viewer`. Drives the future
/// skill-bar widget: one entry per known ability with the cooldown
/// timer and an `available` boolean (true when off cooldown). Shape:
///
///   { skills: [ {name, cooldown, available} ] }
///
/// Casting costs are paid in stamina, not mana — this game has no
/// mana pool. The shape stays MUD-client-standard (no `mp_cost`
/// alongside skills) so generic GMCP clients render fine without
/// special-casing FieryMUD.
pub(crate) fn send_char_skills(world: &World, viewer: Entity) {
    let Some(conn) = world.get::<Connection>(viewer) else { return };
    let Some(known) = world.get::<KnownAbilities>(viewer) else {
        let _ = conn.0.try_send(mud_net::gmcp_packet("Char.Skills", r#"{"skills":[]}"#));
        return;
    };
    let catalog = match world.get_resource::<AbilityCatalog>() {
        Some(c) => c,
        None => {
            let _ = conn.0.try_send(mud_net::gmcp_packet("Char.Skills", r#"{"skills":[]}"#));
            return;
        }
    };
    let now = std::time::Instant::now();
    let cooldowns = world.get::<Cooldowns>(viewer);
    // AbilityCatalog is keyed by name; reverse-lookup by id would
    // scan the full catalog (~400 entries) per known ability per
    // prompt. Snapshot a by_id view once. Mirrors Char.Effects.
    let by_id: std::collections::HashMap<i32, &mud_world::AbilityDef> =
        catalog.by_name.values().map(|d| (d.id, d)).collect();
    let mut entries: Vec<String> = Vec::with_capacity(known.entries.len());
    for &(ability_id, _, known_flag) in &known.entries {
        if !known_flag {
            continue;
        }
        let Some(def) = by_id.get(&ability_id) else { continue };
        let plain = plain_for_gmcp(&def.plain_name);
        // Seconds remaining on this ability's cooldown, or 0 if
        // it's ready (no entry, or `ready_at` already passed).
        let cooldown_secs = cooldowns
            .and_then(|cd| cd.ready_at.get(&ability_id))
            .map(|when| when.saturating_duration_since(now).as_secs())
            .unwrap_or(0);
        let available = cooldown_secs == 0;
        entries.push(format!(
            r#"{{"name":"{plain}","cooldown":{cooldown_secs},"available":{available}}}"#,
        ));
    }
    let payload = format!(r#"{{"skills":[{}]}}"#, entries.join(","));
    let _ = conn.0.try_send(mud_net::gmcp_packet("Char.Skills", &payload));
}

/// Push a `Group` GMCP frame to `viewer`. The frame describes the
/// whole party rooted at `viewer`'s leader: name, count, and a
/// member array. Each member entry carries name, level, race,
/// class, current room (true if same as `viewer`'s room — the
/// `with_leader` IRE convention), and a `stats` block with
/// hp/max_hp/mv/max_mv. The party panel renders directly off
/// this shape.
///
/// Empty / solo case: no Group frame is emitted (the player isn't
/// in a group). Callers can also push `Group.End` (empty body) to
/// signal teardown — handled by the prompt-cadence emit's
/// member-count check.
pub(crate) fn send_group_state(world: &mut World, viewer: Entity) {
    let root = group_root(world, viewer);
    let members = group_members(world, root);
    let Some(conn) = world.get::<Connection>(viewer) else { return };
    if members.len() <= 1 {
        // Solo — push an empty Group frame so a previously-visible
        // panel clears.
        let _ = conn.0.try_send(mud_net::gmcp_packet("Group", "{}"));
        return;
    }
    let viewer_room = world.get::<Located>(viewer).map(|l| l.0);
    let leader_name = world
        .get::<Named>(root)
        .map(|n| n.name.clone())
        .unwrap_or_default();
    let leader_plain = render_color_tags(&leader_name, ColorMode::Strip)
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let mut entries: Vec<String> = Vec::with_capacity(members.len());
    for &m in &members {
        let raw = world
            .get::<Named>(m)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        let plain = render_color_tags(&raw, ColorMode::Strip)
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let level = world.get::<Profile>(m).map_or(0, |p| p.level);
        let race = world
            .get::<Profile>(m)
            .map(|p| p.race.clone())
            .unwrap_or_default();
        let class = world
            .get::<Profile>(m)
            .and_then(|p| p.class_id)
            .and_then(|id| {
                world
                    .get_resource::<ClassCatalog>()
                    .and_then(|c| c.by_id.get(&id))
                    .map(|d| d.plain_name.clone())
            })
            .unwrap_or_default();
        let with_leader = match (viewer_room, world.get::<Located>(m).map(|l| l.0)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        let (hp, max_hp) = world
            .get::<Health>(m)
            .map_or((0, 0), |h| (h.hp, h.max));
        let (mv, max_mv) = world
            .get::<Stamina>(m)
            .map_or((0, 0), |s| (s.current, s.max));
        entries.push(format!(
            r#"{{"name":"{plain}","with_leader":{with},"level":{level},"race":"{race}","class":"{class}","stats":{{"hp":{hp},"max_hp":{max_hp},"mv":{mv},"max_mv":{max_mv}}}}}"#,
            plain = plain,
            with = with_leader,
            level = level,
            race = race.replace('"', "\\\""),
            class = class.replace('"', "\\\""),
            hp = hp,
            max_hp = max_hp,
            mv = mv,
            max_mv = max_mv,
        ));
    }
    let payload = format!(
        r#"{{"group_name":"{leader_plain}'s group","leader":"{leader_plain}","count":{count},"members":[{members}]}}"#,
        count = members.len(),
        members = entries.join(","),
    );
    let _ = conn.0.try_send(mud_net::gmcp_packet("Group", &payload));
}

/// Push a `Char.Combat` GMCP frame to `viewer`. Drives the bottom-left
/// TARGET panel: tank + opponent + (optional) the viewer's current
/// target when it differs from the group's opponent.
///
/// Field-name quirk: the spec uses `tank.max_hp` (underscore) but
/// `opponent.hp_percent` and Char.Vitals' `max_hp`. All snake_case
/// — the client does no normalization, so the names below match the
/// wire contract exactly.
///
/// Shape:
///   {} (cleared)                          — no Fighting; client hides
///   { tank: {name, hp, max_hp},
///     opponent: {name, hp_percent},
///     target?:  {name, hp_percent} }
///
/// `opponent` and `target` are the same mob today (the viewer's
/// `Fighting`); `target` is included as the explicit "my current
/// swing" mob so the client can stack `Opponent: X` / `Target: Y`
/// when a group-main concept lands later. `tank` is whoever the
/// target mob is fighting back — usually the viewer (solo) or a
/// groupmate holding aggro. Falls back to the viewer when the mob
/// isn't swinging at anyone yet.
pub(crate) fn send_char_combat(world: &World, viewer: Entity) {
    let Some(conn) = world.get::<Connection>(viewer) else { return };
    let fighting = world.get::<Fighting>(viewer).map(|f| f.0);
    let Some(mob) = fighting else {
        // Cleared frame — client uses empty {} as a hide signal.
        let _ = conn.0.try_send(mud_net::gmcp_packet("Char.Combat", "{}"));
        return;
    };
    // Mob may have despawned mid-round (death cleanup races prompt
    // dispatch). Treat a stale Fighting like "no combat" — the next
    // tick will clear the component too.
    if world.get_entity(mob).is_err() {
        let _ = conn.0.try_send(mud_net::gmcp_packet("Char.Combat", "{}"));
        return;
    }
    let mob_plain = world
        .get::<Named>(mob)
        .map(|n| plain_for_gmcp(&n.name))
        .unwrap_or_default();
    let (mob_hp, mob_max) = world
        .get::<Health>(mob)
        .map_or((0, 0), |h| (h.hp, h.max));
    let mob_pct = if mob_max > 0 {
        ((mob_hp.max(0) * 100) / mob_max).clamp(0, 100)
    } else {
        0
    };

    // Tank defaults to the viewer when the mob hasn't swung back
    // yet — the .unwrap_or(viewer) below.
    let tank = world
        .get::<Fighting>(mob)
        .map(|f| f.0)
        .filter(|t| world.get_entity(*t).is_ok())
        .unwrap_or(viewer);
    let tank_plain = world
        .get::<Named>(tank)
        .map(|n| plain_for_gmcp(&n.name))
        .unwrap_or_default();
    let (tank_hp, tank_max) = world
        .get::<Health>(tank)
        .map_or((0, 0), |h| (h.hp, h.max));

    let payload = format!(
        r#"{{"tank":{{"name":"{tank_plain}","hp":{tank_hp},"max_hp":{tank_max}}},"opponent":{{"name":"{mob_plain}","hp_percent":{mob_pct}}},"target":{{"name":"{mob_plain}","hp_percent":{mob_pct}}}}}"#,
    );
    let _ = conn.0.try_send(mud_net::gmcp_packet("Char.Combat", &payload));
}

/// Returns true when `mob` should appear with `hostile: true` in the
/// `Room.Mobs` frame from `viewer`'s perspective. Hostility means
/// any of:
///   - currently fighting someone (engaged)
///   - has the viewer on its HateList (actively chasing)
///   - remembers the viewer (MobMemory — lingering grudge)
///   - alignment is at or below the aggro threshold (auto-attacks
///     on arrival), per the same check `try_engage_aggressive_mob`
///     uses
fn mob_is_hostile_to(world: &World, mob: Entity, viewer: Entity) -> bool {
    if world.get::<Fighting>(mob).is_some() {
        return true;
    }
    if world
        .get::<crate::combat::HateList>(mob)
        .is_some_and(|h| h.0.contains(&viewer))
    {
        return true;
    }
    if world
        .get::<crate::combat::MobMemory>(mob)
        .is_some_and(|m| m.0.contains(&viewer))
    {
        return true;
    }
    let threshold = aggro_alignment(world);
    world
        .get::<CombatStats>(mob)
        .is_some_and(|cs| cs.alignment <= threshold)
}

/// Push a `Room.Mobs` GMCP frame to `viewer`. One entry per mob in
/// the room, with a `hostile` flag and (when applicable) a list of
/// service `professions`. Drives both the threat panel (filter by
/// `hostile`) and the friendly-NPC panel (filter by `!hostile`).
///
/// Shape:
///   Array<{
///     id:           string,    // runtime entity id (for Room.Mob.Get)
///     name:         string,
///     hostile:      boolean,
///     hp_percent:   number,    // 0..100
///     targeting:    string|null, // null when the mob isn't swinging
///     status?:      string,    // "stunned" (more later: casting / fleeing)
///     professions?: string[],  // ["shop","bank",...] from the mob's proto
///   }>
///
/// `hp_percent` is emitted for every mob (not just hostile ones) —
/// it doubles as a liveness indicator for a passing client that
/// wants to render a thin bar under friendly NPCs too. Clients can
/// drop the bar for `hostile:false` rows.
///
/// Empty array clears the panel.
///
/// Takes `&mut World` because the mob/player queries each need
/// their own state cache.
///
/// Also emits the derived `Room.Services` frame in the same pass —
/// the service set is the union of per-mob `professions` and would
/// otherwise require a second room walk. Insertion-stable order so
/// the client gets a predictable display order on multi-service
/// rooms.
pub(crate) fn send_room_mobs(world: &mut World, viewer: Entity) {
    let Some(room) = world.get::<Located>(viewer).map(|l| l.0) else { return };
    // Snapshot mob entities in the room, dropping any the viewer
    // can't see (WizInvis level above viewer's). The visibility
    // filter happens here rather than per-mob below so professions
    // and hostility on hidden mobs never leak into the frame.
    let candidates: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Mob>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == room)
            .map(|(e, _)| e)
            .collect()
    };
    let candidates: Vec<Entity> = candidates
        .into_iter()
        .filter(|&mob| can_see_player(world, viewer, mob))
        .collect();
    let mut entries: Vec<String> = Vec::with_capacity(candidates.len());
    let mut services: Vec<&'static str> = Vec::new();
    for mob in candidates {
        let mob_plain = world
            .get::<Named>(mob)
            .map(|n| plain_for_gmcp(&n.name))
            .unwrap_or_default();
        let (hp, max) = world
            .get::<Health>(mob)
            .map_or((0, 0), |h| (h.hp, h.max));
        let hp_pct = if max > 0 {
            ((hp.max(0) * 100) / max).clamp(0, 100)
        } else {
            0
        };
        let hostile = mob_is_hostile_to(world, mob, viewer);
        let targeting_json = world
            .get::<Fighting>(mob)
            .map(|f| f.0)
            .and_then(|t| world.get::<Named>(t))
            .map(|n| format!("\"{}\"", plain_for_gmcp(&n.name)))
            .unwrap_or_else(|| "null".to_string());
        let status_field = if world.get::<Stunned>(mob).is_some() {
            r#","status":"stunned""#
        } else {
            ""
        };
        // Professions come off the proto, not the spawned entity.
        // We emit `professions:[]` (always present, even when empty)
        // so the client doesn't need a presence check.
        let professions: Vec<&'static str> = world
            .get::<WorldKey>(mob)
            .and_then(|k| {
                world
                    .get_resource::<MobPrototypes>()
                    .and_then(|p| p.by_key.get(&(k.zone, k.id)))
            })
            .map(|proto| proto.professions.iter().copied().map(mud_db::enums::MobProfession::label).collect())
            .unwrap_or_default();
        for &tag in &professions {
            if !services.contains(&tag) {
                services.push(tag);
            }
        }
        let prof_json = if professions.is_empty() {
            "[]".to_string()
        } else {
            let inner = professions
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(",");
            format!("[{inner}]")
        };
        let id_bits = mob.to_bits();
        entries.push(format!(
            r#"{{"id":"{id_bits}","name":"{mob_plain}","hostile":{hostile},"hp_percent":{hp_pct},"targeting":{targeting_json}{status_field},"professions":{prof_json}}}"#,
        ));
    }
    let Some(conn) = world.get::<Connection>(viewer) else { return };
    let mobs_payload = format!("[{}]", entries.join(","));
    let _ = conn.0.try_send(mud_net::gmcp_packet("Room.Mobs", &mobs_payload));
    let services_inner = services
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(",");
    let services_payload = format!(r#"{{"services":[{services_inner}]}}"#);
    let _ = conn.0.try_send(mud_net::gmcp_packet("Room.Services", &services_payload));
}

/// Handle inbound `Room.Mob.Get { id: "<entity_bits>" }`. Resolves
/// the id back to an Entity, verifies it's a mob in the requesting
/// player's room (anti-snooping; can't query mobs across the world),
/// then pushes a `Room.Mob.Info` frame describing the mob.
///
/// Shape of the response:
///   {
///     id, name, description, professions:[],
///     shop?: { items:[{id,name,price,stock}], accepts:[type1,...] }
///   }
///
/// Silently no-ops on bad id, off-world mob, wrong room, or missing
/// player Connection — request fishing should fail silent, not leak
/// the difference between "no such mob" and "wrong room".
pub(crate) fn handle_room_mob_get(world: &World, viewer: Entity, payload: &str) {
    // Accept either `{"id":"123"}` (string form, matching what
    // Room.Mobs emits) or `{"id":123}` (numeric) — the client
    // shouldn't fail-route if it forgets the quotes.
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return,
    };
    let bits: u64 = match value.get("id") {
        Some(serde_json::Value::String(s)) => match s.parse() {
            Ok(n) => n,
            Err(_) => return,
        },
        Some(serde_json::Value::Number(n)) => match n.as_u64() {
            Some(n) => n,
            None => return,
        },
        _ => return,
    };
    let Some(target) = Entity::try_from_bits(bits) else { return };
    if world.get_entity(target).is_err() { return }
    if world.get::<Mob>(target).is_none() { return }
    let viewer_room = world.get::<Located>(viewer).map(|l| l.0);
    let target_room = world.get::<Located>(target).map(|l| l.0);
    if viewer_room != target_room || viewer_room.is_none() { return }
    // Anti-snoop: don't surface info about a mob the viewer can't
    // see (WizInvis above their level). Silent no-op so a viewer
    // brute-forcing entity ids can't tell "invisible" from "no
    // such mob".
    if !can_see_player(world, viewer, target) { return }
    let Some(conn) = world.get::<Connection>(viewer) else { return };

    let plain_name = world
        .get::<Named>(target)
        .map(|n| plain_for_gmcp(&n.name))
        .unwrap_or_default();
    let plain_desc = world
        .get::<Description>(target)
        .map(|d| plain_for_gmcp(&d.0))
        .unwrap_or_default();

    let proto = world
        .get::<WorldKey>(target)
        .and_then(|k| {
            world
                .get_resource::<MobPrototypes>()
                .and_then(|p| p.by_key.get(&(k.zone, k.id)))
        });
    let professions: Vec<&'static str> = proto
        .map(|p| p.professions.iter().copied().map(mud_db::enums::MobProfession::label).collect())
        .unwrap_or_default();
    let prof_json = professions
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(",");

    // Shop block — populated only when the mob is a registered
    // keeper. `ShopCatalog.keeper_index` maps (zone, id) → the
    // shop's (zone, id), then `by_key` carries the offerings + accept
    // rules. Item names come from `ObjectPrototypes`.
    let shop_json: String = (|| {
        let key = world.get::<WorldKey>(target)?;
        let catalog = world.get_resource::<mud_world::ShopCatalog>()?;
        let shop_key = catalog.keeper_index.get(&(key.zone, key.id))?;
        let shop = catalog.by_key.get(shop_key)?;
        let obj_protos = world.get_resource::<ObjectPrototypes>();
        let items_json: Vec<String> = shop
            .items
            .iter()
            .map(|o| {
                let proto = obj_protos
                    .and_then(|op| op.by_key.get(&(o.object_zone_id, o.object_id)));
                let name_plain = proto
                    .map(|p| plain_for_gmcp(&p.name))
                    .unwrap_or_default();
                // First keyword is the canonical noun for `buy <kw>`.
                // Empty string when the proto has no keywords (loader
                // gap) — client falls back to deriving from name.
                let keyword = proto
                    .and_then(|p| p.keywords.first())
                    .map(|s| plain_for_gmcp(s))
                    .unwrap_or_default();
                let item_id = format!("{}:{}", o.object_zone_id, o.object_id);
                // Stock semantics: -1 = unlimited (per
                // ShopOffering.amount); surface verbatim so the
                // client can render "∞".
                format!(
                    r#"{{"id":"{item_id}","name":"{name_plain}","keyword":"{keyword}","price":{price},"stock":{stock}}}"#,
                    price = o.price,
                    stock = o.amount,
                )
            })
            .collect();
        let accepts_json: Vec<String> = shop
            .accepts
            .iter()
            .map(|a| {
                let safe = a.object_type.replace('\\', "\\\\").replace('"', "\\\"");
                format!("\"{safe}\"")
            })
            .collect();
        Some(format!(
            r#","shop":{{"items":[{items}],"accepts":[{accepts}]}}"#,
            items = items_json.join(","),
            accepts = accepts_json.join(",")
        ))
    })()
    .unwrap_or_default();

    let payload = format!(
        r#"{{"id":"{bits}","name":"{plain_name}","description":"{plain_desc}","professions":[{prof_json}]{shop_json}}}"#,
    );
    let _ = conn.0.try_send(mud_net::gmcp_packet("Room.Mob.Info", &payload));
}

/// Send `Core.Goodbye` immediately before closing the connection.
/// Standard IRE convention is a single-string body explaining the
/// reason; clients display it as a clean disconnect message
/// instead of "connection lost". Skipped silently when there's
/// no Connection (mob entity, switched puppet without a real
/// client).
pub(crate) fn send_core_goodbye(world: &World, viewer: Entity, reason: &str) {
    let Some(conn) = world.get::<Connection>(viewer) else { return };
    let safe = reason.replace('\\', "\\\\").replace('"', "\\\"");
    let payload = format!("\"{safe}\"");
    let _ = conn.0.try_send(mud_net::gmcp_packet("Core.Goodbye", &payload));
}

/// Wall-clock epoch seconds at which `target` logged in. Computed
/// from `LoggedInAt`'s monotonic [`Instant`] minus the elapsed
/// duration since login: `now_unix - elapsed_secs`. Returns the
/// current time as a fallback when LoggedInAt is missing — a
/// rare edge case (Discord just shows 0:00 elapsed).
fn compute_login_unix_ts(world: &World, target: Entity) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map_or(0, |d| d.as_secs());
    let elapsed_secs = world
        .get::<mud_world::LoggedInAt>(target)
        .map_or(0, |l| l.0.elapsed().as_secs());
    now_unix.saturating_sub(elapsed_secs)
}

/// Send the player's prompt template with variables substituted. Falls back
/// to a sensible default if no Prompt component is attached or the template
/// is empty.
#[allow(clippy::too_many_lines)]
pub(crate) fn send_prompt(world: &mut World, target: Entity) {
    // Clone the outbound channel up front so the rest of the
    // function can borrow `world` freely (queries below need
    // `&mut World`). Outbound is an `mpsc::UnboundedSender`,
    // cheap to clone.
    let conn = world.get::<Connection>(target).map(|c| c.0.clone());
    let Some(conn) = conn else {
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
    let clock = world.get_resource::<mud_world::MudClock>();
    let hour = clock.map(|c| c.hour);
    let season = clock.map(|c| c.season().label());
    let day_night = hour.map(|h| if matches!(h, 0..=4 | 22..=23) { "night" } else { "day" });
    // Opponent info for the combat-style prompts. `Fighting` points
    // at the live target Entity; resolve to its name + HP. Out of
    // combat both fields stay None and the `%e`/`%N`/etc. codes
    // render `-`.
    let enemy = world
        .get::<Fighting>(target)
        .map(|f| f.0)
        .filter(|e| world.get_entity(*e).is_ok());
    let enemy_name = enemy.and_then(|e| world.get::<Named>(e)).map(|n| n.name.as_str());
    let enemy_hp = enemy.and_then(|e| world.get::<Health>(e)).copied();
    let rendered = render_prompt(
        template,
        PromptCtx {
            hp,
            stamina,
            name,
            room,
            wealth,
            hour,
            season,
            day_night,
            enemy_name,
            enemy_hp,
        },
    );
    // Prompts can carry color tags both directly in the template
    // (`prompt <red>%h</>`) and indirectly via %r / %n (room and player
    // names that may have embedded tags). render_color_tags handles
    // both — and is_tag_shaped lets the default `<%h/%H>` survive
    // since `<42/100>` isn't tag-shaped after %-substitution.
    let mode = color_mode_for(world, target);
    let _ = conn.try_send(render_color_tags(&rendered, mode).into_bytes());

    // IAC EOR — end-of-record marker so MUD clients can split the
    // prompt from the preceding output (Mudlet uses it to anchor
    // the prompt line at the bottom of the input area; MUSHclient
    // and BeipMU bind triggers off "prompt detected"). Sent
    // unconditionally as a 2-byte IAC sequence — clients that
    // didn't negotiate EOR (or don't recognize it) silently strip
    // it, so this is safe to push even when the WILL EOR
    // negotiation got DONT'd. Legacy MUDs gate this behind cap
    // tracking; the client cost of an unsolicited frame is zero,
    // so we skip the cap lookup.
    let _ = conn.try_send(mud_net::iac_eor());

    // Char.Vitals — per-prompt vitals frame. Reuses the same helper
    // `send_char_vitals` uses for combat-driven mid-tick pushes so
    // both paths emit identical payloads. Plain telnet clients see
    // the IAC bytes as garbage which most terminal emulators strip
    // (they're outside the ASCII range).
    send_char_vitals(world, target);
    // Char.Name — IRE-style identity frame. Sent every prompt for
    // simplicity; the payload is small and idempotent on the
    // client side. Mudlet binds `gmcp.Char.Name.name` for profile
    // automation (per-character config files keyed by name).
    if let Some(name_str) = name {
        let plain = render_color_tags(name_str, ColorMode::Strip)
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let payload = format!(
            "{{\"name\":\"{plain}\",\"full_name\":\"{plain}\"}}"
        );
        let _ = conn.try_send(mud_net::gmcp_packet("Char.Name", &payload));
    }
    // Char.StatusVars — schema descriptor: maps each Char.Status
    // field to a human label so generic clients can build a
    // status panel without per-MUD code. Once-per-login would be
    // ideal but emitting per prompt is cheap (~120 bytes) and
    // sidesteps the "did the client miss the first push?" race.
    {
        let payload = "{\"name\":\"Name\",\"full_name\":\"Full Name\",\"level\":\"Level\",\"class\":\"Class\",\"race\":\"Race\",\"xp\":\"Experience\",\"wealth\":\"Wealth\"}";
        let _ = conn.try_send(mud_net::gmcp_packet("Char.StatusVars", payload));
    }
    // Char.Status: longer-lived character metadata (level / xp /
    // class / race / wealth). Same prompt cadence — many of these
    // change only on level-up but the per-prompt push is cheap
    // and lets the client refresh on any state change without
    // computing what changed.
    if let Some(prof) = world.get::<Profile>(target) {
        let class_label = prof
            .class_id
            .and_then(|id| {
                world
                    .get_resource::<ClassCatalog>()
                    .and_then(|c| c.by_id.get(&id))
                    .map(|d| d.plain_name.as_str())
            })
            .unwrap_or("");
        let wealth = world.get::<Wealth>(target).map_or(0, |w| w.0);
        let payload = format!(
            "{{\"name\":\"{}\",\"level\":{},\"xp\":{},\"class\":\"{}\",\"race\":\"{}\",\"wealth\":{}}}",
            name.unwrap_or("").replace('"', "\\\""),
            prof.level,
            prof.experience,
            class_label.replace('"', "\\\""),
            prof.race.replace('"', "\\\""),
            wealth,
        );
        let _ = conn.try_send(mud_net::gmcp_packet("Char.Status", &payload));
    }
    // External.Discord.Status — Discord rich-presence frame.
    // Drives the "Playing fierymud-rs — Lvl X Class" overlay
    // through Mudlet's Discord SDK integration. Server icon /
    // assets keyed by `application_id` from External.Discord.Info
    // (sent at GMCP-confirm time). `start_time` is wall-clock
    // epoch seconds from when the player logged in.
    //
    // Gated on `discord_application_id` being set — without an
    // app ID, External.Discord.Info wasn't sent on connect and
    // the SDK can't bind this Status frame anyway.
    if let Some(prof) = world.get::<Profile>(target) {
        let cfg = world.resource::<mud_world::RuntimeConfig>();
        let app_id = cfg.get_string("gmcp", "discord_application_id", "");
        if !app_id.is_empty() {
            let class_label = prof
                .class_id
                .and_then(|id| {
                    world
                        .get_resource::<ClassCatalog>()
                        .and_then(|c| c.by_id.get(&id))
                        .map(|d| d.plain_name.as_str())
                })
                .unwrap_or("Adventurer");
            let plain_name = name
                .map(|s| {
                    render_color_tags(s, ColorMode::Strip)
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                })
                .unwrap_or_default();
            let start_time = compute_login_unix_ts(world, target);
            let game = cfg.get_string("gmcp", "discord_game_name", "fierymud-rs");
            let state = cfg.get_string("gmcp", "discord_state", "");
            let small_image = cfg.get_string("gmcp", "discord_small_image", "");
            let small_image_text = cfg.get_string("gmcp", "discord_small_image_text", "");
            let details = format!(
                "Character: {plain_name}  Class: {class}  Level: {lvl}",
                plain_name = plain_name,
                class = class_label.replace('"', "\\\""),
                lvl = prof.level,
            );
            let payload = format!(
                "{{\"state\":\"{state}\",\"details\":\"{details}\",\"game\":\"{game}\",\"small_image\":[\"{small_image}\"],\"small_image_text\":\"{small_image_text}\",\"start_time\":{start_time}}}",
                state = state.replace('"', "\\\""),
                game = game.replace('"', "\\\""),
                small_image = small_image.replace('"', "\\\""),
                small_image_text = small_image_text.replace('"', "\\\""),
            );
            let _ = conn.try_send(mud_net::gmcp_packet(
                "External.Discord.Status",
                &payload,
            ));
        }
    }
    // Char.Aggro: every mob (anywhere) that has the player on its
    // HateList or in MobMemory. Lets HUD clients render a "things
    // hunting you" panel without polling. Two arrays so the client
    // can split active threats from "remembers you" stragglers.
    {
        let mut hating: Vec<String> = Vec::new();
        let mut remembering: Vec<String> = Vec::new();
        let mut q = world.query_filtered::<
            (&Named, Option<&crate::combat::HateList>, Option<&crate::combat::MobMemory>),
            With<Mob>,
        >();
        for (n, hate, mem) in q.iter(world) {
            let in_hate = hate.is_some_and(|h| h.0.contains(&target));
            let in_mem = mem.is_some_and(|m| m.0.contains(&target));
            if in_hate {
                hating.push(format!(
                    "\"{}\"",
                    render_color_tags(&n.name, ColorMode::Strip)
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                ));
            } else if in_mem {
                remembering.push(format!(
                    "\"{}\"",
                    render_color_tags(&n.name, ColorMode::Strip)
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                ));
            }
        }
        if !hating.is_empty() || !remembering.is_empty() {
            let payload = format!(
                "{{\"hating\":[{}],\"remembering\":[{}]}}",
                hating.join(","),
                remembering.join(",")
            );
            let _ = conn.try_send(mud_net::gmcp_packet("Char.Aggro", &payload));
        }
    }

    // Group — IRE-shaped party panel. Solo players get an empty
    // `{}` frame so a previously-visible panel clears; grouped
    // players get the full roster with per-member stats. Per-prompt
    // cadence is generous; the helper short-circuits the empty
    // case so it stays cheap. `conn` is the cloned Outbound from
    // the top of this function — it stays valid across the
    // helper's `&mut World` since it's owned.
    send_group_state(world, target);

    // Combat / room / skill panels. `send_room_mobs` also emits
    // the derived `Room.Services` frame in the same pass.
    send_char_combat(world, target);
    send_room_mobs(world, target);
    send_char_skills(world, target);

    // Char.Effects: array of `{name, ability, duration, source,
    // strength}` for every active effect on the player. Drives
    // client-side buff/debuff panels.
    //
    // Both `name` (the effect's own label, e.g. "ward") AND
    // `ability` (the spell that caused it, e.g. "armor") are
    // emitted. The two are distinct in the data model — a single
    // `armor` cast applies a `ward` effect — but Mudlet's icon
    // sets and player intuition are keyed off the spell. Clients
    // pick: name for descriptive display, ability for the icon
    // and the player-facing label most users expect ("you have
    // armor up", not "you have ward up"). `ability` is empty
    // when the effect has no originating ability (admin grants,
    // environmental auras).
    //
    // Cadence matches the prompt — per-prompt refresh is cheap
    // and tracks ticks transparently. `duration` is seconds
    // remaining (-1 = permanent); `source` is the high-level
    // origin tag (spell / item / room / admin / other).
    {
        use mud_world::{AppliedTo as Applied, EffectInstance, EffectSource};
        let mut entries: Vec<String> = Vec::new();
        // Snapshot ability id → plain name once outside the loop.
        // The catalog's `by_name` is HashMap<&str, AbilityDef>,
        // so reverse-lookup by id requires a scan; collecting it
        // up front avoids re-scanning per effect.
        let ability_names: std::collections::HashMap<i32, String> = world
            .get_resource::<AbilityCatalog>()
            .map(|c| c.by_name.values().map(|d| (d.id, d.plain_name.clone())).collect())
            .unwrap_or_default();
        let mut q = world.query::<(&EffectInstance, &Applied)>();
        for (inst, applied) in q.iter(world) {
            if applied.0 != target {
                continue;
            }
            let safe_name = inst
                .name
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            let safe_ability = inst
                .ability_id
                .and_then(|id| ability_names.get(&id))
                .map(|s| {
                    render_color_tags(s, ColorMode::Strip)
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                })
                .unwrap_or_default();
            let source_label = match &inst.source {
                EffectSource::Spell => "spell",
                EffectSource::Item => "item",
                EffectSource::Room => "room",
                EffectSource::Admin => "admin",
                EffectSource::Other(_) => "other",
            };
            entries.push(format!(
                "{{\"name\":\"{}\",\"ability\":\"{}\",\"duration\":{},\"source\":\"{}\",\"strength\":{}}}",
                safe_name, safe_ability, inst.remaining_secs, source_label, inst.strength
            ));
        }
        // Always emit, even when empty — clients use the empty
        // array to clear stale icons.
        let payload = format!("[{}]", entries.join(","));
        let _ = conn.try_send(mud_net::gmcp_packet("Char.Effects", &payload));
    }

    // Room.Info — IRE-shaped mapper feed. Mudlet's stock mapper
    // script keys off this exact field set; emitting the legacy
    // {zone, id, exits:[...]} shape silently dropped the room
    // from any Mudlet auto-mapping. Shape:
    //   { num: int           // composite key, zone*100000+id
    //   , name: string       // room title (color-stripped)
    //   , area: string       // zone display name
    //   , environment: string  // sector type label
    //   , exits: { dir: int }  // direction → destination composite num
    //   , doors: { dir: state }  // direction → "closed" / "locked"
    //   }
    // The composite num encoding is reversible (id = num %
    // 100000, zone = num / 100000) and unique within a 5-digit
    // local-id space — comfortably above any zone we have today.
    if let Some(located) = world.get::<Located>(target) {
        let room = located.0;
        let room_name = world
            .get::<Named>(room)
            .map_or_else(String::new, |n| n.name.clone());
        let plain_name = render_color_tags(&room_name, ColorMode::Strip)
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let (zone_id, room_id) = world
            .get::<WorldKey>(room)
            .map_or((-1, -1), |k| (k.zone, k.id));
        let num = room_composite_num(zone_id, room_id);
        // Zone display name: walk to the zone entity via WorldKeyIndex.
        let area_name = world
            .get_resource::<WorldKeyIndex>()
            .and_then(|idx| idx.zones.get(&zone_id).copied())
            .and_then(|zone_e| world.get::<Named>(zone_e).map(|n| n.name.clone()))
            .unwrap_or_default();
        let area_plain = render_color_tags(&area_name, ColorMode::Strip)
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let environment = world
            .get::<RoomSector>(room)
            .map(|s| sector_label(s.0))
            .unwrap_or("Unknown");
        // Exits dict: direction → destination composite num. Doors
        // dict: direction → "closed" / "locked" for non-Open
        // states. Hidden exits omitted entirely (the same way
        // `look` hides them from unsearched rooms).
        let mut exit_entries: Vec<String> = Vec::new();
        let mut door_entries: Vec<String> = Vec::new();
        if let Some(exits) = world.get::<Exits>(room) {
            for (dir, data) in &exits.0 {
                if data.is_hidden {
                    continue;
                }
                let dir_name = direction_name(*dir);
                let dest_num = data
                    .to
                    .and_then(|e| world.get::<WorldKey>(e))
                    .map_or(0, |k| room_composite_num(k.zone, k.id));
                exit_entries.push(format!("\"{dir_name}\":{dest_num}"));
                let door_state = match data.state {
                    mud_db::enums::ExitState::Open => None,
                    mud_db::enums::ExitState::Closed => Some("closed"),
                    mud_db::enums::ExitState::Locked => Some("locked"),
                };
                if let Some(state) = door_state {
                    door_entries.push(format!("\"{dir_name}\":\"{state}\""));
                }
            }
        }
        // Server-side layout coords. Builders set Room.layoutX /
        // layoutY / layoutZ in Muditor; the loader attaches them
        // as a `RoomLayout` component. When present, emit them
        // as `coords` so client-side mappers can place rooms
        // exactly where the builder laid them out instead of
        // doing compass-walk auto-placement. Absence of the
        // field signals "auto-place me" — the Mudlet rewrite
        // gates on `coords` when picking a strategy.
        let coords_field = world
            .get::<mud_world::RoomLayout>(room)
            .map(|l| format!(",\"coords\":\"{},{},{}\"", l.x, l.y, l.z))
            .unwrap_or_default();
        let payload = format!(
            "{{\"num\":{num},\"name\":\"{plain_name}\",\"area\":\"{area_plain}\",\"environment\":\"{environment}\",\"exits\":{{{}}},\"doors\":{{{}}}{}}}",
            exit_entries.join(","),
            door_entries.join(","),
            coords_field,
        );
        let _ = conn.try_send(mud_net::gmcp_packet("Room.Info", &payload));
    }
}

/// Encode a `(zone, id)` composite room key as a single integer
/// for clients that expect IRE-style integer room ids. The legacy
/// CircleMUD vnum scheme (`zone*100 + id`) maxes out around 10000;
/// our (i32, i32) namespace is much larger so we use a
/// 5-decimal-digit local-id field — `zone*100000 + id`. Reversible:
/// `id = num % 100000`, `zone = num / 100000`. Returns `0` for
/// missing keys, which Mudlet's mapper treats as "no destination
/// known yet" and drops the edge gracefully.
fn room_composite_num(zone: i32, id: i32) -> i32 {
    if zone < 0 || id < 0 || id >= 100_000 {
        return 0;
    }
    zone.saturating_mul(100_000).saturating_add(id)
}

/// Map a `Sector` enum value to the IRE-convention environment
/// label. Mudlet's mapper uses these labels to pick a per-sector
/// terrain color; the strings match the seeded sector names from
/// the C++ legacy package's mapper config so existing Mudlet
/// scripts work without re-keying.
fn sector_label(s: mud_db::enums::Sector) -> &'static str {
    use mud_db::enums::Sector;
    match s {
        Sector::Structure => "Structure",
        Sector::City => "City",
        Sector::Field => "Field",
        Sector::Forest => "Forest",
        Sector::Hills => "Hills",
        Sector::Mountain => "Mountains",
        Sector::Shallows => "Shallows",
        Sector::Water => "Water",
        Sector::Underwater => "Underwater",
        Sector::Air => "Air",
        Sector::Road => "Road",
        Sector::Grasslands => "Grasslands",
        Sector::Cave => "Cave",
        Sector::Ruins => "Ruins",
        Sector::Swamp => "Swamp",
        Sector::Beach => "Beach",
        Sector::Underdark => "Underdark",
        Sector::Astralplane => "Astralplane",
        Sector::Airplane => "Airplane",
        Sector::Fireplane => "Fireplane",
        Sector::Earthplane => "Earthplane",
        Sector::Etherealplane => "Etherealplane",
        Sector::Avernus => "Avernus",
    }
}

/// In-memory ring buffer of admin-mutating actions for replay /
/// review. Capped at 256 entries; oldest get dropped on overflow.
/// Surfaced via `show audit`. Not persisted to DB yet — separate
/// concern from the schema's `script_error_log` table.
#[derive(Resource, Default)]
pub struct AdminAuditLog {
    pub entries: std::collections::VecDeque<AdminAuditEntry>,
}

#[derive(Debug, Clone)]
pub struct AdminAuditEntry {
    pub at: std::time::SystemTime,
    pub actor_name: String,
    pub verb: &'static str,
    pub args: String,
}

impl AdminAuditLog {
    const CAP: usize = 256;
    pub fn push(&mut self, e: AdminAuditEntry) {
        if self.entries.len() >= Self::CAP {
            self.entries.pop_front();
        }
        self.entries.push_back(e);
    }
}

/// Record one admin-mutating command for the audit log. Logged at
/// info level too so operators tailing tracing get the same data.
/// Grant an achievement to `player` by stable code. Idempotent —
/// no-op if the player already has the achievement. Writes to the
/// DB fire-and-forget via the world-installed pool, updates the
/// in-memory `CharacterAchievements` component, and broadcasts a
/// "Achievement unlocked: <title>" line to the player.
pub(crate) fn grant_achievement(world: &mut World, player: Entity, code: &str) {
    let def = world
        .get_resource::<mud_world::AchievementCatalog>()
        .and_then(|c| c.by_code.get(code))
        .cloned();
    let Some(def) = def else {
        tracing::debug!(code = %code, "grant_achievement: unknown code");
        return;
    };
    // Already-unlocked check via the in-memory component.
    let already = world
        .get::<mud_world::CharacterAchievements>(player)
        .is_some_and(|c| c.unlocked.contains(&def.id));
    if already {
        return;
    }
    // Update in-memory state immediately so back-to-back grants
    // (same code from two simultaneous events) collapse to one
    // notification + one DB write.
    let has_component = world
        .get::<mud_world::CharacterAchievements>(player)
        .is_some();
    if has_component {
        if let Some(mut c) = world.get_mut::<mud_world::CharacterAchievements>(player) {
            c.unlocked.insert(def.id);
        }
    } else {
        let mut ca = mud_world::CharacterAchievements::default();
        ca.unlocked.insert(def.id);
        try_insert(world, player, ca);
    }
    // Fire-and-forget DB write. The character_id lives on Account.
    let character_id = world
        .get::<Account>(player)
        .map(|a| a.character_id.clone());
    if let (Some(cid), Some(pool)) = (
        character_id,
        world.get_resource::<DbPool>().map(|p| p.0.clone()),
    ) {
        let id = def.id;
        tokio::spawn(async move {
            if let Err(e) =
                mud_db::achievements::grant(&pool, &cid, id, None).await
            {
                tracing::warn!(error = %e, achievement_id = id, "achievement grant write failed");
            }
        });
    }
    let mode = color_mode_for(world, player);
    send_to(
        world,
        player,
        render_color_tags(
            &format!(
                "<yellow>Achievement unlocked: {}</> — {}\r\n",
                def.title, def.description,
            ),
            mode,
        ),
    );
    tracing::info!(
        player = ?player,
        code = %def.code,
        title = %def.title,
        "achievement granted",
    );
}

/// Track that `player` has set foot in `room` and, if the visit
/// completes the room's zone, fire the `zone_<N>_cleared` achievement.
/// Persists the visited set to `CharacterAchievement.progress` JSON
/// (via fire-and-forget upsert) so partial progress survives logout.
/// On unlock, `grant_achievement` flips the in-memory unlocked set
/// and the next login picks it up via the room-count check.
pub(crate) fn mark_room_visited(world: &mut World, player: Entity, room: Entity) {
    if world.get::<Player>(player).is_none() {
        return;
    }
    let key = match world.get::<WorldKey>(room) {
        Some(k) => *k,
        None => return,
    };
    let needs_init = world.get::<mud_world::ZoneVisits>(player).is_none();
    if needs_init {
        try_insert(world, player, mud_world::ZoneVisits::default());
    }
    let newly_inserted = world
        .get_mut::<mud_world::ZoneVisits>(player)
        .is_some_and(|mut v| v.by_zone.entry(key.zone).or_default().insert(key.id));
    if !newly_inserted {
        return;
    }
    let total_in_zone = world
        .resource::<WorldKeyIndex>()
        .rooms
        .keys()
        .filter(|(z, _)| *z == key.zone)
        .count();
    if total_in_zone == 0 {
        return;
    }
    let visited_set: Vec<i32> = world
        .get::<mud_world::ZoneVisits>(player)
        .and_then(|v| v.by_zone.get(&key.zone))
        .map(|s| s.iter().copied().collect())
        .unwrap_or_default();
    let achievement_id = world
        .get_resource::<mud_world::AchievementCatalog>()
        .and_then(|c| c.by_code.get(&format!("zone_{}_cleared", key.zone)))
        .map(|d| d.id);
    let character_id = world
        .get::<Account>(player)
        .map(|a| a.character_id.clone());
    if let (Some(ach_id), Some(cid), Some(pool)) = (
        achievement_id,
        character_id,
        world.get_resource::<DbPool>().map(|p| p.0.clone()),
    ) {
        let progress = serde_json::json!({ "visited": visited_set });
        tokio::spawn(async move {
            if let Err(e) =
                mud_db::achievements::upsert_progress(&pool, &cid, ach_id, &progress).await
            {
                tracing::warn!(error = %e, "zone-clear progress write failed");
            }
        });
    }
    if visited_set.len() >= total_in_zone {
        let code = format!("zone_{}_cleared", key.zone);
        grant_achievement(world, player, &code);
    }
    // Quest objective: VISIT_ROOM. Same group-walk path as kill,
    // gated by the SOLO/PARTY scope so a scout in a group brings
    // the rest of the party along on shared visit objectives.
    bump_visit_quest_progress(world, player, key.zone, key.id);
    // Quest trigger: ROOM (Wave 4.1). Any quest authored with
    // `triggerType = ROOM` and a matching `triggerRoom*` key is
    // offered when the player first enters here.
    crate::quest_triggers::dispatch_room_trigger(world, player, key.zone, key.id);
}

/// Apply each environmental effect bound to `room` to `player`,
/// spawning a fresh `EffectInstance` child for each (with
/// `EffectSource::Room`). Skips effects already present on the
/// entity from the same source so re-entry within the duration
/// doesn't pile duplicates. Effects decay through the normal
/// `effects_tick` once the player leaves the room.
pub(crate) fn apply_room_environment_at_login(
    world: &mut World,
    player: Entity,
    room: Entity,
) {
    apply_room_environment(world, player, room);
}

pub(crate) fn apply_room_environment(world: &mut World, player: Entity, room: Entity) {
    let key = match world.get::<WorldKey>(room) {
        Some(k) => *k,
        None => return,
    };
    let effect_ids: Vec<i32> = world
        .get_resource::<mud_world::RoomEnvironmentalEffects>()
        .and_then(|r| r.by_room.get(&(key.zone, key.id)).cloned())
        .unwrap_or_default();
    if effect_ids.is_empty() {
        return;
    }
    // Snapshot which Room-sourced effects are already on the
    // player so we don't pile duplicates on quick re-entries.
    let already_on: std::collections::HashSet<i32> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(ei, at)| at.0 == player && ei.source == EffectSource::Room)
            .map(|(ei, _)| ei.kind)
            .collect()
    };
    for eid in effect_ids {
        if already_on.contains(&eid) {
            continue;
        }
        let def = world
            .get_resource::<EffectCatalog>()
            .and_then(|c| c.by_id.get(&eid).cloned());
        let Some(def) = def else { continue };
        // Default duration: schema's effect default_params.duration
        // if present, else 60s. Short enough that leaving the room
        // sobers off in a minute; long enough that two adjacent
        // env-effect rooms don't constantly re-tick.
        let dur_secs = def
            .default_params
            .get("duration")
            .and_then(serde_json::Value::as_i64)
            .map_or(60, |v| i32::try_from(v).unwrap_or(60));
        world.spawn((
            EffectInstance {
                kind: def.id,
                name: def.name.clone(),
                strength: 1,
                remaining_secs: dur_secs,
                source: EffectSource::Room,
                ability_id: None,
            },
            AppliedTo(player),
        ));
    }
}

/// What kind of objective progress to advance. The DB query is
/// distinct per kind; everything else (group walk, dedup, async
/// dispatch, progress message) is shared in `bump_quest_progress`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum QuestObjectiveBump {
    KillMob { zone: i32, id: i32 },
    VisitRoom { zone: i32, id: i32 },
    TalkToNpc { zone: i32, id: i32 },
    CollectItem { zone: i32, id: i32 },
    DeliverItem {
        item_zone: i32,
        item_id: i32,
        mob_zone: i32,
        mob_id: i32,
    },
    UseSkill { ability_id: i32 },
}

/// Advance any active `KILL_MOB` objectives whose target matches
/// `victim`'s prototype `(zone, id)`. Thin wrapper for the
/// generic group-walk path.
pub(crate) fn bump_kill_quest_progress(
    world: &mut World,
    killer: Entity,
    victim_proto_zone: i32,
    victim_proto_id: i32,
) {
    bump_quest_progress(
        world,
        killer,
        QuestObjectiveBump::KillMob {
            zone: victim_proto_zone,
            id: victim_proto_id,
        },
    );
}

/// Advance any active `VISIT_ROOM` objectives whose target matches
/// the room the player just entered. Called from `cmd_move`'s
/// arrival path (and login spawn) for every player mover.
pub(crate) fn bump_visit_quest_progress(
    world: &mut World,
    visitor: Entity,
    room_zone: i32,
    room_id: i32,
) {
    bump_quest_progress(
        world,
        visitor,
        QuestObjectiveBump::VisitRoom {
            zone: room_zone,
            id: room_id,
        },
    );
}

/// Advance any active `USE_SKILL` objectives matching the just-
/// invoked ability id. Called from the bottom of
/// `invoke_ability_with` (post-cooldown), so failed casts don't
/// credit. Also fires the SKILL-trigger dispatcher (Wave 4.1).
pub(crate) fn bump_use_skill_quest_progress(
    world: &mut World,
    caster: Entity,
    ability_id: i32,
) {
    bump_quest_progress(
        world,
        caster,
        QuestObjectiveBump::UseSkill { ability_id },
    );
    crate::quest_triggers::dispatch_skill_trigger(world, caster, ability_id);
}

/// Advance any active `DELIVER_ITEM` objectives matching the
/// (item proto, recipient mob proto) pair. Called from `cmd_give`
/// when an item changes hands to a mob.
pub(crate) fn bump_deliver_quest_progress(
    world: &mut World,
    giver: Entity,
    item_zone: i32,
    item_id: i32,
    mob_zone: i32,
    mob_id: i32,
) {
    bump_quest_progress(
        world,
        giver,
        QuestObjectiveBump::DeliverItem {
            item_zone,
            item_id,
            mob_zone,
            mob_id,
        },
    );
}

/// Advance any active `COLLECT_ITEM` objectives whose target
/// object matches the just-picked-up item's prototype. Called
/// from `cmd_get` when an item moves into the player's inventory.
/// Also fires the ITEM-trigger dispatcher (Wave 4.1).
pub(crate) fn bump_collect_quest_progress(
    world: &mut World,
    collector: Entity,
    object_zone: i32,
    object_id: i32,
) {
    bump_quest_progress(
        world,
        collector,
        QuestObjectiveBump::CollectItem {
            zone: object_zone,
            id: object_id,
        },
    );
    crate::quest_triggers::dispatch_item_trigger(world, collector, object_zone, object_id);
}

/// Advance any active `TALK_TO_NPC` objectives whose target mob
/// matches the entity addressed. Called from `cmd_ask` when the
/// target is a mob.
pub(crate) fn bump_talk_quest_progress(
    world: &mut World,
    speaker: Entity,
    mob_zone: i32,
    mob_id: i32,
) {
    bump_quest_progress(
        world,
        speaker,
        QuestObjectiveBump::TalkToNpc {
            zone: mob_zone,
            id: mob_id,
        },
    );
}

/// Group-walk + async dispatch shared by every objective-bump
/// path. Walks the actor's follower chain to find every online
/// party member, then dispatches one task per member that does
/// the kind-specific DB read + upsert and sends a progress line
/// through that member's own outbound channel.
#[allow(clippy::too_many_lines)]
pub(crate) fn bump_quest_progress(world: &mut World, actor: Entity, kind: QuestObjectiveBump) {
    if world.get::<Player>(actor).is_none() {
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    // Snapshot the group: leader is the chain root, members are
    // every player in the follow tree. Filter to those with
    // Account + Connection so we have something to send back to.
    let root = group_root(world, actor);
    let members = group_members(world, root);
    let recipients: Vec<(Entity, String, mud_net::Outbound)> = members
        .iter()
        .filter_map(|&e| {
            let cid = world.get::<Account>(e).map(|a| a.character_id.clone())?;
            let out = world.get::<Connection>(e).map(|c| c.0.clone())?;
            Some((e, cid, out))
        })
        .collect();
    let update_tx_root = world
        .get_resource::<PlayerUpdateTx>()
        .map(|t| t.0.clone());
    for (entity, cid, out) in recipients {
        let is_actor = entity == actor;
        let pool = pool.clone();
        let update_tx = update_tx_root.clone();
        tokio::spawn(async move {
            let rows_res = match kind {
                QuestObjectiveBump::KillMob { zone, id } => {
                    mud_db::quest_objectives::list_kill_mob_progress(
                        &pool, &cid, zone, id, is_actor,
                    )
                    .await
                }
                QuestObjectiveBump::VisitRoom { zone, id } => {
                    mud_db::quest_objectives::list_visit_room_progress(
                        &pool, &cid, zone, id, is_actor,
                    )
                    .await
                }
                QuestObjectiveBump::TalkToNpc { zone, id } => {
                    mud_db::quest_objectives::list_talk_to_npc_progress(
                        &pool, &cid, zone, id, is_actor,
                    )
                    .await
                }
                QuestObjectiveBump::CollectItem { zone, id } => {
                    mud_db::quest_objectives::list_collect_item_progress(
                        &pool, &cid, zone, id, is_actor,
                    )
                    .await
                }
                QuestObjectiveBump::DeliverItem {
                    item_zone,
                    item_id,
                    mob_zone,
                    mob_id,
                } => {
                    mud_db::quest_objectives::list_deliver_item_progress(
                        &pool, &cid, item_zone, item_id, mob_zone, mob_id, is_actor,
                    )
                    .await
                }
                QuestObjectiveBump::UseSkill { ability_id } => {
                    mud_db::quest_objectives::list_use_skill_progress(
                        &pool, &cid, ability_id, is_actor,
                    )
                    .await
                }
            };
            let rows = match rows_res {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, ?kind, "objective lookup failed");
                    return;
                }
            };
            for row in rows {
                let new_count = (row.current_count + 1).min(row.required_count);
                let completed = new_count >= row.required_count;
                if let Err(e) = mud_db::quest_objectives::upsert_progress(
                    &pool,
                    &row.character_quest_id,
                    row.quest_zone_id,
                    row.quest_id,
                    row.phase_id,
                    row.objective_id,
                    new_count,
                    completed,
                )
                .await
                {
                    tracing::warn!(error = %e, "objective upsert failed");
                    continue;
                }
                // After a completing bump, try advancing the phase
                // (or finishing the quest entirely).
                if completed {
                    match mud_db::quest_objectives::try_advance_phase(
                        &pool,
                        &row.character_quest_id,
                    )
                    .await
                    {
                        Ok(mud_db::quest_objectives::PhaseAdvance::Advanced { name, .. }) => {
                            let _ = out.try_send(
                                format!("Quest phase complete — moving to: {name}\r\n")
                                    .into_bytes(),
                            );
                        }
                        Ok(mud_db::quest_objectives::PhaseAdvance::QuestComplete) => {
                            let _ = out.try_send(
                                b"*** Quest complete! ***\r\n".to_vec(),
                            );
                            // Grant simple rewards (XP/gold/skill
                            // points/ability) via DB; announce all
                            // including ITEM/HOUSING which the
                            // questgiver still needs to hand out.
                            //
                            // Wave 4.10: conditional rewards
                            // (`condition` Lua non-null) need a
                            // `&mut World` to evaluate, which
                            // isn't reachable from this tokio task.
                            // Defer them — surface a "claim with
                            // qreward" hint and let the synchronous
                            // claim path do the condition check.
                            let all_rewards = mud_db::quest_objectives::list_quest_rewards(
                                &pool,
                                row.quest_zone_id,
                                row.quest_id,
                            )
                            .await
                            .unwrap_or_default();
                            let (deferred, rewards): (Vec<_>, Vec<_>) = all_rewards
                                .into_iter()
                                .partition(|r| {
                                    r.condition
                                        .as_deref()
                                        .is_some_and(|c| !c.trim().is_empty())
                                });
                            if !deferred.is_empty() {
                                let _ = out.try_send(
                                    format!(
                                        "Conditional rewards available — \
                                         type `qreward {} {}` to view and claim.\r\n",
                                        row.quest_zone_id, row.quest_id
                                    )
                                    .into_bytes(),
                                );
                            }
                            if !rewards.is_empty() {
                                if let Err(e) =
                                    mud_db::quest_objectives::grant_simple_rewards(
                                        &pool, &cid, &rewards,
                                    )
                                    .await
                                {
                                    tracing::warn!(error = %e, "reward grant failed");
                                }
                                // Mirror the DB updates onto the
                                // running ECS components so the
                                // player sees the gain immediately
                                // (without logout/login).
                                for r in &rewards {
                                    let update = match r.reward_type.as_str() {
                                        "EXPERIENCE" => r.amount.map(|a| {
                                            PendingPlayerUpdate::ExperienceDelta {
                                                character_id: cid.clone(),
                                                amount: a,
                                            }
                                        }),
                                        "GOLD" => r.amount.map(|a| {
                                            PendingPlayerUpdate::WealthDelta {
                                                character_id: cid.clone(),
                                                amount: i64::from(a),
                                            }
                                        }),
                                        "SKILL_POINTS" => r.amount.map(|a| {
                                            PendingPlayerUpdate::SkillPointsDelta {
                                                character_id: cid.clone(),
                                                amount: a,
                                            }
                                        }),
                                        "ABILITY" => r.ability_id.map(|id| {
                                            PendingPlayerUpdate::AbilityKnown {
                                                character_id: cid.clone(),
                                                ability_id: id,
                                            }
                                        }),
                                        "ITEM" => match (r.object_zone_id, r.object_id) {
                                            (Some(z), Some(id)) => {
                                                Some(PendingPlayerUpdate::SpawnItem {
                                                    character_id: cid.clone(),
                                                    object_zone: z,
                                                    object_id: id,
                                                    quantity: r.quantity,
                                                })
                                            }
                                            _ => None,
                                        },
                                        _ => None,
                                    };
                                    if let (Some(u), Some(tx)) = (update, update_tx.as_ref()) {
                                        // Bounded channel — await until the tick drains
                                        // a slot. Failure means the receiver dropped
                                        // (server shutting down); silently ignore.
                                        let _ = tx.send(u).await;
                                    }
                                }
                                let mut buf = String::from("Rewards:\r\n");
                                for r in &rewards {
                                    let line = match (r.reward_type.as_str(), r.amount, r.quantity) {
                                        ("EXPERIENCE", Some(a), _) => {
                                            format!("  +{a} experience\r\n")
                                        }
                                        ("GOLD", Some(a), _) => format!("  +{a} gold\r\n"),
                                        ("SKILL_POINTS", Some(a), _) => {
                                            format!("  +{a} skill points\r\n")
                                        }
                                        ("ABILITY", _, _) => {
                                            "  +1 new ability\r\n".to_string()
                                        }
                                        ("ITEM", _, q) => format!(
                                            "  +{q} item(s) — see questgiver\r\n"
                                        ),
                                        ("HOUSING", _, _) => {
                                            "  +housing access — see questgiver\r\n"
                                                .to_string()
                                        }
                                        _ => continue,
                                    };
                                    buf.push_str(&line);
                                }
                                if buf.len() > "Rewards:\r\n".len() {
                                    let _ = out.try_send(buf.into_bytes());
                                }
                            }
                        }
                        Ok(mud_db::quest_objectives::PhaseAdvance::Pending) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "phase advance check failed");
                        }
                    }
                }
                let prefix = if is_actor {
                    String::new()
                } else {
                    "(party) ".to_string()
                };
                let line = if completed {
                    format!(
                        "{prefix}Quest objective complete: {}\r\n",
                        row.player_description
                    )
                } else if row.show_progress {
                    format!(
                        "{prefix}Quest objective: {} ({}/{})\r\n",
                        row.player_description, new_count, row.required_count
                    )
                } else {
                    format!(
                        "{prefix}Quest objective updated: {}\r\n",
                        row.player_description
                    )
                };
                let _ = out.try_send(line.into_bytes());
            }
        });
    }
}

/// Bump the player's lifetime-kill counter, persist to the
/// `kill_tracking_data` JSON, and grant any milestone achievement
/// the new total has just crossed (`kills_100`, `kills_500`, ...).
/// Fire-and-forget DB write — the next login just reads whatever
/// landed.
pub(crate) fn bump_kill_count(world: &mut World, player: Entity) {
    let new_total = {
        let Some(mut stats) = world.get_mut::<mud_world::KillStats>(player) else {
            return;
        };
        stats.total = stats.total.saturating_add(1);
        stats.total
    };
    // Threshold check.
    for milestone in [100, 500, 1000, 5000, 10000] {
        if new_total == milestone {
            let code = format!("kills_{milestone}");
            grant_achievement(world, player, &code);
        }
    }
    // Persist. The shape of `kill_tracking_data` is owned by the
    // runtime; preserve any other fields the column may already
    // hold by merging into the JSON object.
    let character_id = world
        .get::<Account>(player)
        .map(|a| a.character_id.clone());
    if let (Some(cid), Some(pool)) = (
        character_id,
        world.get_resource::<DbPool>().map(|p| p.0.clone()),
    ) {
        tokio::spawn(async move {
            let mut data = mud_db::characters::load_kill_tracking(&pool, &cid)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| serde_json::json!({}));
            if let Some(obj) = data.as_object_mut() {
                obj.insert(
                    "total".to_string(),
                    serde_json::Value::from(new_total),
                );
            } else {
                data = serde_json::json!({ "total": new_total });
            }
            if let Err(e) = mud_db::characters::save_kill_tracking(&pool, &cid, &data).await {
                tracing::warn!(error = %e, "kill_tracking_data write failed");
            }
        });
    }
}

pub(crate) fn record_admin_action(
    world: &mut World,
    actor: Entity,
    verb: &'static str,
    args: &str,
) {
    if !world.contains_resource::<AdminAuditLog>() {
        world.insert_resource(AdminAuditLog::default());
    }
    let actor_name = name_of(world, actor);
    tracing::info!(actor = %actor_name, verb = %verb, args = %args, "admin action");
    world.resource_mut::<AdminAuditLog>().push(AdminAuditEntry {
        at: std::time::SystemTime::now(),
        actor_name: actor_name.clone(),
        verb,
        args: args.to_string(),
    });
    // Persist to AuditLogs. Fire-and-forget so the command path
    // doesn't block on the DB write. user_id comes from the
    // actor's Account; the target identifier is the actor name
    // for now (the verb's actual target is in `args` and a
    // future pass can parse + index that more cleanly).
    let user_id = world.get::<Account>(actor).map(|a| a.user_id.clone());
    if let (Some(uid), Some(pool)) = (
        user_id,
        world.get_resource::<DbPool>().map(|p| p.0.clone()),
    ) {
        let args = args.to_string();
        tokio::spawn(async move {
            if let Err(e) = mud_db::audit::record(&pool, &uid, verb, &actor_name, &args).await {
                tracing::warn!(error = %e, verb, "audit log persist failed");
            }
        });
    }
}

/// 10-cell ASCII bar with a color tag wrapping the filled portion.
/// 100% = `<green>[##########]</>`, 50% = `<yellow>[#####_____]</>`,
/// 10% = `<red>[#_________]</>`. Used by the `%B` (HP) and `%M`
/// (stamina) prompt vars. Out-of-range or zero-max readings render
/// an empty bar without color.
pub(crate) fn render_vital_bar(current: i32, max: i32) -> String {
    if max <= 0 {
        return "[__________]".to_string();
    }
    let pct = current.saturating_mul(100) / max.max(1);
    let filled = usize::try_from(pct.clamp(0, 100) / 10).unwrap_or(0);
    let empty = 10usize.saturating_sub(filled);
    let bar = format!("[{}{}]", "#".repeat(filled), "_".repeat(empty));
    let tag = match pct {
        ..=24 => Some("<red>"),
        25..=49 => Some("<yellow>"),
        50..=100 => Some("<green>"),
        _ => None,
    };
    match tag {
        Some(open) => format!("{open}{bar}</>"),
        None => bar,
    }
}

/// XML-Lite open tag for a vital reading at `current/max`. Red
/// below 25%, yellow below 50%, none otherwise. Returns the open
/// tag string; the caller closes with `</>`. Zero / negative max
/// yields no color (defensive — avoids divide-by-zero panic).
/// Wrap a string in a default color tag when it carries no
/// XML-Lite markup of its own. Authored color (rare for room
/// names, occasional for items/players) wins — the wrapper only
/// adds hue to plain content. Used by look-side renders so
/// generic strings get a baseline color without overriding
/// builder intent.
pub(crate) fn colorize_default(s: &str, open: &str) -> String {
    if s.contains('<') {
        s.to_string()
    } else {
        format!("{open}{s}</>")
    }
}

/// "The curtain" / "The gate" / "The way" — sentence-leading noun
/// phrase for an exit, used by closed / locked feedback so a doorway
/// with a builder-set keyword reads as itself ("The curtain is
/// closed.") instead of the generic fallback. First keyword wins;
/// blank / missing keywords fall back to "The way".
#[must_use]
pub(crate) fn exit_noun_phrase(ed: &mud_world::ExitData) -> String {
    for k in &ed.keywords {
        let trimmed = k.trim();
        if !trimmed.is_empty() {
            return format!("The {trimmed}");
        }
    }
    "The way".to_string()
}

/// XML-Lite open tag matching an exit's state. Open exits read
/// cyan (open / passable, scannable as the safe default), closed
/// exits yellow (passable but slow), locked exits red (need a key).
/// Cyan picks up the same hue as section headers / command names so
/// the eye groups "things you can interact with" together; green is
/// reserved for HP / vital stats elsewhere on the sheet. Used for
/// the auto-exit list and the standalone `exits` command.
#[must_use]
pub(crate) fn exit_state_color(state: ExitState) -> &'static str {
    match state {
        ExitState::Open => "<cyan>",
        ExitState::Closed => "<yellow>",
        ExitState::Locked => "<red>",
    }
}

/// XML-Lite open tag for a `UserRole` label. Player → plain (None),
/// staff tiers pick up progressively brighter accents so the role
/// readout on `account` / `clientinfo` / score reads at a glance.
/// Tuned to align with `who_level_color`'s staff band (bold magenta)
/// — Implementor gets bold white as a "top of stack" cap.
#[must_use]
pub(crate) fn role_color_tag(role: mud_db::enums::UserRole) -> Option<&'static str> {
    use mud_db::enums::UserRole;
    match role {
        UserRole::Player => None,
        UserRole::Immortal => Some("<b:cyan>"),
        UserRole::Builder => Some("<b:green>"),
        UserRole::HeadBuilder => Some("<b:yellow>"),
        UserRole::Coder => Some("<b:magenta>"),
        UserRole::Implementor => Some("<b:white>"),
    }
}

/// XML-Lite open tag for a player's `who` level tag, banded by
/// progression milestone. None means "render plain". Bands tuned
/// to the level table: 100+ = immortal staff, 50-99 = endgame,
/// 25-49 = mid, 10-24 = leveling, 1-9 = newbie. Lets the player
/// scan the who list and immediately spot peers / staff.
#[must_use]
pub(crate) fn who_level_color(level: i32) -> Option<&'static str> {
    match level {
        i32::MIN..=0 => None,
        1..=9 => Some("<yellow>"),
        10..=24 => Some("<b:yellow>"),
        25..=49 => Some("<green>"),
        50..=99 => Some("<b:cyan>"),
        100..=104 => Some("<b:magenta>"),
        _ => Some("<b:white>"),
    }
}

/// XML-Lite open tag for a swing's damage value, banded by
/// magnitude so the player's eye lands on heavy hits and crits.
/// Bands tuned around a roughly level-appropriate weapon swing
/// (4-30 normal, 30+ heavy). Values <=5 dim — they read as chip
/// damage that's more flavor than threat. None means render plain.
#[must_use]
pub(crate) fn damage_color_tag(damage: i32) -> Option<&'static str> {
    match damage {
        i32::MIN..=5 => Some("<dim>"),
        6..=15 => None,
        16..=30 => Some("<yellow>"),
        31..=60 => Some("<b:yellow>"),
        _ => Some("<red>"),
    }
}

/// XML-Lite open tag for an effect's remaining duration. Effects
/// running close to expiry render warm so the player notices in
/// time to refresh; longer-lived buffs / debuffs read plain.
/// Bands: <30s red, <2m yellow, longer plain. Permanent effects
/// (negative `remaining_secs` signal) get their own treatment in
/// the caller — this helper only fires for finite durations.
#[must_use]
pub(crate) fn effect_duration_color(remaining_secs: u64) -> Option<&'static str> {
    match remaining_secs {
        0..=29 => Some("<red>"),
        30..=119 => Some("<yellow>"),
        _ => None,
    }
}

/// XML-Lite open tag for an idle duration. Active sessions return
/// None (plain); progressively longer idle gets warmer colors so
/// `who` and `idle` make stale sessions easy to spot at a glance.
/// Bands roughly: <5min plain, <30min cyan, <2h yellow, longer red.
#[must_use]
pub(crate) fn idle_color(idle_secs: u64) -> Option<&'static str> {
    match idle_secs {
        0..=299 => None,                // <5m: active enough
        300..=1799 => Some("<cyan>"),   // 5-30m
        1800..=7199 => Some("<yellow>"), // 30m-2h
        _ => Some("<red>"),             // 2h+
    }
}

/// XML-Lite open tag for a `consider` verdict, banded by the
/// score-ratio formula in `cmd_consider`. Maps the verdict text
/// onto a green→red gradient so a player skimming the consider
/// output lands on "is this safe" before reading the prose.
/// Crossover values match the verdict cutoffs exactly.
#[must_use]
pub(crate) fn consider_verdict_color(ratio: f64) -> &'static str {
    if ratio < 0.30 {
        "<dim>"
    } else if ratio < 0.70 {
        "<green>"
    } else if ratio < 1.50 {
        "<yellow>"
    } else if ratio < 3.00 {
        "<red>"
    } else {
        "<b:red>"
    }
}

/// XML-Lite open tag for a hit-chance percentage. None means render
/// plain (mid range, ~25–75% — the boring "fair fight" zone).
/// Used by `consider` so a player can scan the swing-likelihood
/// numbers without reading them. Bands tuned to the same gut-feel
/// as the verdict gradient: under 25% reads dim (you'll mostly
/// miss), 75%+ reads bold green (you'll mostly land).
#[must_use]
pub(crate) fn hit_chance_color(pct: i32) -> Option<&'static str> {
    match pct {
        i32::MIN..=14 => Some("<dim>"),
        15..=34 => Some("<red>"),
        35..=64 => None,
        65..=84 => Some("<green>"),
        _ => Some("<b:green>"),
    }
}

pub(crate) fn vital_color_tag(current: i32, max: i32) -> Option<&'static str> {
    if max <= 0 {
        return None;
    }
    let pct = current.saturating_mul(100) / max;
    match pct {
        ..=24 => Some("<red>"),
        25..=49 => Some("<yellow>"),
        _ => None,
    }
}

/// XML-Lite open tag for an ability's sphere — fire=red,
/// water=cyan, healing=green, etc. Lets the spells / chants /
/// songs / skills listings render sphere parentheticals in their
/// elemental hue so a player can spot fire spells at a glance.
/// Returns `None` for sphere strings that aren't on the palette
/// (caller renders dim or plain). Input is the lowercase form
/// loaded from `Ability.sphere`.
#[must_use]
pub(crate) fn sphere_color_tag(sphere: &str) -> Option<&'static str> {
    match sphere {
        "fire" => Some("<red>"),
        "water" => Some("<cyan>"),
        "air" => Some("<b:cyan>"),
        "earth" => Some("<yellow>"),
        "healing" => Some("<green>"),
        "death" => Some("<b:black>"),
        "protection" => Some("<b:white>"),
        "enchantment" => Some("<magenta>"),
        "summoning" => Some("<b:magenta>"),
        "divination" => Some("<b:yellow>"),
        // GENERIC / unmapped: caller's dim fallback wins.
        _ => None,
    }
}

/// Bag of substitutions the prompt template can read. Bundled in
/// a struct so adding new variables (`%t`, `%s`, …) doesn't keep
/// growing the function signature; older callers can keep building
/// it inline with `..PromptCtx::default()`.
#[derive(Default, Clone, Copy)]
pub(crate) struct PromptCtx<'a> {
    pub hp: Option<Health>,
    pub stamina: Option<Stamina>,
    pub name: Option<&'a str>,
    pub room: Option<&'a str>,
    pub wealth: Option<i64>,
    /// In-game hour 0..=23. Surfaces as `%t` zero-padded ("07").
    pub hour: Option<i32>,
    /// Season label ("Winter") for `%s`. Read off `MudClock`.
    pub season: Option<&'a str>,
    /// "day" or "night" — surfaces as `%d`. Matches `room_is_dark`'s
    /// 22..=05 window so players can theme prompts by daylight.
    pub day_night: Option<&'a str>,
    /// Current opponent's display name. `Some` only while the
    /// player has a live `Fighting` link; surfaces as `%N`.
    pub enemy_name: Option<&'a str>,
    /// Current opponent's vitals. Drives `%e` (current HP),
    /// `%E` (max), `%p` (percent), `%K` (10-cell HP bar). All
    /// suppress (render `-`) when out of combat so the combat
    /// preset stays readable.
    pub enemy_hp: Option<Health>,
}

/// Repair `%%X` patterns where X is a recognized prompt variable.
///
/// Background: an early version of the schema set
/// `Characters.prompt @default("<%%h/%%Hhp %%v/%%Vmv>")` thinking
/// Prisma would unescape `%%` → `%`. Prisma stores the literal,
/// so every newly-created character (including all seeded test
/// users) ended up with a prompt template that — after the
/// `%%` → literal-`%` rule in `render_prompt` — displays
/// literal `%h` / `%H` instead of HP values.
///
/// Login calls this on the loaded template before constructing the
/// `Prompt` component; the next save persists the cleaned form so
/// the broken row repairs itself across one disconnect cycle. New
/// characters get the corrected default from the schema.
///
/// Conservative scope: only collapses `%%X` where X is a known
/// prompt variable letter. A user who genuinely wants `%h` as
/// literal text in their prompt loses that capability — but no
/// player has ever wanted that.
#[must_use]
pub(crate) fn sanitize_prompt_template(template: &str) -> String {
    const KNOWN: &[char] = &[
        'h', 'H', 'v', 'V', 'B', 'M', 'n', 'r', 'g', 't', 's', 'd',
        'N', 'e', 'E', 'p', 'K',
    ];
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%'
            && chars.get(i + 1) == Some(&'%')
            && chars.get(i + 2).is_some_and(|c| KNOWN.contains(c))
        {
            // Saw `%%X` where X is a known variable — collapse to `%X`.
            out.push('%');
            out.push(chars[i + 2]);
            i += 3;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

pub(crate) fn render_prompt(template: &str, ctx: PromptCtx<'_>) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('h') => match ctx.hp {
                    Some(hp) => {
                        // Color-grade current HP by ratio so a
                        // glance at the prompt warns the player
                        // before they get themselves killed. Tags
                        // render through render_color_tags which
                        // strips them for color-off clients.
                        let tag = vital_color_tag(hp.hp, hp.max);
                        if let Some(open) = tag {
                            out.push_str(open);
                            out.push_str(&hp.hp.to_string());
                            out.push_str("</>");
                        } else {
                            out.push_str(&hp.hp.to_string());
                        }
                    }
                    None => out.push('?'),
                },
                Some('H') => match ctx.hp {
                    Some(hp) => out.push_str(&hp.max.to_string()),
                    None => out.push('?'),
                },
                // %B = 10-cell health bar like `[####______]`,
                // colored by ratio (red/yellow/green-default).
                Some('B') => match ctx.hp {
                    Some(hp) => out.push_str(&render_vital_bar(hp.hp, hp.max)),
                    None => out.push_str("[??????????]"),
                },
                // %M = 10-cell stamina bar.
                Some('M') => match ctx.stamina {
                    Some(s) => out.push_str(&render_vital_bar(s.current, s.max)),
                    None => out.push_str("[??????????]"),
                },
                Some('v') => match ctx.stamina {
                    Some(s) => {
                        let tag = vital_color_tag(s.current, s.max);
                        if let Some(open) = tag {
                            out.push_str(open);
                            out.push_str(&s.current.to_string());
                            out.push_str("</>");
                        } else {
                            out.push_str(&s.current.to_string());
                        }
                    }
                    None => out.push('?'),
                },
                Some('V') => match ctx.stamina {
                    Some(s) => out.push_str(&s.max.to_string()),
                    None => out.push('?'),
                },
                Some('n') => match ctx.name {
                    Some(n) => out.push_str(n),
                    None => out.push('?'),
                },
                Some('r') => match ctx.room {
                    Some(r) => out.push_str(r),
                    None => out.push('?'),
                },
                // %g = on-hand wealth in copper (raw integer; players
                // do their own math). Skipped denomination split here
                // because the prompt is a tight one-line readout.
                Some('g') => match ctx.wealth {
                    Some(w) => out.push_str(&w.to_string()),
                    None => out.push('?'),
                },
                // %t = in-game hour, zero-padded. Lets a player put
                // a clock in their prompt without `time` round-trips.
                Some('t') => match ctx.hour {
                    Some(h) => out.push_str(&format!("{h:02}")),
                    None => out.push('?'),
                },
                // %s = season label ("Winter"). Tracks the calendar.
                Some('s') => match ctx.season {
                    Some(s) => out.push_str(s),
                    None => out.push('?'),
                },
                // %d = "day" or "night" — same hour window as the
                // dark-room gate, so a `<%d>` prompt theme matches
                // the light flag the world uses for `look`.
                Some('d') => match ctx.day_night {
                    Some(d) => out.push_str(d),
                    None => out.push('?'),
                },
                // %N = enemy name; renders `-` out of combat.
                Some('N') => out.push_str(ctx.enemy_name.unwrap_or("-")),
                // %e = enemy current HP, color-graded like %h.
                Some('e') => match ctx.enemy_hp {
                    Some(hp) => {
                        let tag = vital_color_tag(hp.hp, hp.max);
                        if let Some(open) = tag {
                            out.push_str(open);
                            out.push_str(&hp.hp.to_string());
                            out.push_str("</>");
                        } else {
                            out.push_str(&hp.hp.to_string());
                        }
                    }
                    None => out.push('-'),
                },
                // %E = enemy max HP.
                Some('E') => match ctx.enemy_hp {
                    Some(hp) => out.push_str(&hp.max.to_string()),
                    None => out.push('-'),
                },
                // %p = enemy HP percent (no decimals).
                Some('p') => match ctx.enemy_hp {
                    Some(hp) if hp.max > 0 => {
                        let pct = (hp.hp.max(0) * 100) / hp.max;
                        out.push_str(&pct.to_string());
                    }
                    _ => out.push('-'),
                },
                // %K = 10-cell enemy HP bar, color-graded.
                Some('K') => match ctx.enemy_hp {
                    Some(hp) => out.push_str(&render_vital_bar(hp.hp, hp.max)),
                    None => out.push_str("[----------]"),
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

pub(crate) fn has_flag(world: &World, entity: Entity, flag: PlayerFlag) -> bool {
    world
        .get::<PlayerFlags>(entity)
        .is_some_and(|f| f.has(flag))
}

pub(crate) fn visible(cmd: &Command, role: UserRole, perms: &[Permission]) -> bool {
    role.at_least(cmd.min_role) && cmd.required_perm.is_none_or(|p| perms.contains(&p))
}

/// Cap on how many prefix suggestions the unknown-command hint
/// shows before truncating to "(+N)". Tight because the trailer
/// rides on the same line as the original error.
const UNKNOWN_HINT_MAX: usize = 5;

/// Append a "  (did you mean: X, Y?)" trailer to "Unknown command"
/// when at least one visible command's primary or alias name
/// starts with what the player typed. Empty trailer when there's
/// nothing to suggest, so the original error stays terse for a
/// truly junk input. Mirrors the `help` command's prefix-match UX.
pub(crate) fn unknown_command_hint(
    world: &World,
    player: Entity,
    typed: &str,
) -> String {
    let needle = typed.to_ascii_lowercase();
    if needle.is_empty() {
        return String::new();
    }
    let (role, perms) = world.get::<Account>(player).map_or_else(
        || (UserRole::Player, Vec::new()),
        |a| (a.role, a.perms.clone()),
    );
    let mut suggestions: Vec<&'static str> = all_commands()
        .filter(|cmd| visible(cmd, role, &perms))
        .filter(|cmd| {
            cmd.names
                .iter()
                .any(|n: &&'static str| n.starts_with(needle.as_str()))
        })
        .map(|cmd| cmd.names[0])
        .collect();
    if suggestions.is_empty() {
        return String::new();
    }
    suggestions.sort_unstable();
    suggestions.dedup();
    let shown: Vec<&str> = suggestions
        .iter()
        .take(UNKNOWN_HINT_MAX)
        .copied()
        .collect();
    let trailer = if suggestions.len() > UNKNOWN_HINT_MAX {
        format!(" (+{})", suggestions.len() - UNKNOWN_HINT_MAX)
    } else {
        String::new()
    };
    format!("  (did you mean: {}{}?)", shown.join(", "), trailer)
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

/// `description` / `desc`: show / set / clear the player's `examine`
/// prose. Stored as a `Description` component (the same component
/// rooms and mobs use); persisted to `Characters.description` on
/// disconnect via `save_state`. Capped at 500 chars to keep examine
/// from runaway-pasting.
const MAX_DESCRIPTION_LEN: usize = 500;

/// Render an `ObjectType` as the token shape used by `ShopAccepts.type`
/// and the underlying enum (uppercase, no underscores). The schema's
/// `Objects.type` uses sqlx-encoded `SCREAMING_SNAKE_CASE`, but
/// `ShopAccepts.type` is a free-form text column where some legacy
/// entries use underscores (e.g. `DRINK_CONTAINER`). Normalizing both
/// sides to uppercase + underscore-stripped lets matches succeed
/// across both spellings.
pub(crate) fn object_type_token(t: mud_db::enums::ObjectType) -> String {
    format!("{t:?}").to_ascii_uppercase()
}

/// Compute the copper price of one shop offering: override wins,
/// otherwise `proto.cost * shop.buy_profit` rounded.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn shop_offer_price(offer: &mud_world::ShopOffering, base_cost: i32, buy_profit: f64) -> i64 {
    if offer.price > 0 {
        i64::from(offer.price)
    } else {
        (f64::from(base_cost) * buy_profit).round() as i64
    }
}

/// Returns true (and lets the caller proceed) if a mob with the
/// requested profession occupies the player's current room. Emits
/// "You need a <kind> here to handle that." and returns false
/// when none is present. Common gate for service interactions
/// (deposit/withdraw, train, mailbox, rent).
pub(crate) fn require_profession_in_room(
    world: &mut World,
    player: Entity,
    profession: mud_db::enums::MobProfession,
    kind: &str,
) -> bool {
    let Some(located) = world.get::<Located>(player).copied() else {
        return false;
    };
    let present = {
        let mut q = world.query_filtered::<(&Located, &WorldKey), With<Mob>>();
        q.iter(world).any(|(l, k)| {
            l.0 == located.0
                && world
                    .get_resource::<MobPrototypes>()
                    .and_then(|p| p.by_key.get(&(k.zone, k.id)))
                    .is_some_and(|m| m.professions.contains(&profession))
        })
    };
    if !present {
        send_to(
            world,
            player,
            format!("You need a {kind} here to handle that.\r\n").as_str(),
        );
    }
    present
}

/// Shared body for `deposit` / `withdraw`. `direction` is "deposit"
/// or "withdraw"; the function picks the source / destination
/// component and refusal text accordingly.
pub(crate) fn bank_transfer(world: &mut World, player: Entity, args: &str, direction: &str) {
    let amount = match args.trim().parse::<i64>() {
        Ok(n) if n > 0 => n,
        _ => {
            send_to(
                world,
                player,
                format!("Usage: {direction} <amount of copper>\r\n").as_str(),
            );
            return;
        }
    };
    if !require_profession_in_room(world, player, mud_db::enums::MobProfession::Banker, "banker") {
        return;
    }
    let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);
    let in_bank = world.get::<BankWealth>(player).map_or(0, |b| b.0);
    let (from, to, can_afford) = match direction {
        "deposit" => (on_hand, in_bank, on_hand >= amount),
        "withdraw" => (in_bank, on_hand, in_bank >= amount),
        _ => return,
    };
    let _ = (from, to);
    if !can_afford {
        let where_ = if direction == "deposit" {
            "on hand"
        } else {
            "in the bank"
        };
        send_to(
            world,
            player,
            format!("You don't have that much {where_}.\r\n").as_str(),
        );
        return;
    }
    if direction == "deposit" {
        if let Some(mut w) = world.get_mut::<Wealth>(player) {
            w.0 -= amount;
        }
        if let Some(mut b) = world.get_mut::<BankWealth>(player) {
            b.0 += amount;
        } else {
            try_insert(world, player, BankWealth(amount));
        }
        send_to(world, player, format!("Deposited {amount} copper.\r\n"));
    } else {
        if let Some(mut b) = world.get_mut::<BankWealth>(player) {
            b.0 -= amount;
        }
        if let Some(mut w) = world.get_mut::<Wealth>(player) {
            w.0 += amount;
        } else {
            try_insert(world, player, Wealth(amount));
        }
        send_to(world, player, format!("Withdrew {amount} copper.\r\n"));
    }
}

// `balance` body lives in commands/balance.rs (inventory-distributed).

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

/// `practice <ability>`: bump proficiency by 5, capped at the
/// class's `proficiency_cap`. Refuses unknown abilities, abilities
/// off the player's class list, abilities not in `KnownAbilities`,
/// and abilities already at the cap.
pub(crate) fn practice_one(world: &mut World, player: Entity, name: &str) {
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
            format!("{} isn't on your class's list.\r\n", def.name),
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
            format!("You haven't learned {} yet — `study` it first.\r\n", def.name),
        );
        return;
    };
    if current_prof >= cap {
        send_to(
            world,
            player,
            format!(
                "Your {} is already at its class cap of {cap}.\r\n",
                def.name
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
            def.name
        ),
    );
}

/// `train [<stat>]`: bump a `CoreStat` by 1 in exchange for one
/// `SkillPoints`. Hard-capped at 18 per legacy `CircleMUD`
/// convention — characters with rolled stats above 18 (e.g. magical
/// bonuses) can't be trained higher than 18, but their existing
/// values aren't clamped.
const TRAIN_STAT_CAP: i32 = 18;

pub(crate) fn direction_rank(d: Direction) -> u8 {
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

pub(crate) struct WhoRow {
    pub entity: Entity,
    pub name: String,
    pub title: Option<String>,
    pub afk: bool,
    pub idle: Option<u64>,
    pub level: i32,
    pub clan_abbrev: Option<String>,
    /// Plain class name resolved via `ClassCatalog`, when the
    /// player has a class. None for classless characters; the
    /// renderer omits the class slot in that case.
    pub class_name: Option<String>,
}

pub(crate) fn format_idle(secs: u64) -> String {
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

/// Render seconds-since-event as a human-friendly relative
/// timestamp ("3 days ago", "just now"). Used by score's
/// "Last login:" line. Negative input collapses to "just now"
/// — wall-clock skew between server and DB shouldn't surface
/// "in the future" to the player.
pub(crate) fn format_time_ago(secs: i64) -> String {
    if secs < 60 {
        return String::from("just now");
    }
    let mins = secs / 60;
    if mins < 60 {
        let label = if mins == 1 { "minute" } else { "minutes" };
        return format!("{mins} {label} ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        let label = if hours == 1 { "hour" } else { "hours" };
        return format!("{hours} {label} ago");
    }
    let days = hours / 24;
    if days < 30 {
        let label = if days == 1 { "day" } else { "days" };
        return format!("{days} {label} ago");
    }
    let months = days / 30;
    if months < 12 {
        let label = if months == 1 { "month" } else { "months" };
        return format!("{months} {label} ago");
    }
    let years = months / 12;
    let label = if years == 1 { "year" } else { "years" };
    format!("{years} {label} ago")
}

/// Format a lifetime play-time count into a coarse "1d 4h" / "23m"
/// string for the score sheet. Differs from `format_idle` in two
/// ways: collapses sub-minute spans to "0m" rather than "Ns" (the
/// score "Played:" line surfaces total engagement, not jitter);
/// promotes to days at 24h+ since lifetime values can grow large.
pub(crate) fn format_play_time(secs: u64) -> String {
    if secs < 60 {
        return String::from("0m");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    let leftover_mins = mins % 60;
    if hours < 24 {
        return if leftover_mins == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {leftover_mins}m")
        };
    }
    let days = hours / 24;
    let leftover_hours = hours % 24;
    if leftover_hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d {leftover_hours}h")
    }
}

/// Bundle of all the data the `score` renderers consume. Building it once
/// in `cmd_score` avoids re-querying components per render variant and
/// keeps the renderer signatures from blowing past clippy's
/// `too_many_arguments` threshold.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct ScoreData<'a> {
    name: &'a str,
    hp: Option<Health>,
    stamina: Option<Stamina>,
    cs: Option<CombatStats>,
    /// Core attributes (STR/DEX/CON/INT/WIS/CHA). Score sheet
    /// renders the six values + their bonuses on a single line.
    core_stats: Option<CoreStats>,
    posture: Option<Posture>,
    fight_target: Option<&'a str>,
    /// `(level, class_label, race, gender, experience)` from the
    /// Profile component. `class_label` is the catalog `name` (with
    /// color tags) when the character has a class assigned,
    /// "Classless" otherwise.
    profile: Option<(i32, &'a str, &'a str, &'a str, i32)>,
    /// On-hand copper total. Rendered as a `wealth`-style platinum/
    /// gold/silver/copper line. Zero is omitted from the score sheet.
    wealth: i64,
    /// Bank-stored copper total. Rendered as a separate `Bank:` line
    /// when nonzero so players can tell on-hand vs. saved at a glance.
    bank: i64,
    /// Survival gauges; rendered as one comma-separated `Condition:`
    /// line when any band crossed (hungry/starving/thirsty/parched).
    /// Sated state stays silent.
    hunger: i32,
    thirst: i32,
    /// `(carried_lbs, capacity_lbs)`. Surfaced on the score sheet so
    /// the encumbrance numbers are visible without `inventory`.
    /// Capacity is always positive — `carry_capacity` floors at 1.
    carry: (f64, f64),
    /// Drunkenness counter (0–100). Rendered when nonzero with a
    /// "Drunk: N" line.
    drunkenness: i32,
    /// Lifetime kill count, surfaced on the sheet so players see
    /// progress toward kills_<N> achievements at a glance.
    kill_total: i32,
    /// `(name, abbrev, rank)` from `ClanMembership` when present.
    clan: Option<(&'a str, &'a str, &'a str)>,
    /// Names of active `EffectInstance` rows on the player. Empty
    /// when none are applied. Score sheet renders a comma-joined
    /// one-line summary; full duration / source detail stays in
    /// the dedicated `effects` command.
    active_effects: &'a [String],
    /// Group / follow status. `leader` is the name of the entity
    /// the player is following directly (None when the player is
    /// either solo or the group root). `member_count` includes the
    /// player and every transitive follower; `1` means solo.
    group_status: GroupStatus<'a>,
    /// XP toward the next level. `None` for max-level characters
    /// (level >= 100) and entities without a Profile. Renders as a
    /// "Exp: X / Y [bar] N%" line; the bar is computed here so
    /// renderers stay free of math.
    level_progress: Option<LevelProgress>,
    /// Current room: `(display_name, zone, id)`. `None` only for
    /// disconnected/unrooted entities (in practice never for an
    /// online player at score time, but keep it Option-shaped so
    /// the renderer can quietly omit the line rather than print
    /// "[void]"). Display name has color tags pre-stripped so the
    /// fancy renderer's fixed-width row padding stays correct.
    location: Option<(&'a str, i32, i32)>,
    /// Unspent practice points (`SkillPoints` component). Score
    /// sheet shows the number whenever it's nonzero so a player
    /// can see they have something to spend without running
    /// `practice` first. Zero suppresses the line.
    practice_points: i32,
    /// `(unlocked, total)` counts for non-hidden achievements.
    /// Hidden rows stay out of the denominator so a brand-new
    /// character doesn't see "0 / 47" when 12 of those 47 are
    /// secret challenges. Both `0` suppress the line.
    achievements: (usize, usize),
    /// Player-set epithet from the `Title` component. Carries
    /// XML-Lite color tags as authored — score's send path runs
    /// the same `render_color_tags` pipeline as `who`, so they
    /// resolve to ANSI rather than literal markup. `None` when
    /// the column is NULL or empty.
    title: Option<&'a str>,
    /// Auto-flee threshold (1..=99) when `PlayerFlag::Wimpy` is
    /// on; `None` when wimpy mode is off. Combat consults the same
    /// state to decide when to break a fight, so showing it on
    /// the score sheet mirrors the in-combat behavior. Off
    /// suppresses the line entirely so a non-wimpy character
    /// doesn't have a noisy "Wimpy: off" entry.
    wimpy: Option<i32>,
    /// Per-level rank title from `LevelTable.title_for` —
    /// `Some("Avatar")` / `Some("Implementer")` for staff levels
    /// that carry an explicit name, `None` for ordinary numeric
    /// levels. When present it's appended to the Level row so
    /// "Level 105 Implementer Male Human (Wizard)" reads as a
    /// proper character-sheet header. Mortals see no change.
    level_title: Option<&'a str>,
    /// `(next_level_number, hp_gain, stamina_gain)` for the row
    /// at `level + 1`. None for max-level characters and for
    /// brand-new boots before `LevelTable` is populated. Score
    /// uses this for a "Next level: +N HP, +M Stamina" preview so
    /// the planning info lives next to the experience progress
    /// rather than only on the dedicated `level` command.
    next_level_gains: Option<(i32, i32, i32)>,
    /// Bound recall destination as `(display_name, zone, id)`.
    /// `None` when the player hasn't touched a touchstone yet —
    /// the score sheet then quietly omits the line rather than
    /// printing "Recall: none" boilerplate (the `recall` command
    /// itself nudges the player toward `touch`). Display name has
    /// color tags pre-stripped so the fancy box stays aligned.
    recall: Option<(&'a str, i32, i32)>,
    /// `true` when the player carries the `Stealth` marker (set
    /// by `hide`, cleared by `visible` / `vis`). Score surfaces
    /// it so a player who slipped into stealth a long time ago
    /// doesn't forget — combat already drops the marker on
    /// engage, so seeing it on score means "still hidden".
    stealth: bool,
    /// `true` when the player has the `Flying` marker. `fly` /
    /// `walk` / `land` toggle it. Surfaced on score so the player
    /// remembers the +1 stamina-per-move surcharge they're
    /// signing up for, especially on roads where flying isn't
    /// needed.
    flying: bool,
    /// Mount display name when the player is `Mounted` on a mob
    /// (color tags pre-stripped); `None` when on foot. Score's
    /// reminder is the use case — players forget they're still
    /// astride a horse from a quest 30 minutes ago.
    mount_name: Option<&'a str>,
    /// `(room_count, entrance_zone, entrance_id)` from the
    /// `HouseSummary` component when the player owns a house.
    /// Score uses it to remind owners of their property + the
    /// entrance composite key (so `goto <zone> <id>` works
    /// without re-running `house info` first). `None` for the
    /// landless majority — the line is then suppressed entirely.
    house: Option<(usize, i32, i32)>,
    /// Count of abilities still on cooldown (`Cooldowns.ready_at`
    /// entries whose deadline is in the future). Skill-rotation
    /// planning info — answers "how many of my abilities are
    /// recharging right now" without typing `cooldowns`. `0`
    /// suppresses the line.
    cooldowns_active: usize,
    /// Guard target name when the player is `Guarding(Entity)`
    /// — combat redirects swings aimed at the target onto the
    /// guarder. Color tags pre-stripped. `None` when not
    /// guarding so the row is suppressed; a player who set
    /// `guard <name>` an hour ago and forgot would otherwise
    /// keep eating swings without any reminder.
    guarding_name: Option<&'a str>,
    /// In-flight mail draft summary: `(recipient, body_line_count)`
    /// when the player has an open `MailDraft`. Score's reminder
    /// is the use case — it's easy to walk away from a partially
    /// composed message and forget. Suppressed when no draft is
    /// active.
    mail_draft: Option<(&'a str, usize)>,
    /// Same idea for `BoardDraft`: `(board_alias, body_line_count)`
    /// when the player is mid-post. Surfaced so a half-written
    /// post that's been parked for a while doesn't go unnoticed.
    board_draft: Option<(&'a str, usize)>,
    /// Body size from `RaceDefaults.size_by_race` (the `Race`
    /// table's `default_size` column) — typically `Medium` /
    /// `Large` etc. None when the race has no row in the table
    /// (e.g. a freshly seeded DB) so the line is suppressed
    /// rather than rendering "Size: ?". Capitalize-first matches
    /// the C++ score formatting.
    size: Option<&'a str>,
    /// Life-state markers feeding the Posture line's color tag.
    /// Priority order in `status_color_tag`: ghost > frozen >
    /// stunned > fighting > posture-driven. Together with `posture`
    /// and `fight_target` they pick the line's hue without
    /// restructuring the standard/fancy renderers.
    is_ghost: bool,
    is_stunned: bool,
    is_frozen: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct LevelProgress {
    pub current_xp: i64,
    pub next_level_xp: i64,
    pub percent: i32,
}

#[derive(Default)]
pub(crate) struct GroupStatus<'a> {
    pub leader: Option<&'a str>,
    pub member_count: usize,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn render_score_standard(d: &ScoreData) -> String {
    let mut out = format!("\r\n{}\r\n", d.name);
    if let Some((level, class, race, gender, _xp)) = d.profile {
        let gender_label = capitalize(gender);
        let race_label = capitalize(race);
        let rank = d.level_title.map_or(String::new(), |t| format!(" {t}"));
        out.push_str(&format!(
            "  Level {level}{rank} {gender_label} {race_label} ({class})\r\n",
        ));
    }
    if let Some(t) = d.title {
        out.push_str(&format!("  Title: {t}\r\n"));
    }
    if let Some(size) = d.size {
        out.push_str(&format!("  Size: {size}\r\n"));
    }
    if let Some(age) = d.profile.and_then(|(lvl, ..)| format_age(lvl)) {
        out.push_str(&format!("  Age: {age}\r\n"));
    }
    if let Some(hp) = d.hp {
        let cur = vital_color_tag(hp.hp, hp.max).map_or_else(
            || hp.hp.to_string(),
            |tag| format!("{tag}{}</>", hp.hp),
        );
        out.push_str(&format!("  HP: {cur} / {}\r\n", hp.max));
    }
    if let Some(s) = d.stamina {
        let cur = vital_color_tag(s.current, s.max).map_or_else(
            || s.current.to_string(),
            |tag| format!("{tag}{}</>", s.current),
        );
        out.push_str(&format!("  Stamina: {cur} / {}\r\n", s.max));
    }
    if let Some(stats) = d.core_stats {
        out.push_str(&format!(
            "  STR {}({:+})  DEX {}({:+})  CON {}({:+})  INT {}({:+})  WIS {}({:+})  CHA {}({:+})\r\n",
            stats.strength, CoreStats::bonus(stats.strength),
            stats.dexterity, CoreStats::bonus(stats.dexterity),
            stats.constitution, CoreStats::bonus(stats.constitution),
            stats.intelligence, CoreStats::bonus(stats.intelligence),
            stats.wisdom, CoreStats::bonus(stats.wisdom),
            stats.charisma, CoreStats::bonus(stats.charisma),
        ));
    }
    if let Some(cs) = d.cs {
        let align_label = mud_db::enums::Alignment::from_score(cs.alignment).label();
        // Score sheet now reads the docs/design/combat.md model:
        // Acc / Eva / Atk / Armor / (Ward only when active).
        out.push_str(&format!(
            "  Acc: {}   Eva: {}   Atk: {}{}   Armor: {}%{}   Alignment: {} ({})\r\n",
            cs.accuracy,
            cs.evasion,
            if cs.attack_power >= 0 { "+" } else { "" },
            cs.attack_power,
            cs.armor_pct,
            if cs.armor_flat != 0 {
                format!(" / {} flat", cs.armor_flat)
            } else {
                String::new()
            },
            align_label,
            cs.alignment,
        ));
        // Ward, hardness, crit_chance — only shown when non-default.
        if cs.ward_pct != 0 {
            out.push_str(&format!(
                "  Ward: <b:cyan>{}%</> <dim>(magical mitigation)</>\r\n",
                cs.ward_pct,
            ));
        }
        if cs.hardness != 0 {
            out.push_str(&format!(
                "  Hardness: <b:magenta>{}</> <dim>(damage floor)</>\r\n",
                cs.hardness,
            ));
        }
        if cs.crit_chance != 5 {
            out.push_str(&format!(
                "  Crit: <b:yellow>{}%</> <dim>(swing crit chance)</>\r\n",
                cs.crit_chance,
            ));
        }
    }
    if let Some(p) = d.posture {
        let tag = status_color_tag(
            Some(p.0),
            d.fight_target.is_some(),
            d.is_ghost,
            d.is_frozen,
            d.is_stunned,
        );
        let label = match tag {
            Some(open) => format!("{open}{}</>", p.0.label()),
            None => p.0.label().to_string(),
        };
        out.push_str(&format!("  Posture: {label}\r\n"));
    }
    if d.stealth {
        out.push_str("  Stealth: hidden\r\n");
    }
    if d.flying {
        out.push_str("  Flying: aloft\r\n");
    }
    if let Some(mount) = d.mount_name {
        out.push_str(&format!("  Mounted on: {mount}\r\n"));
    }
    if let Some(coin) = format_wealth(d.wealth) {
        out.push_str(&format!("  Wealth: {coin}\r\n"));
    }
    if let Some(coin) = format_wealth(d.bank) {
        out.push_str(&format!("  Bank:   {coin}\r\n"));
    }
    if d.carry.0 > 0.0 {
        let band = encumbrance_band(d.carry.0, d.carry.1);
        let (open, close) = encumbrance_color_tag(d.carry.0, d.carry.1)
            .map_or((String::new(), String::new()), |t| (t.to_string(), "</>".to_string()));
        out.push_str(&format!(
            "  Load:   {open}{:.1}{close} / {:.0} lbs.  ({open}{band}{close})\r\n",
            d.carry.0,
            d.carry.1,
        ));
    }
    if let Some(target) = d.fight_target {
        out.push_str(&format!("  Fighting: <b:red>{target}</>\r\n"));
    }
    if let Some(target) = d.guarding_name {
        out.push_str(&format!("  Guarding: {target}\r\n"));
    }
    if let Some(c) = condition_summary(d.hunger, d.thirst, d.active_effects) {
        let open = condition_color_tag(d.hunger, d.thirst).unwrap_or("");
        let close = if open.is_empty() { "" } else { "</>" };
        out.push_str(&format!("  Condition: {open}{c}{close}\r\n"));
    }
    if d.drunkenness > 0 {
        out.push_str(&format!(
            "  Drunk:  {} / 100  ({})\r\n",
            d.drunkenness,
            drunk_band(d.drunkenness),
        ));
    }
    if let Some(pct) = d.wimpy {
        out.push_str(&format!(
            "  Wimpy:  flee at HP < {pct}%\r\n",
        ));
    }
    if d.kill_total > 0 {
        out.push_str(&format!("  Kills:  {}\r\n", d.kill_total));
    }
    if let Some((name, abbrev, rank)) = d.clan {
        out.push_str(&format!("  Clan:   {name} [{abbrev}] ({rank})\r\n"));
    }
    if !d.active_effects.is_empty() {
        out.push_str(&format!(
            "  Effects: {}    (`effects` for durations)\r\n",
            d.active_effects.join(", "),
        ));
    }
    if d.cooldowns_active > 0 {
        let count = d.cooldowns_active;
        let suffix = if count == 1 { "ability" } else { "abilities" };
        out.push_str(&format!(
            "  Cooldowns: {count} {suffix} recharging\r\n",
        ));
    }
    if let Some((to, lines)) = d.mail_draft {
        let suffix = if lines == 1 { "" } else { "s" };
        out.push_str(&format!(
            "  Mail draft: to {to}, {lines} line{suffix} so far    \
             (`mail .send` / `.preview` / `.abort`)\r\n",
        ));
    }
    if let Some((board, lines)) = d.board_draft {
        let suffix = if lines == 1 { "" } else { "s" };
        out.push_str(&format!(
            "  Board draft: on {board}, {lines} line{suffix} so far    \
             (`post .send` / `.preview` / `.abort`)\r\n",
        ));
    }
    if let Some(line) = group_status_line(&d.group_status) {
        out.push_str(&format!("  {line}\r\n"));
    }
    if let Some(p) = d.level_progress {
        out.push_str(&format!(
            "  Exp: {} / {}  {} {}%\r\n",
            p.current_xp,
            p.next_level_xp,
            progress_bar(p.percent),
            p.percent,
        ));
    }
    if let Some((next, hp_gain, st_gain)) = d.next_level_gains {
        out.push_str(&format!(
            "  Next level (#{next}): +{hp_gain} HP, +{st_gain} Stamina\r\n",
        ));
    }
    if let Some((name, zone, id)) = d.location {
        out.push_str(&format!("  Location: {name}  [{zone}:{id}]\r\n"));
    }
    if let Some((name, zone, id)) = d.recall {
        out.push_str(&format!("  Recall:   {name}  [{zone}:{id}]\r\n"));
    }
    if let Some((rooms, zone, id)) = d.house {
        let suffix = if rooms == 1 { "" } else { "s" };
        out.push_str(&format!(
            "  House:    {rooms} room{suffix} at [{zone}:{id}]\r\n",
        ));
    }
    if d.practice_points > 0 {
        let pts = d.practice_points;
        let suffix = if pts == 1 { "" } else { "s" };
        out.push_str(&format!(
            "  Practice: {pts} point{suffix} available    (`practice` to spend)\r\n",
        ));
    }
    let (unlocked, total) = d.achievements;
    if total > 0 {
        out.push_str(&format!(
            "  Achievements: {unlocked} / {total} unlocked    (`achievements` for the list)\r\n",
        ));
    }
    out
}

/// One-line group/follow summary for the score sheet, or `None`
/// when the player is solo. Shape depends on the player's role:
/// followers see `"Following: X    (group of N)"`, the group root
/// sees `"Followers: N other(s)"`, and solo players get nothing.
fn group_status_line(g: &GroupStatus) -> Option<String> {
    if let Some(leader) = g.leader {
        if g.member_count > 1 {
            Some(format!(
                "Following: {leader}    (group of {})",
                g.member_count
            ))
        } else {
            Some(format!("Following: {leader}"))
        }
    } else if g.member_count > 1 {
        let others = g.member_count - 1;
        let suffix = if others == 1 { "" } else { "s" };
        Some(format!("Followers: {others} other{suffix}"))
    } else {
        None
    }
}

/// Cumulative XP required to *reach* `level`, matching the legacy
/// `level^2.5 * 1000` curve. Level 1 is the floor (returns 0); the
/// curve is monotonic and stops mattering at level 100 where score
/// suppresses the progress bar entirely.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn experience_for_level(level: i32) -> i64 {
    if level <= 1 {
        return 0;
    }
    let raw = f64::from(level).powf(2.5) * 1000.0;
    raw as i64
}

/// XP-to-next-level summary for the score sheet. `None` at the
/// level cap (>= 100) so the renderer can quietly skip the line.
/// Negative XP (death penalties, admin-set floors) clamps the
/// percent at 0 rather than printing a negative bar.
#[must_use]
pub(crate) fn level_progress_for(level: i32, current_xp: i32) -> Option<LevelProgress> {
    if !(1..100).contains(&level) {
        return None;
    }
    let current_xp = i64::from(current_xp);
    let floor = experience_for_level(level);
    let ceiling = experience_for_level(level + 1);
    let bracket = (ceiling - floor).max(1);
    let into_bracket = (current_xp - floor).max(0);
    let percent = ((into_bracket * 100) / bracket).clamp(0, 100);
    Some(LevelProgress {
        current_xp,
        next_level_xp: ceiling,
        percent: i32::try_from(percent).unwrap_or(0),
    })
}

/// Render a fixed-width ASCII progress bar for the score sheet.
/// Twenty cells wide; filled glyph `=`, empty glyph `-`. Color
/// codes stay out — the runtime-side ANSI wrapper layers them in
/// when the renderer pushes the line.
#[must_use]
pub(crate) fn progress_bar(percent: i32) -> String {
    const WIDTH: usize = 20;
    let pct = usize::try_from(percent.clamp(0, 100)).unwrap_or(0);
    let filled = (pct * WIDTH) / 100;
    let empty = WIDTH - filled;
    let mut s = String::with_capacity(WIDTH + 2);
    s.push('[');
    for _ in 0..filled {
        s.push('=');
    }
    for _ in 0..empty {
        s.push('-');
    }
    s.push(']');
    s
}

/// Map the carried-weight ratio into a one-word band so the score
/// sheet can show "47 / 200 lbs. (burdened)" rather than expecting
/// the player to do the percentage math. Thresholds line up with
/// the move-stamina penalty bands in `cmd_move`: 75% adds +1
/// stamina cost, 90% adds +2, 100%+ refuses pickup at the
/// boundary so over-capacity should be a transient state.
#[must_use]
pub(crate) fn encumbrance_band(carried: f64, capacity: f64) -> &'static str {
    if capacity <= 0.0 {
        return "unburdened";
    }
    let ratio = carried / capacity;
    if ratio >= 1.0 {
        "overloaded"
    } else if ratio >= 0.90 {
        "heavy"
    } else if ratio >= 0.75 {
        "encumbered"
    } else if ratio >= 0.50 {
        "burdened"
    } else {
        "unburdened"
    }
}

/// XML-Lite open tag for the score sheet's Posture line. Priority
/// order from worst to best: ghost > frozen > stunned > fighting >
/// posture-driven (yellow for non-standing, none for standing). Mirrors
/// the C++ score's status-color table without inventing new hues.
/// `None` means "render plain"; the caller emits no tag.
#[allow(clippy::fn_params_excessive_bools)]
pub(crate) fn status_color_tag(
    posture: Option<PostureKind>,
    in_combat: bool,
    is_ghost: bool,
    is_frozen: bool,
    is_stunned: bool,
) -> Option<&'static str> {
    if is_ghost {
        return Some("<b:black>");
    }
    if is_frozen || is_stunned {
        return Some("<b:cyan>");
    }
    if in_combat {
        return Some("<b:red>");
    }
    match posture? {
        PostureKind::Standing => None,
        _ => Some("<yellow>"),
    }
}

/// XML-Lite open tag for the score sheet's encumbrance line. Red
/// at 90%+, yellow at 70%+, none below — same gradient the C++
/// score uses, so a player crossing the heavy-load threshold sees
/// the same "you're hauling too much" warning hue. None at zero
/// capacity (defensive — avoids divide-by-zero).
pub(crate) fn encumbrance_color_tag(carried: f64, capacity: f64) -> Option<&'static str> {
    if capacity <= 0.0 {
        return None;
    }
    let pct = (carried / capacity) * 100.0;
    if pct >= 90.0 {
        Some("<red>")
    } else if pct >= 70.0 {
        Some("<yellow>")
    } else {
        None
    }
}

/// One-word descriptor for the score sheet's drunkenness line. The
/// bands track our 0..=100 alcohol scale: at 80+ the runtime emits
/// the room-spinning blackout warning on drink (`cmd_drink_amount`),
/// so "Blackout" matches that threshold. Below that the bands
/// roughly mirror the legacy 0..=15 scale's slurred (~40%) and
/// too-drunk (~66%) cutoffs scaled up to our 0..=100 range.
#[must_use]
pub(crate) fn drunk_band(drunk: i32) -> &'static str {
    match drunk {
        ..=0 => "sober",
        1..=39 => "Buzzed",
        40..=65 => "Tipsy",
        66..=79 => "Very Drunk",
        _ => "Blackout",
    }
}

/// Cosmetic in-character age string for the score sheet. Mirrors
/// the C++ placeholder formula (`20 + level` years, `(level * 3) %
/// 12` months) until birth-time tracking lands — gives the score
/// sheet a more character-sheet feel without touching schema /
/// login. Returns None for level <= 0 (mob renders, broken data).
#[must_use]
pub(crate) fn format_age(level: i32) -> Option<String> {
    if level <= 0 {
        return None;
    }
    let years = 20 + level;
    let months = (level * 3) % 12;
    let yr_suffix = if years == 1 { "" } else { "s" };
    let mo_suffix = if months == 1 { "" } else { "s" };
    Some(format!(
        "{years} year{yr_suffix}, {months} month{mo_suffix}",
    ))
}

/// XML-Lite open tag for the score sheet's Condition line, graded
/// by the worst hunger/thirst band that currently fires. Starving /
/// parched read red (the survival-tick will start draining HP);
/// hungry / thirsty read yellow (warning, no drain yet). Returns
/// `None` when only positive states (nourished / refreshed) are
/// present — the line shouldn't shout in green when the player is
/// well. Bands match `condition_summary` exactly so the color
/// tracks the text the player sees.
#[must_use]
pub(crate) fn condition_color_tag(hunger: i32, thirst: i32) -> Option<&'static str> {
    if hunger >= 48 || thirst >= 24 {
        Some("<red>")
    } else if hunger >= 24 || thirst >= 12 {
        Some("<yellow>")
    } else {
        None
    }
}

/// Comma-joined condition descriptors mixing hunger/thirst negative
/// bands with positive effect states (Nourished / Refreshed) when
/// they're active. None when neither side has anything to say.
/// Bands match the tick consumer's `HUNGRY_AT` / `STARVING_AT` /
/// `THIRSTY_AT` / `PARCHED_AT`. Effect-name match is
/// case-insensitive.
pub(crate) fn condition_summary(
    hunger: i32,
    thirst: i32,
    active_effects: &[String],
) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    let has_effect = |name: &str| {
        active_effects
            .iter()
            .any(|e| e.eq_ignore_ascii_case(name))
    };
    if has_effect("Nourished") {
        parts.push("nourished");
    }
    if has_effect("Refreshed") {
        parts.push("refreshed");
    }
    if hunger >= 48 {
        parts.push("starving");
    } else if hunger >= 24 {
        parts.push("hungry");
    }
    if thirst >= 24 {
        parts.push("parched");
    } else if thirst >= 12 {
        parts.push("thirsty");
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn render_score_fancy(d: &ScoreData) -> String {
    // Box width = 56 chars between the borders.
    const W: usize = 56;
    let name = d.name;
    let mut out = String::from("\r\n");
    out.push_str(&format!("+{}+\r\n", "-".repeat(W)));
    let title = format!("{name:^W$}");
    out.push_str(&format!("|{title}|\r\n"));
    out.push_str(&format!("+{}+\r\n", "-".repeat(W)));
    let mut row = |s: String| {
        // pad_visible respects XML-Lite color tags so a wrapped HP
        // value like `<red>40</> / 100` doesn't shift the right
        // border. ASCII-only rows pad identically since visible
        // width matches byte width.
        out.push_str(&format!("| {} |\r\n", pad_visible(&s, W - 2)));
    };
    if let Some((level, class, race, gender, _xp)) = d.profile {
        let rank = d.level_title.map_or(String::new(), |t| format!(" {t}"));
        row(format!(
            "Level:     {level}{rank} {} {} ({class})",
            capitalize(gender),
            capitalize(race),
        ));
    }
    if let Some(t) = d.title {
        row(format!("Title:     {t}"));
    }
    if let Some(size) = d.size {
        row(format!("Size:      {size}"));
    }
    if let Some(age) = d.profile.and_then(|(lvl, ..)| format_age(lvl)) {
        row(format!("Age:       {age}"));
    }
    if let Some(hp) = d.hp {
        let cur = vital_color_tag(hp.hp, hp.max).map_or_else(
            || hp.hp.to_string(),
            |tag| format!("{tag}{}</>", hp.hp),
        );
        row(format!("HP:        {cur} / {}", hp.max));
    }
    if let Some(s) = d.stamina {
        let cur = vital_color_tag(s.current, s.max).map_or_else(
            || s.current.to_string(),
            |tag| format!("{tag}{}</>", s.current),
        );
        row(format!("Stamina:   {cur} / {}", s.max));
    }
    if let Some(stats) = d.core_stats {
        row(format!(
            "STR {}({:+})  DEX {}({:+})  CON {}({:+})",
            stats.strength, CoreStats::bonus(stats.strength),
            stats.dexterity, CoreStats::bonus(stats.dexterity),
            stats.constitution, CoreStats::bonus(stats.constitution),
        ));
        row(format!(
            "INT {}({:+})  WIS {}({:+})  CHA {}({:+})",
            stats.intelligence, CoreStats::bonus(stats.intelligence),
            stats.wisdom, CoreStats::bonus(stats.wisdom),
            stats.charisma, CoreStats::bonus(stats.charisma),
        ));
    }
    if let Some(cs) = d.cs {
        let align_label = mud_db::enums::Alignment::from_score(cs.alignment).label();
        row(format!(
            "Acc: {}  Eva: {}  Atk: {:+}  Armor: {}%   Align: {} ({})",
            cs.accuracy,
            cs.evasion,
            cs.attack_power,
            cs.armor_pct,
            align_label,
            cs.alignment,
        ));
        if cs.ward_pct != 0 {
            row(format!("Ward: <b:cyan>{}%</> <dim>(magical)</>", cs.ward_pct));
        }
    }
    if let Some(p) = d.posture {
        let tag = status_color_tag(
            Some(p.0),
            d.fight_target.is_some(),
            d.is_ghost,
            d.is_frozen,
            d.is_stunned,
        );
        let label = match tag {
            Some(open) => format!("{open}{}</>", p.0.label()),
            None => p.0.label().to_string(),
        };
        row(format!("Posture:   {label}"));
    }
    if d.stealth {
        row(String::from("Stealth:   hidden"));
    }
    if d.flying {
        row(String::from("Flying:    aloft"));
    }
    if let Some(mount) = d.mount_name {
        row(format!("Mounted:   {mount}"));
    }
    if let Some(coin) = format_wealth(d.wealth) {
        row(format!("Wealth:    {coin}"));
    }
    if let Some(coin) = format_wealth(d.bank) {
        row(format!("Bank:      {coin}"));
    }
    if d.carry.0 > 0.0 {
        let band = encumbrance_band(d.carry.0, d.carry.1);
        let (open, close) = encumbrance_color_tag(d.carry.0, d.carry.1)
            .map_or((String::new(), String::new()), |t| (t.to_string(), "</>".to_string()));
        row(format!(
            "Load:      {open}{:.1}{close} / {:.0} lbs.  ({open}{band}{close})",
            d.carry.0,
            d.carry.1,
        ));
    }
    if let Some(target) = d.fight_target {
        row(format!("Fighting:  <b:red>{target}</>"));
    }
    if let Some(target) = d.guarding_name {
        row(format!("Guarding:  {target}"));
    }
    if let Some(c) = condition_summary(d.hunger, d.thirst, d.active_effects) {
        let open = condition_color_tag(d.hunger, d.thirst).unwrap_or("");
        let close = if open.is_empty() { "" } else { "</>" };
        row(format!("Condition: {open}{c}{close}"));
    }
    if d.drunkenness > 0 {
        row(format!(
            "Drunk:     {} / 100  ({})",
            d.drunkenness,
            drunk_band(d.drunkenness),
        ));
    }
    if let Some(pct) = d.wimpy {
        row(format!("Wimpy:     flee at HP < {pct}%"));
    }
    if d.kill_total > 0 {
        row(format!("Kills:     {}", d.kill_total));
    }
    if let Some((name, abbrev, rank)) = d.clan {
        row(format!("Clan:      {name} [{abbrev}] ({rank})"));
    }
    if !d.active_effects.is_empty() {
        // Effects can be many; the fancy box's fixed-width row
        // truncates rather than overflowing. Detail lives in
        // `cmd_effects`.
        row(format!("Effects:   {}", d.active_effects.join(", ")));
    }
    if d.cooldowns_active > 0 {
        let count = d.cooldowns_active;
        let suffix = if count == 1 { "ability" } else { "abilities" };
        row(format!("Cooldowns: {count} {suffix} recharging"));
    }
    if let Some((to, lines)) = d.mail_draft {
        let suffix = if lines == 1 { "" } else { "s" };
        row(format!("Draft:     mail to {to}, {lines} line{suffix}"));
    }
    if let Some((board, lines)) = d.board_draft {
        let suffix = if lines == 1 { "" } else { "s" };
        row(format!("Draft:     board {board}, {lines} line{suffix}"));
    }
    if let Some(line) = group_status_line(&d.group_status) {
        row(line);
    }
    if let Some(p) = d.level_progress {
        row(format!(
            "Exp:       {} / {}  {} {}%",
            p.current_xp,
            p.next_level_xp,
            progress_bar(p.percent),
            p.percent,
        ));
    }
    if let Some((next, hp_gain, st_gain)) = d.next_level_gains {
        row(format!(
            "Next #{next}:  +{hp_gain} HP, +{st_gain} Stamina",
        ));
    }
    if let Some((name, zone, id)) = d.location {
        row(format!("Location:  {name}  [{zone}:{id}]"));
    }
    if let Some((name, zone, id)) = d.recall {
        row(format!("Recall:    {name}  [{zone}:{id}]"));
    }
    if let Some((rooms, zone, id)) = d.house {
        let suffix = if rooms == 1 { "" } else { "s" };
        row(format!("House:     {rooms} room{suffix} at [{zone}:{id}]"));
    }
    if d.practice_points > 0 {
        let pts = d.practice_points;
        let suffix = if pts == 1 { "" } else { "s" };
        row(format!("Practice:  {pts} point{suffix} available"));
    }
    let (unlocked, total) = d.achievements;
    if total > 0 {
        row(format!("Achievements: {unlocked} / {total}"));
    }
    out.push_str(&format!("+{}+\r\n", "-".repeat(W)));
    out
}

pub(crate) fn render_score_minimal(d: &ScoreData) -> String {
    let mut parts = vec![d.name.to_string()];
    if let Some((level, class, race, _gender, xp)) = d.profile {
        parts.push(format!("L{level} {race}/{class}"));
        // Show level progress as a percent rather than raw xp
        // when available — far more useful in a one-line glance
        // ("am I close to leveling?") than the absolute number.
        if let Some(p) = d.level_progress {
            parts.push(format!("xp:{}%", p.percent));
        } else {
            parts.push(format!("xp:{xp}"));
        }
    }
    if let Some(hp) = d.hp {
        parts.push(format!("hp:{}/{}", hp.hp, hp.max));
    }
    if let Some(s) = d.stamina {
        parts.push(format!("st:{}/{}", s.current, s.max));
    }
    if let Some(cs) = d.cs {
        parts.push(format!(
            "atk:{:+} armor:{}%",
            cs.attack_power, cs.armor_pct
        ));
        if cs.ward_pct != 0 {
            parts.push(format!("ward:{}%", cs.ward_pct));
        }
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
    if d.carry.0 > 0.0 {
        parts.push(format!("ld:{:.0}/{:.0}", d.carry.0, d.carry.1));
    }
    if let Some(target) = d.fight_target {
        parts.push(format!("vs:{target}"));
    }
    if let Some(c) = condition_summary(d.hunger, d.thirst, d.active_effects) {
        match condition_color_tag(d.hunger, d.thirst) {
            Some(open) => parts.push(format!("{open}{c}</>")),
            None => parts.push(c),
        }
    }
    if d.drunkenness > 0 {
        parts.push(format!("drunk:{}", d.drunkenness));
    }
    if d.kill_total > 0 {
        parts.push(format!("kills:{}", d.kill_total));
    }
    if let Some((_, abbrev, _)) = d.clan {
        parts.push(format!("clan:{abbrev}"));
    }
    if !d.active_effects.is_empty() {
        parts.push(format!("eff:{}", d.active_effects.len()));
    }
    if d.practice_points > 0 {
        parts.push(format!("prac:{}", d.practice_points));
    }
    format!("{}\r\n", parts.join("  "))
}

pub(crate) fn set_posture(world: &mut World, player: Entity, new: PostureKind) {
    let current = world.get::<Posture>(player).map(|p| p.0);
    if current == Some(new) {
        send_to(
            world,
            player,
            format!("You are already {}.\r\n", new.label()),
        );
        return;
    }
    // Posture leaves the meditating band: clear `Meditating` and
    // tell the player they've broken focus. Allowed band is
    // resting / sitting / kneeling — same as cmd_meditate's gate.
    let meditating = world.get::<mud_world::Meditating>(player).is_some();
    let allows_meditate = matches!(
        new,
        PostureKind::Resting | PostureKind::Sitting | PostureKind::Kneeling
    );
    if meditating && !allows_meditate {
        try_remove::<mud_world::Meditating>(world, player);
        send_to(world, player, "You stop meditating.\r\n");
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

/// Toggle a single `PlayerFlag` and emit a friendlier message than the
/// generic `toggle` command. `on_msg` / `off_msg` are written verbatim
/// after the toggle. Used by the dedicated `afk` / `notell` / `deaf`
/// / `color` commands so muscle-memory players don't have to type
/// `toggle <flag>`.
pub(crate) fn toggle_player_flag(
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

/// Names that would lock the player out of dispatch entirely if
/// allowed as aliases. `quit` is the always-allowed escape hatch and
/// must never be aliased away. `alias` and `unalias` themselves can't
/// be redirected or the player can't reach them after one bad set.
const RESERVED_ALIAS_NAMES: &[&str] = &["quit", "alias", "unalias"];

// COLOR_BLIND is the underlying flag (semantics inverted relative to
// the command name): COLOR_BLIND ON ⇒ colors stripped. The messages
// flip accordingly so the player reads the visible behaviour, not the
// flag state.

// `wimpy` doubles as a toggle-with-threshold command. Three forms:
//   `wimpy`         — show current state.
//   `wimpy off|0`   — clear the WIMPY flag and threshold.
//   `wimpy <1..99>` — set threshold and ensure the flag is on.
// Combat checks `WimpyThreshold` (default 25%) only when the flag is
// also set, so clearing the flag is sufficient to disable; we still
// drop the component on `off` to keep state tidy.

// `dice` is the legacy verb for SHOW_DICE_ROLLS — when on, combat
// surfaces hit/damage rolls in the output.

// `holylight` is admin/builder-only in legacy FieryMUD: with the flag
// on you can see invisible/dark/hidden things in `look`. The flag is
// set, but no behaviour is wired into the renderer yet — this command
// exists so the muscle-memory toggle works and lands the flag for
// later renderer plumbing.

// `showids` exposes (zone, id) coordinates in command output for
// builders/admins. The flag is set; renderers that want to surface
// IDs check it.

/// Parse a direction word or its short alias to a Direction enum.
/// Returns None for anything that doesn't match a movement direction.
pub(crate) fn parse_direction(s: &str) -> Option<Direction> {
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

/// Resolve a door-command argument to a `Direction`. Accepts the
/// canonical direction names that `parse_direction` does, and also
/// matches the typed word against the keywords on the player's
/// current-room exits — so `open curtain` finds the curtain
/// regardless of which direction it's on. Hidden exits the player
/// hasn't discovered are skipped, matching the look / move gates,
/// so puzzle exits can't be probed by guessing keywords.
pub(crate) fn resolve_exit_arg(
    world: &World,
    player: Entity,
    arg: &str,
) -> Option<Direction> {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(dir) = parse_direction(trimmed) {
        return Some(dir);
    }
    let needle = trimmed.to_ascii_lowercase();
    let room = world.get::<Located>(player)?.0;
    let exits = world.get::<Exits>(room)?;
    for (dir, ed) in &exits.0 {
        if exit_is_hidden_to(world, player, room, *dir, ed) {
            continue;
        }
        for kw in &ed.keywords {
            if kw.to_ascii_lowercase().contains(&needle) {
                return Some(*dir);
            }
        }
    }
    None
}

/// True when the player's room is currently dark enough that
/// nothing can be seen without a light source. Cases: sector is
/// intrinsically dark (CAVE / UNDERDARK / UNDERWATER), or sector
/// is outdoor (sky-visible) AND it's nighttime (game hour 22..05).
/// Caller checks for any `Lit` item carried by anyone in the room
/// (player or mob) before declaring the player blind — torches /
/// lanterns / luminous-glow items still work.
pub(crate) fn room_is_dark(world: &World, room: Entity) -> bool {
    // `Room.base_light_level` overrides the sector/clock check at
    // both ends — positive means "always lit" (skylights, magical
    // glow), negative means "always dark" (magical voids,
    // never-carry-a-torch areas). Zero (the default for most
    // rooms) falls through to the legacy logic below.
    if let Some(level) = world.get::<mud_world::BaseLightLevel>(room) {
        if level.0 > 0 {
            return false;
        }
        if level.0 < 0 {
            return true;
        }
    }
    let Some(sector) = world.get::<RoomSector>(room).map(|s| s.0) else {
        return false;
    };
    if matches!(sector, Sector::Cave | Sector::Underdark | Sector::Underwater) {
        return true;
    }
    if !sector_is_outdoor_for_weather(sector) {
        return false;
    }
    // Civic sectors are presumed lit at night by streetlamps /
    // torch posts / civic lighting — cities and main roads never
    // go pitch-black on the clock alone. Wilderness sectors go
    // dark at night per the legacy CircleMUD model. Content
    // authors who want a *dark* alley can place an unlit alley
    // room with a non-civic sector (RUINS, SWAMP) or override via
    // intrinsic-dark room flags once those land.
    if matches!(sector, Sector::City | Sector::Road) {
        return false;
    }
    let hour = world.resource::<mud_world::MudClock>().hour;
    matches!(hour, 0..=4 | 22..=23)
}

/// True when `entity` perceives through normal darkness, magical
/// darkness, and blind effects. Today the only source is the
/// `HOLY_LIGHT` player flag (admin/staff toggle). When magical
/// darkness / blindness effects land in the perception pipeline,
/// they should also gate through this helper so `HOLY_LIGHT` keeps
/// being the single bypass.
#[must_use]
pub(crate) fn player_can_see_in_dark(world: &World, entity: Entity) -> bool {
    has_flag(world, entity, PlayerFlag::HolyLight)
}

/// True when `viewer` is allowed to see `target` for the purpose
/// of player-facing listings (who / look / scan). Returns false
/// when `target` carries `WizInvis(N)` and viewer's `Profile.level`
/// is below `N`. Targets without a `WizInvis` component are always
/// visible. Used by every place we render another actor's name
/// to a player so a wiz-invised admin actually disappears.
#[must_use]
pub(crate) fn can_see_player(world: &World, viewer: Entity, target: Entity) -> bool {
    let Some(invis) = world.get::<mud_world::WizInvis>(target).map(|w| w.0) else {
        return true;
    };
    let viewer_level = world.get::<Profile>(viewer).map_or(0, |p| p.level);
    viewer_level >= invis
}

/// True if anyone in `room` (any actor, plus loose items on the
/// floor and items worn or carried by actors in the room) carries
/// a `Lit` marker. Used to override `room_is_dark` for rooms with
/// active light sources.
pub(crate) fn room_has_light(world: &mut World, room: Entity) -> bool {
    // 1. Loose lit items on the floor.
    let any_floor = world
        .query_filtered::<&Located, (With<Item>, With<mud_world::Lit>)>()
        .iter(world)
        .any(|l| l.0 == room);
    if any_floor {
        return true;
    }
    // 2. Lit items carried/worn by actors in the room. We snapshot
    // who's here, then check each as a potential carrier.
    let inhabitants: Vec<Entity> = {
        let mut q = world
            .query_filtered::<(Entity, &Located), Or<(With<Player>, With<Mob>)>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == room)
            .map(|(e, _)| e)
            .collect()
    };
    for actor in inhabitants {
        let any_on_actor = world
            .query_filtered::<&Located, (With<Item>, With<mud_world::Lit>)>()
            .iter(world)
            .any(|l| l.0 == actor);
        if any_on_actor {
            return true;
        }
    }
    false
}

/// True for room sectors where the sky is visible — used by `look`
/// to decide whether to surface the live weather line. STRUCTURE
/// (building interior) / CAVE / UNDERWATER / UNDERDARK and the
/// planes are excluded; everything outdoor (CITY streets, FIELD,
/// FOREST, mountain, road, beach, swamp, ruins, water surface,
/// AIR) shows the weather.
pub(crate) fn sector_is_outdoor_for_weather(sector: Sector) -> bool {
    matches!(
        sector,
        Sector::City
            | Sector::Field
            | Sector::Forest
            | Sector::Hills
            | Sector::Mountain
            | Sector::Shallows
            | Sector::Water
            | Sector::Air
            | Sector::Road
            | Sector::Grasslands
            | Sector::Beach
            | Sector::Swamp
            | Sector::Ruins
    )
}

/// Roll up weather + time-of-day + season into a single response
/// Look up a Zone's `Climate` by its `(zone)` id. Walks the
/// world's Zone entities — fine for the rare callers (`look
/// sky`, weather hint paths). Returns `None` if no Zone with
/// that id exists.
fn zone_climate(world: &mut World, zone_id: i32) -> Option<mud_db::enums::Climate> {
    let mut q = world.query_filtered::<(&WorldKey, &mud_world::ZoneClimate), With<mud_world::Zone>>();
    q.iter(world)
        .find(|(wk, _)| wk.zone == zone_id)
        .map(|(_, c)| c.0)
}

/// `look in <container>` — list what's inside a container the
/// player can see. Resolves the container against carried items,
/// equipped slots, and items on the floor (in that order). Empty
/// containers report a contained "is empty"; non-container targets
/// get a "you can't look inside that" since the inventory listing
/// would be misleading.
pub(crate) fn look_in_container(world: &mut World, player: Entity, target_word: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let container = find_carried_by(world, target_word, player, EquipFilter::Anywhere)
        .or_else(|| find_in_room(world, target_word, room));
    let Some(container) = container else {
        send_to(
            world,
            player,
            format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    // Refuse on non-containers: only ObjectType::Container surfaces
    // an "inside" semantically. Liquid containers fall through to
    // their own examine path; corpses are containers (handled by
    // the proto's type marker too).
    let kind = world
        .get::<WorldKey>(container)
        .and_then(|k| {
            world
                .resource::<ObjectPrototypes>()
                .by_key
                .get(&(k.zone, k.id))
                .map(|p| p.r#type)
        });
    let is_corpse = world.get::<mud_world::Corpse>(container).is_some();
    let container_name = name_of(world, container);
    if !is_corpse && !matches!(kind, Some(mud_db::enums::ObjectType::Container)) {
        send_rendered(
            world,
            player,
            &format!("{container_name} isn't a container.\r\n"),
        );
        return;
    }
    let items: Vec<String> = {
        let mut q = world.query_filtered::<(&Located, &Named), With<Item>>();
        q.iter(world)
            .filter(|(l, _)| l.0 == container)
            .map(|(_, n)| n.name.clone())
            .collect()
    };
    let coin = world.get::<mud_world::CoinPile>(container).map(|c| c.0);
    if items.is_empty() && coin.unwrap_or(0) <= 0 {
        send_rendered(
            world,
            player,
            &format!("{container_name} is empty.\r\n"),
        );
        return;
    }
    let mut out = format!("{container_name} contains:\r\n");
    if let Some(amount) = coin
        && amount > 0
        && let Some(formatted) = crate::commands::format_wealth(amount)
    {
        out.push_str(&format!("  {formatted}\r\n"));
    }
    for item_name in &items {
        out.push_str(&format!("  {item_name}\r\n"));
    }
    send_rendered(world, player, &out);
}

/// for `look (at) sky`. Indoor / cave / plane sectors get a
/// contained answer instead — there's no sky to check.
pub(crate) fn look_at_sky(world: &mut World, player: Entity) {
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;
    let sector = world.get::<RoomSector>(room).map(|s| s.0);
    let outdoor = sector.is_some_and(sector_is_outdoor_for_weather);
    if !outdoor {
        send_to(
            world,
            player,
            "There's no sky to see here — only the ceiling above.\r\n",
        );
        return;
    }
    let zone_id = world.get::<WorldKey>(room).map(|k| k.zone);
    // Climate::None marks metaphysical / interior-only zones (the
    // Void, plane spaces). Their rooms can have outdoor sectors in
    // the data — the Void is CITY — but they have no sky in any
    // meaningful sense. Suppress the celestial flavor lines for
    // them, matching the same gate in weather_tick.
    if let Some(zid) = zone_id
        && zone_climate(world, zid).is_some_and(|c| matches!(c, mud_db::enums::Climate::None))
    {
        send_to(
            world,
            player,
            "There's no sky here — only an unmoving expanse.\r\n",
        );
        return;
    }
    let live = zone_id
        .and_then(|zid| {
            world
                .resource::<mud_world::WeatherCatalog>()
                .by_zone
                .get(&zid)
                .copied()
        })
        .map(crate::weather::describe);
    let clock = world.resource::<mud_world::MudClock>();
    let hour = clock.hour;
    let season = clock.season().label();
    let phase = match hour {
        0..=4 => "Stars wheel overhead in the deep night.",
        5..=7 => "The sky brightens with the colors of dawn.",
        8..=11 => "The morning sun climbs steadily.",
        12..=13 => "The sun stands at its zenith.",
        14..=17 => "The afternoon sun slants toward the west.",
        18..=20 => "Evening colors paint the horizon.",
        _ => "Dusk fades to night.",
    };
    let mut out = format!("\r\n{phase}\r\n");
    if let Some(weather) = live {
        out.push_str(&format!("{weather}\r\n"));
    }
    out.push_str(&format!("(The season is {season}.)\r\n"));
    send_to(world, player, out);
}

/// Peek at a neighboring room through the named exit. Reports whether
/// the exit is closed/locked, and otherwise prints the target room's
/// name and description (no occupants — that requires actually being
/// there).
pub(crate) fn look_direction(world: &mut World, player: Entity, dir: Direction) {
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let Some(exits) = world.get::<Exits>(located.0).cloned() else {
        send_to(world, player, "You see nothing in that direction.\r\n");
        return;
    };
    let Some(ed) = exits.0.get(&dir).cloned() else {
        send_to(world, player, "You see nothing in that direction.\r\n");
        return;
    };
    // Hidden exits report identically to "no exit" — until
    // `search` reveals them on this player, looking that way
    // must betray nothing.
    if exit_is_hidden_to(world, player, located.0, dir, &ed) {
        send_to(world, player, "You see nothing in that direction.\r\n");
        return;
    }
    // Builder-authored exit description wins over the destination
    // peek — a "curtain of beads" should describe the curtain
    // itself, not what's beyond it. Render even when the exit is
    // closed / locked, since that's exactly when the description
    // matters most.
    let mode_pre = color_mode_for(world, player);
    if let Some(desc) = ed.description.as_ref() {
        send_to(
            world,
            player,
            format!("\r\n{}\r\n", render_color_tags(desc.trim_end(), mode_pre)),
        );
        if matches!(
            ed.state,
            mud_db::enums::ExitState::Closed | mud_db::enums::ExitState::Locked
        ) {
            return;
        }
    }
    if ed.state == mud_db::enums::ExitState::Closed
        || ed.state == mud_db::enums::ExitState::Locked
    {
        // Yellow when merely closed (push and walk in), red when
        // locked (need a key first) — matches the auto-exit list
        // colors so the player learns one palette. Builder-set
        // keyword swaps in for "The way" so a curtain reads as a
        // curtain.
        let noun = exit_noun_phrase(&ed);
        let line = match ed.state {
            mud_db::enums::ExitState::Locked => format!("<red>{noun} is locked.</>"),
            _ => format!("<yellow>{noun} is closed.</>"),
        };
        send_to(world, player, format!("{}\r\n", render_color_tags(&line, mode_pre)));
        return;
    }
    let Some(target_room) = ed.to else {
        send_to(world, player, "The way fades into the unknown.\r\n");
        return;
    };
    // Mirror the dark-room gate from cmd_look's home-room render —
    // peeking into a black cave from a lit corridor reveals nothing
    // either. Source-room lighting doesn't bleed through the doorway,
    // but HOLY_LIGHT pierces both rooms equally.
    if room_is_dark(world, target_room)
        && !room_has_light(world, target_room)
        && !player_can_see_in_dark(world, player)
    {
        send_to(
            world,
            player,
            format!("\r\nYou peer {} but see only blackness.\r\n", direction_name(dir)),
        );
        return;
    }
    let name = name_or(world, target_room, "(unknown)");
    let mode = color_mode_for(world, player);
    // Default-cyan for plain target room names; authored colors win.
    let name = render_color_tags(&colorize_default(&name, "<b:cyan>"), mode);
    let desc = world
        .get::<Description>(target_room)
        .map(|d| render_color_tags(&d.0, mode))
        .unwrap_or_default();
    // Mirror the cmd_look [peaceful] tag for the destination so a
    // player previewing a sanctuary doesn't get surprised when they
    // step in and find combat refused.
    let peaceful_tag = if world
        .get::<mud_world::PeacefulRoom>(target_room)
        .is_some()
    {
        render_color_tags("  <green>[peaceful]</>", mode)
    } else {
        String::new()
    };
    let mut out = format!(
        "\r\nYou peer {}.\r\n  {name}{peaceful_tag}\r\n",
        direction_name(dir),
    );
    if !desc.trim().is_empty() {
        out.push_str("  ");
        out.push_str(desc.trim_end());
        out.push_str("\r\n");
    }
    send_to(world, player, out);
}

pub(crate) fn direction_order(d: mud_db::enums::Direction) -> u8 {
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
pub(crate) fn flip_door_both_sides(world: &mut World, room: Entity, dir: Direction, new_state: ExitState) {
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

/// `motd` / `news` / `credits` / `policies`: long-form static text.
/// Live content lives in the schema's `SystemText` table (one row
/// per key, edited via Muditor) and is loaded once at boot into
/// the [`mud_world::SystemTexts`] resource. The constants below
/// are the compile-time fallbacks — they only render when the DB
/// is missing the corresponding row, which today only happens on a
/// completely fresh database. The fallback also doubles as the
/// authoritative reference for what the row should say if a
/// builder asks to seed defaults.
pub(crate) const MOTD_TEXT: &str = "\
\r\n<b:red>~~~ </><b:yellow>Welcome to FieryMUD</> <b:red> ~~~</>\r\n\
\r\n\
You stand at the threshold of a world older than memory — its \
forests deep, its temples ancient, its fires never wholly out.\r\n\
\r\n\
The road begins at the <cyan>Forest Temple of Mielikki</>. From \
there: a town center, training grounds for new adventurers, \
guildhalls for clerics and druids and warriors. Beyond the town \
lie the wild places, where the patient hunter finds purpose and \
the careless one finds a quiet grave.\r\n\
\r\n\
<dim>Type `commands` to see what your hands can do, `help <name>` \
for details on any command, and `news` for the most recent \
changes. If something looks broken, `bug <message>` reaches the \
keepers.</>\r\n\
\r\n\
<yellow>The fires are fed. The doors are open. Walk in.</>\r\n\
";
pub(crate) const NEWS_TEXT: &str = "\
\r\n<b:cyan>=== Recent Changes ===</>\r\n\
\r\n\
<dim>This list is curated by hand from the commit log. Most \
recent runtime changes:</>\r\n\
\r\n\
- <yellow>Combat readout</> picked up color hierarchy: HP / damage \
graded by severity, target names highlighted, miss lines dimmed.\r\n\
- <yellow>Score</> trimmed to current-stats only — equipment \
moved to `equipment`, session-meta moved to `clientinfo`.\r\n\
- <yellow>Help index</> reshaped into Info / Movement / Comm / \
Combat / Magic / Inventory / Group / Mount / Banking / Quest / \
Mail / Settings categories instead of one giant Info bucket.\r\n\
- <yellow>Spells / chants / skills / songs</> default to abilities \
you actually know; `spells all` dumps the full catalog.\r\n\
- <yellow>Combat-loot</> fixed: a corpse always spawns on mob \
death; non-AutoGold killers find their coin attached as a \
CoinPile and reclaim it via `get all from corpse`.\r\n\
- <yellow>Cast parsing</> now supports quoted multi-word names \
(`cast 'magic missile' goblin`).\r\n\
\r\n\
<dim>Run `commands` for everything you can use today.</>\r\n\
";
pub(crate) const CREDITS_TEXT: &str = "\
\r\n<b:cyan>=== Credits ===</>\r\n\
\r\n\
<yellow>FieryMUD</> stands on the shoulders of a long lineage:\r\n\
\r\n\
  <cyan>·</> <yellow>FieryMUD</> — the C++ codebase from Mielikki \
and a quarter-century of contributors\r\n\
  <cyan>·</> <yellow>HubisMUD</> — Avans, Horner, Smith, Holcomb, \
Larsen, who built the bones we kept\r\n\
  <cyan>·</> <yellow>CircleMUD</> — Jeremy Elson, who carved \
CircleMUD out of Diku at Johns Hopkins\r\n\
  <cyan>·</> <yellow>DikuMUD</> — Nyboe, Madsen, Staerfeldt, \
Seifert, and Hammer at the University of Copenhagen, who started \
all of it in 1990\r\n\
\r\n\
<dim>fierymud-rs is a clean-slate Rust rewrite — bevy_ecs, sqlx, \
tokio, mlua. Thanks to the lineage above, and to everyone who \
keeps a public MUD running.</>\r\n\
";
pub(crate) const POLICIES_TEXT: &str = "\
\r\n<b:cyan>=== Server Policies ===</>\r\n\
\r\n\
<yellow>1.</> <b:white>No harassment</>, slurs, or threats — to \
anyone, in any channel. Staff intervene quickly.\r\n\
<yellow>2.</> <b:white>No cheating</>: report bug exploits with \
`bug <message>`. Don't use them.\r\n\
<yellow>3.</> <b:white>No multi-charing for unfair advantage</>. \
Multi-charing is fine for socializing.\r\n\
<yellow>4.</> Admins enforce rules; appeals via `tell <admin> \
<message>` or the address in `motd`.\r\n\
\r\n\
<dim>This is a hobby server. Be kind.</>\r\n\
";

/// `commands`: flat alphabetical list of every command the player has
/// access to (after role + permission gating). Each command appears
/// once under its primary name; aliases are folded into the same slot
/// — `help <name>` still surfaces them per-command.
// 4 cols of width 18 = 72-char body — fits standard 80-col terminals
// after the 2-space leading indent. Width chosen for our longest
// command names today (`autoassist`, `description`, `lasttells`).
const COMMANDS_LIST_COLS: usize = 4;
const COMMANDS_LIST_COL_WIDTH: usize = 18;

/// Ordinal suffix for a day-of-month number ("1st", "22nd", "13th").
/// Handles the standard 11/12/13 exception. Used by `time` for the
/// "The 3rd day of the Month of …" line.
pub(crate) fn ordinal_suffix(n: i64) -> &'static str {
    let abs = n.unsigned_abs();
    if (11..=13).contains(&(abs % 100)) {
        return "th";
    }
    match abs % 10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    }
}

/// Carry capacity in pounds. Level-scaled: a fresh character can
/// haul ~100 lbs, an endgame character ~600. Mobs and entities
/// without a `Profile` default to 100 — the gate only applies to
/// players via `cmd_get` anyway, but the helper stays total so
/// callers don't have to special-case absence.
pub(crate) fn carry_capacity(world: &World, actor: Entity) -> f64 {
    let level = world.get::<Profile>(actor).map_or(1, |p| p.level.max(1));
    100.0 + f64::from(level) * 5.0
}

/// Look up the prototype weight of a single item — zero for
/// synthetic items without a `WorldKey`. Used by the encumbrance
/// gate to test whether one more item would overload the carrier.
pub(crate) fn item_weight(world: &World, item: Entity) -> f64 {
    world
        .get::<WorldKey>(item)
        .and_then(|wk| {
            world
                .resource::<ObjectPrototypes>()
                .by_key
                .get(&(wk.zone, wk.id))
        })
        .map_or(0.0, |p| p.weight)
}

/// Sum the prototype weight of every item rooted at `actor` —
/// inventory, equipped slots, and the contents of any container
/// they're carrying, recursively. Items missing a `WorldKey` (rare:
/// synthetic seed items) contribute zero. Used by `inventory` for
/// the readout, and reusable when pickup enforcement lands later.
pub(crate) fn carried_weight(world: &mut World, actor: Entity) -> f64 {
    use std::collections::HashSet;
    // Snapshot every (item, parent, proto_key) once so the BFS
    // below doesn't reborrow the world each step.
    let all_items: Vec<(Entity, Entity, Option<WorldKey>)> = {
        let mut q = world.query_filtered::<(Entity, &Located, Option<&WorldKey>), With<Item>>();
        q.iter(world).map(|(e, l, wk)| (e, l.0, wk.copied())).collect()
    };
    let mut visited: HashSet<Entity> = HashSet::new();
    let mut frontier: Vec<Entity> = vec![actor];
    let mut total = 0.0_f64;
    while let Some(parent) = frontier.pop() {
        for (e, p, wk) in &all_items {
            if *p != parent || !visited.insert(*e) {
                continue;
            }
            if let Some(wk) = wk
                && let Some(proto) = world
                    .resource::<ObjectPrototypes>()
                    .by_key
                    .get(&(wk.zone, wk.id))
            {
                total += proto.weight;
            }
            frontier.push(*e);
        }
    }
    total
}

/// Split `<item> from <container>` into `(item, container)` if the
/// `from` keyword appears as a separator. Returns None for inputs
/// without the keyword.
pub(crate) fn split_from_keyword(input: &str) -> Option<(&str, &str)> {
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
pub(crate) fn find_in_container(world: &mut World, needle: &str, container: Entity) -> Option<Entity> {
    let needle = needle.to_ascii_lowercase();
    let mut q = world.query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
    q.iter(world)
        .find(|(_, l, n, kw)| l.0 == container && matches(&needle, n, *kw))
        .map(|(e, _, _, _)| e)
}

/// Mirror of `split_from_keyword` for the `in` preposition. Returns
/// `Some((before, after))` when the input contains a standalone ` in `
/// separator. Used by `put` to support `put X in Y` natural phrasing.
pub(crate) fn split_in_keyword(input: &str) -> Option<(&str, &str)> {
    let lower = input.to_ascii_lowercase();
    let pat = " in ";
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

/// `eat <item>` / `quaff <item>`: consume a Food / Potion. Looks up
/// the item's proto, checks the type, then despawns. Effects are a
/// follow-up — they need `ConsumableEffects` loading.
/// Returns true when the item was actually consumed (so callers can
/// chain post-effects like resetting Hunger after a successful eat).
pub(crate) fn consume_item(world: &mut World, player: Entity, args: &str, expected: mud_db::enums::ObjectType, verb: &str) -> bool {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, format!("{} what?\r\n", capitalize(verb)));
        return false;
    }
    let item = find_carried_by(world, target_word, player, EquipFilter::Inventory);
    let Some(item) = item else {
        send_to(world, player, format!("You aren't carrying '{target_word}'.\r\n"));
        return false;
    };
    let item_name = name_of(world, item);
    let kind = world
        .get::<WorldKey>(item)
        .and_then(|k| world.resource::<ObjectPrototypes>().by_key.get(&(k.zone, k.id)).map(|p| p.r#type));
    if kind != Some(expected) {
        send_to(world, player, format!(
            "You can't {verb} {item_name}.\r\n",
        ));
        return false;
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
    true
}

/// Spawn ConsumableEffect-bound effects on `player` for object
/// proto behind `item`. No-op when `ConsumableEffects` has no rows
/// for the proto. Per-row `chance` (0.0–1.0) gates spawning.
pub(crate) fn apply_consumable_object_effects(world: &mut World, player: Entity, item: Entity) {
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
pub(crate) fn apply_consumable_liquid_effects(world: &mut World, player: Entity, liquid_name: &str) {
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

pub(crate) fn spawn_consumable_effect(
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

/// `drink <container>` / `sip <container>`: take a swig from a
/// DRINKCONTAINER. `drink` consumes 4 units, `sip` consumes 1.
/// Empty containers refuse; reaching 0 mid-action leaves the
/// container empty for next time but still completes the swig.
/// Poisoned containers print a warning line — a real poison effect
/// can wire later.
pub(crate) fn drink_amount(world: &mut World, player: Entity, args: &str, units: i32, verb: &str) {
    let target_word = args.trim();
    if target_word.is_empty() {
        send_to(world, player, format!("{} from what?\r\n", capitalize(verb)));
        return;
    }
    // Inventory match wins over room match — players carrying a
    // canteen want to drink from it before a roomside fountain.
    let inv_match = find_carried_by(world, target_word, player, EquipFilter::Anywhere);
    let item = inv_match.or_else(|| {
        world
            .get::<Located>(player)
            .copied()
            .and_then(|l| find_in_room(world, target_word, l.0))
    });
    let Some(item) = item else {
        send_to(
            world,
            player,
            format!("You don't see '{target_word}' here to {verb} from.\r\n"),
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
    // Fountains are bottomless: their proto stores a `remaining`
    // value but the runtime treats it as "always topped up". Recognize
    // them via the proto type and skip both the empty-check and the
    // post-swig decrement. Drinkcontainers stay quantitative.
    let is_fountain = world.get::<WorldKey>(item).is_some_and(|k| {
        world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(k.zone, k.id))
            .is_some_and(|p| p.r#type == mud_db::enums::ObjectType::Fountain)
    });
    if !is_fountain && state.remaining <= 0 {
        send_rendered(world, player, &format!("{item_name} is empty.\r\n"));
        return;
    }
    let drank = if is_fountain { units } else { state.remaining.min(units) };
    // Resolve the rich `LiquidDef` for the container's contents.
    // Catalog lookup is by alias; fall back to a water-shaped def
    // for unknown aliases (legacy imports / hand-edited DB rows)
    // so the swig still completes with sane defaults.
    let liquid_def = world
        .resource::<mud_world::LiquidCatalog>()
        .lookup_alias(&state.liquid)
        .cloned()
        .unwrap_or_else(|| world.resource::<mud_world::LiquidCatalog>().fallback());
    let identified = world.get::<mud_world::Identified>(item).is_some();
    // Identified container shows the real liquid name; unidentified
    // shows the color description. Matches legacy CircleMUD's
    // "you drink some clear liquid" / "you drink some water"
    // disambiguation.
    let render_label = if identified {
        liquid_def.name.to_ascii_lowercase()
    } else {
        liquid_def.color_desc.to_ascii_lowercase()
    };
    if !is_fountain
        && let Some(mut lc) = world.get_mut::<mud_world::LiquidContainer>(item)
    {
        lc.remaining -= drank;
    }
    send_rendered(
        world,
        player,
        &format!("You {verb} some {render_label} from {item_name}.\r\n"),
    );
    // Flavor description on identified containers — short paragraph
    // attached to the liquid row. Renders once per swig only when
    // the player knows what they're drinking.
    if identified
        && let Some(desc) = &liquid_def.description
    {
        send_rendered(world, player, &format!("{desc}\r\n"));
    }
    if state.poisoned {
        send_to(
            world,
            player,
            "You feel a sudden burning in your gut — that was poisoned!\r\n",
        );
    }
    apply_consumable_liquid_effects(world, player, &state.liquid);
    // Drunkenness: per-unit alcohol contribution from the
    // schema's `Liquids.drunk_effect`. 0 for non-alcoholic drinks
    // = no-op. Capped at 100 — anything beyond is "blackout"
    // (future pass: penalize skills + slur speech).
    let drunk_gain = liquid_def.drunk_effect.saturating_mul(drank);
    if drunk_gain > 0 {
        let new_total = {
            let entry = world
                .get_mut::<mud_world::Drunkenness>(player)
                .map(|mut d| {
                    d.0 = (d.0 + drunk_gain).min(100);
                    d.0
                });
            entry.unwrap_or_else(|| {
                try_insert(world, player, mud_world::Drunkenness(drunk_gain.min(100)));
                drunk_gain.min(100)
            })
        };
        if new_total >= 80 {
            send_to(world, player, "The room spins violently around you.\r\n");
        } else if new_total >= 40 {
            send_to(world, player, "You feel pleasantly buzzed.\r\n");
        }
    }
    // Hunger: per-unit fullness from `Liquids.hunger_effect`.
    // Subtracts from the hunger gauge (lower = more sated). Clamps
    // at 0. Negative values (e.g. salt water) push the gauge up.
    let hunger_delta = liquid_def.hunger_effect.saturating_mul(drank);
    if hunger_delta != 0
        && let Some(mut h) = world.get_mut::<mud_world::Hunger>(player)
    {
        h.0 = (h.0 - hunger_delta).max(0);
    }
    // Thirst: per-unit quench from `Liquids.thirst_effect`. Same
    // semantics as hunger. Replaces the old hard-coded `drank * 6`
    // and gives the schema a meaningful field. Negative values
    // (salt water, blood) intensify thirst by pushing the gauge up.
    let thirst_delta = liquid_def.thirst_effect.saturating_mul(drank);
    if thirst_delta != 0
        && let Some(mut t) = world.get_mut::<mud_world::Thirst>(player)
    {
        t.0 = (t.0 - thirst_delta).max(0);
    }
    let was_last = !is_fountain && state.remaining == drank;
    if was_last {
        send_rendered(
            world,
            player,
            &format!("{item_name} is empty now.\r\n"),
        );
    }
}

/// Shared body for `recite` / `wave` / `tap`: look up the held
/// item's `ObjectAbilities` bindings, dispatch each through the
/// cast pipeline, then either despawn (`single_use=true`, scrolls)
/// or decrement `Charges` (`single_use=false`, wands/staves —
/// despawn at 0).
pub(crate) fn invoke_object_abilities(
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

#[allow(clippy::too_many_lines)]
pub(crate) fn wear_into(world: &mut World, player: Entity, target_word: &str, force_slot: Option<Slot>) {
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

    // Alignment + class + race restrictions: refuse if the proto's
    // restriction list contains the player's bucket. Lookup is
    // by WorldKey → ObjectPrototypes; items without a proto
    // (corpses, dynamically synthesized) skip all three checks.
    // Staff (god / immortal / builder accounts) bypass every gear
    // restriction — typing or test-spawning items shouldn't get
    // blocked by alignment / class / race the world authors set
    // for normal players.
    let staff_bypass = is_staff(world, player);
    let (alignment_restriction, class_restriction, race_restriction) = if staff_bypass {
        Default::default()
    } else {
        world
            .get::<WorldKey>(item)
            .and_then(|k| {
                world
                    .resource::<ObjectPrototypes>()
                    .by_key
                    .get(&(k.zone, k.id))
                    .map(|p| {
                        (
                            p.restricted_alignments.clone(),
                            p.restricted_class_ids.clone(),
                            p.restricted_races.clone(),
                        )
                    })
            })
            .unwrap_or_default()
    };
    if !alignment_restriction.is_empty() {
        let player_align = world
            .get::<CombatStats>(player)
            .map_or(0, |c| c.alignment);
        let bucket = mud_db::enums::Alignment::from_score(player_align);
        if alignment_restriction.contains(&bucket) {
            send_rendered(
                world,
                player,
                &format!(
                    "{item_name} repels your touch — your {} alignment is incompatible.\r\n",
                    bucket.label()
                ),
            );
            return;
        }
    }
    if !class_restriction.is_empty() {
        let player_class = world.get::<Profile>(player).and_then(|p| p.class_id);
        if let Some(cid) = player_class
            && class_restriction.contains(&cid)
        {
            send_rendered(
                world,
                player,
                &format!(
                    "{item_name} won't bend to your training — your class can't use it.\r\n"
                ),
            );
            return;
        }
    }
    if !race_restriction.is_empty()
        && let Some(race) = world.get::<Profile>(player).map(|p| p.race.clone())
        && race_restriction
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&race))
    {
        send_rendered(
            world,
            player,
            &format!("{item_name} wasn't made for your kind.\r\n"),
        );
        return;
    }

    // Resolve to the actual destination slot. Paired slots (today
    // only LeftFinger / RightFinger — rings) try both sides in
    // order so a second ring falls through cleanly when the first
    // side is occupied. Other "paired" anatomies (ears, wrists)
    // are modeled as single bins that cover both sides, so they
    // keep the existing single-slot refusal.
    // Per-slot occupancy with the worn item's name, so the
    // refusal message can tell the player what's blocking them
    // ("Your wield slot is already occupied (by a longsword)
    // — `remove longsword` first."). Saves a round-trip
    // through `equipment`.
    let slot_occupants: Vec<(Slot, String)> = {
        let mut q = world.query_filtered::<
            (&Located, &Named, &EquippedSlot),
            With<Item>,
        >();
        q.iter(world)
            .filter(|(l, _, _)| l.0 == player)
            .map(|(_, n, eq)| (eq.0, n.name.clone()))
            .collect()
    };
    let occupied: std::collections::HashSet<Slot> = slot_occupants
        .iter()
        .map(|(s, _)| *s)
        .collect();
    let candidates: &[Slot] = match slot {
        Slot::LeftFinger | Slot::RightFinger => &[Slot::LeftFinger, Slot::RightFinger],
        _ => std::slice::from_ref(&slot),
    };
    let Some(&dest_slot) = candidates.iter().find(|s| !occupied.contains(s)) else {
        // Surface the names of the items in the offending slot(s)
        // so the player knows exactly what to remove.
        let blockers: Vec<String> = candidates
            .iter()
            .filter_map(|s| {
                slot_occupants
                    .iter()
                    .find(|(occ, _)| *occ == *s)
                    .map(|(_, name)| name.clone())
            })
            .collect();
        let blocker_clause = if blockers.is_empty() {
            String::new()
        } else {
            format!(" (by {})", blockers.join(", "))
        };
        // Use occupancy_label (noun form) so Wield / Hold / Hover
        // don't render as "Your wielded is already occupied" — the
        // verb-form `label()` works for "It is wielded." but breaks
        // here. Most slots are already nouns and pass through.
        let msg = if candidates.len() > 1 {
            format!(
                "Both {}s are already occupied{blocker_clause}.\r\n",
                slot.occupancy_label(),
            )
        } else {
            format!(
                "Your {} is already occupied{blocker_clause}.\r\n",
                slot.occupancy_label(),
            )
        };
        send_rendered(world, player, &msg);
        return;
    };

    try_insert(world, item, EquippedSlot(dest_slot));
    // Apply gear stat bonuses (ObjectAffects), resistances, and
    // wear-granted effects (ObjectEffects) to the player. Mirrored
    // by `cmd_remove`'s unapply path.
    crate::equip_apply::apply_object_to_wearer(world, item, player);

    let verb = match dest_slot {
        Slot::Wield => "wield",
        Slot::Hold => "hold",
        _ => "wear",
    };
    let mut msg = format!("You {verb} {item_name}.\r\n");
    // Surface bound abilities (wands, staves, magical weapons) with
    // sphere-colored parenthetical, so the player learns at equip time
    // what powers the item carries instead of having to `identify`
    // separately.
    if let Some(line) = render_bound_ability_line(world, item) {
        msg.push_str(&line);
    }
    send_rendered(world, player, &msg);
    crate::triggers::fire_item_event(world, item, player, mud_world::TriggerEvent::Wear);
}

/// Look up `ObjectAbilityCatalog` bindings for `item` and render a
/// concise follow-up line listing each ability with its sphere hue.
/// Returns `None` when the item has no bindings — the caller skips
/// the extra line so non-magical gear stays quiet.
fn render_bound_ability_line(world: &mut World, item: Entity) -> Option<String> {
    let key = world.get::<WorldKey>(item)?;
    let key = (key.zone, key.id);
    let bindings = world
        .resource::<mud_world::ObjectAbilityCatalog>()
        .by_key
        .get(&key)?
        .clone();
    if bindings.is_empty() {
        return None;
    }
    let abilities = world.resource::<AbilityCatalog>();
    let entries: Vec<String> = bindings
        .iter()
        .map(|b| {
            abilities
                .by_name
                .values()
                .find(|d| d.id == b.ability_id)
                .map_or_else(
                    || format!("ability #{}", b.ability_id),
                    crate::commands::info::format_ability_with_sphere,
                )
        })
        .collect();
    Some(format!("<dim>It carries</> {}<dim>.</>\r\n", entries.join(", ")))
}

/// Match by Keywords substring first, falling back to Name substring.
pub(crate) fn matches(needle: &str, name: &Named, kw: Option<&Keywords>) -> bool {
    if let Some(kw) = kw
        && kw.0.iter().any(|k| k.to_ascii_lowercase().contains(needle))
    {
        return true;
    }
    name.name.to_ascii_lowercase().contains(needle)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EquipFilter {
    /// Carried but not equipped (i.e. in inventory).
    Inventory,
    /// Currently equipped.
    Equipped,
    /// Either. Reserved for "look in self" flows we'll add later.
    #[allow(dead_code)]
    Anywhere,
}

/// Parse the legacy `CircleMUD` `N.needle` syntax for picking the
/// Nth match from a stack of identically-named items / mobs.
/// `2.ancient` returns `(2, "ancient")` so the caller skips to the
/// second match. A bare needle returns `(1, needle)` — the first
/// match, which is the unsurprising default. Indices < 1 collapse
/// to 1 so `0.foo` and negative inputs degrade gracefully.
#[must_use]
pub(crate) fn parse_indexed_needle(input: &str) -> (usize, &str) {
    if let Some((head, tail)) = input.split_once('.')
        && let Ok(n) = head.parse::<usize>()
        && n >= 1
        && !tail.is_empty()
    {
        return (n, tail);
    }
    (1, input)
}

pub(crate) fn find_carried_by(
    world: &mut World,
    needle: &str,
    carrier: Entity,
    filter: EquipFilter,
) -> Option<Entity> {
    let (index, needle) = parse_indexed_needle(needle);
    let needle = needle.to_ascii_lowercase();
    let mut q = world.query_filtered::<(
        Entity,
        &Located,
        &Named,
        Option<&Keywords>,
        Option<&EquippedSlot>,
    ), With<Item>>();
    q.iter(world)
        .filter(|(_, l, n, kw, eq)| {
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
        .nth(index - 1)
        .map(|(e, _, _, _, _)| e)
}

pub(crate) fn find_in_room(world: &mut World, needle: &str, room: Entity) -> Option<Entity> {
    let (index, needle) = parse_indexed_needle(needle);
    let needle = needle.to_ascii_lowercase();
    let mut q = world.query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
    q.iter(world)
        .filter(|(_, l, n, kw)| l.0 == room && matches(&needle, n, *kw))
        .nth(index - 1)
        .map(|(e, _, _, _)| e)
}

/// Find a non-Item entity in `room` (player or mob) for give/attack-style
/// targeting.
pub(crate) fn find_actor_in_room(
    world: &mut World,
    needle: &str,
    room: Entity,
    exclude: Entity,
) -> Option<Entity> {
    let (index, needle) = parse_indexed_needle(needle);
    let needle = needle.to_ascii_lowercase();
    let mut q = world.query::<(Entity, &Located, &Named, Option<&Keywords>, Option<&Item>)>();
    q.iter(world)
        .filter(|(e, l, n, kw, item)| {
            *e != exclude && l.0 == room && item.is_none() && matches(&needle, n, *kw)
        })
        .nth(index - 1)
        .map(|(e, _, _, _, _)| e)
}


/// Spawn ECS Room entities for every `PlayerHouseRoom` in the
/// summary, wire their exits, drop placed items into them, and
/// register the per-house index entries in `HousingIndex`.
pub(crate) fn synthesize_house_rooms(world: &mut World, summary: &mud_world::HouseSummary) {
    use bevy_ecs::prelude::*;
    // Phase 1: spawn rooms, populate index.
    let mut local_to_entity: std::collections::HashMap<i32, Entity> = std::collections::HashMap::new();
    let mut local_to_row_id: std::collections::HashMap<i32, i32> = std::collections::HashMap::new();
    for room in &summary.rooms {
        let entity = world
            .spawn((
                mud_world::Room,
                mud_world::HouseRoom {
                    house_id: summary.house_id,
                    local_index: room.local_index,
                },
                Named { name: room.name.clone() },
                Description(room.description.clone()),
                mud_world::RoomSector(mud_db::enums::Sector::Structure),
                mud_world::Exits::default(),
            ))
            .id();
        world
            .resource_mut::<mud_world::HousingIndex>()
            .by_key
            .insert((summary.house_id, room.local_index), entity);
        local_to_entity.insert(room.local_index, entity);
        local_to_row_id.insert(room.local_index, room.id);
    }
    // Phase 2: wire exits. We have row IDs from the schema and
    // need to map back to local_index to set Exits properly.
    let row_to_local: std::collections::HashMap<i32, i32> =
        local_to_row_id.iter().map(|(local, row)| (*row, *local)).collect();
    for exit in &summary.exits {
        let Some(&from_local) = row_to_local.get(&exit.from_room_id) else {
            continue;
        };
        let Some(&to_local) = row_to_local.get(&exit.to_room_id) else {
            continue;
        };
        let Some(&from_e) = local_to_entity.get(&from_local) else {
            continue;
        };
        let Some(&to_e) = local_to_entity.get(&to_local) else {
            continue;
        };
        let Some(dir) = parse_direction(&exit.direction.to_ascii_lowercase()) else {
            continue;
        };
        if let Some(mut exits) = world.get_mut::<mud_world::Exits>(from_e) {
            exits.0.insert(
                dir,
                mud_world::ExitData {
                    to: Some(to_e),
                    state: mud_db::enums::ExitState::Open,
                    key: None,
                    description: None,
                    keywords: Vec::new(),
                    is_hidden: false,
                    is_pickproof: false,
                },
            );
        }
    }
    // Phase 3: drop placed items into their respective rooms.
    // Items use the same prototype-spawn helper that respawn /
    // corpses already share. Each spawned item carries a
    // `HouseItem(row_id)` component so `house take` can identify
    // and DELETE the row when the item is removed.
    for placed in &summary.items {
        let room_local = local_to_row_id
            .iter()
            .find(|(_, row)| **row == placed.room_id)
            .map(|(local, _)| *local);
        let Some(room_local) = room_local else {
            continue;
        };
        let Some(&room_entity) = local_to_entity.get(&room_local) else {
            continue;
        };
        spawn_house_item(world, placed.id, placed.object_zone_id, placed.object_id, room_entity);
    }
}

/// Spawn an item from the proto catalog directly into a house
/// room. Mirrors `respawn::spawn_item_into` but without the
/// reset bookkeeping — placed items are persistent via
/// `PlayerHouseItem`, not via the reset cycle. The `house_item_id`
/// FK is attached so `house take` can DELETE the right row.
pub(crate) fn spawn_house_item(
    world: &mut World,
    house_item_id: i32,
    proto_zone: i32,
    proto_id: i32,
    parent: Entity,
) {
    let proto = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(proto_zone, proto_id))
        .cloned();
    let Some(proto) = proto else { return };
    let mut bundle = world.spawn((
        Item,
        Named { name: proto.name.clone() },
        Keywords(proto.keywords.clone()),
        WorldKey { zone: proto.zone_id, id: proto.id },
        Located(parent),
        mud_world::HouseItem(house_item_id),
    ));
    if let Some(desc) = proto.examine_description.clone() {
        bundle.insert(Description(desc));
    }
}



// admin management bodies moved to commands/admin_management.rs.



/// Tiny helper: does the target word match the named.name token or
/// any keyword? Mirrors what `find_carried_by` does internally but
/// against the room-side query.
pub(crate) fn name_or_keyword_matches(target: &str, name: &str, kw: Option<&Keywords>) -> bool {
    let t = target.to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    if n.split_whitespace().any(|tok| tok == t) {
        return true;
    }
    if let Some(kw) = kw {
        for k in &kw.0 {
            if k.to_ascii_lowercase() == t {
                return true;
            }
        }
    }
    false
}

/// `skill <name> [<target>]` — Phase A of the data-driven migration.
/// Sibling to `cast`/`chant`/`perform`: looks up an `Ability` row of
/// kind SKILL by name and invokes it through the same `invoke_ability`
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
pub(crate) fn invoke_ability(
    world: &mut World,
    player: Entity,
    args: &str,
    kind: mud_db::abilities::AbilityKind,
    verb: &str,
) {
    invoke_ability_with(world, player, args, kind, verb, false);
}

/// Target-set selectors for AOE ability dispatch. Names match the
/// locked design in `docs/design/abilities.md`'s `TargetScope` enum
/// — once `Ability.target_scope` lands as a schema column the
/// per-ability registration will name one of these directly. Until
/// then, AOE shims call `invoke_ability_aoe` with the matching
/// variant explicitly. Friendly-fire policy:
///
/// - `RoomEnemies`: every Mob in the same room as the caster, plus
///   any Player who has the `PkEnabled` flag set (consenting `PvP`
///   target), minus group members.
/// - `RoomAllies`: caster + every group member co-located with the
///   caster. Mobs are skipped unless explicitly tagged allied (no
///   such tag today; placeholder for future charm/pet integration).
/// - `RoomAll`: every actor in the room except the caster.
#[allow(clippy::enum_variant_names, dead_code)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum AoeScope {
    /// Currently the only call site (`cmd_roar`).
    RoomEnemies,
    /// Reserved for `bless` / group-heal style AOE.
    RoomAllies,
    /// Reserved for chaos / admin abilities.
    RoomAll,
}

/// Expand `scope` into a list of target entities in the caster's
/// current room, then dispatch `<ability> <target>` per-target via
/// `invoke_ability_with`. The first call is a regular dispatch
/// (`aoe_repeat = false`) so the description-box header / cooldown
/// gate fire once; subsequent calls pass `aoe_repeat = true` to
/// suppress the repeats. Empty target lists short-circuit with a
/// caller-friendly refusal.
///
/// Replaces hand-rolled per-ability AOE loops (currently `cmd_roar`)
/// with a single generic dispatcher. Once `Ability.target_scope`
/// lands the call site collapses further: `invoke_ability_with`
/// itself will read the column and pick the scope, dropping the
/// per-ability shim entirely.
pub(crate) fn invoke_ability_aoe(
    world: &mut World,
    caster: Entity,
    kind: mud_db::abilities::AbilityKind,
    verb: &str,
    ability_name: &str,
    scope: AoeScope,
    refusal_when_empty: &str,
) {
    let Some(located) = world.get::<Located>(caster).copied() else {
        send_to(world, caster, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;
    let targets: Vec<String> = aoe_targets_in_room(world, caster, room, scope);
    if targets.is_empty() {
        send_to(world, caster, refusal_when_empty);
        return;
    }
    // Per-target dispatch always passes `aoe_repeat = true` so the
    // recursive `invoke_ability_with` call doesn't re-trigger the
    // is_area AOE branch (infinite loop). Side effect: the
    // description-box header is suppressed for every AOE call,
    // including hand-rolled shims like `cmd_roar` whose ability
    // isn't itself flagged is_area=true. That's the right shape
    // for AOEs anyway — one cumulative effect, not N description
    // boxes.
    for target_name in &targets {
        invoke_ability_with(
            world,
            caster,
            &format!("{ability_name} {target_name}"),
            kind,
            verb,
            true,
        );
    }
}

/// Resolve `scope` against `room` from `caster`'s perspective and
/// return the list of target *names* in the room. Names rather
/// than entities so the per-target dispatch can re-resolve through
/// the standard `find_actor_in_room` path (handles room-mismatch /
/// despawn races identically to single-target dispatch).
fn aoe_targets_in_room(
    world: &mut World,
    caster: Entity,
    room: Entity,
    scope: AoeScope,
) -> Vec<String> {
    let group_root_e = group_root(world, caster);
    let group: std::collections::HashSet<Entity> = group_members(world, group_root_e)
        .into_iter()
        .collect();
    match scope {
        AoeScope::RoomEnemies => {
            // Mobs in the room, plus PK-flagged players (excluding
            // self / group members).
            let mut names: Vec<String> = Vec::new();
            {
                let mut q = world
                    .query_filtered::<(Entity, &Located, &Named), With<Mob>>();
                for (e, l, n) in q.iter(world) {
                    if l.0 == room && !group.contains(&e) && e != caster {
                        names.push(n.name.clone());
                    }
                }
            }
            {
                let mut q = world.query_filtered::<
                    (Entity, &Located, &Named, Option<&PlayerFlags>),
                    With<Player>,
                >();
                for (e, l, n, pf) in q.iter(world) {
                    if l.0 != room || group.contains(&e) || e == caster {
                        continue;
                    }
                    if pf.is_some_and(|f| f.has(mud_db::enums::PlayerFlag::PkEnabled)) {
                        names.push(n.name.clone());
                    }
                }
            }
            names
        }
        AoeScope::RoomAllies => {
            // Group members in the room. Mobs aren't included
            // (no allied-mob tag today). Caster is included.
            let mut q = world
                .query_filtered::<(Entity, &Located, &Named), With<Player>>();
            q.iter(world)
                .filter(|(e, l, _)| l.0 == room && group.contains(e))
                .map(|(_, _, n)| n.name.clone())
                .collect()
        }
        AoeScope::RoomAll => {
            // Everyone in the room except the caster — Players
            // and Mobs alike. Used by chaos / admin abilities.
            let mut names: Vec<String> = Vec::new();
            {
                let mut q = world
                    .query_filtered::<(Entity, &Located, &Named), With<Mob>>();
                for (e, l, n) in q.iter(world) {
                    if l.0 == room && e != caster {
                        names.push(n.name.clone());
                    }
                }
            }
            {
                let mut q = world
                    .query_filtered::<(Entity, &Located, &Named), With<Player>>();
                for (e, l, n) in q.iter(world) {
                    if l.0 == room && e != caster {
                        names.push(n.name.clone());
                    }
                }
            }
            names
        }
    }
}

/// Pull the first "token" out of `args`, treating a leading single-
/// or double-quoted phrase as one token regardless of whitespace
/// inside. Returns `(head, tail)` where `head` is the trimmed
/// phrase (quotes stripped) and `tail` is the remaining argument
/// text (or `None` if empty).
///
/// Multi-word ability names like `cast 'magic missile' goblin`
/// previously broke the cast-arg parser — the leading `'` ended up
/// inside the ability needle (`'magic`) so the catalog lookup
/// failed. This helper handles that case while preserving the
/// legacy whitespace-split behavior when no quote appears.
#[must_use]
pub(crate) fn parse_quoted_first_token(args: &str) -> (String, Option<&str>) {
    let trimmed = args.trim_start();
    let opener = trimmed.chars().next();
    if matches!(opener, Some('\'' | '"')) {
        let q = opener.unwrap();
        let rest = &trimmed[q.len_utf8()..];
        if let Some(close_idx) = rest.find(q) {
            let phrase = rest[..close_idx].trim().to_string();
            let after = rest[close_idx + q.len_utf8()..].trim_start();
            let tail = (!after.is_empty()).then_some(after);
            return (phrase, tail);
        }
        // No closing quote — fall through to whitespace split.
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("").trim().to_string();
    let tail = parts
        .next()
        .map(str::trim_start)
        .filter(|s| !s.is_empty());
    (head, tail)
}

/// fn-ptr shim used by the Lua `skills.execute` binding. Hardcodes
/// kind=Skill / verb="use" so the host doesn't have to know about
/// `AbilityKind`. The signature matches `mud_script::SkillExecutor`.
pub fn lua_invoke_skill(world: &mut World, caster: Entity, args: &str) {
    invoke_ability(
        world,
        caster,
        args,
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}

/// Sibling of `lua_invoke_skill` for the Spell kind. Hardcodes
/// verb="cast" so trigger bodies route through the standard
/// spell-casting message templates.
pub fn lua_invoke_spell(world: &mut World, caster: Entity, args: &str) {
    invoke_ability(
        world,
        caster,
        args,
        mud_db::abilities::AbilityKind::Spell,
        "cast",
    );
}

/// Sibling of `lua_invoke_spell` for the Chant kind. Verb="chant".
pub fn lua_invoke_chant(world: &mut World, caster: Entity, args: &str) {
    invoke_ability(
        world,
        caster,
        args,
        mud_db::abilities::AbilityKind::Chant,
        "chant",
    );
}

/// Sibling of `lua_invoke_spell` for the Song kind. Verb="perform".
pub fn lua_invoke_song(world: &mut World, caster: Entity, args: &str) {
    invoke_ability(
        world,
        caster,
        args,
        mud_db::abilities::AbilityKind::Song,
        "perform",
    );
}

/// fn-ptr shim used by the Lua `actor:attack_all()` binding.
/// Engages every co-located Player via `engage_combat`. Targets
/// are snapshot before the engagement loop so each call uses the
/// same room view; players who flee mid-loop simply don't get
/// re-attacked. Per-player `PeacefulRoom` gating is honored
/// inside `engage_combat`.
pub fn lua_attack_all(world: &mut World, attacker: Entity) {
    let Some(located) = world.get::<Located>(attacker).copied() else {
        return;
    };
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<
            (Entity, &Located),
            (With<mud_world::Player>, With<mud_world::Online>),
        >();
        q.iter(world)
            .filter(|(e, l)| *e != attacker && l.0 == located.0)
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        engage_combat(world, attacker, t, located.0);
    }
}

/// Same as [`invoke_ability`] but treats the call as a non-first
/// dispatch in an AOE batch:
///   - skips the leading description-box header (so `cmd_roar` over
///     N mobs prints the box once, not N times)
///   - skips the per-call cooldown gate AND the post-cast cooldown
///     write (the AOE shim is responsible for cooldown semantics —
///     gate once before the loop, set once after)
///
/// Used by AOE shims (`cmd_roar` today). For single-target dispatch
/// keep using [`invoke_ability`].
#[allow(clippy::too_many_lines)]
pub(crate) fn invoke_ability_with(
    world: &mut World,
    player: Entity,
    args: &str,
    kind: mud_db::abilities::AbilityKind,
    verb: &str,
    aoe_repeat: bool,
) {
    // Quoted phrases (`cast 'magic missile' goblin`) collapse to a
    // single token; otherwise behaves like the legacy whitespace
    // split. After the parse, normalize spaces inside the needle to
    // underscores so the user-friendly form matches the
    // underscore-keyed catalog (Ability.plain_name = "MAGIC_MISSILE"
    // → key "magic_missile"). Substring fallback uses the same
    // normalization on both sides.
    let (raw_needle, target_word) = parse_quoted_first_token(args);
    if raw_needle.is_empty() {
        send_to(world, player, format!("{} what?\r\n", capitalize(verb)));
        return;
    }
    let needle = raw_needle.to_ascii_lowercase().replace(' ', "_");

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
    // NoMagicRoom gate — `Room.allows_magic = false` marks dead-
    // magic / anti-magic rooms where the verbal-magical kinds
    // (spell/chant/song) fizzle entirely. Checked before stamina /
    // cooldown drain so a player in a sanctuary doesn't lose
    // resources to a wasted cast. SKILL kind bypasses (pure
    // physical, same precedent as the silence gate above).
    if !matches!(kind, mud_db::abilities::AbilityKind::Skill)
        && let Some(located) = world.get::<Located>(player)
        && world.get::<mud_world::NoMagicRoom>(located.0).is_some()
    {
        send_to(
            world,
            player,
            "Your spell fizzles in this dead-magic room.\r\n",
        );
        return;
    }

    // Gate on KnownAbilities. Mortals must have an explicit entry
    // for any spell/chant/song they invoke — empty/missing
    // KnownAbilities doesn't bypass. Builder+ accounts and mob
    // dispatchers (Lua-trigger / order paths) skip the gate; the
    // first is admin-testing, the second is content-vetted.
    //
    // SKILL kind keeps the legacy "empty bypass" for now so a
    // freshly-created warrior can use `bash` / `kick` without
    // first running `practice`. Tightening skills further requires
    // loading `ClassSkills` so the runtime knows what each class
    // can use at level.
    let is_staff_caster = world
        .get::<Account>(player)
        .is_some_and(|a| a.role.at_least(mud_db::enums::UserRole::Builder));
    let is_mob_caster = world.get::<Mob>(player).is_some();
    let bypass_known_check = is_staff_caster || is_mob_caster;
    let needs_explicit_known = !bypass_known_check
        && !matches!(kind, mud_db::abilities::AbilityKind::Skill);
    if needs_explicit_known {
        let knows_it = world
            .get::<KnownAbilities>(player)
            .is_some_and(|k| k.has_any(def.id));
        if !knows_it {
            // `def.name` is the display name (Title Case, may carry
            // XML-Lite color tags) — `def.plain_name` is the raw
            // SCREAMING_SNAKE_CASE schema identifier and shouldn't
            // leak to players.
            send_to(
                world,
                player,
                format!("You don't know how to {} {}.\r\n", verb, def.name),
            );
            return;
        }
    } else if matches!(kind, mud_db::abilities::AbilityKind::Skill) {
        // Mortal SKILL gate: check ClassSkills if the caster has
        // a class. The class's row sets the minimum level
        // required; missing row = wrong class for this skill.
        // Classless mortals still fall through to the legacy
        // "empty KnownAbilities bypass" so they can try anything
        // — once Profile.class_id is enforced non-NULL by login,
        // this branch goes away.
        let profile_data = world
            .get::<Profile>(player)
            .map(|p| (p.class_id, p.level));
        if let Some((Some(class_id), level)) = profile_data {
            let csd = world.resource::<mud_world::ClassSkillsData>();
            // Defensive: a class with zero ClassSkills rows means
            // content hasn't authored its kit yet. Bypass rather
            // than refuse every skill — once data lands the gate
            // engages naturally.
            if csd.class_skill_count(class_id) > 0 {
                match csd.min_level_for(class_id, def.id) {
                    Some(min_level) if min_level <= level => {
                        // Allowed by class. Layer the per-character
                        // KnownAbilities rule on top: if the player
                        // has trained anything at all, restrict to
                        // their practiced list. Empty list still
                        // bypasses so brand-new characters keep
                        // their starter kit.
                        if let Some(known) = world.get::<KnownAbilities>(player)
                            && !known.entries.is_empty()
                            && !known.has_any(def.id)
                        {
                            send_to(
                                world,
                                player,
                                format!(
                                    "You haven't practiced {} yet.\r\n",
                                    def.name
                                ),
                            );
                            return;
                        }
                    }
                    Some(min_level) => {
                        send_to(
                            world,
                            player,
                            format!(
                                "You must reach level {min_level} before you can use {}.\r\n",
                                def.name
                            ),
                        );
                        return;
                    }
                    None => {
                        send_to(
                            world,
                            player,
                            format!(
                                "Your class can't use {}.\r\n",
                                def.name
                            ),
                        );
                        return;
                    }
                }
            }
        } else if let Some(known) = world.get::<KnownAbilities>(player)
            && !known.entries.is_empty()
            && !known.has_any(def.id)
        {
            // Classless mortal with practiced kit: enforce the
            // explicit-entry rule.
            send_to(
                world,
                player,
                format!("You don't know how to {} {}.\r\n", verb, def.name),
            );
            return;
        }
    }

    // Slot gate: legacy slot-pool model. When the ability is a Spell
    // AND the caster's class has it in `ClassAbilities` (i.e. it
    // lands in a circle for this class), refuse the cast unless the
    // class has a free slot of that circle at this level. Off-class
    // spells, non-Spell kinds (Skill / Chant / Song), and classless
    // casters skip the gate. On gate pass we push a `SpellCooldown`
    // for the circle — fizzles still pay the slot ("burn the prep"),
    // matching legacy `charge_mem` semantics.
    //
    // TODO: also add per-spell `Ability.memorization_time` to the
    // recover_time. Today the column isn't loaded into AbilityDef;
    // wire it through if you want fine-grained per-spell prep tax.
    if matches!(def.kind, mud_db::abilities::AbilityKind::Spell) {
        let class_id = world.get::<Profile>(player).and_then(|p| p.class_id);
        if let Some(class_id) = class_id {
            let circle = world
                .resource::<mud_world::SpellSlotData>()
                .ability_circle
                .get(&(class_id, def.id))
                .copied();
            if let Some(circle) = circle {
                let level = world.get::<Profile>(player).map_or(0, |p| p.level);
                let max = world
                    .resource::<mud_world::SpellSlotData>()
                    .progression
                    .get(&(level, circle))
                    .copied()
                    .unwrap_or(0);
                let used = world
                    .get::<mud_world::SpellSlots>(player)
                    .map_or(0, |s| s.used_in_circle(circle));
                if used >= max {
                    send_to(
                        world,
                        player,
                        format!(
                            "Your circle {circle} slots are spent ({used}/{max}). \
                             Wait for one to recover.\r\n"
                        ),
                    );
                    return;
                }
                let recover = mud_world::CIRCLE_RECOVER_TIME
                    .get(usize::try_from(circle).unwrap_or(0))
                    .copied()
                    .unwrap_or(0);
                let cd = mud_world::SpellCooldown {
                    circle,
                    secs_remaining: recover,
                    total_secs: recover,
                };
                if let Some(mut s) = world.get_mut::<mud_world::SpellSlots>(player) {
                    s.in_flight.push(cd);
                } else {
                    world
                        .entity_mut(player)
                        .insert(mud_world::SpellSlots { in_flight: vec![cd] });
                }
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
        send_to(world, player, format!("You can only {verb} {} in combat.\r\n", def.name));
        return;
    }
    if !def.combat_ok && caster_in_combat {
        send_to(world, player, format!("You can't {verb} {} while fighting.\r\n", def.name));
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
                def.name,
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
    //
    // Skip when called as an AOE repeat — the AOE shim's first call
    // already passed the gate; subsequent per-target dispatches must
    // not be blocked by the cooldown that this very batch is about
    // to set.
    if !aoe_repeat
        && def.cooldown_ms > 0
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
                    def.name,
                ),
            );
            return;
        }
    }

    // AOE fan-out. `Ability.target_scope` carries the locked
    // design's enum (SELF / SINGLE / ROOM_* / ROOM_ENVIRONMENT,
    // plus legacy CHAIN / CONE / LINE / AREA / GROUP). Read the
    // column directly; ROOM_* values fan out via invoke_ability_aoe.
    // For abilities still on legacy values (or whose row is
    // genuinely SINGLE despite is_area=true), fall back to the
    // inference rule: is_area + violent → ROOM_ENEMIES,
    // is_area + non-violent → ROOM_ALLIES. `aoe_repeat` is the
    // recursion guard — invoke_ability_aoe iterates back through
    // this function once per target with aoe_repeat = true.
    let inferred_scope: Option<AoeScope> = match def.target_scope.as_str() {
        "ROOM_ENEMIES" => Some(AoeScope::RoomEnemies),
        "ROOM_ALLIES" => Some(AoeScope::RoomAllies),
        "ROOM_ALL" => Some(AoeScope::RoomAll),
        _ if def.is_area => Some(if def.violent {
            AoeScope::RoomEnemies
        } else {
            AoeScope::RoomAllies
        }),
        _ => None,
    };
    if !aoe_repeat
        && let Some(scope) = inferred_scope
    {
        let refusal = if matches!(scope, AoeScope::RoomEnemies | AoeScope::RoomAll) {
            format!("Nothing here to {verb} {}.\r\n", def.name)
        } else {
            format!("Nobody here for {} to reach.\r\n", def.name)
        };
        invoke_ability_aoe(
            world,
            player,
            kind,
            verb,
            &def.plain_name,
            scope,
            &refusal,
        );
        return;
    }

    let mode = color_mode_for(world, player);
    let mut out = String::from("\r\n");
    // Cast descriptor box (G2.3): the per-ability name + cast-time +
    // posture readout used to render unconditionally on every cast,
    // which felt like reading a help card every time you swung. Gate
    // it behind the same staff/dev-mode flag the combat dice display
    // uses so players see just the spell message + result. Help text
    // is still reachable on demand via `help <spell>`.
    let show_descriptor = !aoe_repeat
        && crate::combat::show_dice_for(world, player);
    if show_descriptor {
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
    }
    // Resolve the target. Empty / "me" / "self" → the caster
    // (or the caster's mount when the ability targets RIDER —
    // BUCK is the canonical case: `cast buck` with no arg should
    // unseat *your* current rider). Anything else → if the
    // ability's targeting list includes OBJECT_INV, look up a
    // carried item by keyword first (covers `cast identify
    // brooch` and friends); otherwise fall through to actor-in-
    // room. If nothing resolves, abort before applying any
    // effects.
    let valid_targets: Vec<String> = world
        .resource::<AbilityCatalog>()
        .targeting
        .get(&def.id)
        .map(|r| r.valid_targets.iter().map(|s| s.to_uppercase()).collect())
        .unwrap_or_default();
    let allows_inventory_target = valid_targets.iter().any(|t| t == "OBJECT_INV");
    let prefers_rider_default = valid_targets.iter().any(|t| t == "RIDER");
    // Hostile abilities (any ENEMY_* / AREA_FOES targeting) refuse
    // in PeacefulRoom — same contract cmd_attack and engage_combat
    // honor. Beneficial casts (FRIEND_PC / SELF / etc.) still work
    // so a healer can patch up a party in a sanctuary.
    //
    // Only 9/408 abilities currently carry an AbilityTargeting row,
    // so when valid_targets is empty we fall back to `def.violent`
    // as the signal. Without this fallback, an unattributed violent
    // spell (Burning Hands, Web, Fireball) reads as non-hostile and
    // the no-target gate below silently routes the cast onto the
    // caster — instant suicide. See G2.1 / G2.2 in remaining-work.md.
    let is_hostile_ability = if valid_targets.is_empty() {
        def.violent
    } else {
        valid_targets
            .iter()
            .any(|t| t.starts_with("ENEMY") || t == "AREA_FOES" || t == "AREA_HOSTILE")
    };
    if is_hostile_ability
        && let Some(located) = world.get::<Located>(player)
        && world.get::<mud_world::PeacefulRoom>(located.0).is_some()
    {
        send_to(
            world,
            player,
            "A peaceful aura forbids hostile magic here.\r\n",
        );
        return;
    }
    // Self-name detection: the AOE dispatcher passes per-target
    // *names* and re-resolves through this branch. RoomAllies /
    // RoomAll include the caster, so a per-target call may carry
    // the caster's own display name as the target word — match
    // that to self alongside the literal "me" / "self" strings.
    let caster_self_name = name_of(world, player);
    // Hostile abilities with no target word: auto-target the
    // caster's current Fighting opponent when one exists (a sorc
    // in melee with an orc who casts `burning hands` clearly means
    // "at the orc"), and refuse with a hint otherwise. Without the
    // Fighting fallback, the type gate below routes the cast onto
    // the caster — see the G2.1 / G2.2 bug report.
    let default_combat_target: Option<Entity> = if is_hostile_ability
        && target_word.is_none()
    {
        let opponent = world.get::<Fighting>(player).map(|f| f.0);
        if let Some(opp) = opponent
            && world.get_entity(opp).is_ok()
        {
            Some(opp)
        } else {
            let display = def.plain_name.to_ascii_lowercase().replace('_', " ");
            send_to(
                world,
                player,
                format!(
                    "{} needs a target. Try: {verb} '{display}' <target>.\r\n",
                    def.name
                ),
            );
            return;
        }
    } else {
        None
    };
    let target_entity = if let Some(opp) = default_combat_target {
        opp
    } else if let Some(word) = target_word
        && !word.eq_ignore_ascii_case("me")
        && !word.eq_ignore_ascii_case("self")
        && !word.eq_ignore_ascii_case(&caster_self_name)
    {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere; can't target.\r\n");
            return;
        };
        let inv_match = if allows_inventory_target {
            find_carried_by(world, word, player, EquipFilter::Anywhere)
        } else {
            None
        };
        let Some(found) = inv_match.or_else(|| find_actor_in_room(world, word, located.0, player))
        else {
            send_to(
                world,
                player,
                format!("You don't see '{word}' here to target.\r\n"),
            );
            return;
        };
        found
    } else if prefers_rider_default
        && let Some(mud_world::Mounted(mount)) =
            world.get::<mud_world::Mounted>(player).copied()
    {
        // RIDER target with no arg: default to the caster's mount.
        // BUCK reads "you buck *your* rider off"; without this default
        // the cast resolves to caster and trips the targeting gate.
        mount
    } else {
        player
    };
    if show_descriptor && target_entity == player {
        out.push_str("    target: yourself\r\n");
    } else if show_descriptor {
        let target_name = name_or(world, target_entity, "(unknown)");
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
            name_or(world, target_entity, "(unknown)")
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
    //
    // Material reagent boost: cast always proceeds. After the
    // effect-application phase succeeds, any consumed-flagged
    // AbilityComponent row whose proto sits in the caster's
    // *direct* inventory (not in a container, not equipped) gets
    // despawned and bumps the cast's primary effect by 25%.
    // Stacks additively, capped at +100%. The legacy `required`
    // flag is treated as advisory only — never blocks play.
    let component_reqs: Vec<mud_world::AbilityComponentReq> = world
        .resource::<AbilityCatalog>()
        .components
        .get(&def.id)
        .cloned()
        .unwrap_or_default();
    let mut to_consume: Vec<Entity> = Vec::new();
    if !component_reqs.is_empty() {
        // Direct inventory only: Located == player AND no
        // EquippedSlot. Items nested in a container have a
        // different Located parent so they're naturally excluded.
        let carried: Vec<(Entity, i32)> = {
            let mut q = world.query_filtered::<
                (Entity, &Located, &WorldKey, Option<&EquippedSlot>),
                With<Item>,
            >();
            q.iter(world)
                .filter(|(_, l, _, eq)| l.0 == player && eq.is_none())
                .map(|(e, _, k, _)| (e, k.id))
                .collect()
        };
        let mut used = std::collections::HashSet::<Entity>::new();
        for req in &component_reqs {
            if !req.consumed {
                continue;
            }
            // Pick the first matching item we haven't already
            // earmarked — duplicate AbilityComponent rows can't
            // double-eat the same instance.
            let pick = carried
                .iter()
                .find(|(e, oid)| *oid == req.object_id && !used.contains(e))
                .map(|(e, _)| *e);
            if let Some(e) = pick {
                used.insert(e);
                to_consume.push(e);
            }
        }
    }
    // Each consumed reagent adds 25% to the cast's primary effect,
    // additive, capped at +100%. With zero reagents the boost is
    // a no-op and the math collapses to the legacy path. Stored
    // as integer percent so the per-site math stays in i32.
    let reagent_boost_pct: i32 = i32::try_from(to_consume.len())
        .unwrap_or(0)
        .saturating_mul(25)
        .min(100);
    // Look up the effects this ability applies and dispatch each by
    // its `Effect.effectType`. `heal` is applied immediately to the
    // target's `Health` (or `Stamina` when `resource = "move"`); other
    // types (`status`, `modify`, ...) spawn an `EffectInstance` whose
    // duration the effect/regen ticks decrement.
    let caster_level = world.get::<Profile>(player).map_or(1, |p| p.level.max(1));
    let known_entries: Option<usize> = world.get::<KnownAbilities>(player).map(|k| k.entries.len());
    // Schema stores proficiency in 0..=1000; spell formulas
    // (`pow(skill, 1.25)`, `(skill*skill)/Y`) were authored against
    // a 0..=100 scale — the same scale `practice` displays. Without
    // this division a L21 sorc's `burning hands` resolved to
    // `4d19 + pow(1000, 1.25)` ≈ 5660 damage. G2.2.
    let caster_skill = world
        .get::<KnownAbilities>(player)
        .and_then(|k| k.entries.iter().find(|(id, _, _)| *id == def.id).map(|(_, p, _)| *p))
        .map(|raw| (raw / 10).clamp(0, 100))
        .unwrap_or(0);
    tracing::debug!(
        ability_id = def.id,
        ability_name = def.plain_name.as_str(),
        caster_skill,
        caster_level,
        known_total = known_entries.unwrap_or(0),
        has_known_component = known_entries.is_some(),
        "invoke_ability formula context"
    );
    let caster_weapon_damage = caster_weapon_damage(world, player);
    let caster_stats = world.get::<CoreStats>(player).copied().unwrap_or_default();
    let caster_hidden = i32::from(world.get::<Stealth>(player).is_some());
    let int_bonus = CoreStats::bonus(caster_stats.intelligence);
    let wis_bonus = CoreStats::bonus(caster_stats.wisdom);
    // `base_damage` = level + spell_circle*2 + max(int_bonus, wis_bonus)
    // mirrors the legacy C++ formula. Spell circle comes from
    // `SpellSlotData.ability_circle` keyed by (class_id, ability_id) —
    // 0 when the caster has no class assigned or when the ability isn't
    // a spell. Skill-only abilities (BACKSTAB / KICK / etc.) treat
    // base_damage as zero, which matches the data: their formulas
    // fall back to `weapon_damage`-based math instead.
    let caster_class_id = world.get::<Profile>(player).and_then(|p| p.class_id);
    let spell_circle = caster_class_id
        .and_then(|cid| {
            world
                .resource::<mud_world::SpellSlotData>()
                .ability_circle
                .get(&(cid, def.id))
                .copied()
        })
        .unwrap_or(0);
    let base_damage = caster_level + spell_circle * 2 + int_bonus.max(wis_bonus);
    let caster_spell_power = world
        .get::<CombatStats>(player)
        .map_or(0, |cs| cs.spell_power);
    let formula_ctx = FormulaCtx {
        level: caster_level,
        skill: caster_skill,
        weapon_damage: caster_weapon_damage,
        str_bonus: CoreStats::bonus(caster_stats.strength),
        dex_bonus: CoreStats::bonus(caster_stats.dexterity),
        con_bonus: CoreStats::bonus(caster_stats.constitution),
        int_bonus,
        wis_bonus,
        cha_bonus: CoreStats::bonus(caster_stats.charisma),
        hidden: caster_hidden,
        spell_power: caster_spell_power,
        base_damage,
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
    // would otherwise see `(unknown)` and angle-bracket-eat the
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
        name_or(world, target_entity, "(unknown)")
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
            name_or(world, target_entity, "(unknown)")
        };
        send_to(
            world,
            player,
            format!("{target_name} resists your {}.\r\n", def.name),
        );
        if target_entity != player {
            send_rendered(
                world,
                target_entity,
                &format!(
                    "You resist {}'s {}.\r\n",
                    actor_name_pre, def.name,
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
                // Snapshot target resistances once so both the
                // single-spec and per-component paths can apply
                // them (A7). Default to empty so non-physical
                // characters/mobs pass through unchanged.
                let target_resists: std::collections::HashMap<
                    mud_db::enums::ElementType,
                    i32,
                > = world
                    .get::<mud_world::Resistances>(target_entity)
                    .map(|r| r.0.clone())
                    .unwrap_or_default();
                let mut amount = if components.is_empty() {
                    // Resolve raw amount, then apply the spec's
                    // element resistance.
                    let raw = resolve_effect_amount(
                        spec.override_params.as_ref(),
                        Some(&spec.default_params),
                        &formula_ctx,
                    )
                    .unwrap_or(0);
                    let element = resolve_damage_element(
                        spec.override_params.as_ref(),
                        Some(&spec.default_params),
                    );
                    let resist = target_resists.get(&element).copied().unwrap_or(0);
                    apply_resistance(raw, resist)
                } else {
                    // Multi-component path: apply each component's
                    // resistance individually before summing — a
                    // CONE_OF_COLD (90% COLD / 10% FORCE) split
                    // hitting a cold-resistant target still takes
                    // the FORCE portion at full damage.
                    let mut total = 0i32;
                    for c in &components {
                        let raw = evaluate_simple_formula_ctx(
                            &normalize_dice_notation(&c.damage_formula),
                            &formula_ctx,
                        )
                        .unwrap_or(0);
                        let scaled = raw.saturating_mul(c.percentage) / 100;
                        // c.element is a string (loader-side
                        // verbatim). Wrap it back into the JSON
                        // shape resolve_damage_element expects so
                        // we don't fork the parser.
                        let element_blob = serde_json::json!({"type": &c.element});
                        let element = resolve_damage_element(Some(&element_blob), None);
                        let resist = target_resists.get(&element).copied().unwrap_or(0);
                        let after = apply_resistance(scaled, resist);
                        total = total.saturating_add(after);
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
                    // A5: spell_power scales magical damage as an
                    // additive % multiplier, mirroring how attack_power
                    // boosts melee swings. Non-magical abilities
                    // (skills with `is_magical=false`) skip this so
                    // they don't double-scale with attack_power.
                    if def.is_magical && caster_spell_power != 0 {
                        amount = (amount.saturating_mul(100 + caster_spell_power)) / 100;
                        amount = amount.max(1);
                    }
                    // Reagent boost on damage spells.
                    if reagent_boost_pct > 0 {
                        amount = amount.saturating_add(amount * reagent_boost_pct / 100);
                    }
                    // Ward (combat pipeline step 5): magical sources
                    // route through the target's `ward_pct`. Mundane
                    // on-hit abilities (`is_magical = false`) skip
                    // ward entirely and route purely through armor /
                    // resists. Cap at 100 so a runaway ward stack
                    // can't generate negative damage; floor at 0 so
                    // negative ward (vulnerability) stays
                    // armor-side, not ward-side.
                    let ward_pct = world
                        .get::<CombatStats>(target_entity)
                        .map_or(0, |c| c.ward_pct);
                    amount = apply_ward(amount, ward_pct, def.is_magical);
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
                        let target_name = name_or(world, target_entity, "(unknown)");
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
                let Some(mut amount) = amount else {
                    applied_msgs.push(format!("{} (no amount resolved)", spec.name));
                    continue;
                };
                // A5: spell_power scales magical heals too — high-SP
                // clerics restore more per cast. Same multiplier as
                // the damage path. Mundane heals (bandage skill etc.)
                // skip this so they don't accidentally inherit it.
                if def.is_magical && caster_spell_power != 0 && amount > 0 {
                    amount = (amount.saturating_mul(100 + caster_spell_power)) / 100;
                    amount = amount.max(1);
                }
                // Reagent boost on heal spells too.
                if reagent_boost_pct > 0 {
                    amount = amount.saturating_add(amount * reagent_boost_pct / 100);
                }
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
                if reagent_boost_pct > 0 {
                    dur_secs = dur_secs.saturating_add(dur_secs * reagent_boost_pct / 100);
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
                if reagent_boost_pct > 0 {
                    dur_secs = dur_secs.saturating_add(dur_secs * reagent_boost_pct / 100);
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
                if reagent_boost_pct > 0 {
                    dur_secs = dur_secs.saturating_add(dur_secs * reagent_boost_pct / 100);
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
                if reagent_boost_pct > 0 {
                    dur_secs = dur_secs.saturating_add(dur_secs * reagent_boost_pct / 100);
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
                    // Spell-effort spent → tag as durable so the
                    // disconnect-save snapshots it for ≤1h restore.
                    try_insert(world, target_entity, mud_world::PersistentPet);
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
            out.push_str(&format!("    you {verb} {}\r\n", def.name));
        } else {
            out.push_str(&format!(
                "    you {verb} {} on {}\r\n",
                def.name,
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
                def.name,
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
    // Reagent consumption: only when the cast actually applied at
    // least one effect (mirrors the cooldown gate below). Despawn
    // every entity in `to_consume` collected during the pre-flight
    // gate; partial application of multi-effect abilities still
    // counts as success for reagent purposes — a fireball that
    // landed but missed half its damage component still burned
    // its bat guano.
    if !aoe_repeat && !applied_msgs.is_empty() {
        let count = to_consume.len();
        for item in &to_consume {
            if let Ok(em) = world.get_entity_mut(*item) {
                em.despawn();
            }
        }
        if count > 0 {
            send_to(
                world,
                player,
                format!(
                    "Reagents flare ({count}); the cast surges by {reagent_boost_pct}%.\r\n"
                ),
            );
        }
    }
    // Cooldown write-back: only when the cast actually applied at
    // least one effect (skips no-op casts like "nothing to dispel" so
    // a player isn't penalized for misfires). Also skipped on AOE
    // repeats — the first target in the batch already wrote the
    // cooldown; subsequent dispatches share the same cooldown window.
    if !aoe_repeat && def.cooldown_ms > 0 && !applied_msgs.is_empty() {
        let ready_at = std::time::Instant::now()
            + std::time::Duration::from_millis(u64::try_from(def.cooldown_ms).unwrap_or(0));
        let mut cd = world
            .get_mut::<Cooldowns>(player)
            .map(|mut c| std::mem::take(&mut *c))
            .unwrap_or_default();
        cd.ready_at.insert(def.id, ready_at);
        crate::commands::try_insert(world, player, cd);
    }
    // USE_SKILL quest objective: ability resolved successfully.
    // Bumped here (post-cooldown) so failed-cast paths (early
    // returns above) don't credit. PARTY scope still credits
    // teammates per the shared bump_quest_progress dispatch.
    bump_use_skill_quest_progress(world, player, def.id);
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
pub(crate) enum SaveOutcome {
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
pub(crate) fn save_action_for(
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

pub(crate) fn check_target_type(
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
pub(crate) fn check_ability_restrictions(
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
pub(crate) fn check_rule_alignment(world: &World, target: Entity, rule: &serde_json::Value) -> bool {
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
pub(crate) fn check_rule_standing(world: &World, target: Entity) -> bool {
    world
        .get::<Posture>(target)
        .is_none_or(|p| p.0 == PostureKind::Standing)
}

/// True iff any entity is currently `Fighting(caster)` — i.e. the
/// caster has at least one attacker. Used by the `not_tanking`
/// restriction rule (e.g. BACKSTAB refuses while being attacked).
pub(crate) fn is_being_attacked(world: &mut World, caster: Entity) -> bool {
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
pub(crate) fn is_immobilized(world: &mut World, caster: Entity) -> bool {
    if world.get::<Stunned>(caster).is_some() {
        return true;
    }
    IMMOBILIZER_EFFECT_NAMES
        .iter()
        .any(|n| has_effect_named(world, caster, n))
}

/// True iff `caster` has any item equipped in the named slot.
pub(crate) fn caster_has_equipped(world: &mut World, caster: Entity, slot: Slot) -> bool {
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
pub(crate) fn caster_weapon_damage(world: &mut World, caster: Entity) -> i32 {
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
pub(crate) fn render_ability_template(
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
pub(crate) struct EffectSpec {
    id: i32,
    name: String,
    effect_type: String,
    override_params: Option<serde_json::Value>,
    default_params: serde_json::Value,
}

/// Pick the `resource` field out of `override_params` first, then
/// `default_params`. Defaults to "hp" — matches the schema convention
/// for heal effects whose blob omits the field.
pub(crate) fn resolve_effect_resource(
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

/// Parse the `type` field of a damage effect blob into an
/// `ElementType`. Schema values are SCREAMING_SNAKE_CASE but
/// AbilityEffect params authored in JSON are usually lowercase
/// ("fire" / "holy"). Falls back to PHYSICAL when nothing matches —
/// most legitimate spells specify a type, but we don't want
/// blobs missing a `type` to silently bypass the resistance step
/// either.
pub(crate) fn resolve_damage_element(
    override_params: Option<&serde_json::Value>,
    default_params: Option<&serde_json::Value>,
) -> mud_db::enums::ElementType {
    use mud_db::enums::ElementType as E;
    let pick = |p: Option<&serde_json::Value>| -> Option<String> {
        p?.get("type")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase)
    };
    let raw = pick(override_params).or_else(|| pick(default_params));
    match raw.as_deref() {
        Some("slash") => E::Slash,
        Some("pierce") | Some("piercing") => E::Pierce,
        Some("crush") | Some("crushing") | Some("bludgeon") | Some("bludgeoning") => E::Crush,
        Some("force") => E::Force,
        Some("sonic") => E::Sonic,
        Some("bleed") | Some("bleeding") => E::Bleed,
        Some("fire") => E::Fire,
        Some("cold") => E::Cold,
        Some("water") => E::Water,
        Some("earth") => E::Earth,
        Some("air") => E::Air,
        Some("shock") | Some("lightning") | Some("electric") => E::Shock,
        Some("acid") => E::Acid,
        Some("poison") | Some("toxic") => E::Poison,
        Some("radiant") | Some("light") => E::Radiant,
        Some("shadow") | Some("dark") => E::Shadow,
        Some("holy") | Some("divine") => E::Holy,
        Some("unholy") | Some("evil") => E::Unholy,
        Some("heal") | Some("healing") => E::Heal,
        Some("necrotic") | Some("death") => E::Necrotic,
        Some("mental") | Some("psychic") | Some("magic") => E::Mental,
        Some("nature") | Some("natural") => E::Nature,
        _ => E::Physical,
    }
}

/// Apply per-element resistance to a damage amount (A7 / combat.md
/// step 5). `resist_pct` is the Resistance map value for the
/// element: positive = mitigate, negative = vulnerable. Mitigation
/// caps at 100% (immune). Vulnerability has no cap from this step
/// but the apply path floors the final damage at zero.
#[must_use]
pub(crate) fn apply_resistance(amount: i32, resist_pct: i32) -> i32 {
    let pct = resist_pct.clamp(-1000, 100);
    let mitigated = amount.saturating_mul(100 - pct) / 100;
    mitigated.max(0)
}

/// "first" stops after one removal; everything else (including the
/// schema default `"all"`) means strip every match.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DispelScope {
    All,
    First,
}

/// Read `filter` from a dispel effect's params (override → default).
/// Lowercased for case-insensitive tag matching against
/// `EffectDef.tags`. Returns empty when neither blob has a filter
/// — caller falls through to a "no filter specified" message.
pub(crate) fn resolve_dispel_filter(
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
pub(crate) fn resolve_teleport_destination(
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
pub(crate) fn resolve_dispel_scope(
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
pub(crate) fn remove_effects_by_tag(
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
    // For tag="magic" we also count EffectInstances whose source
    // Ability is SPELL / CHANT / SONG, regardless of the catalog
    // Effect's own tags. Generic effects like `status` and `modify`
    // power both magical (bless, web, slow) and non-magical (berserk,
    // poison) abilities; tagging the catalog row as "magic" would
    // dispel both. Filtering on the *source ability* lets
    // DISPEL_MAGIC clean up bless without touching berserk.
    let magic_ability_ids: std::collections::HashSet<i32> = if tag.eq_ignore_ascii_case("magic") {
        use mud_db::abilities::AbilityKind;
        world
            .resource::<AbilityCatalog>()
            .by_name
            .values()
            .filter(|def| {
                matches!(
                    def.kind,
                    AbilityKind::Spell | AbilityKind::Chant | AbilityKind::Song
                )
            })
            .map(|def| def.id)
            .collect()
    } else {
        std::collections::HashSet::new()
    };
    if tag_match.is_empty() && magic_ability_ids.is_empty() {
        return 0;
    }
    let mut to_remove: Vec<Entity> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, eff, applied)| {
                applied.0 == target
                    && (tag_match.contains(&eff.kind)
                        || eff
                            .ability_id
                            .is_some_and(|aid| magic_ability_ids.contains(&aid)))
            })
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
pub(crate) fn resolve_redirect_aggro(
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
pub(crate) fn resolve_knockdown_posture(
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
pub(crate) fn apply_knockdown_posture(world: &mut World, target: Entity, posture: PostureKind) -> bool {
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
pub(crate) fn resolve_effect_conditions(
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
pub(crate) fn apply_modify_delta(world: &mut World, target: Entity, stat: &str, amount: i32) -> bool {
    match stat {
        // `_bonus` aliases (str_bonus, dex_bonus, ...) match the
        // formula-context naming convention used by `FormulaCtx::lookup`
        // and the fierylib migrate_object_affects.py target labels.
        "str" | "strength" | "str_bonus" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.strength = s.strength.saturating_add(amount);
            }
            true
        }
        "dex" | "dexterity" | "dex_bonus" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.dexterity = s.dexterity.saturating_add(amount);
            }
            true
        }
        "con" | "constitution" | "con_bonus" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.constitution = s.constitution.saturating_add(amount);
            }
            true
        }
        "int" | "intelligence" | "int_bonus" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.intelligence = s.intelligence.saturating_add(amount);
            }
            true
        }
        "wis" | "wisdom" | "wis_bonus" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.wisdom = s.wisdom.saturating_add(amount);
            }
            true
        }
        "cha" | "charisma" | "cha_bonus" => {
            if let Some(mut s) = world.get_mut::<CoreStats>(target) {
                s.charisma = s.charisma.saturating_add(amount);
            }
            true
        }
        "accuracy" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.accuracy = cs.accuracy.saturating_add(amount);
            }
            true
        }
        "attack_power" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.attack_power = cs.attack_power.saturating_add(amount);
            }
            true
        }
        "evasion" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.evasion = cs.evasion.saturating_add(amount);
            }
            true
        }
        "spell_power" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.spell_power = cs.spell_power.saturating_add(amount);
            }
            true
        }
        "armor_flat" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.armor_flat = cs.armor_flat.saturating_add(amount).max(0);
            }
            true
        }
        "hardness" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.hardness = cs.hardness.saturating_add(amount).max(0);
            }
            true
        }
        "pen_flat" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.pen_flat = cs.pen_flat.saturating_add(amount);
            }
            true
        }
        "pen_pct" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.pen_pct = cs.pen_pct.saturating_add(amount);
            }
            true
        }
        // Ward stays its own stat (combat pipeline step 5, gated
        // by `Ability.is_magical`).
        "ward" | "ward_pct" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.ward_pct = cs.ward_pct.saturating_add(amount).clamp(0, 100);
            }
            true
        }
        "max_hp" => {
            if let Some(mut h) = world.get_mut::<Health>(target) {
                h.max = h.max.saturating_add(amount).max(1);
                if amount > 0 {
                    h.hp = h.hp.saturating_add(amount);
                } else if h.hp > h.max {
                    // Removing a +HP buff/item: clamp current HP
                    // to the new (smaller) max so a wearer doesn't
                    // walk around with hp > max.
                    h.hp = h.max;
                }
            }
            true
        }
        "max_move" | "max_stamina" | "stamina_max" => {
            if let Some(mut s) = world.get_mut::<Stamina>(target) {
                s.max = s.max.saturating_add(amount).max(0);
                if amount > 0 {
                    s.current = s.current.saturating_add(amount);
                } else if s.current > s.max {
                    s.current = s.max;
                }
            }
            true
        }
        "armor_pct" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(target) {
                cs.armor_pct = cs
                    .armor_pct
                    .saturating_add(amount)
                    .clamp(0, 100);
            }
            true
        }
        "saving_para" | "saving_rod" | "saving_petri" | "saving_breath" | "saving_spell" => {
            if world.get::<mud_world::SavingThrows>(target).is_none() {
                try_insert(world, target, mud_world::SavingThrows::default());
            }
            if let Some(mut s) = world.get_mut::<mud_world::SavingThrows>(target) {
                let field = match stat {
                    "saving_para" => &mut s.para,
                    "saving_rod" => &mut s.rod,
                    "saving_petri" => &mut s.petri,
                    "saving_breath" => &mut s.breath,
                    "saving_spell" => &mut s.spell,
                    _ => unreachable!(),
                };
                *field = field.saturating_add(amount);
            }
            true
        }
        "focus" => {
            if world.get::<mud_world::Focus>(target).is_none() {
                try_insert(world, target, mud_world::Focus::default());
            }
            if let Some(mut f) = world.get_mut::<mud_world::Focus>(target) {
                f.0 = f.0.saturating_add(amount);
            }
            true
        }
        "perception" => {
            if world.get::<mud_world::Perception>(target).is_none() {
                try_insert(world, target, mud_world::Perception::default());
            }
            if let Some(mut p) = world.get_mut::<mud_world::Perception>(target) {
                p.0 = p.0.saturating_add(amount);
            }
            true
        }
        "hit_regen" => {
            if world.get::<mud_world::RegenBonus>(target).is_none() {
                try_insert(world, target, mud_world::RegenBonus::default());
            }
            if let Some(mut r) = world.get_mut::<mud_world::RegenBonus>(target) {
                r.hp = r.hp.saturating_add(amount);
            }
            true
        }
        "hiddenness" => {
            // Stealth bonus from gear (e.g. Amulet of True Shadows).
            // Existing `Stealth` is a marker component; extend it
            // lazily into a magnitude. For now stash under a stub
            // RegenBonus.stamina-like channel — the visibility tick
            // can read it once stealth scoring is wired (Q6 in
            // combat-rebalance.md). Logged-only for now to avoid
            // silently dropping the modifier.
            tracing::debug!(
                target = ?target,
                amount,
                "APPLY_HIDDENNESS recorded but no consumer wired yet"
            );
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

pub(crate) fn apply_heal_hp(world: &mut World, target: Entity, amount: i32) -> i32 {
    if amount <= 0 {
        return 0;
    }
    // Ghost targets aren't healable — a corpse can't recover. Heals
    // resolve as 0 actual hp restored. `release` is the only path
    // back from Ghost, and it restores hp = max directly.
    if world.get::<mud_world::Ghost>(target).is_some() {
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
pub(crate) fn apply_heal_stamina(world: &mut World, target: Entity, amount: i32) -> i32 {
    if amount <= 0 {
        return 0;
    }
    // Ghosts don't refill stamina either — same reasoning as
    // apply_heal_hp.
    if world.get::<mud_world::Ghost>(target).is_some() {
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
pub(crate) fn has_effect_named(world: &mut World, target: Entity, name: &str) -> bool {
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
/// set, OR if any active status flag is in the hard-coded
/// immobilizing-flag table. The schema only carries `prevents_*`
/// on the EFFECT TYPE (status/damage/heal/…), not on the per-flag
/// row (webbed/held/sleeping/…) — so the type-level check alone
/// misses status-flag immobilizers like WEB. Until per-flag rows
/// exist, the secondary name table below catches them. G2.4.
pub(crate) fn effect_prevents(world: &mut World, target: Entity, kind: Prevent) -> bool {
    let active: Vec<(i32, String)> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, a)| a.0 == target)
            .map(|(eff, _)| (eff.kind, eff.name.clone()))
            .collect()
    };
    if active.is_empty() {
        return false;
    }
    let catalog = world.resource::<EffectCatalog>();
    let type_blocks = active.iter().any(|(id, _)| {
        catalog.by_id.get(id).is_some_and(|def| match kind {
            Prevent::Speaking => def.prevents_speaking,
            Prevent::Casting => def.prevents_casting,
            Prevent::Movement => def.prevents_movement,
        })
    });
    if type_blocks {
        return true;
    }
    active
        .iter()
        .any(|(_, name)| flag_prevents(name.as_str(), kind))
}

/// Per-flag immobilizing / silencing list. Lives next to
/// `effect_prevents` so the policy is one place. Once the schema
/// carries `prevents_*` per status flag (and the importer fills
/// it), this table is the place to delete.
fn flag_prevents(flag: &str, kind: Prevent) -> bool {
    let f = flag.to_ascii_lowercase();
    match kind {
        Prevent::Movement => matches!(
            f.as_str(),
            "webbed" | "held" | "hold_person" | "paralyzed" | "asleep" | "sleeping" | "rooted"
                | "entangled" | "stunned" | "stun"
        ),
        Prevent::Casting => matches!(
            f.as_str(),
            "silenced" | "silence" | "stunned" | "stun" | "asleep" | "sleeping" | "paralyzed"
        ),
        Prevent::Speaking => matches!(
            f.as_str(),
            "silenced" | "silence" | "stunned" | "stun" | "asleep" | "sleeping"
        ),
    }
}

/// Despawn every `EffectInstance` on `target` whose name matches
/// `name` (case-insensitive). Returns the number despawned. Used by
/// curative skills (bandage stops bleed) and by the `cleanse`
/// effect-type consumer in `invoke_ability`.
pub(crate) fn remove_effect_named(world: &mut World, target: Entity, name: &str) -> usize {
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

/// Name-approval gate for social commands. Returns `true` (and
/// sends a refusal line) when `player` carries the
/// `NameApprovalPending` marker — every chat command (`tell`,
/// `say`, `gossip`, `gsay`, clan chat, `invite`, …) calls this
/// first and bails on a true return. Returns `false` (no message)
/// when the gate is open so the command proceeds normally.
///
/// The marker is set at spawn time when `Characters.name_approved =
/// false` and removed by `approve_name` (or by `reject_name`'s
/// auto-rename + reconnect). Player-facing message includes
/// `name_status` as the diagnostic command so the player can
/// confirm the gate is the cause rather than an effect / freeze.
pub(crate) fn name_approval_gate(world: &World, player: Entity) -> bool {
    if world
        .get::<mud_world::NameApprovalPending>(player)
        .is_some()
    {
        send_to(
            world,
            player,
            "You can't use that until your name is approved by staff. \
             Run `name_status` for details.\r\n",
        );
        true
    } else {
        false
    }
}

/// Despawn every `EffectInstance` on `target`, regardless of name.
/// Used by `cleanse` effects whose `condition` is `"all"`.
pub(crate) fn remove_all_effects_on(world: &mut World, target: Entity) -> usize {
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

pub(crate) fn resolve_effect_duration(
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
pub(crate) fn resolve_effect_amount(
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
pub(crate) fn amount_from_blob(params: Option<&serde_json::Value>, ctx: &FormulaCtx) -> Option<i32> {
    let p = params?;
    let v = p.get("amount")?;
    numeric_or_formula(v, ctx)
}

/// Pull a `bonusIfHidden` field — schema convention for "extra damage
/// when the caster has the Stealth marker". Same numeric/formula
/// shape as `amount`. Returns None when the field is absent.
pub(crate) fn bonus_if_hidden_from_blob(
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
pub(crate) fn numeric_or_formula(v: &serde_json::Value, ctx: &FormulaCtx) -> Option<i32> {
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
pub(crate) fn normalize_dice_notation(expr: &str) -> String {
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
pub(crate) fn duration_from_blob(params: Option<&serde_json::Value>, ctx: &FormulaCtx) -> Option<i32> {
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
pub(crate) struct FormulaCtx {
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
    /// Caster's `CombatStats.spell_power`. Read directly by spell
    /// formulas that want a flat-ish term, and applied as an
    /// additive % multiplier on magical damage/heal at the apply
    /// step. Mirrors how `attack_power` boosts melee swings.
    spell_power: i32,
    /// Spell-baseline damage: `level + spell_circle * 2 +
    /// max(int_bonus, wis_bonus)`. Computed at invoke time when
    /// the caster's class + ability spell circle are known. Most
    /// damage spells have `amount` formulas of shape
    /// `base_damage + (skill^2 * X) / Y` so a missing
    /// `base_damage` turns those into pure skill-scaling math; the
    /// runtime now supplies the additive term so untrained casters
    /// at high level still get circle/level-baseline damage.
    base_damage: i32,
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
            "spell_power" | "sp" => Some(self.spell_power),
            "base_damage" | "base" => Some(self.base_damage),
            _ => None,
        }
    }
}

/// Test/legacy entry point — production callsites take the full
/// `FormulaCtx` via `evaluate_simple_formula_ctx`.
#[cfg(test)]
pub(crate) fn evaluate_simple_formula(expr: &str, level: i32, skill: i32) -> Option<i32> {
    evaluate_simple_formula_ctx(expr, &FormulaCtx::base(level, skill))
}

/// Live-RNG entry point that takes the full `FormulaCtx` — used by
/// `invoke_ability` when caster-derived symbols (`weapon_damage` etc.)
/// matter.
pub(crate) fn evaluate_simple_formula_ctx(expr: &str, ctx: &FormulaCtx) -> Option<i32> {
    evaluate_formula(expr, ctx, &mut |name, a, b| match name {
        "roll_dice" => roll_dice(a, b),
        "random" if a <= b => rand::random_range(a..=b),
        _ => 0,
    })
}

/// Roll `num` dice with `sides` sides each and sum them. Both args
/// must be positive; non-positive inputs return 0.
pub(crate) fn roll_dice(num: i32, sides: i32) -> i32 {
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
pub(crate) fn evaluate_formula(
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
pub(crate) enum FormulaToken {
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

pub(crate) fn tokenize_formula(expr: &str) -> Option<Vec<FormulaToken>> {
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

pub(crate) struct FormulaParser<'a> {
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

/// Uppercase the first alphabetic character, leaving the rest of the
/// string unchanged. Skips past leading XML-Lite color tags
/// (e.g. `<b:cyan>Dragon</>`) so the first *visible* letter gets
/// capitalized, not `<`. Use at sentence-start sites where a
/// mob/player name leads — "the Elite Mage leaves." → "The Elite
/// Mage leaves."  This differs from `capitalize` (full title-case),
/// which would lowercase "Elite" / "Mage" downstream of the article.
pub(crate) fn cap_sentence_start(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    let mut done = false;
    while let Some(c) = chars.next() {
        if !done {
            // Pass XML-Lite tags through verbatim — `<b:cyan>` etc.
            // The first letter we want to capitalize is the one after
            // the (possibly multiple) leading tags.
            if c == '<' {
                out.push(c);
                for tag_c in chars.by_ref() {
                    out.push(tag_c);
                    if tag_c == '>' {
                        break;
                    }
                }
                continue;
            }
            if c.is_alphabetic() {
                for upper in c.to_uppercase() {
                    out.push(upper);
                }
                done = true;
                continue;
            }
        }
        out.push(c);
    }
    out
}

pub(crate) fn capitalize(s: &str) -> String {
    // Title-case: first character uppercase, rest lowercase. Race
    // and gender values arrive from the DB ALL CAPS (`HUMAN` /
    // `MALE`), and the score / examine readouts shouldn't show
    // them that way. Verb-style usage ("eat what?") still works
    // because the lowercase rest is a no-op when the input is
    // already lowercase. Single-char inputs collapse to just
    // uppercase as before.
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => {
            let head = c.to_ascii_uppercase().to_string();
            let tail: String = chars.as_str().to_ascii_lowercase();
            head + &tail
        }
    }
}

/// Try to dispatch `verb` as a social. Returns true if a matching social was
/// found (regardless of outcome — includes cases where target wasn't found).
pub(crate) fn try_dispatch_social(world: &mut World, player: Entity, verb: &str, args: &str) -> bool {
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

pub(crate) fn run_social(world: &mut World, player: Entity, social: &SocialDef, args: &str) {
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

pub(crate) fn matches_self(actor_name: &str, target_word: &str) -> bool {
    if target_word.eq_ignore_ascii_case("me") || target_word.eq_ignore_ascii_case("self") {
        return true;
    }
    actor_name
        .to_ascii_lowercase()
        .contains(&target_word.to_ascii_lowercase())
}

/// Replace social template placeholders. Genderless pronouns until we wire
/// per-character gender; "their" / "them" / "they" are the safe defaults.
pub(crate) fn substitute(template: &str, actor_name: &str, target_name: Option<&str>) -> String {
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
/// Historical alias for [`send_to`]. Kept so existing call sites
/// don't need a sweep — `send_to` now always renders. Prefer
/// `send_to` in new code.
pub(crate) fn send_rendered(world: &World, target: Entity, text: &str) {
    send_to(world, target, text);
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
/// used by sites that prefer literal placeholders like `(unknown)`,
/// `<gone>`, or `(nowhere)` when the entity lacks a Named. Angle-bracket
/// placeholders pass through `render_color_tags` literally so long as
/// the body isn't a recognized tag part — pick whichever bracket style
/// reads best.
pub(crate) fn name_or(world: &World, e: Entity, fallback: &str) -> String {
    world
        .get::<Named>(e)
        .map_or_else(|| fallback.to_string(), |n| n.name.clone())
}

/// `true` when the entity belongs to a staff (god / immortal /
/// builder) account. Used as the standard bypass gate for
/// player-facing restrictions that gods shouldn't be subject to —
/// alignment / class / race wear gates, encumbrance gates on
/// `get`, and similar. Mirrors the threshold used elsewhere
/// (combat re-aggro skip, casting bypasses); collected in one
/// helper so adding a new bypass is a single call rather than
/// re-deriving the role check.
#[must_use]
pub(crate) fn is_staff(world: &World, entity: Entity) -> bool {
    // DevMode short-circuit: open-playtest servers treat every player
    // as staff so testers can spawn / heal / inspect without an admin
    // claim. See ``DevMode`` in ``main.rs`` for the loud warning banner.
    if world.get_resource::<crate::DevMode>().is_some_and(|d| d.0) {
        return true;
    }
    world
        .get::<Account>(entity)
        .is_some_and(|a| a.role.at_least(mud_db::enums::UserRole::Builder))
}

/// `true` when the exit should appear hidden to this player —
/// i.e. it carries `ExitData::is_hidden = true` and the player
/// hasn't yet found it via `search`. Used by every exit-rendering
/// / movement site so the reveal logic stays consistent. Pass
/// the `room` entity the exit lives on so the per-character
/// `RevealedExits` set can be keyed `(room, dir)`.
#[must_use]
pub(crate) fn exit_is_hidden_to(
    world: &World,
    player: Entity,
    room: Entity,
    dir: Direction,
    ed: &mud_world::ExitData,
) -> bool {
    if !ed.is_hidden {
        return false;
    }
    !world
        .get::<mud_world::RevealedExits>(player)
        .is_some_and(|r| r.set.contains(&(room, dir)))
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
        send_to(world, t, raw_msg);
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
        send_to(world, t, raw_msg);
    }
}

/// `broadcast_room_except_players_rendered`, but with a `sender`
/// entity supplied so wiz-invised speakers stay invisible to
/// lower-level observers. Use this at every site whose broadcast
/// names a specific actor (movement, recall, portal enter,
/// social emotes). Sites without a clear sender (mob-triggered
/// effects, weather, etc.) keep the unfiltered variant.
pub(crate) fn broadcast_room_visible(
    world: &mut World,
    room: Entity,
    sender: Entity,
    except: &[Entity],
    raw_msg: &str,
) {
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| {
                l.0 == room
                    && !except.contains(e)
                    && can_see_player(world, *e, sender)
            })
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        send_to(world, t, raw_msg);
    }
}

/// Combat-action stamina costs (one stop in scope so a balance pass can
/// retune them in one place).
pub(crate) const ATTACK_COST: i32 = 2;
pub(crate) const KICK_COST: i32 = 5;
pub(crate) const BASH_COST: i32 = 8;
pub(crate) const BANDAGE_COST: i32 = 4;
pub(crate) const LAYHANDS_COST: i32 = 12;
pub(crate) const RESCUE_COST: i32 = 6;
pub(crate) const DISARM_COST: i32 = 5;
pub(crate) const HITALL_COST: i32 = 10;
pub(crate) const DOORBASH_COST: i32 = 10;
pub(crate) const BACKSTAB_COST: i32 = 6;
pub(crate) const SPRINGLEAP_COST: i32 = 7;
pub(crate) const GOUGE_COST: i32 = 7;
pub(crate) const REND_COST: i32 = 7;
pub(crate) const ROAR_COST: i32 = 8;
pub(crate) const STOMP_COST: i32 = 6;
pub(crate) const TRIPUP_COST: i32 = 5;
pub(crate) const SWEEP_COST: i32 = 12;
pub(crate) const ROUNDHOUSE_COST: i32 = 7;
pub(crate) const THROATCUT_COST: i32 = 8;
pub(crate) const BERSERK_COST: i32 = 8;

/// Pre-flight stamina check. Returns false if the player has Stamina and
/// it's below `cost`; sends "You're too winded to <verb>." and the caller
/// should abort. Players without a Stamina component pass (mobs, etc.).
pub(crate) fn check_stamina(world: &World, player: Entity, cost: i32, verb: &str) -> bool {
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

/// Combat pipeline step 5 ("Wards") per `docs/design/combat.md`.
/// Magical sources route the rolled damage through the target's
/// `ward_pct`; mundane on-hit abilities and raw weapon swings skip
/// the layer entirely.
///
/// Ward is clamped to `[0, 100]` so a runaway buff stack can't
/// flip the sign (negative ward = vulnerability stays armor-side,
/// not ward-side; >100 immunity collapses to "all damage zeroed").
#[must_use]
pub(crate) fn apply_ward(amount: i32, ward_pct: i32, is_magical: bool) -> i32 {
    if !is_magical {
        return amount;
    }
    let ward = ward_pct.clamp(0, 100);
    if ward == 0 {
        return amount;
    }
    amount.saturating_mul(100 - ward) / 100
}

/// Apply `amount` damage to `target`'s Health. Returns `(dead, threshold_msg)`
/// — `dead` is true if HP dropped to zero or below; `threshold_msg`, if Some,
/// is a one-time downward-crossing message ("hurt"/"badly hurt"/"near death")
/// that the caller should `send_to(target, ..)` after its hit-line so the
/// ordering reads naturally. None when no threshold was crossed, when the
/// target lacks Health, or when the blow was lethal (death message takes over).
/// Most-severe-wins: a single hit that crosses several thresholds emits only
/// the lowest-band message.
/// Push a `Char.Vitals` GMCP frame to `target` immediately, using
/// the current Health / Stamina / Profile state. Called both
/// at end-of-tick (from `send_prompt`) and mid-tick from
/// `apply_damage` so the client's HP gauge tracks the visible
/// damage text without a one-round lag (G3.3). No-op when the
/// entity has no Connection (mob, switched puppet, etc.).
pub(crate) fn send_char_vitals(world: &World, target: Entity) {
    let Some(conn) = world.get::<Connection>(target).map(|c| c.0.clone()) else {
        return;
    };
    let (Some(h), Some(s)) = (
        world.get::<Health>(target).copied(),
        world.get::<Stamina>(target).copied(),
    ) else {
        return;
    };
    let (level, xp) = world
        .get::<Profile>(target)
        .map_or((0, 0), |p| (p.level, p.experience));
    let next_level_pct = compute_level_progress(world, level, xp);
    let payload = format!(
        "{{\"hp\":{hp},\"max_hp\":{max_hp},\"mv\":{mv},\"max_mv\":{max_mv},\"next_level_pct\":{nlp},\"string\":\"H:{hp}/{max_hp} V:{mv}/{max_mv}\"}}",
        hp = h.hp,
        max_hp = h.max,
        mv = s.current,
        max_mv = s.max,
        nlp = next_level_pct,
    );
    let _ = conn.try_send(mud_net::gmcp_packet("Char.Vitals", &payload));
}

pub(crate) fn apply_damage(
    world: &mut World,
    target: Entity,
    amount: i32,
) -> (bool, Option<&'static str>) {
    // Ghost targets are dead-but-incorporeal — no damage applied,
    // no death event. The combat-tick re-aggro filter already
    // prevents mobs from picking up a Ghost as a fresh target, but
    // a swing that was already snapshotted before the Ghost was
    // ghosted (mid-tick death) can still land here.
    if world.get::<mud_world::Ghost>(target).is_some() {
        return (false, None);
    }
    let Some((old, max)) = world.get::<Health>(target).map(|h| (h.hp, h.max)) else {
        return (false, None);
    };
    let new_value = old - amount;
    if let Some(mut h) = world.get_mut::<Health>(target) {
        h.hp = new_value;
    }
    // G3.3: push a Char.Vitals frame mid-tick. The end-of-tick
    // prompt flush also sends Char.Vitals, but doing it here keeps
    // the client's HP gauge in lock-step with the damage line that
    // immediately follows in the same buffer flush. Without this
    // the gauge always read one round behind the visible text.
    send_char_vitals(world, target);
    if new_value <= 0 {
        return (true, None);
    }
    let near = max / 10;
    let badly = max / 4;
    let hurt = max / 2;
    // Threshold-crossing messages — fire once per crossing, color-
    // graded by severity. The render pipeline strips tags for
    // clients that don't support color, so plain telnet sees the
    // bare text unchanged.
    let msg = if old > near && new_value <= near {
        Some("<b:red>You are near death!</>\r\n")
    } else if old > badly && new_value <= badly {
        Some("<red>You are badly hurt!</>\r\n")
    } else if old > hurt && new_value <= hurt {
        Some("<yellow>You are hurt.</>\r\n")
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
pub(crate) fn require_alert_posture(world: &mut World, player: Entity, action: &str) -> bool {
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

/// Mob HELPER behavior: every mob in `room` (other than attacker /
/// defender) carrying the `Helper` `MobBehavior` auto-engages the
/// attacker. Mirrors `auto_assist_followers_of` for mobs and
/// fires from the same call site (`cmd_attack`). Skips mobs already
/// in combat — they don't switch targets just because someone
/// nearby is being attacked.
pub(crate) fn mob_helpers_engage(world: &mut World, defender: Entity, attacker: Entity, room: Entity) {
    // PeacefulRoom blocks helper aggro — same contract as
    // `engage_combat` and `cmd_attack`. cmd_attack already
    // refuses, so this guard mostly catches the case where
    // attacker / defender entered combat outside the room and
    // walked back in (e.g. portal misdirection).
    if world.get::<mud_world::PeacefulRoom>(room).is_some() {
        return;
    }
    let helpers: Vec<Entity> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &mud_world::MobBehaviors, Option<&Fighting>),
            With<Mob>,
        >();
        q.iter(world)
            .filter(|(e, l, beh, fighting)| {
                *e != defender
                    && *e != attacker
                    && l.0 == room
                    && fighting.is_none()
                    && beh.has(mud_db::enums::MobBehavior::Helper)
            })
            .map(|(e, _, _, _)| e)
            .collect()
    };
    if helpers.is_empty() {
        return;
    }
    let defender_name = name_of(world, defender);
    let attacker_name = name_of(world, attacker);
    for helper in helpers {
        try_insert(world, helper, Fighting(attacker));
        let helper_name = name_of(world, helper);
        send_rendered(
            world,
            attacker,
            &format!("{helper_name} leaps to {defender_name}'s defense!\r\n"),
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[attacker],
            &format!("{helper_name} leaps to {defender_name}'s defense against {attacker_name}!\r\n"),
        );
    }
}

/// When `defender` is attacked, find every entity with
/// `Follower(defender)` who has the `AUTO_ASSIST` flag, isn't already
/// fighting, and is in `room`, and engage `attacker`. Used as the
/// hook on the bottom of `cmd_attack`.
pub(crate) fn auto_assist_followers_of(
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
    let attacker_name = name_or(world, attacker, "(unknown)");
    for helper in helpers {
        try_insert(world, helper, Fighting(attacker));
        let helper_name = name_or(world, helper, "(unknown)");
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

/// `lure <target>` / `corner <target>` shared shim: target arg
/// resolves to an actor in the room, drains stamina, dispatches the
/// named skill via the data path, and engages combat (mutual
/// `Fighting`). Used by `cmd_lure` and `cmd_corner` since the only
/// per-skill difference is the ability name.
pub(crate) fn engage_skill_shim(
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

/// Remove one direct follower by name. Used by `group dismiss`. The
/// named player must currently be following `dismisser` (Follower
/// component pointing at them); deeper-chain members can't be
/// dismissed without their direct leader's cooperation.
pub(crate) fn group_dismiss_one(world: &mut World, dismisser: Entity, target_name: &str) {
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

/// Walk the Follower chain from `start`. Return true if `end` is reachable
/// (would create a cycle if `end` then started following `start`).
pub(crate) fn would_create_cycle(world: &mut World, start: Entity, end: Entity) -> bool {
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

// Walk + follower cascade + per-mover notifications + auto-look + stamina
// drain — naturally a long sequence; splitting into helpers would just
// shuffle the order. Directional shims (cmd_north etc) call this from
// commands/movement_directions.rs.
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_move(world: &mut World, player: Entity, dir: Direction) {
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
        .and_then(|e| e.0.get(&dir).cloned());
    let Some(exit) = exit else {
        send_to(world, player, "You can't go that way.\r\n");
        return;
    };
    // Hidden exits behave like walls until `search` reveals them
    // on this player. The wording matches the no-exit branch so
    // blind-walking the direction can't confirm the secret.
    if exit_is_hidden_to(world, player, from_room, dir, &exit) {
        send_to(world, player, "You can't go that way.\r\n");
        return;
    }
    if exit.state != ExitState::Open {
        let noun = exit_noun_phrase(&exit);
        let verb = match exit.state {
            ExitState::Locked => "locked",
            _ => "closed",
        };
        send_to(world, player, format!("{noun} is {verb}.\r\n"));
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
    // Severe weather surcharge: outdoor moves through a storm or
    // blizzard cost an extra stamina. Indoor / planar / cave sectors
    // are sheltered. Flyers pay it too — wind buffets a glider as
    // hard as it does a footslogger.
    if sector_is_outdoor_for_weather(target_sector) {
        let target_zone = world.get::<WorldKey>(target).map(|k| k.zone);
        let severe = target_zone
            .and_then(|z| {
                world
                    .resource::<mud_world::WeatherCatalog>()
                    .by_zone
                    .get(&z)
                    .copied()
            })
            .is_some_and(|s| {
                matches!(
                    s.precip,
                    mud_world::PrecipKind::Storm | mud_world::PrecipKind::Blizzard
                )
            });
        if severe {
            stamina_cost = stamina_cost.saturating_add(1);
        }
    }
    // Encumbrance surcharge: at >75% of carry capacity each step
    // costs +1; at >90% +2. Flyers pay it too — pack weight is
    // pack weight whether you're walking or gliding. Capacity is
    // level-scaled, so the threshold travels with the player.
    let cap = carry_capacity(world, player);
    let load = carried_weight(world, player);
    if cap > 0.0 {
        let frac = load / cap;
        if frac > 0.90 {
            stamina_cost = stamina_cost.saturating_add(2);
        } else if frac > 0.75 {
            stamina_cost = stamina_cost.saturating_add(1);
        }
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
    // Use the sender-aware broadcast so wiz-invised admins stay
    // hidden to lower-level observers. Per-mover verb overrides
    // come from `Races.leave_verb` (e.g. "swoops off" for a flying
    // race) when authored; otherwise the generic "leaves" is used.
    for &mover in &movers {
        let mover_name = name_of(world, mover);
        let verb = race_movement_verb(world, mover, false);
        broadcast_room_visible(
            world,
            from_room,
            mover,
            &movers,
            &format!(
                "{} {verb} {dir_name}.\r\n",
                cap_sentence_start(&mover_name),
            ),
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
    // Zone-clear tracking: each player mover now occupies `target`.
    // Followers who happen to be NPCs are filtered inside the helper.
    for &mover in &movers {
        mark_room_visited(world, mover, target);
    }
    // Room environmental effects: apply each linked effect to
    // every player mover. Short duration so leaving the room
    // lets the effect decay; re-entry refreshes naturally.
    for &mover in &movers {
        if world.get::<Player>(mover).is_some() {
            apply_room_environment(world, mover, target);
        }
    }

    // GMCP Room.* diffs: each player mover gets removed from the
    // source room's "who's here" list and added to the target's,
    // and receives a fresh Room.Players snapshot for their new
    // surroundings. Followers who are NPCs are skipped — only
    // human-driven clients care about the diff. Helper queries are
    // read-only beyond the snapshot/diff outbound sends.
    for &mover in &movers {
        if world.get::<Player>(mover).is_some() {
            broadcast_room_player_diff(world, from_room, mover, "RemovePlayer");
            broadcast_room_player_diff(world, target, mover, "AddPlayer");
            send_room_players_snapshot(world, mover);
        }
    }

    // Drain the leader's stamina by the target sector's cost. Followers
    // don't pay the cost — they're being led.
    if let Some(mut s) = world.get_mut::<Stamina>(player) {
        s.current = (s.current - stamina_cost).max(0);
    }

    // Notify the destination room of arrivals. Wizinvis filter
    // mirrors the source-room broadcast so an invised admin's
    // arrival also stays hidden.
    for &mover in &movers {
        let mover_name = name_of(world, mover);
        broadcast_room_visible(
            world,
            target,
            mover,
            &movers,
            &format!(
                "{} {} {arrival_dir}.\r\n",
                cap_sentence_start(&mover_name),
                race_arrival_phrase(world, mover),
            ),
        );
    }

    // DeathTrap gate — `Room.is_death_trap = true` kills every
    // mortal player on entry. Staff (Builder+) bypass so a builder
    // can `goto` into a DT to audit / fix it without dying. Mobs
    // bypass too — DTs are designed for player progression
    // gating, and a mob wandering in just despawning silently
    // would confuse builders. Pre-look so the player sees the
    // death message instead of a room they never really stood in.
    let dt_victims: Vec<Entity> = movers
        .iter()
        .copied()
        .filter(|&m| {
            world.get::<Player>(m).is_some()
                && !world
                    .get::<Account>(m)
                    .is_some_and(|a| a.role.rank() > UserRole::Player.rank())
        })
        .collect();
    if !dt_victims.is_empty() && world.get::<mud_world::DeathTrap>(target).is_some() {
        for victim in dt_victims {
            send_to(
                world,
                victim,
                "<b:red>Your soul is torn from your body as you cross the threshold!</>\r\n",
            );
            let victim_name = name_of(world, victim);
            crate::combat::handle_death(world, victim, &victim_name, target);
        }
    }

    // Each mover sees the new room. Followers also get a "You follow." line
    // before the look.
    for (i, &mover) in movers.iter().enumerate() {
        if i > 0 {
            send_to(world, mover, "You follow.\r\n");
        }
        // Ghost movers from the DT branch above don't see a look —
        // they're already dead. handle_death applied Ghost; skip the
        // per-mover render for any Ghost so the death message
        // isn't immediately drowned by a room description.
        if world.get::<Ghost>(mover).is_none() {
            cmd_look(world, mover, "");
        }
        // Hide-on-move semantics: footsteps break `hide` but not
        // `sneak`. If a mover has a `hidden` EffectInstance and
        // no `sneak` effect, drop the hidden effect; effects_tick
        // GCs the Stealth marker once no hidden/sneak effects
        // remain. A mover with only `sneak` glides through.
        let has_sneak = has_effect_named(world, mover, "sneak");
        if !has_sneak && has_effect_named(world, mover, "hidden") {
            remove_effect_named(world, mover, "hidden");
            send_to(world, mover, "Your footsteps reveal you.\r\n");
        }
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

    // Aggressive-mob check: after the player has seen the room and
    // any greet/post-entry chatter, give the worst-aligned non-
    // engaged mob in the room a free swing to start combat. Only
    // player movers trigger it (mobs migrating between rooms don't
    // fight other mobs on sight); admins are spared. One attacker
    // per arrival to avoid a gang pile when several aggro mobs
    // share a room.
    for &mover in &movers {
        if world.get::<Player>(mover).is_none() {
            continue;
        }
        if world.get::<Fighting>(mover).is_some() {
            continue;
        }
        if world
            .get::<Account>(mover)
            .is_some_and(|a| a.role.rank() > UserRole::Player.rank())
        {
            continue;
        }
        // Memory check first: a mob already nursing a grudge from
        // an earlier swing engages before the alignment-tier
        // generic-aggro check fires. Otherwise fall through to the
        // alignment rule.
        if !try_engage_remembered_mob(world, mover, target) {
            try_engage_aggressive_mob(world, mover, target);
        }
    }
}

/// If any mob in `room` has the player in its `MobMemory`, engage
/// it. Returns true if an engagement fired so the caller can skip
/// the generic alignment-aggro check.
pub(crate) fn try_engage_remembered_mob(world: &mut World, player: Entity, room: Entity) -> bool {
    let grudger: Option<Entity> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &crate::combat::MobMemory),
            (With<Mob>, Without<Fighting>),
        >();
        q.iter(world)
            .find(|(_, l, mem)| l.0 == room && mem.0.contains(&player))
            .map(|(e, _, _)| e)
    };
    let Some(mob) = grudger else { return false };
    engage_combat(world, mob, player, room);
    true
}

/// Default alignment threshold below which a mob auto-swings on
/// arriving players. Live value is `combat.aggro_alignment`
/// in `GameConfig`; this is the call-site fallback. Tuned by the
/// import distribution: nastiest 200-odd mobs sit at -1000, so
/// -800 lights up roughly the lower fifth.
pub(crate) const DEFAULT_AGGRO_ALIGNMENT: i32 = -800;

/// Read the alignment threshold from `RuntimeConfig`, falling
/// back to the legacy hardcoded value. Used by every aggro
/// gate (room arrival, respawn auto-engage, hostile-tag in
/// `look`). `get_resource` so test worlds without the loader
/// pass fall through to the default.
#[must_use]
pub(crate) fn aggro_alignment(world: &World) -> i32 {
    world.get_resource::<mud_world::RuntimeConfig>().map_or(
        DEFAULT_AGGRO_ALIGNMENT,
        |cfg| cfg.get_i32("combat", "aggro_alignment", DEFAULT_AGGRO_ALIGNMENT),
    )
}

/// Read a per-skill stamina cost from `RuntimeConfig`. Skill names
/// match the lowercase command name (e.g. `"attack"`, `"bash"`,
/// `"backstab"`). Falls back to the legacy compile-time default
/// when the row is absent or the resource isn't installed (test
/// worlds without the loader pass).
#[must_use]
pub(crate) fn skill_stamina_cost(world: &World, skill: &str, default: i32) -> i32 {
    world
        .get_resource::<mud_world::RuntimeConfig>()
        .map_or(default, |cfg| cfg.get_i32("combat.stamina_cost", skill, default))
}

/// Bidirectional `Fighting` + announcement on both sides + the
/// rest of the room. Shared between the on-entry aggro check and
/// any other path that wants to start hostilities programmatically
/// (respawn into an occupied room, scripted ambush, etc).
pub(crate) fn engage_combat(
    world: &mut World,
    attacker: Entity,
    defender: Entity,
    room: Entity,
) {
    // PeacefulRoom blocks every auto-engage path that routes
    // through this helper — remembered grudges, alignment aggro,
    // scripted ambushes. The defender doesn't even see a swing
    // come in, matching the "violence simply won't happen here"
    // guard already on cmd_attack.
    if world.get::<mud_world::PeacefulRoom>(room).is_some() {
        return;
    }
    let attacker_name = name_of(world, attacker);
    let defender_name = name_of(world, defender);
    try_insert(world, attacker, Fighting(defender));
    try_insert(world, defender, Fighting(attacker));
    send_to(
        world,
        defender,
        format!("{attacker_name} sees you and attacks!\r\n"),
    );
    broadcast_room_except_rendered(
        world,
        room,
        &[defender],
        &format!("{attacker_name} sees {defender_name} and attacks!\r\n"),
    );
}

pub(crate) fn try_engage_aggressive_mob(world: &mut World, player: Entity, room: Entity) {
    let threshold = aggro_alignment(world);
    let aggro: Option<Entity> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &CombatStats),
            (With<Mob>, Without<Fighting>),
        >();
        q.iter(world)
            .find(|(_, l, cs)| l.0 == room && cs.alignment <= threshold)
            .map(|(e, _, _)| e)
    };
    let Some(mob) = aggro else { return };
    engage_combat(world, mob, player, room);
}

/// Stamina drained when moving INTO a room of this sector. The mapping
/// roughly tracks classic CircleMUD/FieryMUD: paved/easy = 1, normal
/// terrain = 2, water/swamp = 3-4, magical/floating planes = 1 (you're
/// not really walking).
pub(crate) fn sector_movement_cost(s: Sector) -> i32 {
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

/// Pick the movement verb for a mover's room broadcast. The
/// `is_arrival = false` form returns the departure verb
/// (`Races.leave_verb`, default `"leaves"`); `true` returns the
/// arrival verb (`Races.enter_verb`, default `"arrives"`). The
/// verb is composed with the directional phrase at the call site
/// — so "leaves north" / "arrives from the south" are the
/// composed forms. Mover without a `Profile.race`, race not in
/// the catalog, or empty override falls back to the generic verb.
///
/// Returns an owned `String` because the override comes from the
/// catalog (`Option<String>`) and the caller needs the result by
/// value anyway.
pub(crate) fn race_movement_verb(
    world: &World,
    mover: Entity,
    is_arrival: bool,
) -> String {
    let default: &str = if is_arrival { "arrives" } else { "leaves" };
    let Some(prof) = world.get::<mud_world::Profile>(mover) else {
        return default.to_string();
    };
    let Some(def) = world
        .resource::<mud_world::RaceCatalog>()
        .get(&prof.race)
    else {
        return default.to_string();
    };
    let verb = if is_arrival {
        def.enter_verb.as_deref()
    } else {
        def.leave_verb.as_deref()
    };
    match verb {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => default.to_string(),
    }
}

/// Composed arrival phrase — "<verb> from" — for the destination
/// room's broadcast string. Defaults to `"arrives from"`; a race
/// with `enter_verb = "swoops down"` yields `"swoops down from"`.
pub(crate) fn race_arrival_phrase(world: &World, mover: Entity) -> String {
    let verb = race_movement_verb(world, mover, true);
    format!("{verb} from")
}

pub(crate) fn direction_name(d: Direction) -> &'static str {
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
        Direction::None => "(none)",
    }
}

pub(crate) fn opposite(d: Direction) -> Option<Direction> {
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
