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
use std::collections::VecDeque;
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
use mud_db::{characters, characters::CharacterRow, character_items, sqlx::PgPool, users, users::User};
use mud_net::Outbound;
use mud_world::{
    AppliedTo, AttachedTriggers, BoardLink, CombatStats, Description, EffectInstance, Exits,
    Health, Item, Keywords, LiquidContainer, Located, Mob, MobPrototypes, Named,
    ObjectPrototypes, Online, Player, Posture, PostureKind, Profile, Stamina, TriggerCatalog,
    WearableIn, WorldKey, WorldKeyIndex, wear_flags_primary_slot,
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
    pub rx: mpsc::UnboundedReceiver<Vec<u8>>,
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
    LookRoom { zone_id: i32, id: i32 },
    InspectActor { name: String },
    SessionCreate {
        player_name: String,
        user: Box<User>,
        character: Box<CharacterRow>,
        items: Vec<character_items::CharacterItemRow>,
        abilities: Vec<mud_db::character_abilities::CharacterAbilityRow>,
        aliases: Vec<mud_db::character_aliases::CharacterAliasRow>,
        script_vars_json: Option<serde_json::Value>,
        trophy_json: Option<serde_json::Value>,
        spell_cooldowns_json: Option<serde_json::Value>,
        cooldowns_json: Option<serde_json::Value>,
        ignore_list_json: Option<serde_json::Value>,
    },
    SessionDestroy { player_name: String },
    /// Mark an online (or virtual-session) player with `PendingSave`
    /// so the post-tick autosave loop checkpoints them within the
    /// next tick. Used by smoke tests to force the disconnect-save
    /// path without actually disconnecting; not part of the regular
    /// gameplay surface.
    MarkPendingSave { player_name: String },
    Command { executor: String, command: String },
    Teleport { player_name: String, zone_id: i32, room_id: i32 },
    Spawn { kind: String, zone_id: i32, id: i32, room_zone: i32, room_id: i32 },
    InspectMob { zone_id: i32, id: i32 },
    PauseWorld,
    UnpauseWorld,
    TickWorld { count: u32 },
    TriggerInfo { zone_id: Option<i32>, id: Option<i32> },
    TriggerErrors { limit: Option<usize> },
    TriggerStats,
    /// New catalog assembled from the DB by the HTTP handler;
    /// the world dispatch swaps the resource and re-applies room
    /// attachments. Mob/object spawns will pick up new trigger
    /// rows on their next respawn naturally.
    ReloadTriggers { catalog: Box<mud_world::TriggerCatalog> },
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
/// `127.0.0.1:8080`). Returns the inbox receiver so the caller
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
pub fn spawn_admin_server(pool: PgPool) -> mpsc::Receiver<AdminCommand> {
    let (tx, rx) = mpsc::channel::<AdminCommand>(ADMIN_QUEUE_CAP);
    let addr: SocketAddr = std::env::var("ADMIN_LISTEN_ADDR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1:8080".parse().expect("default admin addr parses"));
    let token = std::env::var("ADMIN_TOKEN").ok().map(Arc::new);
    let allow_unauth_local = std::env::var("ADMIN_ALLOW_UNAUTH_LOCAL")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);
    if token.is_none() && !addr.ip().is_loopback() && !allow_unauth_local {
        warn!(
            %addr,
            "admin HTTP refused: non-loopback bind without ADMIN_TOKEN. \
             Set ADMIN_TOKEN, or ADMIN_ALLOW_UNAUTH_LOCAL=true to bypass (dev only)."
        );
        return rx;
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
    rx
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/admin/world/status", get(handle_world_status))
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
        .route("/api/admin/triggers/{zone_id}/{id}", get(handle_trigger_info_one))
        .route("/api/admin/triggers/errors", get(handle_trigger_errors))
        .route("/api/admin/triggers/stats", get(handle_trigger_stats))
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
    let Some(expected) = state.token.as_ref() else {
        return Ok(());
    };
    let got = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if constant_time_eq(got.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "missing or invalid bearer token".into()))
    }
}

