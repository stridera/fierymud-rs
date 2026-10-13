//! HTTP admin/MCP control endpoint.
//!
//! Mirrors the C++ `FieryMUD` `/api/admin/*` surface so the existing
//! TypeScript MCP server (`fierymud-mcp`) can drive the Rust process
//! unchanged. Listens on `127.0.0.1:8080` by default; override via
//! `ADMIN_LISTEN_ADDR`. Optional bearer token via `ADMIN_TOKEN`.
//!
//! Architecture: HTTP handlers translate each request to an
//! [`AdminRequest`] and post it through an mpsc channel. The world
//! tick drains the channel each frame via [`drain_admin_requests`],
//! services the request synchronously against the live `World`, and
//! replies through a oneshot. This keeps `World` single-threaded
//! (matching the rest of the runtime) without requiring `Send`/`Sync`
//! on bevy resources beyond the channel itself.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use bevy_ecs::prelude::*;
use mud_db::{
    character_items, characters, characters::CharacterRow, sqlx::PgPool, users, users::User,
};
use mud_net::Outbound;
use mud_world::{
    Account, ClassCatalog, CombatStats, EffectInstance, Exits, Health, Item, LastInputAt, Located,
    LoggedInAt, Mob, MobPrototypes, Named, ObjectPrototypes, Online, Player, Posture, Profile,
    Stamina, TriggerCatalog, WorldKey, WorldKeyIndex,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::commands::{self, name_of};

/// Where the world tick reads pending admin requests from. Installed
/// as a resource at boot.
#[derive(Resource)]
pub struct AdminInbox(pub Mutex<mpsc::Receiver<AdminCommand>>);

/// A heavy request pulled off the inbox while the tick had already done
/// other admin work: it waits here and runs first thing next tick, alone.
/// Created on demand by the drain, so boot code needs no extra wiring.
#[derive(Resource, Default)]
struct AdminDeferred(Mutex<Option<AdminCommand>>);

/// A command whose database work is still running off the tick when its
/// dispatch returned (see `commands::off_tick`).
struct Waiting {
    executor: String,
    entity: Entity,
    command: String,
    pending_output: String,
    deadline: std::time::Instant,
    /// The line itself went into the session's input queue (an earlier
    /// command's off-tick work was still out), so it has not run yet.
    own_line_queued: bool,
}

/// A [`Waiting`] command plus the HTTP request to answer once it settles.
struct Parked {
    waiting: Waiting,
    reply: oneshot::Sender<AdminResponse>,
}

/// Commands waiting for their session's off-tick work; answered by
/// [`answer_parked`] at the start of a tick. The tick never waits for them.
#[derive(Resource, Default)]
struct AdminParked(Mutex<Vec<Parked>>);

enum CommandRun {
    Done(AdminResponse),
    Waiting(Waiting),
}

/// Cap for the admin command channel. The control plane is shared-
/// secret only and very low-traffic; 256 in-flight commands is
/// generous. On overflow `enqueue` returns 503 — caller can retry.
pub const ADMIN_QUEUE_CAP: usize = 256;

/// Pause state. While `paused`, the main tick loop skips the
/// gameplay schedule (combat, effects, regen, mob AI, respawn,
/// Lua coroutines). The admin inbox keeps draining so unpause /
/// tick / status calls still go through. `forced_ticks` lets a
/// paused world step a fixed number of frames for deterministic
/// testing — set by `POST /api/admin/world/tick`.
#[derive(Resource, Default)]
pub struct WorldPause {
    pub paused: bool,
    pub forced_ticks: u32,
}

/// Active virtual sessions keyed by player name. Each holds the
/// spawned Player entity plus the receiver that captures its
/// outbound bytes — drained at the end of each `execute_command`
/// to return command output to the caller.
#[derive(Resource, Default)]
pub struct VirtualSessions {
    pub by_name: Mutex<HashMap<String, VirtualSession>>,
}

pub struct VirtualSession {
    pub entity: Entity,
    pub rx: mpsc::Receiver<Vec<u8>>,
}

/// One queued HTTP request pending world dispatch. The handler
/// awaits the oneshot to formulate its HTTP response.
pub struct AdminCommand {
    pub request: AdminRequest,
    pub reply: oneshot::Sender<AdminResponse>,
}

#[allow(clippy::large_enum_variant)]
pub enum AdminRequest {
    WorldStatus,
    Players,
    LookRoom {
        zone_id: i32,
        id: i32,
    },
    InspectActor {
        name: String,
    },
    SessionCreate {
        player_name: String,
        user: Box<User>,
        character: Box<CharacterRow>,
        items: Vec<character_items::CharacterItemRow>,
        abilities: Vec<mud_db::character_abilities::CharacterAbilityRow>,
        aliases: Vec<mud_db::character_aliases::CharacterAliasRow>,
        achievements: Vec<mud_db::achievements::CharacterAchievementRow>,
        script_vars_json: Option<serde_json::Value>,
        trophy_json: Option<serde_json::Value>,
        spell_cooldowns_json: Option<serde_json::Value>,
        cooldowns_json: Option<serde_json::Value>,
        ignore_list_json: Option<serde_json::Value>,
        command_grants_json: Option<serde_json::Value>,
        effect_instances_json: Option<serde_json::Value>,
    },
    SessionDestroy {
        player_name: String,
    },
    /// Mark an online (or virtual-session) player with `PendingSave`
    /// so the post-tick autosave loop checkpoints them within the
    /// next tick. Used by smoke tests to force the disconnect-save
    /// path without actually disconnecting; not part of the regular
    /// gameplay surface.
    MarkPendingSave {
        player_name: String,
    },
    Command {
        executor: String,
        command: String,
    },
    Teleport {
        player_name: String,
        zone_id: i32,
        room_id: i32,
    },
    Spawn {
        kind: String,
        zone_id: i32,
        id: i32,
        room_zone: i32,
        room_id: i32,
    },
    InspectMob {
        zone_id: i32,
        id: i32,
    },
    PauseWorld,
    UnpauseWorld,
    TickWorld {
        count: u32,
    },
    TriggerInfo {
        zone_id: Option<i32>,
        id: Option<i32>,
    },
    TriggerErrors {
        limit: Option<usize>,
    },
    TriggerStats,
    /// Syntax-check every loaded trigger body (optionally one zone)
    /// without running it. Backs `GET /api/admin/triggers/validate`.
    ValidateTriggers {
        zone_id: Option<i32>,
    },
    /// New catalog assembled from the DB by the HTTP handler;
    /// the world dispatch swaps the resource and re-applies room
    /// attachments. Mob/object spawns will pick up new trigger
    /// rows on their next respawn naturally.
    ReloadTriggers {
        catalog: Box<mud_world::TriggerCatalog>,
    },
    /// Manually invoke a trigger body against a chosen `self`
    /// entity, optionally with an `actor` binding. Bypasses the
    /// usual event-flag gating — you can fire any body regardless
    /// of whether the trigger has the matching event flag set.
    FireTrigger {
        zone_id: i32,
        id: i32,
        self_name: String,
        actor_name: Option<String>,
    },
    /// Write a typed integer attribute on a player or mob entity.
    /// Field name is checked against an allowlist server-side so
    /// a typo can't silently no-op.
    SetPlayerField {
        player_name: String,
        field: String,
        value: i64,
    },
}

pub type AdminResponse = Result<Value, (StatusCode, String)>;

#[derive(Clone)]
struct AppState {
    tx: mpsc::Sender<AdminCommand>,
    token: Option<Arc<String>>,
    pool: PgPool,
}

/// Spawn the HTTP listener on `ADMIN_LISTEN_ADDR` (default
/// `127.0.0.1:8080`; `ADMIN_PORT` overrides just the port). Returns the
/// effective bind address (`None` when refused) and the inbox receiver so the caller
/// can install it as a bevy resource. The pool is used by handlers
/// that need DB access (e.g. `session/create` loads the character
/// and its inventory before handing off to the world tick).
///
/// Auth policy: `ADMIN_TOKEN` is required when binding to a non-loopback
/// address; production deploys that set `ADMIN_LISTEN_ADDR=0.0.0.0:...`
/// without a token are refused and the listener is not started. To
/// override for a one-off dev run on a non-loopback bind (e.g. testing
/// from another machine on a trusted LAN), set
/// `ADMIN_ALLOW_UNAUTH_LOCAL=true` — explicit and easy to grep for in
/// production logs.
pub fn spawn_admin_server(pool: PgPool) -> (mpsc::Receiver<AdminCommand>, Option<SocketAddr>) {
    let (tx, rx) = mpsc::channel::<AdminCommand>(ADMIN_QUEUE_CAP);
    let addr = crate::envflags::resolve_admin_addr(
        crate::envflags::port_from_env("ADMIN_PORT"),
        std::env::var("ADMIN_LISTEN_ADDR").ok().as_deref(),
    );
    // An empty / whitespace-only ADMIN_TOKEN is "unset", never a valid
    // secret (an empty expected token would match a request with no header).
    let token = crate::envflags::non_blank(std::env::var("ADMIN_TOKEN").ok()).map(Arc::new);
    // The unauthenticated-bind override is a dev convenience; production
    // never honours it (startup already requires a token there).
    let allow_unauth_local = !crate::envflags::is_production()
        && std::env::var("ADMIN_ALLOW_UNAUTH_LOCAL").is_ok_and(|v| crate::envflags::is_truthy(&v));
    if token.is_none() && !addr.ip().is_loopback() && !allow_unauth_local {
        warn!(
            %addr,
            "admin HTTP refused: non-loopback bind without ADMIN_TOKEN. \
             Set ADMIN_TOKEN, or ADMIN_ALLOW_UNAUTH_LOCAL=true to bypass (dev only)."
        );
        return (rx, None);
    }
    if token.is_none() {
        warn!(%addr, "admin HTTP API starting WITHOUT ADMIN_TOKEN: all /api/admin/* endpoints are unauthenticated");
    }
    let state = AppState { tx, token, pool };
    tokio::spawn(async move {
        let app = build_router(state);
        info!(%addr, "admin HTTP listener configured");
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                if let Err(e) = axum::serve(listener, app).await {
                    warn!(error = %e, "admin HTTP server stopped");
                }
            }
            Err(e) => warn!(%addr, error = %e, "admin HTTP listener bind failed"),
        }
    });
    (rx, Some(addr))
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/admin/world/status", get(handle_world_status))
        .route("/api/admin/players", get(handle_players))
        .route("/api/admin/room/{zone_id}/{id}", get(handle_look_room))
        .route("/api/admin/actor/{name}", get(handle_inspect_actor))
        .route("/api/admin/mob/{zone_id}/{id}", get(handle_inspect_mob))
        .route("/api/admin/session/create", post(handle_session_create))
        .route("/api/admin/session/destroy", post(handle_session_destroy))
        .route("/api/admin/player/save", post(handle_player_save))
        .route("/api/admin/command", post(handle_command))
        .route("/api/admin/teleport", post(handle_teleport))
        .route("/api/admin/spawn", post(handle_spawn))
        .route("/api/admin/world/pause", post(handle_pause_world))
        .route("/api/admin/world/unpause", post(handle_unpause_world))
        .route("/api/admin/world/tick", post(handle_tick_world))
        .route("/api/admin/triggers", get(handle_trigger_info))
        .route(
            "/api/admin/triggers/{zone_id}/{id}",
            get(handle_trigger_info_one),
        )
        .route("/api/admin/triggers/errors", get(handle_trigger_errors))
        .route("/api/admin/triggers/stats", get(handle_trigger_stats))
        .route("/api/admin/triggers/validate", get(handle_trigger_validate))
        .route("/api/admin/triggers/reload", post(handle_trigger_reload))
        .route("/api/admin/triggers/fire", post(handle_trigger_fire))
        .route("/api/admin/player/set", post(handle_player_set))
        // 64 KiB cap on admin request bodies. Axum's default is 2 MiB,
        // generous for a shared-secret control plane that only ever
        // receives small JSON. Tighter cap means a flooding caller
        // can't pin server memory through a single in-flight request.
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
}

/// Constant-time byte-slice equality. Returns true iff the slices have
/// the same length and all bytes match, without short-circuiting on
/// the first mismatch. Used for bearer-token comparison so an attacker
/// can't time-side-channel the prefix.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    check_bearer(state.token.as_deref().map(String::as_str), headers)
}

/// True when the `Host` header names the loopback interface
/// (`127.0.0.1`, `localhost` or `[::1]`, with an optional port). A web page
/// on another origin that rebinds its DNS name to 127.0.0.1 still sends its
/// own name as `Host`, so this rejects DNS-rebinding requests.
fn host_is_loopback(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let name = if let Some(rest) = host.strip_prefix('[') {
        // Bracketed IPv6 literal: `[::1]` or `[::1]:8080`.
        match rest.split_once(']') {
            Some((addr, tail)) if tail.is_empty() || tail.starts_with(':') => {
                return addr == "::1" && port_ok(tail.strip_prefix(':'));
            }
            _ => return false,
        }
    } else {
        match host.split_once(':') {
            Some((name, port)) => {
                if !port_ok(Some(port)) {
                    return false;
                }
                name
            }
            None => host,
        }
    };
    name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost")
}

