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
    Follower, Frozen, Health, Item, Keywords, KnownAbilities, LastInputAt, Located, LoggedInAt,
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
pub struct PlayerUpdateTx(pub tokio::sync::mpsc::UnboundedSender<PendingPlayerUpdate>);

/// Receiver side, drained once per tick by `drain_player_updates`.
#[derive(Resource)]
pub struct PlayerUpdateInbox(
    pub std::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<PendingPlayerUpdate>>,
);

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
#[path = "commands/info.rs"]
mod info;
pub(crate) use info::cmd_look;
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
/// that ship pre-rendered ANSI.
pub(crate) fn send_raw(world: &World, target: Entity, text: impl Into<String>) {
    if let Some(conn) = world.get::<Connection>(target) {
        let _ = conn.0.send(text.into().into_bytes());
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
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '<' {
            count += 1;
            continue;
        }
        // Buffer up to the matching `>` (or end). If the buffered
        // content isn't tag-shaped, we should have counted the `<`
        // and the buffered chars and the `>` as visible. Mirrors
        // render_color_tags' "literal text" fallback so the two
        // functions agree on what counts as printable.
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
            // Unterminated `<`: render_color_tags drops the rest;
            // we mirror that and stop counting here.
            break;
        }
        if !is_tag_shaped(&tag) {
            // Literal `<...>` — counts as visible (`<`, body, `>`).
            count += 2 + tag.chars().count();
        }
        // Tag-shaped: contributes 0 visible chars.
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
    if let Some(rest) = p.strip_prefix('#') {
        return rest.len() == 6 && rest.bytes().all(|b| b.is_ascii_hexdigit());
    }
    named_color(p).is_some()
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
        ColorMode, PromptCtx, amount_from_blob, apply_damage, apply_heal_hp, apply_heal_stamina,
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
        // Unterminated `<` truncates (matches render_color_tags).
        assert_eq!(super::visible_width("hi <b:yellow"), 3);
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

    #[test]
    fn render_color_tags_strip_mode_matches_legacy() {
        // No tags: identity.
        assert_eq!(strip("plain text"), "plain text");
        // Single tag pair.
        assert_eq!(strip("<red>red</>"), "red");
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
    let clock = world.get_resource::<mud_world::MudClock>();
    let hour = clock.map(|c| c.hour);
    let season = clock.map(|c| c.season().label());
    let day_night = hour.map(|h| if matches!(h, 0..=4 | 22..=23) { "night" } else { "day" });
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
        },
    );
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
        let _ = conn.0.send(mud_net::gmcp_packet("Char.Status", &payload));
    }
    // Room.Info: lightweight room metadata for Mudlet-style
    // mappers. Same prompt cadence as Char.Vitals; per-prompt
    // re-emit is cheap (one telnet frame, ~80 bytes) and lets
    // the client refresh on any view change. Room name is
    // sanitized of XML-Lite tags so the JSON stays well-formed.
    if let Some(located) = world.get::<Located>(target) {
        let room = located.0;
        let room_name = world
            .get::<Named>(room)
            .map_or_else(String::new, |n| n.name.clone());
        let plain = render_color_tags(&room_name, ColorMode::Strip)
            .replace('"', "\\\"")
            .replace('\\', "\\\\");
        let (zone, id) = world
            .get::<WorldKey>(room)
            .map_or((-1, -1), |k| (k.zone, k.id));
        let exits: Vec<&'static str> = world
            .get::<Exits>(room)
            .map(|e| {
                e.0.keys()
                    .copied()
                    .map(direction_name)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let exits_json = exits
            .iter()
            .map(|e| format!("\"{e}\""))
            .collect::<Vec<_>>()
            .join(",");
        let payload = format!(
            "{{\"name\":\"{plain}\",\"zone\":{zone},\"id\":{id},\"exits\":[{exits_json}]}}"
        );
        let _ = conn.0.send(mud_net::gmcp_packet("Room.Info", &payload));
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
/// credit.
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
                            let _ = out.send(
                                format!("Quest phase complete — moving to: {name}\r\n")
                                    .into_bytes(),
                            );
                        }
                        Ok(mud_db::quest_objectives::PhaseAdvance::QuestComplete) => {
                            let _ = out.send(
                                b"*** Quest complete! ***\r\n".to_vec(),
                            );
                            // Grant simple rewards (XP/gold/skill
                            // points/ability) via DB; announce all
                            // including ITEM/HOUSING which the
                            // questgiver still needs to hand out.
                            let rewards = mud_db::quest_objectives::list_quest_rewards(
                                &pool,
                                row.quest_zone_id,
                                row.quest_id,
                            )
                            .await
                            .unwrap_or_default();
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
                                        let _ = tx.send(u);
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
                                    let _ = out.send(buf.into_bytes());
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
                let _ = out.send(line.into_bytes());
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
    entity: Entity,
    name: String,
    title: Option<String>,
    afk: bool,
    idle: Option<u64>,
    level: i32,
    clan_abbrev: Option<String>,
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

/// Bundle of all the data the `score` renderers consume. Building it once
/// in `cmd_score` avoids re-querying components per render variant and
/// keeps the renderer signatures from blowing past clippy's
/// `too_many_arguments` threshold.
pub(crate) struct ScoreData<'a> {
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
}

pub(crate) fn render_score_standard(d: &ScoreData) -> String {
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
    if d.carry.0 > 0.0 {
        out.push_str(&format!(
            "  Load:   {:.1} / {:.0} lbs.\r\n",
            d.carry.0, d.carry.1,
        ));
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
    if let Some(c) = condition_summary(d.hunger, d.thirst) {
        out.push_str(&format!("  Condition: {c}\r\n"));
    }
    if d.drunkenness > 0 {
        out.push_str(&format!("  Drunk:  {} / 100\r\n", d.drunkenness));
    }
    if d.kill_total > 0 {
        out.push_str(&format!("  Kills:  {}\r\n", d.kill_total));
    }
    if let Some((name, abbrev, rank)) = d.clan {
        out.push_str(&format!("  Clan:   {name} [{abbrev}] ({rank})\r\n"));
    }
    out
}

/// Comma-joined hunger/thirst descriptors, or None if both are
/// below their warning thresholds. Bands match the tick consumer's
/// `HUNGRY_AT` / `STARVING_AT` / `THIRSTY_AT` / `PARCHED_AT`.
pub(crate) fn condition_summary(hunger: i32, thirst: i32) -> Option<String> {
    let mut parts = Vec::new();
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
    if d.carry.0 > 0.0 {
        row(format!("Load:      {:.1} / {:.0} lbs.", d.carry.0, d.carry.1));
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
    if let Some(c) = condition_summary(d.hunger, d.thirst) {
        row(format!("Condition: {c}"));
    }
    if d.drunkenness > 0 {
        row(format!("Drunk:     {} / 100", d.drunkenness));
    }
    if d.kill_total > 0 {
        row(format!("Kills:     {}", d.kill_total));
    }
    if let Some((name, abbrev, rank)) = d.clan {
        row(format!("Clan:      {name} [{abbrev}] ({rank})"));
    }
    out.push_str(&format!("+{}+\r\n", "-".repeat(W)));
    out
}

pub(crate) fn render_score_minimal(d: &ScoreData) -> String {
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
    if d.carry.0 > 0.0 {
        parts.push(format!("ld:{:.0}/{:.0}", d.carry.0, d.carry.1));
    }
    if let Some(target) = d.fight_target {
        parts.push(format!("vs:{target}"));
    }
    if let Some(c) = condition_summary(d.hunger, d.thirst) {
        parts.push(c);
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

/// True when the player's room is currently dark enough that
/// nothing can be seen without a light source. Cases: sector is
/// intrinsically dark (CAVE / UNDERDARK / UNDERWATER), or sector
/// is outdoor (sky-visible) AND it's nighttime (game hour 22..05).
/// Caller checks for any `Lit` item carried by anyone in the room
/// (player or mob) before declaring the player blind — torches /
/// lanterns / luminous-glow items still work.
pub(crate) fn room_is_dark(world: &World, room: Entity) -> bool {
    let Some(sector) = world.get::<RoomSector>(room).map(|s| s.0) else {
        return false;
    };
    if matches!(sector, Sector::Cave | Sector::Underdark | Sector::Underwater) {
        return true;
    }
    if !sector_is_outdoor_for_weather(sector) {
        return false;
    }
    let hour = world.resource::<mud_world::MudClock>().hour;
    matches!(hour, 0..=4 | 22..=23)
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
        send_to(world, player, "The way is closed.\r\n");
        return;
    }
    let Some(target_room) = ed.to else {
        send_to(world, player, "The way fades into the unknown.\r\n");
        return;
    };
    // Mirror the dark-room gate from cmd_look's home-room render —
    // peeking into a black cave from a lit corridor reveals nothing
    // either. Source-room lighting doesn't bleed through the doorway.
    if room_is_dark(world, target_room) && !room_has_light(world, target_room) {
        send_to(
            world,
            player,
            format!("\r\nYou peer {} but see only blackness.\r\n", direction_name(dir)),
        );
        return;
    }
    let name = name_or(world, target_room, "(unknown)");
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
    // Drunkenness: per-unit alcohol contribution from the
    // schema's `Liquids.drunk_effect`. 0 for non-alcoholic drinks
    // = no-op. Capped at 100 — anything beyond is "blackout"
    // (future pass: penalize skills + slur speech).
    let drunk_per_unit = world
        .resource::<mud_world::LiquidIndex>()
        .drunk_effect
        .get(&state.liquid.to_ascii_lowercase())
        .copied()
        .unwrap_or(0);
    let drunk_gain = drunk_per_unit.saturating_mul(drank);
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
    // Reset thirst proportional to swig size — `sip` (1 unit) takes
    // a small bite out, `drink` (4 units) sates entirely. Clamps at
    // 0 (never goes negative).
    if let Some(mut t) = world.get_mut::<mud_world::Thirst>(player) {
        t.0 = (t.0 - drank.saturating_mul(6)).max(0);
    }
    let was_last = state.remaining == drank;
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

    // Alignment + class restrictions: refuse if the proto's
    // restriction list contains the player's bucket. Lookup is
    // by WorldKey → ObjectPrototypes; items without a proto
    // (corpses, dynamically synthesized) skip both checks.
    let (alignment_restriction, class_restriction, race_restriction) = world
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
        .unwrap_or_default();
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

pub(crate) fn find_carried_by(
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

pub(crate) fn find_in_room(world: &mut World, needle: &str, room: Entity) -> Option<Entity> {
    let needle = needle.to_ascii_lowercase();
    let mut q = world.query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
    q.iter(world)
        .find(|(_, l, n, kw)| l.0 == room && matches(&needle, n, *kw))
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
    let needle = needle.to_ascii_lowercase();
    let mut q = world.query::<(Entity, &Located, &Named, Option<&Keywords>, Option<&Item>)>();
    q.iter(world)
        .find(|(e, l, n, kw, item)| {
            *e != exclude && l.0 == room && item.is_none() && matches(&needle, n, *kw)
        })
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

/// Resolve a spell name to (`ability_id`, circle) for the player's
/// class. Returns Err with a player-facing message on failure.
pub(crate) fn resolve_spell_for_class(
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
                    def.plain_name,
                ),
            );
            return;
        }
    }

    let mode = color_mode_for(world, player);
    let mut out = String::from("\r\n");
    if !aoe_repeat {
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
    // Resolve the target. Empty / "me" / "self" → the caster.
    // Anything else → if the ability's targeting list includes
    // OBJECT_INV, look up a carried item by keyword first
    // (covers `cast identify brooch` and friends); otherwise fall
    // through to actor-in-room. If nothing resolves, abort before
    // applying any effects.
    let allows_inventory_target = world
        .resource::<AbilityCatalog>()
        .targeting
        .get(&def.id)
        .is_some_and(|r| {
            r.valid_targets
                .iter()
                .any(|t| t.eq_ignore_ascii_case("OBJECT_INV"))
        });
    let target_entity = if let Some(word) = target_word
        && !word.eq_ignore_ascii_case("me")
        && !word.eq_ignore_ascii_case("self")
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
    } else {
        player
    };
    if target_entity == player {
        out.push_str("    target: yourself\r\n");
    } else {
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
    let caster_skill = world
        .get::<KnownAbilities>(player)
        .and_then(|k| k.entries.iter().find(|(id, _, _)| *id == def.id).map(|(_, p, _)| *p))
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
                    // Reagent boost on damage spells.
                    if reagent_boost_pct > 0 {
                        amount = amount.saturating_add(amount * reagent_boost_pct / 100);
                    }
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

pub(crate) fn apply_heal_hp(world: &mut World, target: Entity, amount: i32) -> i32 {
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
pub(crate) fn apply_heal_stamina(world: &mut World, target: Entity, amount: i32) -> i32 {
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

pub(crate) fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
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

/// Alignment threshold below which a mob will swing on a player
/// who walks into the room (or, on respawn, lands in a room with
/// a player already there). Tuned by the import distribution: the
/// nastiest two-hundred-odd mobs sit at -1000, so -800 lights up
/// roughly the lower fifth — enough to make low-zone exploration
/// have teeth without making every wandering goblin hostile.
pub(crate) const AGGRO_ALIGNMENT: i32 = -800;

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
    let aggro: Option<Entity> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &CombatStats),
            (With<Mob>, Without<Fighting>),
        >();
        q.iter(world)
            .find(|(_, l, cs)| l.0 == room && cs.alignment <= AGGRO_ALIGNMENT)
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