async fn enqueue(
    state: &AppState,
    request: AdminRequest,
) -> Result<Value, (StatusCode, String)> {
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
    rx.await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "no reply from world tick".into()))?
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
                format!("character '{}' references missing user_id", body.player_name),
            ));
        }
        Err(e) => return json_err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    let items = character_items::list_for(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    let abilities = mud_db::character_abilities::list_for(&state.pool, &character.id)
        .await
        .unwrap_or_default();
    let aliases = mud_db::character_aliases::list_for(&state.pool, &character.id)
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
                script_vars_json,
                trophy_json,
                spell_cooldowns_json,
                cooldowns_json,
                ignore_list_json,
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
        enqueue(&state, AdminRequest::SessionDestroy { player_name: body.player_name }).await,
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
        enqueue(&state, AdminRequest::MarkPendingSave { player_name: body.player_name }).await,
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
        Value::Number(n) => n.as_i64().and_then(|v| i32::try_from(v).ok()).ok_or_else(|| {
            serde::de::Error::custom("number out of range for i32")
        }),
        Value::String(s) => s
            .parse::<i32>()
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
            AdminRequest::Command { executor: body.executor, command: body.command },
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
            AdminRequest::TriggerInfo { zone_id: None, id: None },
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
/// synchronously. Bounded by [`MAX_PER_TICK`] so a request flood
/// can't starve gameplay systems.
const MAX_PER_TICK: usize = 32;

#[allow(clippy::needless_pass_by_value)]
pub fn drain_admin_requests(world: &mut World) {
    let mut pending: VecDeque<AdminCommand> = VecDeque::new();
    {
        let inbox = world.resource::<AdminInbox>();
        let mut rx = inbox.0.lock().expect("admin inbox poisoned");
        for _ in 0..MAX_PER_TICK {
            match rx.try_recv() {
                Ok(cmd) => pending.push_back(cmd),
                Err(_) => break,
            }
        }
    }
    while let Some(cmd) = pending.pop_front() {
        let resp = service(world, cmd.request);
        let _ = cmd.reply.send(resp);
    }
}

fn service(world: &mut World, req: AdminRequest) -> AdminResponse {
    match req {
        AdminRequest::WorldStatus => Ok(world_status(world)),
        AdminRequest::LookRoom { zone_id, id } => look_room(world, zone_id, id),
        AdminRequest::InspectActor { name } => inspect_actor(world, &name),
        AdminRequest::SessionCreate {
            player_name, user, character, items, abilities, aliases,
            script_vars_json, trophy_json, spell_cooldowns_json, cooldowns_json,
            ignore_list_json,
        } => session_create(
            world, &player_name, &user, &character, &items, &abilities, &aliases,
            script_vars_json, trophy_json, spell_cooldowns_json, cooldowns_json,
            ignore_list_json,
        ),
        AdminRequest::SessionDestroy { player_name } => session_destroy(world, &player_name),
        AdminRequest::MarkPendingSave { player_name } => mark_pending_save(world, &player_name),
        AdminRequest::Command { executor, command } => run_command(world, &executor, &command),
        AdminRequest::Teleport { player_name, zone_id, room_id } => {
            teleport(world, &player_name, zone_id, room_id)
        }
        AdminRequest::Spawn { kind, zone_id, id, room_zone, room_id } => {
            spawn_into(world, &kind, zone_id, id, room_zone, room_id)
        }
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
        AdminRequest::ReloadTriggers { catalog } => Ok(reload_triggers(world, *catalog)),
        AdminRequest::FireTrigger { zone_id, id, self_name, actor_name } => {
            fire_trigger(world, zone_id, id, &self_name, actor_name.as_deref())
        }
        AdminRequest::SetPlayerField { player_name, field, value } => {
            set_player_field(world, &player_name, &field, value)
        }
    }
}

/// Write a typed integer attribute on a player or mob entity.
/// `field` is checked against an explicit allowlist; unknown
/// names yield a 400 so a typo never silently no-ops. Saturating
/// casts on the i64 → narrower-int conversions to avoid wrap.
#[allow(clippy::too_many_lines)]
fn set_player_field(
    world: &mut World,
    player_name: &str,
    field: &str,
    value: i64,
) -> AdminResponse {
    let Some(entity) = find_actor_by_name(world, player_name) else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no actor matching '{player_name}'"),
        ));
    };
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
            } else {
                applied = false;
            }
        }
        "level" => {
            if let Some(mut p) = world.get_mut::<Profile>(entity) {
                p.level = v32.max(1);
            } else {
                applied = false;
            }
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
        "hit_roll" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.hit_roll = v32;
            } else {
                applied = false;
            }
        }
        "dmg_roll" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.dmg_roll = v32;
            } else {
                applied = false;
            }
        }
        "ac" => {
            if let Some(mut cs) = world.get_mut::<CombatStats>(entity) {
                cs.ac = v32;
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
                     hit_roll, dmg_roll, ac"
                ),
            ));
        }
    }
    if !applied {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "actor '{player_name}' has no '{field}' component to write"
            ),
        ));
    }
    Ok(json!({
        "ok": true,
        "actor": player_name,
        "field": field,
        "value": value,
    }))
}