/// An absent port, or a non-empty all-digit one.
fn port_ok(port: Option<&str>) -> bool {
    port.is_none_or(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `expected == None` means no token is configured (loopback-only dev
/// run): requests pass only when their `Host` header is a loopback name,
/// so DNS rebinding cannot turn a visitor's browser into an admin client.
/// A configured-but-blank token can never authenticate anyone; it is
/// rejected outright.
fn check_bearer(expected: Option<&str>, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let Some(expected) = expected else {
        return if host_is_loopback(headers) {
            Ok(())
        } else {
            Err((
                StatusCode::FORBIDDEN,
                "admin API without ADMIN_TOKEN only answers to Host 127.0.0.1 or localhost".into(),
            ))
        };
    };
    if expected.trim().is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "missing or invalid bearer token".into(),
        ));
    }
    let got = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if constant_time_eq(got.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            "missing or invalid bearer token".into(),
        ))
    }
}

async fn enqueue(state: &AppState, request: AdminRequest) -> Result<Value, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();
    // try_send instead of .send().await — admin requests are
    // synchronous from the caller's perspective, and a saturated
    // queue means the world tick is already overloaded; better to
    // 503 immediately than have the HTTP request hang on backpressure.
    state
        .tx
        .try_send(AdminCommand { request, reply: tx })
        .map_err(|e| match e {
            tokio::sync::mpsc::error::TrySendError::Full(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "admin command queue full; world tick saturated".into(),
            ),
            tokio::sync::mpsc::error::TrySendError::Closed(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "world tick channel closed".into(),
            ),
        })?;
    rx.await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "no reply from world tick".into(),
        )
    })?
}

async fn handle_world_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(enqueue(&state, AdminRequest::WorldStatus).await)
}

async fn handle_players(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(enqueue(&state, AdminRequest::Players).await)
}

async fn handle_look_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((zone_id, id)): Path<(i32, i32)>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(enqueue(&state, AdminRequest::LookRoom { zone_id, id }).await)
}

async fn handle_inspect_actor(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(enqueue(&state, AdminRequest::InspectActor { name }).await)
}

async fn handle_inspect_mob(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((zone_id, id)): Path<(i32, i32)>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(enqueue(&state, AdminRequest::InspectMob { zone_id, id }).await)
}

#[derive(Deserialize)]
struct SessionBody {
    player_name: String,
}

async fn handle_session_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<SessionBody>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    // DB lookups happen here in async context; the world tick is
    // sync and can't await. We hand the loaded character/user/items
    // to the tick via the request enum.
    let character = match characters::find_by_name(&state.pool, &body.player_name).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return json_err((
                StatusCode::NOT_FOUND,
                format!("no character named '{}'", body.player_name),
            ));
        }
        Err(e) => return json_err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    let Some(user_id) = character.user_id.clone() else {
        return json_err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("character '{}' has no user_id", body.player_name),
        ));
    };
    let user = match users::find_by_id(&state.pool, &user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return json_err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "character '{}' references missing user_id",
                    body.player_name
                ),
            ));
        }
        Err(e) => return json_err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    let items = character_items::list_for(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    let mut abilities = mud_db::character_abilities::list_for(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    // Same race-innate grant as a telnet login so virtual sessions match.
    if let Ok(innates) = mud_db::race_abilities::list_for_race(&state.pool, &character.race).await {
        mud_db::race_abilities::merge_innates(&mut abilities, &innates);
    }
    let aliases = mud_db::character_aliases::list_for(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    let achievements = mud_db::achievements::unlocked_for(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    let script_vars_json = characters::load_script_vars(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    let trophy_json = characters::load_trophy(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    let spell_cooldowns_json = characters::load_spell_cooldowns(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    let cooldowns_json = characters::load_cooldowns(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    let ignore_list_json = characters::load_ignore_list(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    let command_grants_json = characters::load_command_grants(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    let effect_instances_json = characters::load_effect_instances(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    json_ok(
        enqueue(
            &state,
            AdminRequest::SessionCreate {
                player_name: body.player_name,
                user: Box::new(user),
                character: Box::new(character),
                items,
                abilities,
                aliases,
                achievements,
                script_vars_json,
                trophy_json,
                spell_cooldowns_json,
                cooldowns_json,
                ignore_list_json,
                command_grants_json,
                effect_instances_json,
            },
        )
        .await,
    )
}

async fn handle_session_destroy(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<SessionBody>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(
        enqueue(
            &state,
            AdminRequest::SessionDestroy {
                player_name: body.player_name,
            },
        )
        .await,
    )
}

async fn handle_player_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<SessionBody>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(
        enqueue(
            &state,
            AdminRequest::MarkPendingSave {
                player_name: body.player_name,
            },
        )
        .await,
    )
}

#[derive(Deserialize)]
struct CommandBody {
    executor: String,
    command: String,
}

// MCP clients sometimes send numeric fields as JSON strings
// (the MCP framework's JSON-schema-based marshalling is lenient
// here). Accept either int literal or numeric string.
fn de_i32_lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i32, D::Error> {
    use serde::Deserialize as _;
    match Value::deserialize(d)? {
        Value::Number(n) => n
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| serde::de::Error::custom("number out of range for i32")),
        Value::String(s) => s
            .parse::<i32>()
            .map_err(|e| serde::de::Error::custom(e.to_string())),
        other => Err(serde::de::Error::custom(format!(
            "expected number or numeric string, got {other}"
        ))),
    }
}

// Same shape but for i64. Used by SetPlayerField which carries a
// wider range (xp, gold, alignment can go to ±2.1B but stay safe at
// i64). The MCP TS layer marshals these as strings, so without this
// `set_player_field hp 100` over MCP fails parse.
fn de_i64_lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    use serde::Deserialize as _;
    match Value::deserialize(d)? {
        Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| serde::de::Error::custom("number out of range for i64")),
        Value::String(s) => s
            .parse::<i64>()
            .map_err(|e| serde::de::Error::custom(e.to_string())),
        other => Err(serde::de::Error::custom(format!(
            "expected number or numeric string, got {other}"
        ))),
    }
}

#[derive(Deserialize)]
struct FireTriggerBody {
    #[serde(deserialize_with = "de_i32_lenient")]
    zone_id: i32,
    #[serde(deserialize_with = "de_i32_lenient")]
    id: i32,
    self_name: String,
    actor_name: Option<String>,
}

#[derive(Deserialize)]
struct SetPlayerFieldBody {
    player_name: String,
    field: String,
    #[serde(deserialize_with = "de_i64_lenient")]
    value: i64,
}

#[derive(Deserialize)]
struct TeleportBody {
    player_name: String,
    #[serde(deserialize_with = "de_i32_lenient")]
    zone_id: i32,
    #[serde(deserialize_with = "de_i32_lenient")]
    room_id: i32,
}

#[derive(Deserialize)]
struct SpawnBody {
    #[serde(rename = "type")]
    kind: String,
    #[serde(deserialize_with = "de_i32_lenient")]
    zone_id: i32,
    #[serde(deserialize_with = "de_i32_lenient")]
    id: i32,
    #[serde(deserialize_with = "de_i32_lenient")]
    room_zone: i32,
    #[serde(deserialize_with = "de_i32_lenient")]
    room_id: i32,
}

async fn handle_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<CommandBody>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(
        enqueue(
            &state,
            AdminRequest::Command {
                executor: body.executor,
                command: body.command,
            },
        )
        .await,
    )
}

async fn handle_teleport(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<TeleportBody>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(
        enqueue(
            &state,
            AdminRequest::Teleport {
                player_name: body.player_name,
                zone_id: body.zone_id,
                room_id: body.room_id,
            },
        )
        .await,
    )
}

// Lenient optional u32 — accepts int literal, numeric string,
// null, or absent. Mirrors `de_i32_lenient` for the MCP TS layer
// that ships numeric tool args as JSON strings.
#[derive(Deserialize, Default)]
struct TickBody {
    #[serde(default, deserialize_with = "de_u32_lenient_opt")]
    count: Option<u32>,
}

fn de_u32_lenient_opt<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    use serde::Deserialize as _;
    match Option::<Value>::deserialize(d)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => Ok(Some(
            n.as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| serde::de::Error::custom("number out of range for u32"))?,
        )),
        Some(Value::String(s)) => Ok(Some(
            s.parse::<u32>()
                .map_err(|e| serde::de::Error::custom(e.to_string()))?,
        )),
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected number or numeric string, got {other}"
        ))),
    }
}

async fn handle_pause_world(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(enqueue(&state, AdminRequest::PauseWorld).await)
}

async fn handle_unpause_world(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(enqueue(&state, AdminRequest::UnpauseWorld).await)
}

async fn handle_tick_world(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<axum::Json<TickBody>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    let count = body.and_then(|b| b.0.count).unwrap_or(1).max(1);
    json_ok(enqueue(&state, AdminRequest::TickWorld { count }).await)
}

async fn handle_spawn(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<SpawnBody>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(
        enqueue(
            &state,
            AdminRequest::Spawn {
                kind: body.kind,
                zone_id: body.zone_id,
                id: body.id,
                room_zone: body.room_zone,
                room_id: body.room_id,
            },
        )
        .await,
    )
}

async fn handle_trigger_info(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(
        enqueue(
            &state,
            AdminRequest::TriggerInfo {
                zone_id: None,
                id: None,
            },
        )
        .await,
    )
}

async fn handle_trigger_info_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((zone_id, id)): Path<(i32, i32)>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(
        enqueue(
            &state,
            AdminRequest::TriggerInfo {
                zone_id: Some(zone_id),
                id: Some(id),
            },
        )
        .await,
    )
}

async fn handle_trigger_errors(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    let limit = params.get("limit").and_then(|s| s.parse().ok());
    json_ok(enqueue(&state, AdminRequest::TriggerErrors { limit }).await)
}

async fn handle_trigger_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(enqueue(&state, AdminRequest::TriggerStats).await)
}

async fn handle_trigger_validate(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    let zone_id = params.get("zone").and_then(|s| s.parse().ok());
    json_ok(enqueue(&state, AdminRequest::ValidateTriggers { zone_id }).await)
}

async fn handle_player_set(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<SetPlayerFieldBody>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(
        enqueue(
            &state,
            AdminRequest::SetPlayerField {
                player_name: body.player_name,
                field: body.field,
                value: body.value,
            },
        )
        .await,
    )
}

/// Manually invoke a trigger body. Bypasses the usual event-flag
/// gating; lets a tester exercise a specific body without
/// engineering a real GREET / SPEECH / RECEIVE event.
async fn handle_trigger_fire(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<FireTriggerBody>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    json_ok(
        enqueue(
            &state,
            AdminRequest::FireTrigger {
                zone_id: body.zone_id,
                id: body.id,
                self_name: body.self_name,
                actor_name: body.actor_name,
            },
        )
        .await,
    )
}

/// Re-pull the trigger catalog from the DB, then post the rebuilt
/// resource into the world tick for atomic swap. The DB query
/// runs here in the async handler — by the time the world dispatch
/// runs, all rows are already in memory.
async fn handle_trigger_reload(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return json_err(e);
    }
    let catalog = match mud_world::load_trigger_catalog(&state.pool).await {
        Ok(c) => Box::new(c),
        Err(e) => {
            return json_err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("trigger catalog query failed: {e}"),
            ));
        }
    };
    json_ok(enqueue(&state, AdminRequest::ReloadTriggers { catalog }).await)
}

fn json_ok(r: Result<Value, (StatusCode, String)>) -> (StatusCode, axum::Json<Value>) {
    match r {
        Ok(v) => (StatusCode::OK, axum::Json(v)),
        Err((code, msg)) => (code, axum::Json(json!({ "error": msg }))),
    }
}

fn json_err((code, msg): (StatusCode, String)) -> (StatusCode, axum::Json<Value>) {
    (code, axum::Json(json!({ "error": msg })))
}

/// Drain pending admin requests each tick and service them
/// synchronously. Bounded by [`MAX_PER_TICK`] and by [`DRAIN_BUDGET`] of
/// wall-clock time, so a request flood (or a run of slow commands) can't
/// starve gameplay systems: what is left stays in the inbox for the next
/// tick.
const MAX_PER_TICK: usize = 32;

/// Wall-clock a tick may spend on admin requests before it stops taking
/// more. A request already started runs to completion (a command is
/// capped by [`COMMAND_TIMEOUT`]), so a tick overshoots by at most that.
const DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

/// Requests whose world-side work is large enough to spend a whole tick's
/// admin budget on their own. Creating a session spawns a player entity and
/// its entire inventory in one go (its database reads already happened in
/// the HTTP handler, off the tick). A heavy request never shares a tick with
/// other admin work: it runs first in a tick, and nothing follows it.
fn is_heavy(request: &AdminRequest) -> bool {
    matches!(request, AdminRequest::SessionCreate { .. })
}

pub async fn drain_admin_requests(world: &mut World, pool: &PgPool) {
    drain_within(world, pool, DRAIN_BUDGET).await;
}