/// Resolve an actor by case-insensitive name match against any
/// Mob or Player. Used by `fire_trigger` self/actor binding.
fn find_actor_by_name(world: &mut World, name: &str) -> Option<Entity> {
    let needle = name.to_ascii_lowercase();
    let mut q = world.query_filtered::<(Entity, &Named), bevy_ecs::prelude::Or<(With<Mob>, With<Player>)>>();
    q.iter(world)
        .find(|(_, n)| n.name.to_ascii_lowercase().contains(&needle))
        .map(|(e, _)| e)
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
    let Some(self_entity) = find_actor_by_name(world, self_name) else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no actor matching '{self_name}'"),
        ));
    };
    let actor_entity = match actor_name {
        Some(n) => match find_actor_by_name(world, n) {
            Some(e) => e,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    format!("no actor matching '{n}'"),
                ));
            }
        },
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
    json!({
        "paused": paused,
        "pending_forced_ticks": pending_ticks,
        "tick": tick,
        "online_players": players,
        "mobs": mobs,
        "items": items,
    })
}

fn look_room(world: &mut World, zone_id: i32, id: i32) -> AdminResponse {
    let Some(room) = world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(zone_id, id))
        .copied()
    else {
        return Err((StatusCode::NOT_FOUND, format!("room ({zone_id}, {id}) not loaded")));
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
        let mut q = world.query_filtered::<(Entity, &Located, &Named, Option<&WorldKey>), With<Item>>();
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

fn inspect_actor(world: &mut World, name: &str) -> AdminResponse {
    let needle = name.to_ascii_lowercase();
    let mut q = world
        .query_filtered::<(Entity, &Named), Or<(With<Mob>, With<Player>)>>();
    let entity = q
        .iter(world)
        .find(|(_, n)| n.name.to_ascii_lowercase().contains(&needle))
        .map(|(e, _)| e);
    let Some(entity) = entity else {
        return Err((StatusCode::NOT_FOUND, format!("no actor matching '{name}'")));
    };
    let actor_name = name_of(world, entity);
    let world_key = world
        .get::<WorldKey>(entity)
        .map(|wk| json!([wk.zone, wk.id]));
    let location = world
        .get::<Located>(entity)
        .and_then(|l| world.get::<WorldKey>(l.0).copied())
        .map(|wk| json!([wk.zone, wk.id]));
    let health = world.get::<Health>(entity).map(|h| json!({"hp": h.hp, "max": h.max}));
    let stamina = world
        .get::<Stamina>(entity)
        .map(|s| json!({"current": s.current, "max": s.max}));
    let posture = world.get::<Posture>(entity).map(|p| p.0.label().to_string());
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
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, applied)| applied.0 == entity)
            .map(|(eff, _)| {
                json!({
                    "name": eff.name,
                    "remaining_secs": eff.remaining_secs,
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
        "effects": effects,
    }))
}

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
    script_vars_json: Option<serde_json::Value>,
    trophy_json: Option<serde_json::Value>,
    spell_cooldowns_json: Option<serde_json::Value>,
    cooldowns_json: Option<serde_json::Value>,
    ignore_list_json: Option<serde_json::Value>,
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
        q.iter(world).any(|n| n.name.eq_ignore_ascii_case(player_name))
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
    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let outbound: Outbound = tx;
    let entity = crate::login::spawn_player(world, user, character, outbound);
    let item_count = crate::login::spawn_inventory(world, entity, items);
    // Match login::complete_login: attach KnownAbilities + Aliases +
    // Title + Description so commands that read those (invoke_ability's
    // skill lookup, alias expansion, etc.) work for virtual sessions.
    // Without this, every ability formula referencing `skill` resolves
    // to 0 because KnownAbilities is missing.
    let known_abilities = mud_world::KnownAbilities::from_rows(abilities);
    let ability_count = known_abilities.entries.len();
    let alias_set = mud_world::Aliases::from_rows(aliases);
    let alias_count = alias_set.entries.len();
    if let Ok(mut e) = world.get_entity_mut(entity) {
        e.insert(known_abilities);
        e.insert(alias_set);
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
        if let Some(json) = script_vars_json
            && let Ok(map) = serde_json::from_value::<
                std::collections::BTreeMap<String, String>,
            >(json)
            && !map.is_empty()
        {
            e.insert(mud_world::ScriptVars(map));
        }
        if let Some(json) = trophy_json
            && let Ok(entries) = serde_json::from_value::<
                std::collections::VecDeque<mud_world::TrophyEntry>,
            >(json)
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
        if let Some(json) = cooldowns_json
            && let Ok(map) = serde_json::from_value::<
                std::collections::HashMap<String, i64>,
            >(json)
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
                    now_inst + std::time::Duration::from_secs(u64::try_from(secs_left).unwrap_or(0)),
                );
            }
            if !cd.ready_at.is_empty() {
                e.insert(cd);
            }
        }
    }
    let mut by_name = world.resource::<VirtualSessions>().by_name.lock().expect("sessions poisoned");
    by_name.insert(
        player_name.to_string(),
        VirtualSession { entity, rx },
    );
    Ok(json!({
        "success": true,
        "player_name": player_name,
        "entity": format!("{:?}", entity),
        "items_loaded": item_count,
        "abilities_loaded": ability_count,
        "aliases_loaded": alias_count,
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
        let mut q = world
            .query_filtered::<(Entity, &Named), With<Player>>();
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

fn run_command(world: &mut World, executor: &str, command_line: &str) -> AdminResponse {
    let entity = {
        let sessions = world.resource::<VirtualSessions>();
        let by_name = sessions.by_name.lock().expect("sessions poisoned");
        by_name.get(executor).map(|s| s.entity)
    };
    let Some(entity) = entity else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no virtual session for '{executor}'; create one first"),
        ));
    };
    // Run the command. Any send_to / send_rendered the dispatch
    // performs flows through the entity's Connection (which we
    // replaced on session_create) into our captured rx.
    commands::dispatch(world, entity, command_line);
    // Drain whatever bytes accumulated. The mpsc is unbounded and
    // sends are sync, so by this point everything the command wrote
    // is sitting in the receiver.
    let mut buf = Vec::<u8>::new();
    {
        let sessions = world.resource::<VirtualSessions>();
        let mut by_name = sessions.by_name.lock().expect("sessions poisoned");
        if let Some(session) = by_name.get_mut(executor) {
            while let Ok(bytes) = session.rx.try_recv() {
                buf.extend_from_slice(&bytes);
            }
        }
    }
    Ok(json!({
        "success": true,
        "command": command_line,
        "output": String::from_utf8_lossy(&buf).to_string(),
    }))
}