/// [`drain_admin_requests`] with an explicit budget. Always services at
/// least one request when any is waiting, so the queue makes progress.
async fn drain_within(world: &mut World, pool: &PgPool, budget: std::time::Duration) {
    world.init_resource::<AdminDeferred>();
    world.init_resource::<AdminParked>();
    // Finish what the previous tick's off-tick work produced, then answer
    // the commands that were waiting on it.
    commands::off_tick::drain_command_jobs(world);
    answer_parked(world);
    let started = std::time::Instant::now();
    for taken in 0..MAX_PER_TICK {
        if taken > 0 && started.elapsed() >= budget {
            break;
        }
        // The guards are dropped before the request is serviced (it may await).
        let deferred = world
            .resource::<AdminDeferred>()
            .0
            .lock()
            .expect("admin deferred poisoned")
            .take();
        let next = if let Some(cmd) = deferred {
            Ok(cmd)
        } else {
            let inbox = world.resource::<AdminInbox>();
            let mut rx = inbox.0.lock().expect("admin inbox poisoned");
            rx.try_recv()
        };
        let Ok(cmd) = next else { break };
        let heavy = is_heavy(&cmd.request);
        if heavy && taken > 0 {
            // Other admin work already ran this tick: start this one next
            // tick, on a fresh budget.
            *world
                .resource::<AdminDeferred>()
                .0
                .lock()
                .expect("admin deferred poisoned") = Some(cmd);
            break;
        }
        let AdminCommand { request, reply } = cmd;
        let resp = match request {
            // Player commands take the same async path as a telnet line
            // (aliases, async-only commands, shared pre-gate).
            AdminRequest::Command { executor, command } => {
                match start_command(world, pool, &executor, &command).await {
                    CommandRun::Done(resp) => resp,
                    CommandRun::Waiting(waiting) => {
                        // Its database work runs off the tick: answer the
                        // request when that has landed, not now.
                        world
                            .resource::<AdminParked>()
                            .0
                            .lock()
                            .expect("admin parked poisoned")
                            .push(Parked { waiting, reply });
                        continue;
                    }
                }
            }
            other => service(world, other),
        };
        let _ = reply.send(resp);
        if heavy {
            break;
        }
    }
}

fn service(world: &mut World, req: AdminRequest) -> AdminResponse {
    match req {
        AdminRequest::WorldStatus => Ok(world_status(world)),
        AdminRequest::Players => Ok(online_players(world)),
        AdminRequest::LookRoom { zone_id, id } => look_room(world, zone_id, id),
        AdminRequest::InspectActor { name } => inspect_actor(world, &name),
        AdminRequest::SessionCreate {
            player_name,
            user,
            character,
            items,
            abilities,
            aliases,
            achievements,
            script_vars_json,
            trophy_json,
            spell_cooldowns_json,
            cooldowns_json,
            ignore_list_json,
            command_grants_json,
            effect_instances_json,
        } => session_create(
            world,
            &player_name,
            &user,
            &character,
            &items,
            &abilities,
            &aliases,
            &achievements,
            script_vars_json,
            trophy_json,
            spell_cooldowns_json,
            cooldowns_json,
            ignore_list_json,
            command_grants_json,
            effect_instances_json,
        ),
        AdminRequest::SessionDestroy { player_name } => session_destroy(world, &player_name),
        AdminRequest::MarkPendingSave { player_name } => mark_pending_save(world, &player_name),
        // Handled by `drain_admin_requests` (needs the async dispatcher).
        AdminRequest::Command { .. } => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "command requests must go through drain_admin_requests".to_string(),
        )),
        AdminRequest::Teleport {
            player_name,
            zone_id,
            room_id,
        } => teleport(world, &player_name, zone_id, room_id),
        AdminRequest::Spawn {
            kind,
            zone_id,
            id,
            room_zone,
            room_id,
        } => spawn_into(world, &kind, zone_id, id, room_zone, room_id),
        AdminRequest::InspectMob { zone_id, id } => inspect_mob(world, zone_id, id),
        AdminRequest::PauseWorld => {
            let mut p = world.resource_mut::<WorldPause>();
            let was = p.paused;
            p.paused = true;
            Ok(json!({ "paused": true, "was_paused": was }))
        }
        AdminRequest::UnpauseWorld => {
            let mut p = world.resource_mut::<WorldPause>();
            let was = p.paused;
            p.paused = false;
            p.forced_ticks = 0;
            Ok(json!({ "paused": false, "was_paused": was }))
        }
        AdminRequest::TickWorld { count } => {
            let mut p = world.resource_mut::<WorldPause>();
            // Saturating so a misbehaving caller can't overflow.
            p.forced_ticks = p.forced_ticks.saturating_add(count);
            let pending = p.forced_ticks;
            let paused = p.paused;
            Ok(json!({
                "ticks_queued": count,
                "pending": pending,
                "paused": paused,
                "note": if paused { "ticks will fire one per real frame while paused" } else { "world is running; forced_ticks accumulates but has no effect until pause" },
            }))
        }
        AdminRequest::TriggerInfo { zone_id, id } => Ok(trigger_info(world, zone_id, id)),
        AdminRequest::TriggerErrors { limit } => Ok(trigger_errors(world, limit)),
        AdminRequest::TriggerStats => Ok(trigger_stats(world)),
        AdminRequest::ValidateTriggers { zone_id } => Ok(validate_triggers(world, zone_id)),
        AdminRequest::ReloadTriggers { catalog } => Ok(reload_triggers(world, *catalog)),
        AdminRequest::FireTrigger {
            zone_id,
            id,
            self_name,
            actor_name,
        } => fire_trigger(world, zone_id, id, &self_name, actor_name.as_deref()),
        AdminRequest::SetPlayerField {
            player_name,
            field,
            value,
        } => set_player_field(world, &player_name, &field, value),
    }
}

/// Write a typed integer attribute on a player or mob entity.
/// `field` is checked against an explicit allowlist; unknown
/// names yield a 400 so a typo never silently no-ops. Saturating
/// casts on the i64 → narrower-int conversions to avoid wrap.
#[allow(clippy::too_many_lines)]
pub(crate) fn set_player_field(
    world: &mut World,
    player_name: &str,
    field: &str,
    value: i64,
) -> AdminResponse {
    let entity = find_actor_by_name(world, player_name)?;
    let clamped = value.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    let v32: i32 = i32::try_from(clamped).unwrap_or(0);
    let mut applied = true;
    match field {
        "hp" => {
            if let Some(mut h) = world.get_mut::<Health>(entity) {
                h.hp = v32.min(h.max);
            } else {
                applied = false;
            }
        }
        "hp_max" => {
            if let Some(mut h) = world.get_mut::<Health>(entity) {
                h.max = v32.max(1);
                h.hp = h.hp.min(h.max);
                crate::commands::clear_stat_overflow(world, entity, "max_hp");
            } else {
                applied = false;
            }
        }
        "stamina" => {
            if let Some(mut s) = world.get_mut::<Stamina>(entity) {
                s.current = v32.min(s.max);
            } else {
                applied = false;
            }
        }
        "stamina_max" => {
            if let Some(mut s) = world.get_mut::<Stamina>(entity) {
                s.max = v32.max(1);
                s.current = s.current.min(s.max);
                crate::commands::clear_stat_overflow(world, entity, "max_stamina");
            } else {
                applied = false;
            }
        }
        "level" => {
            // Level >= 100 confers in-game staff rank
            // (`mud_db::enums::effective_rank`). This endpoint is already
            // authenticated at the HTTP layer (bearer `ADMIN_TOKEN`, or
            // loopback-only bind), which is the equivalent of the
            // Implementor requirement on in-game `set`/`advance`. Every
            // staff-level grant is audit-logged.
            let new_level = v32.max(1);
            if new_level >= mud_db::enums::MIN_STAFF_LEVEL {
                warn!(player = %player_name, new_level, "admin HTTP: staff-level grant");
                if !world.contains_resource::<commands::AdminAuditLog>() {
                    world.insert_resource(commands::AdminAuditLog::default());
                }
                world
                    .resource_mut::<commands::AdminAuditLog>()
                    .push(commands::AdminAuditEntry {
                        at: std::time::SystemTime::now(),
                        actor_name: "admin-http".to_string(),
                        verb: "staff_level_grant",
                        args: format!("{player_name} {new_level}"),
                    });
            }
            let old_level = world.get::<Profile>(entity).map_or(0, |p| p.level);
            if let Some(mut p) = world.get_mut::<Profile>(entity) {
                p.level = new_level;
            } else {
                applied = false;
            }
            // Keep cached rank (Account.role) and level perms in sync.
            crate::combat::after_level_change(world, entity, old_level);
        }
        "experience" => {
            if let Some(mut p) = world.get_mut::<Profile>(entity) {
                p.experience = v32.max(0);
            } else {
                applied = false;
            }
        }
        "hunger" => {
            if let Some(mut h) = world.get_mut::<mud_world::Hunger>(entity) {
                h.0 = v32.max(0);
            } else {
                applied = false;
            }
        }
        "thirst" => {
            if let Some(mut t) = world.get_mut::<mud_world::Thirst>(entity) {
                t.0 = v32.max(0);
            } else {
                applied = false;
            }
        }
        "alignment" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.alignment = v32.clamp(-1000, 1000);
            } else {
                applied = false;
            }
        }
        "accuracy" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.accuracy = v32;
            } else {
                applied = false;
            }
        }
        "evasion" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.evasion = v32;
            } else {
                applied = false;
            }
        }
        "attack_power" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.attack_power = v32;
            } else {
                applied = false;
            }
        }
        "spell_power" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.spell_power = v32;
            } else {
                applied = false;
            }
        }
        "armor_pct" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.armor_pct = v32.clamp(0, 100);
                crate::commands::clear_stat_overflow(world, entity, "armor_pct");
            } else {
                applied = false;
            }
        }
        "armor_flat" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.armor_flat = v32.max(0);
                crate::commands::clear_stat_overflow(world, entity, "armor_flat");
            } else {
                applied = false;
            }
        }
        "ward_pct" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.ward_pct = v32.clamp(0, 100);
                crate::commands::clear_stat_overflow(world, entity, "ward_pct");
            } else {
                applied = false;
            }
        }
        "hardness" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.hardness = v32.max(0);
                crate::commands::clear_stat_overflow(world, entity, "hardness");
            } else {
                applied = false;
            }
        }
        "crit_chance" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.crit_chance = v32.clamp(0, 100);
            } else {
                applied = false;
            }
        }
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "unsupported field '{other}'. Allowed: hp, hp_max, stamina, \
                     stamina_max, level, experience, hunger, thirst, alignment, \
                     accuracy, evasion, attack_power, spell_power, armor_pct, \
                     armor_flat, ward_pct, hardness, crit_chance"
                ),
            ));
        }
    }
    if !applied {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("actor '{player_name}' has no '{field}' component to write"),
        ));
    }
    Ok(json!({
        "ok": true,
        "actor": player_name,
        "field": field,
        "value": value,
    }))
}

/// Resolve an actor by name against any Mob or Player. Used by the
/// write paths (`player/set`, `triggers/fire`) and `inspect_actor`, so
/// it must never guess: an exact (case-insensitive) name wins, players
/// before mobs; otherwise the typed words must be a prefix match for
/// exactly one distinct name. Ambiguity is a 409 listing candidates.
fn find_actor_by_name(world: &mut World, name: &str) -> Result<Entity, (StatusCode, String)> {
    let needle = name.trim().to_ascii_lowercase();
    let mut q =
        world.query_filtered::<(Entity, &Named, Has<Player>), Or<(With<Mob>, With<Player>)>>();
    let all: Vec<(Entity, String, bool)> = q
        .iter(world)
        .map(|(e, n, is_player)| (e, n.name.clone(), is_player))
        .collect();
    let not_found = || (StatusCode::NOT_FOUND, format!("no actor matching '{name}'"));
    if needle.is_empty() {
        return Err(not_found());
    }
    let exact = |want_player: bool| {
        all.iter()
            .find(|(_, n, p)| *p == want_player && n.to_ascii_lowercase() == needle)
            .map(|(e, _, _)| *e)
    };
    if let Some(e) = exact(true).or_else(|| exact(false)) {
        return Ok(e);
    }
    let hits: Vec<&(Entity, String, bool)> = all
        .iter()
        .filter(|(_, n, _)| mud_world::targeting::names_match(&needle, std::iter::once(n.as_str())))
        .collect();
    let mut distinct: Vec<&str> = hits.iter().map(|(_, n, _)| n.as_str()).collect();
    distinct.sort_unstable();
    distinct.dedup();
    match distinct.len() {
        0 => Err(not_found()),
        1 => {
            // Same display name on several entities: prefer a player.
            let pick = hits.iter().find(|(_, _, p)| *p).or_else(|| hits.first());
            pick.map(|(e, _, _)| *e).ok_or_else(not_found)
        }
        _ => {
            distinct.truncate(10);
            Err((
                StatusCode::CONFLICT,
                format!("'{name}' is ambiguous; matches: {}", distinct.join(", ")),
            ))
        }
    }
}

/// Manually invoke a trigger body. Looks up `(zone_id, id)` in the
/// catalog, resolves `self_name` (and optional `actor_name`) to
/// entities, and runs the body via `LuaHost::exec_for_listener_with_extras`.
/// Side effects (`room.send`, state mutations) happen for real —
/// callers should pause the world first if they want isolation.
fn fire_trigger(
    world: &mut World,
    zone_id: i32,
    id: i32,
    self_name: &str,
    actor_name: Option<&str>,
) -> AdminResponse {
    let body: String = {
        let catalog = world.resource::<TriggerCatalog>();
        let Some(def) = catalog.by_key.get(&(zone_id, id)) else {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no trigger at ({zone_id}, {id})"),
            ));
        };
        def.commands.clone()
    };
    let self_entity = find_actor_by_name(world, self_name)?;
    let actor_entity = match actor_name {
        Some(n) => find_actor_by_name(world, n)?,
        None => self_entity,
    };
    let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
        host.exec_for_listener_with_extras(world, self_entity, actor_entity, &body, &[])
    });
    commands::drain_lua_outbox(world);
    match result {
        Ok(_) => Ok(json!({
            "ok": true,
            "zone_id": zone_id,
            "id": id,
            "self_entity": format!("{self_entity:?}"),
            "actor_entity": format!("{actor_entity:?}"),
        })),
        Err(e) => Ok(json!({
            "ok": false,
            "zone_id": zone_id,
            "id": id,
            "error": e.clone(),
        })),
    }
}

/// Swap the live `TriggerCatalog` for `new` and refresh
/// `AttachedTriggers` on every Room entity using the new
/// `room_attachments` map. Mob and item entities keep whatever
/// attachments they were spawned with — the next respawn cycle
/// picks up additions/removals naturally; reloading existing
/// instances would risk surprising live combat.
fn reload_triggers(world: &mut World, new: mud_world::TriggerCatalog) -> Value {
    let stats = crate::triggers::apply_reloaded_catalog(world, new);
    json!({
        "ok": true,
        "total": stats.total,
        "mob_links": stats.mob_links,
        "object_links": stats.object_links,
        "room_links": stats.room_links,
        "rooms_with_triggers": stats.rooms_with_triggers,
        "note": "mob/object instance attachments unchanged; next respawn picks up catalog edits",
    })
}

/// Enumerate trigger catalog rows as JSON. With no filter, returns
/// every trigger; with both `zone_id` and `id`, returns just that
/// row. Each row carries name, attached counts, and the event flag
/// list — the body is omitted to keep the payload tractable.
fn trigger_info(world: &World, zone_id: Option<i32>, id: Option<i32>) -> Value {
    let catalog = world.resource::<TriggerCatalog>();
    let key_filter = zone_id.zip(id);
    let mut rows: Vec<Value> = catalog
        .by_key
        .iter()
        .filter(|((z, i), _)| match key_filter {
            Some((wz, wi)) => *z == wz && *i == wi,
            None => true,
        })
        .map(|((z, i), def)| {
            let events: Vec<String> = def.flags.iter().map(|f| format!("{f:?}")).collect();
            let mob_attachments = catalog
                .mob_attachments
                .values()
                .filter(|keys| keys.contains(&(*z, *i)))
                .count();
            let object_attachments = catalog
                .object_attachments
                .values()
                .filter(|keys| keys.contains(&(*z, *i)))
                .count();
            let room_attachments = catalog
                .room_attachments
                .values()
                .filter(|keys| keys.contains(&(*z, *i)))
                .count();
            json!({
                "zone_id": z,
                "id": i,
                "name": def.name,
                "events": events,
                "mob_attachments": mob_attachments,
                "object_attachments": object_attachments,
                "room_attachments": room_attachments,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        let za = a.get("zone_id").and_then(Value::as_i64).unwrap_or(0);
        let zb = b.get("zone_id").and_then(Value::as_i64).unwrap_or(0);
        let ia = a.get("id").and_then(Value::as_i64).unwrap_or(0);
        let ib = b.get("id").and_then(Value::as_i64).unwrap_or(0);
        za.cmp(&zb).then(ia.cmp(&ib))
    });
    json!({
        "total": rows.len(),
        "triggers": rows,
    })
}

/// Drain the in-memory `ScriptErrorLog` ring buffer to JSON,
/// most-recent first. Default cap of 50 entries when no limit
/// query param is supplied; clamped to the buffer size either way.
fn trigger_errors(world: &World, limit: Option<usize>) -> Value {
    let Some(log) = world.get_resource::<mud_world::ScriptErrorLog>() else {
        return json!({
            "total": 0,
            "errors": [],
        });
    };
    let cap = limit.unwrap_or(50).min(log.entries.len());
    let entries: Vec<Value> = log
        .entries
        .iter()
        .rev()
        .take(cap)
        .map(|e| {
            let secs_ago = std::time::SystemTime::now()
                .duration_since(e.at)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            json!({
                "secs_ago": secs_ago,
                "trigger_zone": e.trigger_zone,
                "trigger_id": e.trigger_id,
                "trigger_name": e.trigger_name,
                "event": e.event,
                "message": e.message,
            })
        })
        .collect();
    json!({
        "total": log.entries.len(),
        "errors": entries,
    })
}

/// Snapshot of the runtime trigger fire counters. Per-event keys
/// match the `Debug` form of `TriggerEvent` ("Greet", "Speech",
/// …). Resets when the process restarts.
fn trigger_stats(world: &World) -> Value {
    let Some(stats) = world.get_resource::<crate::triggers::TriggerStats>() else {
        return json!({
            "total_fired": 0,
            "total_succeeded": 0,
            "total_failed": 0,
            "by_event": {},
        });
    };
    let by_event: serde_json::Map<String, Value> = stats
        .by_event
        .iter()
        .map(|(k, c)| {
            (
                k.clone(),
                json!({
                    "fired": c.fired,
                    "succeeded": c.succeeded,
                    "failed": c.failed,
                }),
            )
        })
        .collect();
    json!({
        "total_fired": stats.total_fired,
        "total_succeeded": stats.total_succeeded,
        "total_failed": stats.total_failed,
        "by_event": by_event,
    })
}

/// Compile-check every loaded trigger body and report failures. Syntax
/// only: nothing is executed, so this is safe against a live world.
fn validate_triggers(world: &World, zone_id: Option<i32>) -> Value {
    let report = crate::triggers::validate_catalog(world.resource::<TriggerCatalog>(), zone_id);
    let failures: Vec<Value> = report
        .failures
        .iter()
        .map(|f| {
            json!({
                "zone_id": f.zone_id,
                "id": f.id,
                "name": f.name,
                "error": f.error,
            })
        })
        .collect();
    json!({
        "total": report.total,
        "passed": report.total - report.failures.len(),
        "failed": report.failures.len(),
        "failures": failures,
    })
}

fn world_status(world: &mut World) -> Value {
    let tick = world.resource::<crate::TickCount>().0;
    let pause = world.resource::<WorldPause>();
    let paused = pause.paused;
    let pending_ticks = pause.forced_ticks;
    let players = world
        .query_filtered::<(), (With<Player>, With<Online>)>()
        .iter(world)
        .count();
    let mobs = world.query_filtered::<(), With<Mob>>().iter(world).count();
    let items = world.query_filtered::<(), With<Item>>().iter(world).count();
    // Bevy never frees an archetype, and fresh queries walk them: a count
    // that keeps climbing under steady play means some component is being
    // added late or toggled (see `bookkeeping`).
    let archetypes = world.archetypes().len();
    // Tick duration percentiles over the last minute (issue #29: lag was
    // invisible). Absent resource (unit-test worlds) reads as all zeros.
    let timing = world.get_resource::<crate::tick_stats::TickStats>().map_or(
        crate::tick_stats::TickSummary {
            samples: 0,
            p50_ms: 0.0,
            p95_ms: 0.0,
            max_ms: 0.0,
        },
        |t| t.summary(std::time::Instant::now()),
    );
    json!({
        "paused": paused,
        "pending_forced_ticks": pending_ticks,
        "tick": tick,
        "online_players": players,
        "mobs": mobs,
        "items": items,
        "archetypes": archetypes,
        "tick_window_secs": 60,
        "tick_samples": timing.samples,
        "tick_p50_ms": timing.p50_ms,
        "tick_p95_ms": timing.p95_ms,
        "tick_max_ms": timing.max_ms,
    })
}

/// Every logged-in player session (`Player` + `Online` + `Account`).
/// Pre-login connections have no player entity, so they never appear.
fn online_players(world: &mut World) -> Value {
    type Row<'a> = (
        &'a Named,
        &'a Profile,
        Option<&'a Located>,
        Option<&'a LastInputAt>,
        Option<&'a LoggedInAt>,
    );
    struct Snap {
        name: String,
        level: i32,
        class_id: Option<i32>,
        race: String,
        room: Option<Entity>,
        idle: u64,
        connected: u64,
    }
    let rows: Vec<Snap> = {
        let mut q = world.query_filtered::<Row<'_>, (With<Player>, With<Online>, With<Account>)>();
        q.iter(world)
            .map(|(named, profile, located, last, login)| {
                let connected = login.map_or(0, |l| l.0.elapsed().as_secs());
                let idle = last.map_or(connected, |l| l.0.elapsed().as_secs());
                Snap {
                    name: named.name.clone(),
                    level: profile.level,
                    class_id: profile.class_id,
                    race: profile.race.clone(),
                    room: located.map(|l| l.0),
                    idle,
                    connected,
                }
            })
            .collect()
    };
    let mut players: Vec<Value> = rows
        .into_iter()
        .map(|snap| {
            let class = snap.class_id.and_then(|id| {
                world
                    .get_resource::<ClassCatalog>()
                    .and_then(|c| c.by_id.get(&id))
                    .map(|c| c.plain_name.clone())
            });
            let room = snap
                .room
                .and_then(|r| world.get::<WorldKey>(r).copied())
                .map(|wk| json!({"zone_id": wk.zone, "id": wk.id}));
            json!({
                "name": snap.name,
                "level": snap.level,
                "class": class,
                "race": snap.race,
                "room": room,
                "idle_seconds": snap.idle,
                "connected_seconds": snap.connected,
            })
        })
        .collect();
    players.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json!({ "count": players.len(), "players": players })
}

fn look_room(world: &mut World, zone_id: i32, id: i32) -> AdminResponse {
    let Some(room) = world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(zone_id, id))
        .copied()
    else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("room ({zone_id}, {id}) not loaded"),
        ));
    };
    let name = name_of(world, room);
    let mut exits_json = Vec::new();
    if let Some(exits) = world.get::<Exits>(room).cloned() {
        for (dir, ed) in &exits.0 {
            let to = ed
                .to
                .and_then(|t| world.get::<WorldKey>(t).copied())
                .map(|wk| json!([wk.zone, wk.id]));
            exits_json.push(json!({
                "direction": format!("{dir:?}"),
                "state": format!("{:?}", ed.state),
                "to": to,
            }));
        }
    }
    let mut mobs_json = Vec::new();
    let mut players_json = Vec::new();
    let mut items_json = Vec::new();
    let actor_rows: Vec<(Entity, Located, Named, Option<WorldKey>, bool)> = {
        let mut q = world
            .query_filtered::<(Entity, &Located, &Named, Option<&WorldKey>, Option<&Player>), Or<(With<Mob>, With<Player>)>>();
        q.iter(world)
            .map(|(e, l, n, wk, p)| (e, *l, n.clone(), wk.copied(), p.is_some()))
            .collect()
    };
    for (e, l, n, wk, is_player) in &actor_rows {
        if l.0 != room {
            continue;
        }
        let row = json!({
            "entity": format!("{:?}", e),
            "name": n.name,
            "world_key": wk.map(|w| json!([w.zone, w.id])),
        });
        if *is_player {
            players_json.push(row);
        } else {
            mobs_json.push(row);
        }
    }
    let item_rows: Vec<(Entity, Located, Named, Option<WorldKey>)> = {
        let mut q =
            world.query_filtered::<(Entity, &Located, &Named, Option<&WorldKey>), With<Item>>();
        q.iter(world)
            .map(|(e, l, n, wk)| (e, *l, n.clone(), wk.copied()))
            .collect()
    };
    for (e, l, n, wk) in &item_rows {
        if l.0 != room {
            continue;
        }
        items_json.push(json!({
            "entity": format!("{:?}", e),
            "name": n.name,
            "world_key": wk.map(|w| json!([w.zone, w.id])),
        }));
    }
    Ok(json!({
        "world_key": [zone_id, id],
        "name": name,
        "exits": exits_json,
        "players": players_json,
        "mobs": mobs_json,
        "items": items_json,
    }))
}