/// Move an actor (player or mob) by name to the given room. Does not
/// require a virtual session — useful for setting up scenarios via
/// MCP before spawning a session at the destination. Looks up the
/// target by case-insensitive Named match.
fn teleport(world: &mut World, name: &str, zone_id: i32, room_id: i32) -> AdminResponse {
    let needle = name.to_ascii_lowercase();
    let entity = {
        let mut q = world
            .query_filtered::<(Entity, &Named), Or<(With<Mob>, With<Player>)>>();
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
    if let Some(mut l) = world.get_mut::<Located>(entity) {
        l.0 = room_entity;
    } else if let Ok(mut e) = world.get_entity_mut(entity) {
        e.insert(Located(room_entity));
    }
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
fn spawn_into(
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
            let hp = proto.rolled_hp();
            let dmg = proto.avg_damage();
            let trigger_keys = world
                .resource::<TriggerCatalog>()
                .mob_attachments
                .get(&(zone_id, id))
                .cloned();
            let mut em = world.spawn((
                Mob,
                Named { name: proto.name.clone() },
                Keywords(proto.keywords.clone()),
                Description(proto.room_description.clone()),
                WorldKey { zone: proto.zone_id, id: proto.id },
                Located(room_entity),
                Health { hp, max: hp },
                CombatStats {
                    hit_roll: proto.hit_roll,
                    dmg_roll: dmg,
                    ac: proto.armor_class,
                    alignment: proto.alignment,
                    ward_pct: proto.ward_percent,
                },
                Posture(PostureKind::Standing),
            ));
            if let Some(keys) = trigger_keys {
                em.insert(AttachedTriggers(keys));
            }
            if !proto.examine_description.trim().is_empty() {
                em.insert(mud_world::ExamineText(proto.examine_description.clone()));
            }
            let entity = em.id();
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
            let primary_slot = wear_flags_primary_slot(&proto.wear_flags);
            let trigger_keys = world
                .resource::<TriggerCatalog>()
                .object_attachments
                .get(&(zone_id, id))
                .cloned();
            let mut bundle = world.spawn((
                Item,
                Named { name: proto.name.clone() },
                Keywords(proto.keywords.clone()),
                WorldKey { zone: proto.zone_id, id: proto.id },
                Located(room_entity),
            ));
            if let Some(desc) = proto.examine_description.clone() {
                bundle.insert(Description(desc));
            }
            if let Some(s) = primary_slot {
                bundle.insert(WearableIn(s));
            }
            if let Some(board_id) = proto.board_id {
                bundle.insert(BoardLink(board_id));
            }
            if let Some(liq) = proto.liquid.clone() {
                bundle.insert(LiquidContainer {
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
            if let Some(keys) = trigger_keys {
                bundle.insert(AttachedTriggers(keys));
            }
            let entity = bundle.id();
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
        "hit_roll": p.hit_roll,
        "armor_class": p.armor_class,
        "wealth": p.wealth,
        "class_id": p.class_id,
    }))
}