pub(crate) fn inspect_actor(world: &mut World, name: &str) -> AdminResponse {
    let entity = find_actor_by_name(world, name)?;
    let actor_name = name_of(world, entity);
    let world_key = world
        .get::<WorldKey>(entity)
        .map(|wk| json!([wk.zone, wk.id]));
    let location = world
        .get::<Located>(entity)
        .and_then(|l| world.get::<WorldKey>(l.0).copied())
        .map(|wk| json!([wk.zone, wk.id]));
    let health = world
        .get::<Health>(entity)
        .map(|h| json!({"hp": h.hp, "max": h.max}));
    let stamina = world
        .get::<Stamina>(entity)
        .map(|s| json!({"current": s.current, "max": s.max}));
    let posture = world
        .get::<Posture>(entity)
        .map(|p| p.0.label().to_string());
    // Clamp-ledger debt per stat (non-zero entries only), so a stat stuck
    // short of its bound is visible.
    let stat_overflow: serde_json::Map<String, Value> =
        crate::commands::stat_overflow_entries(world, entity)
            .into_iter()
            .map(|(k, n)| (k.to_string(), json!(n)))
            .collect();
    let kind = if world.get::<Player>(entity).is_some() {
        "player"
    } else if world.get::<Mob>(entity).is_some() {
        "mob"
    } else {
        "other"
    };
    let level = world.get::<Profile>(entity).map(|p| p.level);
    let hunger = world.get::<mud_world::Hunger>(entity).map(|h| h.0);
    let thirst = world.get::<mud_world::Thirst>(entity).map(|t| t.0);
    let effects: Vec<Value> = {
        mud_world::effects_on(world, entity)
            .into_iter()
            .filter_map(|e| world.get::<EffectInstance>(e))
            .map(|eff| {
                json!({
                    "name": eff.name,
                    "remaining_secs": eff.remaining_secs,
                    "permanent": eff.remaining_secs < 0,
                })
            })
            .collect()
    };
    Ok(json!({
        "entity": format!("{:?}", entity),
        "name": actor_name,
        "kind": kind,
        "world_key": world_key,
        "location": location,
        "level": level,
        "health": health,
        "stamina": stamina,
        "hunger": hunger,
        "thirst": thirst,
        "posture": posture,
        "stat_overflow": stat_overflow,
        "effects": effects,
    }))
}

/// World-side time of one session create above which a breakdown is logged.
const SLOW_SESSION_CREATE: std::time::Duration = std::time::Duration::from_millis(25);

/// The tick-side half of `session/create`: spawn the player and its items
/// from rows the HTTP handler already read from the database. Takes no pool
/// and awaits nothing, so it cannot wait on the database from the tick.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
fn session_create(
    world: &mut World,
    player_name: &str,
    user: &User,
    character: &CharacterRow,
    items: &[character_items::CharacterItemRow],
    abilities: &[mud_db::character_abilities::CharacterAbilityRow],
    aliases: &[mud_db::character_aliases::CharacterAliasRow],
    achievements: &[mud_db::achievements::CharacterAchievementRow],
    script_vars_json: Option<serde_json::Value>,
    trophy_json: Option<serde_json::Value>,
    spell_cooldowns_json: Option<serde_json::Value>,
    cooldowns_json: Option<serde_json::Value>,
    ignore_list_json: Option<serde_json::Value>,
    command_grants_json: Option<serde_json::Value>,
    effect_instances_json: Option<serde_json::Value>,
) -> AdminResponse {
    // Reject duplicate by name.
    {
        let sessions = world.resource::<VirtualSessions>();
        let by_name = sessions.by_name.lock().expect("sessions poisoned");
        if by_name.contains_key(player_name) {
            return Err((
                StatusCode::CONFLICT,
                format!("virtual session '{player_name}' already exists"),
            ));
        }
    }
    // If the character is already logged in via telnet, refuse —
    // double-spawning would corrupt the world (two entities for
    // the same character_id, conflicting saves on disconnect).
    let already_online = {
        let mut q = world.query_filtered::<&Named, (With<Player>, With<Online>)>();
        q.iter(world)
            .any(|n| n.name.eq_ignore_ascii_case(player_name))
    };
    if already_online {
        return Err((
            StatusCode::CONFLICT,
            format!("'{player_name}' is already logged in via telnet; destroy that session first"),
        ));
    }
    // Reuse the same spawn machinery the telnet login path uses:
    // spawn_player attaches Connection/health/stats/profile, then
    // spawn_inventory rehydrates carried + worn items including
    // the proto-derived component set (LiquidContainer, WearableIn,
    // BoardLink, AttachedTriggers — see commit c332609).
    let started = std::time::Instant::now();
    let (tx, rx) = mpsc::channel::<Vec<u8>>(mud_net::OUTBOUND_QUEUE_CAP);
    let outbound: Outbound = tx;
    let entity = crate::login::spawn_player(world, user, character, outbound);
    let player_done = started.elapsed();
    let item_count = crate::login::spawn_inventory(world, entity, items);
    let inventory_done = started.elapsed();
    // Apply gear stat bonuses for every item that respawned with
    // an EquippedSlot. Mirrors the recompute pass in
    // login::complete_login so virtual sessions land with the same
    // stat sheet a real-telnet login would.
    crate::equip_apply::recompute_equipped_keeping_vitals(world, entity);
    // Match login::complete_login: attach KnownAbilities + Aliases +
    // Title + Description so commands that read those (invoke_ability's
    // skill lookup, alias expansion, etc.) work for virtual sessions.
    // Without this, every ability formula referencing `skill` resolves
    // to 0 because KnownAbilities is missing.
    let mut known_abilities = mud_world::KnownAbilities::from_rows(abilities);
    crate::login::grant_class_skills_for(
        world,
        character.class_id,
        character.level,
        &mut known_abilities,
    );
    let ability_count = known_abilities.entries.len();
    let alias_set = mud_world::Aliases::from_rows(aliases);
    let alias_count = alias_set.entries.len();
    // Achievements: hydrate via the shared login helper so a
    // virtual session shows the same `achievements` output a
    // real-telnet login would (E12 — including the unlocked_at
    // timestamp the renderer surfaces).
    let (ca_built, zv_built) = crate::login::build_achievement_components(world, achievements);
    let achievement_count = ca_built.unlocked.len();
    if let Ok(mut e) = world.get_entity_mut(entity) {
        e.insert(known_abilities);
        e.insert(alias_set);
        if !ca_built.unlocked.is_empty() {
            e.insert(ca_built);
        }
        if !zv_built.by_zone.is_empty() {
            e.insert(zv_built);
        }
        if let Some(t) = character.title.as_deref()
            && !t.trim().is_empty()
        {
            e.insert(mud_world::Title(t.trim().to_string()));
        }
        if let Some(d) = character.description.as_deref()
            && !d.trim().is_empty()
        {
            e.insert(mud_world::Description(d.trim().to_string()));
        }
        // Mirror login::complete_login for the persisted-state
        // components — without these the round-trip looks like
        // a regression even when save_state wrote them correctly.
        if character.invis_level > 0 {
            e.insert(mud_world::WizInvis(character.invis_level));
        }
        if character.freeze_level.is_some() {
            e.insert(mud_world::Frozen);
        }
        if character.wimpy_threshold > 0 {
            e.insert(mud_world::WimpyThreshold(character.wimpy_threshold));
        }
        if character.poof_in.is_some() || character.poof_out.is_some() {
            e.insert(mud_world::Poofs {
                poof_in: character.poof_in.clone(),
                poof_out: character.poof_out.clone(),
            });
        }
        // Same hydration as the telnet login: the reserved camp wake-kit
        // key becomes a component (not a visible var), so this session's
        // save re-writes it instead of erasing the queued wake bonus.
        if let Some(json) = script_vars_json {
            crate::login::insert_loaded_script_vars(&mut e, json, character.rest_source);
        }
        if let Some(json) = trophy_json
            && let Ok(entries) =
                serde_json::from_value::<std::collections::VecDeque<mud_world::TrophyEntry>>(json)
            && !entries.is_empty()
        {
            e.insert(mud_world::Trophy { entries });
        }
        if let Some(json) = spell_cooldowns_json
            && let Ok(slots) = serde_json::from_value::<mud_world::SpellSlots>(json)
            && !slots.in_flight.is_empty()
        {
            e.insert(slots);
        }
        if let Some(json) = ignore_list_json
            && let Ok(list) = serde_json::from_value::<Vec<String>>(json)
            && !list.is_empty()
        {
            e.insert(mud_world::IgnoreList(list));
        }
        if let Some(json) = command_grants_json
            && let Ok(grants) = serde_json::from_value::<mud_world::CommandGrants>(json)
            && !grants.is_empty()
        {
            e.insert(grants);
        }
        if let Some(json) = cooldowns_json
            && let Ok(map) = serde_json::from_value::<std::collections::HashMap<String, i64>>(json)
        {
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            let now_inst = std::time::Instant::now();
            let mut cd = mud_world::Cooldowns::default();
            for (k, ready_unix) in map {
                let Ok(id) = k.parse::<i32>() else { continue };
                let secs_left = ready_unix.saturating_sub(now_unix);
                if secs_left <= 0 {
                    continue;
                }
                cd.ready_at.insert(
                    id,
                    now_inst
                        + std::time::Duration::from_secs(u64::try_from(secs_left).unwrap_or(0)),
                );
            }
            if !cd.ready_at.is_empty() {
                e.insert(cd);
            }
        }
    }
    // EffectInstance hydration runs OUTSIDE the entity_mut scope —
    // each effect spawns a child entity with `AppliedTo(player)`.
    // Mirrors the login path's restore logic (login.rs); shared
    // policy: drop non-Admin effects beyond a 1-hour disconnect cap,
    // restore the rest with elapsed time deducted.
    if let Some(json) = effect_instances_json
        && let Ok(persisted) = serde_json::from_value::<crate::login::PersistedEffects>(json)
    {
        crate::login::restore_persisted_effects(world, entity, persisted);
    }
    crate::login::apply_player_race_effects(world, entity);
    let mut by_name = world
        .resource::<VirtualSessions>()
        .by_name
        .lock()
        .expect("sessions poisoned");
    by_name.insert(player_name.to_string(), VirtualSession { entity, rx });
    let total = started.elapsed();
    if total >= SLOW_SESSION_CREATE {
        // Everything here runs on the world tick; the breakdown says which
        // stage to look at when a session create shows up in a slow tick.
        warn!(
            player = player_name,
            items = item_count,
            total_ms = total.as_millis(),
            spawn_player_ms = player_done.as_millis(),
            spawn_inventory_ms = inventory_done.saturating_sub(player_done).as_millis(),
            rest_ms = total.saturating_sub(inventory_done).as_millis(),
            "virtual session spawn is slow on the tick"
        );
    }
    Ok(json!({
        "success": true,
        "player_name": player_name,
        "entity": format!("{:?}", entity),
        "items_loaded": item_count,
        "abilities_loaded": ability_count,
        "aliases_loaded": alias_count,
        "achievements_loaded": achievement_count,
        "note": "virtual session spawned standalone (no telnet required); destroy_session despawns",
    }))
}

fn session_destroy(world: &mut World, player_name: &str) -> AdminResponse {
    let removed = {
        let sessions = world.resource::<VirtualSessions>();
        let mut by_name = sessions.by_name.lock().expect("sessions poisoned");
        by_name.remove(player_name)
    };
    let Some(session) = removed else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no virtual session for '{player_name}'"),
        ));
    };
    // Despawn the player entity itself plus everything Located on
    // them (worn equipment, carried inventory). Skip a DB save —
    // virtual sessions are ephemeral by design, and saving could
    // overwrite legitimate state from a separate telnet session.
    let mut to_despawn: Vec<Entity> = vec![session.entity];
    let mut frontier = vec![session.entity];
    while let Some(parent) = frontier.pop() {
        let children: Vec<Entity> = {
            let mut q = world.query::<(Entity, &Located)>();
            q.iter(world)
                .filter(|(_, l)| l.0 == parent)
                .map(|(e, _)| e)
                .collect()
        };
        for c in children {
            to_despawn.push(c);
            frontier.push(c);
        }
    }
    for e in to_despawn {
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
    Ok(json!({
        "success": true,
        "player_name": player_name,
        "note": "virtual session despawned (entity + carried items removed; not saved to DB)",
    }))
}

fn mark_pending_save(world: &mut World, player_name: &str) -> AdminResponse {
    let entity = {
        let mut q = world.query_filtered::<(Entity, &Named), With<Player>>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(player_name))
            .map(|(e, _)| e)
    };
    let Some(entity) = entity else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no online player matching '{player_name}'"),
        ));
    };
    if let Ok(mut em) = world.get_entity_mut(entity) {
        em.insert(mud_world::PendingSave);
    }
    Ok(json!({
        "success": true,
        "player_name": player_name,
        "note": "PendingSave marker inserted; main-loop autosave will checkpoint on the next tick",
    }))
}

/// Longest an admin-issued command may run before the request is answered
/// with a timeout. Commands whose database work runs off the tick (`save`,
/// `visit`, mail, ...) wait parked for it; this bounds that wait, and a
/// stuck handler that still awaits inline, so it can't hold the tick loop
/// for long.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

async fn start_command(
    world: &mut World,
    pool: &PgPool,
    executor: &str,
    command_line: &str,
) -> CommandRun {
    let entity = {
        let sessions = world.resource::<VirtualSessions>();
        let by_name = sessions.by_name.lock().expect("sessions poisoned");
        by_name.get(executor).map(|s| s.entity)
    };
    let Some(entity) = entity else {
        return CommandRun::Done(Err((
            StatusCode::NOT_FOUND,
            format!("no virtual session for '{executor}'; create one first"),
        )));
    };
    // Anything already queued for the session (mob arrivals, combat rounds,
    // channel chatter since the last command) is not this command's output.
    // Take it out first and hand it back separately as `pending_output`, so
    // the caller never reads the previous command's tail as this one's.
    let pending_output = drain_session_output(world, executor);
    // Run the command through the telnet input path so async-only
    // commands work and the gates / room triggers fire exactly once. Any
    // send_to / send_rendered the dispatch performs flows through the
    // entity's Connection (which we replaced on session_create) into our
    // captured rx.
    let queued_before = commands::input_queue::queued_len(world, entity);
    let timed_out = tokio::time::timeout(
        COMMAND_TIMEOUT,
        commands::dispatch_with_async(world, entity, pool, command_line),
    )
    .await
    .is_err();
    // Sends are synchronous, so by now everything the command wrote is
    // sitting in the receiver.
    let output = drain_session_output(world, executor);
    if timed_out {
        return CommandRun::Done(Err(command_timed_out(command_line, &output)));
    }
    // The command (or an earlier one it queued behind) started database work
    // that finishes on a later tick: its output is not here yet.
    if commands::off_tick::is_busy(world, entity) {
        return CommandRun::Waiting(Waiting {
            executor: executor.to_string(),
            entity,
            command: command_line.to_string(),
            pending_output: format!("{pending_output}{output}"),
            deadline: std::time::Instant::now() + COMMAND_TIMEOUT,
            own_line_queued: commands::input_queue::queued_len(world, entity) > queued_before,
        });
    }
    // Lines this command left waiting (a wait state such as fear's lag, a
    // cast lock, an alias chain): they run on later ticks, one per tick
    // (`drain_virtual_input_queues`), and what they print is handed back as
    // `pending_output` of the session's next command.
    let queued = commands::input_queue::queued_len(world, entity).saturating_sub(queued_before);
    CommandRun::Done(Ok(command_response(
        command_line,
        &pending_output,
        &output,
        queued,
    )))
}

/// Release one waiting input line per virtual session whose hold (wait
/// state, cast lock, off-tick work) has lifted. The telnet sessions get the
/// same service from `Router::drain_input_queues`; virtual sessions are not
/// connections, so without this a line queued behind a wait state (or an
/// alias chain) would never run. Driven once per tick, after the schedule.
/// Output lands in the session's buffer for its next command to collect.
pub async fn drain_virtual_input_queues(world: &mut World, pool: &PgPool) {
    let waiting: Vec<Entity> = {
        let Some(sessions) = world.get_resource::<VirtualSessions>() else {
            return;
        };
        let by_name = sessions.by_name.lock().expect("sessions poisoned");
        by_name
            .values()
            .map(|s| s.entity)
            .filter(|e| commands::has_queued_input(world, *e))
            .collect()
    };
    for entity in waiting {
        let ran = tokio::time::timeout(
            COMMAND_TIMEOUT,
            commands::run_queued_input(world, entity, pool),
        )
        .await;
        if ran.is_err() {
            warn!(?entity, "queued virtual-session line timed out");
        }
    }
}

fn command_timed_out(command_line: &str, output: &str) -> (StatusCode, String) {
    (
        StatusCode::GATEWAY_TIMEOUT,
        format!(
            "command '{command_line}' timed out after {}s; partial output: {output}",
            COMMAND_TIMEOUT.as_secs()
        ),
    )
}

fn command_response(
    command_line: &str,
    pending_output: &str,
    output: &str,
    queued_lines: usize,
) -> Value {
    let mut resp = json!({
        "success": true,
        "command": command_line,
        "pending_output": pending_output,
        "output": output,
    });
    if queued_lines > 0 {
        resp["queued_lines"] = json!(queued_lines);
        resp["note"] = json!(
            "the session is held (wait state, cast, or earlier work), so the line was queued, \
             not run; it executes on a later tick and its output arrives as pending_output \
             of the next command"
        );
    }
    resp
}

/// Answer the parked commands whose work has landed, or whose
/// [`COMMAND_TIMEOUT`] is up. One reply per session per pass, oldest first: a
/// session's output is one stream, so a command is answered with what the
/// session has printed since the one before it was, never with the next
/// command's. A command is settled once the session has no off-tick work out
/// and, if its own line had to wait in the input queue, that queue has run.
/// A command that timed out is still running: what it prints later arrives
/// as `pending_output` of the session's next command.
fn answer_parked(world: &mut World) {
    let parked = std::mem::take(
        &mut *world
            .resource::<AdminParked>()
            .0
            .lock()
            .expect("admin parked poisoned"),
    );
    let mut still_waiting = Vec::new();
    let mut answered: Vec<Entity> = Vec::new();
    for Parked { waiting, reply } in parked {
        // The session's earlier command is still waiting, or was answered
        // this very pass: this one is next.
        if still_waiting
            .iter()
            .any(|p: &Parked| p.waiting.entity == waiting.entity)
            || answered.contains(&waiting.entity)
        {
            still_waiting.push(Parked { waiting, reply });
            continue;
        }
        let gone = world.get_entity(waiting.entity).is_err();
        let busy = commands::off_tick::is_busy(world, waiting.entity);
        let line_pending =
            waiting.own_line_queued && commands::has_queued_input(world, waiting.entity);
        let settled = gone || (!busy && !line_pending);
        let expired = std::time::Instant::now() >= waiting.deadline;
        if !settled && !expired {
            still_waiting.push(Parked { waiting, reply });
            continue;
        }
        let output = drain_session_output(world, &waiting.executor);
        let resp = if busy && !settled {
            Err(command_timed_out(&waiting.command, &output))
        } else {
            let queued = if line_pending {
                commands::input_queue::queued_len(world, waiting.entity)
            } else {
                0
            };
            Ok(command_response(
                &waiting.command,
                &waiting.pending_output,
                &output,
                queued,
            ))
        };
        let _ = reply.send(resp);
        answered.push(waiting.entity);
    }
    world
        .resource::<AdminParked>()
        .0
        .lock()
        .expect("admin parked poisoned")
        .extend(still_waiting);
}

/// [`start_command`] for callers that need the answer in hand: a world with
/// no off-tick jobs never parks a command.
#[cfg(test)]
async fn run_command(
    world: &mut World,
    pool: &PgPool,
    executor: &str,
    command_line: &str,
) -> AdminResponse {
    match start_command(world, pool, executor, command_line).await {
        CommandRun::Done(resp) => resp,
        CommandRun::Waiting(_) => panic!("command parked in a world without off-tick jobs"),
    }
}

/// Everything the virtual session's receiver has accumulated, decoded.
fn drain_session_output(world: &World, executor: &str) -> String {
    let mut buf = Vec::<u8>::new();
    let sessions = world.resource::<VirtualSessions>();
    let mut by_name = sessions.by_name.lock().expect("sessions poisoned");
    if let Some(session) = by_name.get_mut(executor) {
        while let Ok(bytes) = session.rx.try_recv() {
            buf.extend_from_slice(&bytes);
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

/// Move an actor (player or mob) by name to the given room. Does not
/// require a virtual session — useful for setting up scenarios via
/// MCP before spawning a session at the destination. Looks up the
/// target by case-insensitive Named match.
fn teleport(world: &mut World, name: &str, zone_id: i32, room_id: i32) -> AdminResponse {
    let needle = name.to_ascii_lowercase();
    let entity = {
        let mut q = world.query_filtered::<(Entity, &Named), Or<(With<Mob>, With<Player>)>>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(&needle))
            .map(|(e, _)| e)
    };
    let Some(entity) = entity else {
        return Err((StatusCode::NOT_FOUND, format!("no actor matching '{name}'")));
    };
    let Some(room_entity) = world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(zone_id, room_id))
        .copied()
    else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("room ({zone_id}, {room_id}) not loaded"),
        ));
    };
    mud_world::movement::move_to_room(world, entity, room_entity);
    Ok(json!({
        "success": true,
        "name": name_of(world, entity),
        "destination": [zone_id, room_id],
    }))
}

/// Spawn a fresh mob or item directly into a target room. Mirrors
/// the proto-derived component set the loader's reset pass uses
/// (`Description`, `Health`, `CombatStats` for mobs; `WearableIn`,
/// `BoardLink`, `LiquidContainer` for items) so the spawned entity
/// is indistinguishable from one produced by world load. No reset
/// row is associated — destroying a spawned entity won't trigger a
/// respawn since `FromMobReset` / `FromObjectReset` aren't
/// attached.
#[allow(clippy::too_many_lines)]
pub(crate) fn spawn_into(
    world: &mut World,
    kind: &str,
    zone_id: i32,
    id: i32,
    room_zone: i32,
    room_id: i32,
) -> AdminResponse {
    let Some(room_entity) = world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(room_zone, room_id))
        .copied()
    else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("room ({room_zone}, {room_id}) not loaded"),
        ));
    };
    match kind.to_ascii_lowercase().as_str() {
        "mob" | "mobile" | "npc" => {
            let proto = world
                .resource::<MobPrototypes>()
                .by_key
                .get(&(zone_id, id))
                .cloned();
            let Some(proto) = proto else {
                return Err((
                    StatusCode::NOT_FOUND,
                    format!("no mob prototype ({zone_id}, {id})"),
                ));
            };
            let entity = mud_world::spawn_mob_from_proto(world, &proto, room_entity, None);
            Ok(json!({
                "success": true,
                "kind": "mob",
                "entity": format!("{:?}", entity),
                "name": proto.name,
                "world_key": [zone_id, id],
                "room": [room_zone, room_id],
            }))
        }
        "obj" | "object" | "item" => {
            let proto = world
                .resource::<ObjectPrototypes>()
                .by_key
                .get(&(zone_id, id))
                .cloned();
            let Some(proto) = proto else {
                return Err((
                    StatusCode::NOT_FOUND,
                    format!("no object prototype ({zone_id}, {id})"),
                ));
            };
            let entity = crate::item_spawn::spawn_item_from_proto(world, &proto, room_entity);
            Ok(json!({
                "success": true,
                "kind": "object",
                "entity": format!("{:?}", entity),
                "name": proto.name,
                "world_key": [zone_id, id],
                "room": [room_zone, room_id],
            }))
        }
        other => Err((
            StatusCode::BAD_REQUEST,
            format!("unknown spawn kind '{other}'; expected 'mob' or 'object'"),
        )),
    }
}

/// Look up a mob prototype by (zone, id) in the catalog and return
/// its template stats. This is the *prototype* (loader-time data),
/// not a live instance — for live state use `inspect_actor` against
/// a spawned mob's name.
fn inspect_mob(world: &mut World, zone_id: i32, id: i32) -> AdminResponse {
    let proto = world
        .resource::<MobPrototypes>()
        .by_key
        .get(&(zone_id, id))
        .cloned();
    let Some(p) = proto else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no mob prototype ({zone_id}, {id})"),
        ));
    };
    Ok(json!({
        "world_key": [zone_id, id],
        "name": p.name,
        "keywords": p.keywords,
        "room_description": p.room_description,
        "level": p.level,
        "alignment": p.alignment,
        "role": format!("{:?}", p.role),
        "hp_dice": format!("{}d{}+{}", p.hp_dice_num, p.hp_dice_size, p.hp_dice_bonus),
        "hp_avg": p.rolled_hp(),
        "damage_dice": format!("{}d{}+{}", p.damage_dice_num, p.damage_dice_size, p.damage_dice_bonus),
        "damage_avg": p.avg_damage(),
        "accuracy": p.accuracy,
        "evasion": p.evasion,
        "attack_power": p.attack_power,
        "spell_power": p.spell_power,
        "armor_rating": p.armor_rating,
        "damage_reduction_percent": p.damage_reduction_percent,
        "soak": p.soak,
        "hardness": p.hardness,
        "ward_percent": p.ward_percent,
        "wealth": p.wealth,
        "class_id": p.class_id,
        "aggression_formula": p.aggression_formula,
    }))
}

#[cfg(test)]
mod validate_tests {
    use super::*;
    use mud_world::{TriggerAttach, TriggerDef};

    #[test]
    fn validate_triggers_json_shape() {
        let mut world = World::new();
        let mut cat = TriggerCatalog::default();
        for (id, body) in [(1, "return 1"), (2, "if then")] {
            cat.by_key.insert(
                (7, id),
                TriggerDef {
                    zone_id: 7,
                    id,
                    name: format!("t{id}"),
                    attach_type: TriggerAttach::Mob,
                    commands: body.to_string(),
                    flags: vec![],
                    arg_list: vec![],
                    num_args: 0,
                },
            );
        }
        world.insert_resource(cat);
        let v = validate_triggers(&world, None);
        assert_eq!(v["total"], 2);
        assert_eq!(v["passed"], 1);
        assert_eq!(v["failed"], 1);
        assert_eq!(v["failures"][0]["zone_id"], 7);
        assert_eq!(v["failures"][0]["id"], 2);
        let none = validate_triggers(&world, Some(8));
        assert_eq!(none["total"], 0);
    }
}

#[cfg(test)]
mod players_tests {
    use super::*;
    use mud_db::enums::UserRole;
    use mud_world::{ClassDef, Room};

    fn account(tag: &str) -> Account {
        Account {
            user_id: format!("u-{tag}"),
            character_id: format!("c-{tag}"),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        }
    }

    fn profile(level: i32, class_id: Option<i32>) -> Profile {
        Profile {
            level,
            class_id,
            race: "HUMAN".to_string(),
            experience: 0,
            gender: "neutral".to_string(),
        }
    }

    #[test]
    fn admin_http_level_set_is_exempt_from_ceiling_but_audited() {
        let mut world = World::new();
        world.spawn((
            Player,
            Online,
            account("boss"),
            Named {
                name: "Boss".to_string(),
            },
            profile(104, None),
        ));
        // No in-game actor: operator tooling may mint a 105 (database /
        // website equivalent), unlike in-game `advance` / `set`.
        let resp = set_player_field(&mut world, "Boss", "level", 105);
        assert!(resp.is_ok(), "{resp:?}");
        let level = world
            .query::<&Profile>()
            .iter(&world)
            .next()
            .map(|p| p.level);
        assert_eq!(level, Some(105));
        let role = world
            .query::<&Account>()
            .iter(&world)
            .next()
            .map(|a| a.role);
        assert_eq!(role, Some(UserRole::Implementor));
        assert!(
            world
                .resource::<commands::AdminAuditLog>()
                .entries
                .iter()
                .any(|e| e.verb == "staff_level_grant" && e.args == "Boss 105"),
            "HTTP staff-level grant must be audit-logged"
        );
    }

    #[test]
    fn players_lists_only_live_sessions() {
        let mut world = World::new();
        let mut classes = ClassCatalog::default();
        classes.by_id.insert(
            3,
            ClassDef {
                id: 3,
                name: "<b>Warrior</b>".to_string(),
                plain_name: "Warrior".to_string(),
                is_subclass: false,
                parent_class_id: None,
                description: None,
                hit_dice: "1d10".to_string(),
                primary_stat: None,
                hp_per_level: 10,
                exp_gain_factor: 1.0,
                alignment_bias: 0,
                campcraft_bonus: false,
                tags: Vec::new(),
                combat_rates: mud_db::characters::ClassCombatRates::default(),
                resistances: HashMap::new(),
            },
        );
        world.insert_resource(classes);
        let room = world.spawn((Room, WorldKey { zone: 30, id: 45 })).id();
        world.spawn((
            Player,
            Online,
            account("bob"),
            Named {
                name: "Bob".to_string(),
            },
            profile(25, Some(3)),
            Located(room),
            LoggedInAt(std::time::Instant::now()),
        ));
        world.spawn((
            Player,
            Online,
            account("alice"),
            Named {
                name: "Alice".to_string(),
            },
            profile(10, None),
        ));
        // Offline player entity (no Online marker) and a mob must not appear.
        world.spawn((
            Player,
            account("ghost"),
            Named {
                name: "Ghost".to_string(),
            },
            profile(5, None),
        ));
        world.spawn((
            Mob,
            Named {
                name: "rat".to_string(),
            },
        ));

        let v = online_players(&mut world);
        assert_eq!(v["count"], 2);
        let players = v["players"].as_array().expect("players array");
        assert_eq!(players[0]["name"], "Alice");
        assert!(players[0]["class"].is_null());
        assert!(players[0]["room"].is_null());
        assert_eq!(players[1]["name"], "Bob");
        assert_eq!(players[1]["level"], 25);
        assert_eq!(players[1]["class"], "Warrior");
        assert_eq!(players[1]["race"], "HUMAN");
        assert_eq!(players[1]["room"]["zone_id"], 30);
        assert_eq!(players[1]["room"]["id"], 45);
        assert!(players[1]["idle_seconds"].is_u64());
        assert!(players[1]["connected_seconds"].is_u64());
    }

    #[test]
    fn players_empty_when_nobody_online() {
        let mut world = World::new();
        let v = online_players(&mut world);
        assert_eq!(v["count"], 0);
        assert_eq!(v["players"].as_array().map(Vec::len), Some(0));
    }
}

#[cfg(test)]
mod actor_lookup_tests {
    use super::*;

    fn spawn(world: &mut World, name: &str, player: bool) -> Entity {
        let named = Named {
            name: name.to_string(),
        };
        if player {
            world.spawn((Player, named)).id()
        } else {
            world.spawn((Mob, named)).id()
        }
    }

    #[test]
    fn exact_name_wins_over_longer_prefix_match() {
        let mut w = World::new();
        let sam = spawn(&mut w, "Sam", true);
        let _samui = spawn(&mut w, "Samui", true);
        assert_eq!(find_actor_by_name(&mut w, "sam").unwrap(), sam);
    }

    #[test]
    fn exact_player_beats_exact_mob() {
        let mut w = World::new();
        let _mob = spawn(&mut w, "Sam", false);
        let player = spawn(&mut w, "Sam", true);
        assert_eq!(find_actor_by_name(&mut w, "SAM").unwrap(), player);
    }

    #[test]
    fn unique_prefix_resolves_substring_does_not() {
        let mut w = World::new();
        let samui = spawn(&mut w, "Samui", true);
        assert_eq!(find_actor_by_name(&mut w, "sam").unwrap(), samui);
        assert_eq!(
            find_actor_by_name(&mut w, "amui").unwrap_err().0,
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn ambiguous_prefix_is_refused_and_lists_candidates() {
        let mut w = World::new();
        spawn(&mut w, "Samui", true);
        spawn(&mut w, "Samantha", true);
        let (code, msg) = find_actor_by_name(&mut w, "sam").unwrap_err();
        assert_eq!(code, StatusCode::CONFLICT);
        assert!(msg.contains("Samui") && msg.contains("Samantha"), "{msg}");
        // The write path refuses too.
        assert!(set_player_field(&mut w, "sam", "hp", 1).is_err());
    }
}

#[cfg(test)]
mod teleport_fight_tests {
    use super::*;
    use mud_world::{Fighting, Located, Named};

    #[test]
    fn admin_teleport_ends_the_fight_both_ways() {
        let mut w = World::new();
        w.insert_resource(WorldKeyIndex::default());
        let from = w.spawn_empty().id();
        let to = w.spawn_empty().id();
        w.resource_mut::<WorldKeyIndex>().rooms.insert((30, 2), to);
        let foe = w.spawn(Located(from)).id();
        let target = w
            .spawn((
                Mob,
                Named {
                    name: "Wolf".to_string(),
                },
                Located(from),
                Fighting(foe),
            ))
            .id();
        w.entity_mut(foe).insert(Fighting(target));
        teleport(&mut w, "wolf", 30, 2).unwrap();
        assert_eq!(w.get::<Located>(target).map(|l| l.0), Some(to));
        assert!(w.get::<Fighting>(target).is_none());
        assert!(w.get::<Fighting>(foe).is_none());
    }
}

#[cfg(test)]
mod auth_token_tests {
    use super::*;

    fn bearer(value: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(v) = value {
            h.insert("authorization", v.parse().unwrap());
        }
        h
    }

    #[test]
    fn empty_token_is_unset_not_a_bypass_secret() {
        // ADMIN_TOKEN="" and ADMIN_TOKEN="   " both resolve to "no token".
        assert_eq!(crate::envflags::non_blank(Some(String::new())), None);
        assert_eq!(crate::envflags::non_blank(Some(" \t ".into())), None);
    }

    #[test]
    fn blank_expected_token_never_authenticates() {
        // Defence in depth: even if a blank token reached the checker, a
        // header-less request (got == "") must not match it.
        assert!(check_bearer(Some(""), &bearer(None)).is_err());
        assert!(check_bearer(Some("  "), &bearer(Some("Bearer   "))).is_err());
        assert!(check_bearer(Some(""), &bearer(Some("Bearer "))).is_err());
    }

    #[test]
    fn real_token_requires_exact_bearer() {
        assert!(check_bearer(Some("s3cret"), &bearer(None)).is_err());
        assert!(check_bearer(Some("s3cret"), &bearer(Some("Bearer nope"))).is_err());
        assert!(check_bearer(Some("s3cret"), &bearer(Some("Bearer s3cret"))).is_ok());
    }

    fn with_host(host: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("host", host.parse().unwrap());
        h
    }

    #[test]
    fn no_token_configured_passes_for_loopback_hosts() {
        for host in [
            "127.0.0.1",
            "127.0.0.1:8080",
            "localhost",
            "LOCALHOST:8080",
            "[::1]:8080",
        ] {
            assert!(check_bearer(None, &with_host(host)).is_ok(), "{host}");
        }
    }

    #[test]
    fn no_token_configured_rejects_rebinding_hosts() {
        for host in [
            "evil.example.com",
            "evil.example.com:8080",
            "127.0.0.1.evil.com",
            "localhost.evil.com:8080",
            "127.0.0.1:",
            "127.0.0.1:80x",
            "0.0.0.0:8080",
            "[::1]x",
        ] {
            assert!(check_bearer(None, &with_host(host)).is_err(), "{host}");
        }
        assert!(check_bearer(None, &bearer(None)).is_err(), "missing Host");
    }

    #[test]
    fn configured_token_ignores_the_host_header() {
        let mut h = with_host("admin.internal:8080");
        h.insert("authorization", "Bearer s3cret".parse().unwrap());
        assert!(check_bearer(Some("s3cret"), &h).is_ok());
    }
}

#[cfg(test)]
mod command_dispatch_tests {
    use super::*;
    use mud_db::enums::UserRole;
    use mud_world::{Account, Fighting, Health, Located, Named, Profile, Room};

    fn pool() -> PgPool {
        mud_db::sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(200))
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap()
    }

    /// A world with one virtual session named "Tester" in a quiet room.
    fn world_with_session() -> (World, Entity) {
        let mut world = World::new();
        world.init_resource::<mud_world::SocialRegistry>();
        world.init_resource::<VirtualSessions>();
        let room = world
            .spawn((
                Room,
                Named {
                    name: "A quiet hall".into(),
                },
                mud_world::Exits::default(),
            ))
            .id();
        let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
        let player = world
            .spawn((
                Player,
                Named {
                    name: "Tester".into(),
                },
                Located(room),
                crate::commands::Connection(tx),
                Account {
                    user_id: "u".into(),
                    character_id: "c".into(),
                    role: UserRole::Player,
                    account_role: UserRole::Player,
                    perms: vec![],
                },
                Health { hp: 100, max: 100 },
                Profile {
                    level: 10,
                    class_id: None,
                    race: "HUMAN".into(),
                    experience: 0,
                    gender: "male".into(),
                },
            ))
            .id();
        world
            .resource::<VirtualSessions>()
            .by_name
            .lock()
            .unwrap()
            .insert("Tester".into(), VirtualSession { entity: player, rx });
        (world, player)
    }

    fn output(resp: AdminResponse) -> String {
        resp.expect("command ok")["output"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_only_visit_runs_and_is_refused_while_fighting() {
        let (mut world, player) = world_with_session();
        let foe = world.spawn_empty().id();
        world.entity_mut(player).insert(Fighting(foe));
        let out = output(run_command(&mut world, &pool(), "Tester", "visit tester").await);
        assert!(!out.contains("async-only"), "{out}");
        assert!(!out.contains("DB error"), "reached the database: {out}");
        assert!(out.contains("can't"), "{out}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_only_save_gives_its_normal_output() {
        let (mut world, _player) = world_with_session();
        // The pool is unreachable, so the checkpoint rolls back: the
        // handler's own failure line, not the async-only stub text.
        let out = output(run_command(&mut world, &pool(), "Tester", "save").await);
        assert!(!out.contains("async-only"), "{out}");
        assert!(out.contains("Save failed"), "{out}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_commands_still_work() {
        let (mut world, _player) = world_with_session();
        let out = output(run_command(&mut world, &pool(), "Tester", "look").await);
        assert!(out.contains("A quiet hall"), "{out}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn earlier_output_comes_back_as_pending_not_as_this_commands() {
        let (mut world, player) = world_with_session();
        // Queued between commands: a mob arrival, say.
        commands::send_to(&world, player, "A rat arrives.\r\n");
        let resp = run_command(&mut world, &pool(), "Tester", "look")
            .await
            .expect("command ok");
        let pending = resp["pending_output"].as_str().unwrap();
        let out = resp["output"].as_str().unwrap();
        assert!(pending.contains("A rat arrives."), "{pending}");
        assert!(!out.contains("A rat arrives."), "{out}");
        assert!(out.contains("A quiet hall"), "{out}");
        // The next command has nothing pending.
        let resp = run_command(&mut world, &pool(), "Tester", "look")
            .await
            .expect("command ok");
        assert_eq!(resp["pending_output"].as_str().unwrap(), "");
    }

    /// Queue `command` requests for "Tester" and hand back their replies.
    fn queue_commands(
        world: &mut World,
        commands: &[&str],
    ) -> Vec<oneshot::Receiver<AdminResponse>> {
        let (tx, rx) = mpsc::channel(ADMIN_QUEUE_CAP);
        let replies = commands
            .iter()
            .map(|command| {
                let (reply, reply_rx) = oneshot::channel();
                tx.try_send(AdminCommand {
                    request: AdminRequest::Command {
                        executor: "Tester".into(),
                        command: (*command).into(),
                    },
                    reply,
                })
                .ok()
                .unwrap();
                reply_rx
            })
            .collect();
        world.insert_resource(AdminInbox(Mutex::new(rx)));
        replies
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_off_tick_command_is_answered_when_its_work_lands_not_before() {
        let (mut world, _player) = world_with_session();
        world.init_resource::<commands::off_tick::CommandJobs>();
        world.init_resource::<crate::autosave::SaveCoordinator>();
        world.insert_resource(WorldPause::default());
        // `save` writes to an unreachable database: the pool gives up after
        // 200 ms, off the tick.
        let mut reply = queue_commands(&mut world, &["save"]).remove(0);
        let started = std::time::Instant::now();
        drain_admin_requests(&mut world, &pool()).await;
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "the tick waited for the database: {:?}",
            started.elapsed()
        );
        assert!(reply.try_recv().is_err(), "answered before the work landed");
        // A later tick finds the work done and answers with the output.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        drain_admin_requests(&mut world, &pool()).await;
        let resp = reply.try_recv().expect("answered").expect("command ok");
        assert_eq!(resp["pending_output"], "");
        let out = resp["output"].as_str().unwrap();
        assert!(out.contains("Save failed"), "{out}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn two_commands_for_one_session_each_get_their_own_output() {
        let (mut world, player) = world_with_session();
        world.init_resource::<commands::off_tick::CommandJobs>();
        world.init_resource::<crate::autosave::SaveCoordinator>();
        world.insert_resource(WorldPause::default());
        let mut replies = queue_commands(&mut world, &["save", "look"]);
        drain_admin_requests(&mut world, &pool()).await;
        assert!(replies.iter_mut().all(|r| r.try_recv().is_err()));
        // The save lands; `look` is still queued behind it. The first reply
        // is the save's alone.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        drain_admin_requests(&mut world, &pool()).await;
        let first = replies[0].try_recv().expect("save answered").unwrap();
        let out = first["output"].as_str().unwrap();
        assert!(
            out.contains("Save failed") && !out.contains("A quiet hall"),
            "{out}"
        );
        assert!(replies[1].try_recv().is_err(), "look has not run yet");
        // The input queue releases `look`; the next pass answers it.
        assert!(commands::run_queued_input(&mut world, player, &pool()).await);
        drain_admin_requests(&mut world, &pool()).await;
        let second = replies[1].try_recv().expect("look answered").unwrap();
        let out = second["output"].as_str().unwrap();
        assert!(
            out.contains("A quiet hall") && !out.contains("Save failed"),
            "{out}"
        );
    }

    fn set_tick(world: &mut World, tick: u64) {
        world.insert_resource(crate::TickCount(tick));
    }

    /// A line sent during a wait state (fear's lag) is queued, the response
    /// says so, and the tick system runs it once the wait lifts.
    #[tokio::test(flavor = "current_thread")]
    async fn a_line_sent_during_a_wait_state_runs_when_the_wait_ends() {
        let (mut world, player) = world_with_session();
        set_tick(&mut world, 100);
        commands::input_queue::impose_wait(&mut world, player, 30);
        let resp = run_command(&mut world, &pool(), "Tester", "look")
            .await
            .expect("command ok");
        assert_eq!(resp["output"], "");
        assert_eq!(resp["queued_lines"], 1);
        assert!(resp["note"].as_str().unwrap().contains("queued"));
        // Still waiting: nothing is released.
        set_tick(&mut world, 120);
        drain_virtual_input_queues(&mut world, &pool()).await;
        assert!(commands::has_queued_input(&world, player));
        assert_eq!(drain_session_output(&world, "Tester"), "");
        // The wait is over: one line per tick runs, output is buffered.
        set_tick(&mut world, 130);
        drain_virtual_input_queues(&mut world, &pool()).await;
        assert!(!commands::has_queued_input(&world, player));
        // The next command collects it as pending_output.
        let next = run_command(&mut world, &pool(), "Tester", "look")
            .await
            .expect("command ok");
        let pending = next["pending_output"].as_str().unwrap();
        assert!(pending.contains("A quiet hall"), "{pending}");
        assert!(next.get("queued_lines").is_none());
    }

    /// An alias chain typed during a wait drains one line per tick, in order.
    #[tokio::test(flavor = "current_thread")]
    async fn an_alias_chain_on_a_virtual_session_drains_one_line_per_tick() {
        let (mut world, player) = world_with_session();
        world.entity_mut(player).insert(mud_world::Aliases {
            entries: vec![("both".into(), "look;say hello".into())],
        });
        set_tick(&mut world, 100);
        commands::input_queue::impose_wait(&mut world, player, 5);
        let resp = run_command(&mut world, &pool(), "Tester", "both")
            .await
            .expect("command ok");
        assert_eq!(resp["queued_lines"], 2);
        set_tick(&mut world, 105);
        drain_virtual_input_queues(&mut world, &pool()).await;
        let first = drain_session_output(&world, "Tester");
        assert!(first.contains("A quiet hall"), "{first}");
        assert!(!first.contains("hello"), "{first}");
        drain_virtual_input_queues(&mut world, &pool()).await;
        let second = drain_session_output(&world, "Tester");
        assert!(second.contains("hello"), "{second}");
        assert!(!commands::has_queued_input(&world, player));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_command_that_outlives_the_timeout_gets_a_gateway_timeout() {
        let (mut world, player) = world_with_session();
        world.init_resource::<AdminParked>();
        world
            .entity_mut(player)
            .insert(commands::off_tick::AsyncBusy);
        let (reply, mut rx) = oneshot::channel();
        world
            .resource::<AdminParked>()
            .0
            .lock()
            .unwrap()
            .push(Parked {
                waiting: Waiting {
                    executor: "Tester".into(),
                    entity: player,
                    command: "save".into(),
                    pending_output: String::new(),
                    own_line_queued: false,
                    deadline: std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(1))
                        .unwrap(),
                },
                reply,
            });
        answer_parked(&mut world);
        let err = rx.try_recv().expect("answered").unwrap_err();
        assert_eq!(err.0, StatusCode::GATEWAY_TIMEOUT);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_session_is_not_found() {
        let (mut world, _player) = world_with_session();
        let err = run_command(&mut world, &pool(), "Nobody", "look")
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }
}

#[cfg(test)]
mod drain_budget_tests {
    use super::*;

    /// Queue `n` pause requests; return the world, the reply receivers and
    /// the lazy (never connected) pool.
    fn setup(n: usize) -> (World, Vec<oneshot::Receiver<AdminResponse>>, PgPool) {
        let (tx, rx) = mpsc::channel(ADMIN_QUEUE_CAP);
        let mut world = World::new();
        world.insert_resource(AdminInbox(Mutex::new(rx)));
        world.insert_resource(WorldPause::default());
        let replies = (0..n)
            .map(|_| {
                let (reply, rx) = oneshot::channel();
                tx.try_send(AdminCommand {
                    request: AdminRequest::PauseWorld,
                    reply,
                })
                .ok()
                .unwrap();
                rx
            })
            .collect();
        let pool = mud_db::sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap();
        (world, replies, pool)
    }

    fn answered(replies: &mut [oneshot::Receiver<AdminResponse>]) -> usize {
        replies.iter_mut().filter_map(|r| r.try_recv().ok()).count()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_exhausted_budget_leaves_the_rest_in_the_inbox() {
        let (mut world, mut replies, pool) = setup(5);
        drain_within(&mut world, &pool, std::time::Duration::ZERO).await;
        assert_eq!(answered(&mut replies), 1, "one request always goes through");
        // The next tick picks the rest up.
        drain_within(&mut world, &pool, std::time::Duration::from_secs(5)).await;
        assert_eq!(answered(&mut replies), 4);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_normal_budget_drains_up_to_the_per_tick_cap() {
        let (mut world, mut replies, pool) = setup(MAX_PER_TICK + 3);
        drain_admin_requests(&mut world, &pool).await;
        assert_eq!(answered(&mut replies), MAX_PER_TICK);
    }
}

#[cfg(test)]
mod session_create_tests {
    use super::*;
    use mud_world::{Named, Room, WorldKeyIndex};

    fn pool() -> PgPool {
        mud_db::sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap()
    }

    fn user() -> User {
        User {
            id: "u".into(),
            email: "e".into(),
            display_name: String::new(),
            role: mud_db::enums::UserRole::Player,
            failed_login_attempts: 0,
            locked_until: None,
            account_wealth: 0,
        }
    }

    /// A world with the resources a session create reads, one room at (30, 1)
    /// and `n_protos` weapon prototypes (30, `0..n_protos`).
    fn world(n_protos: i32) -> World {
        let mut world = World::new();
        world.init_resource::<VirtualSessions>();
        world.init_resource::<mud_world::RaceDefaults>();
        world.init_resource::<mud_world::RaceCatalog>();
        world.init_resource::<WorldKeyIndex>();
        world.init_resource::<mud_world::ObjectPrototypes>();
        world.init_resource::<mud_world::TriggerCatalog>();
        world.init_resource::<mud_world::ObjectAbilityCatalog>();
        let room = world.spawn((Room, Named { name: "r".into() })).id();
        world
            .resource_mut::<WorldKeyIndex>()
            .rooms
            .insert((30, 1), room);
        for i in 0..n_protos {
            let proto = crate::commands::test_support::object_proto(
                30,
                i,
                mud_db::enums::ObjectType::Weapon,
            );
            world
                .resource_mut::<mud_world::ObjectPrototypes>()
                .by_key
                .insert((30, i), proto);
        }
        world
    }

    fn item_rows(n: i32, worn: i32) -> Vec<character_items::CharacterItemRow> {
        (0..n)
            .map(|i| {
                serde_json::from_value(json!({
                    "id": i + 1,
                    "character_id": "c",
                    "object_zone_id": 30,
                    "object_id": i,
                    "container_id": null,
                    "equipped_location": if i < worn { json!("BODY") } else { json!(null) },
                    "charges": -1,
                    "liquid_remaining": 0,
                    "liquid_type": null,
                    "lit": false,
                    "custom_name": null,
                    "custom_examine_description": null,
                    "custom_keywords": null,
                    "custom_curse": null,
                    "custom_container": null,
                }))
                .unwrap()
            })
            .collect()
    }

    fn create_request(name: &str, items: Vec<character_items::CharacterItemRow>) -> AdminRequest {
        let mut character = crate::login::tests::row(Some((30, 1)), None);
        character.name = name.into();
        AdminRequest::SessionCreate {
            player_name: name.into(),
            user: Box::new(user()),
            character: Box::new(character),
            items,
            abilities: vec![],
            aliases: vec![],
            achievements: vec![],
            script_vars_json: None,
            trophy_json: None,
            spell_cooldowns_json: None,
            cooldowns_json: None,
            ignore_list_json: None,
            command_grants_json: None,
            effect_instances_json: None,
        }
    }

    fn queue(
        world: &mut World,
        requests: Vec<AdminRequest>,
    ) -> Vec<oneshot::Receiver<AdminResponse>> {
        let (tx, rx) = mpsc::channel(ADMIN_QUEUE_CAP);
        world.insert_resource(AdminInbox(Mutex::new(rx)));
        world.insert_resource(WorldPause::default());
        requests
            .into_iter()
            .map(|request| {
                let (reply, rx) = oneshot::channel();
                tx.try_send(AdminCommand { request, reply }).ok().unwrap();
                rx
            })
            .collect()
    }

    fn answered(replies: &mut [oneshot::Receiver<AdminResponse>]) -> usize {
        replies.iter_mut().filter_map(|r| r.try_recv().ok()).count()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn creating_a_session_spawns_the_player_and_every_item() {
        let mut world = world(71);
        let mut replies = queue(&mut world, vec![create_request("Probe", item_rows(71, 20))]);
        drain_admin_requests(&mut world, &pool()).await;
        let resp = replies[0].try_recv().expect("answered").expect("created");
        assert_eq!(resp["items_loaded"], 71);
        let entity = {
            let sessions = world.resource::<VirtualSessions>();
            sessions
                .by_name
                .lock()
                .unwrap()
                .get("Probe")
                .unwrap()
                .entity
        };
        let mut carried = world.query_filtered::<&mud_world::Located, With<mud_world::Item>>();
        assert_eq!(carried.iter(&world).filter(|l| l.0 == entity).count(), 71);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_session_create_never_shares_a_tick_with_other_admin_work() {
        let mut world = world(2);
        let mut replies = queue(
            &mut world,
            vec![
                AdminRequest::PauseWorld,
                create_request("Probe", item_rows(2, 0)),
                AdminRequest::UnpauseWorld,
            ],
        );
        // Tick 1: the pause runs; the create is held back, the unpause waits
        // behind it.
        drain_admin_requests(&mut world, &pool()).await;
        assert_eq!(answered(&mut replies[..1]), 1);
        assert!(replies[1].try_recv().is_err(), "create must wait a tick");
        assert!(replies[2].try_recv().is_err());
        // Tick 2: the create runs alone.
        drain_admin_requests(&mut world, &pool()).await;
        assert!(replies[1].try_recv().expect("answered").is_ok());
        assert!(replies[2].try_recv().is_err(), "nothing rides along");
        // Tick 3: the rest of the queue.
        drain_admin_requests(&mut world, &pool()).await;
        assert!(replies[2].try_recv().expect("answered").is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_creates_run_one_per_tick() {
        let mut world = world(1);
        let mut replies = queue(
            &mut world,
            vec![
                create_request("One", item_rows(1, 0)),
                create_request("Two", item_rows(1, 0)),
            ],
        );
        drain_admin_requests(&mut world, &pool()).await;
        assert_eq!(answered(&mut replies), 1);
        drain_admin_requests(&mut world, &pool()).await;
        assert_eq!(answered(&mut replies), 1);
    }
}
