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
use axum::extract::{Path, State};
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
pub struct AdminInbox(pub Mutex<mpsc::UnboundedReceiver<AdminCommand>>);

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
    },
    SessionDestroy { player_name: String },
    Command { executor: String, command: String },
    Teleport { player_name: String, zone_id: i32, room_id: i32 },
    Spawn { kind: String, zone_id: i32, id: i32, room_zone: i32, room_id: i32 },
    InspectMob { zone_id: i32, id: i32 },
    PauseWorld,
    UnpauseWorld,
    TickWorld { count: u32 },
}

pub type AdminResponse = Result<Value, (StatusCode, String)>;

#[derive(Clone)]
struct AppState {
    tx: mpsc::UnboundedSender<AdminCommand>,
    token: Option<Arc<String>>,
    pool: PgPool,
}

/// Spawn the HTTP listener on `ADMIN_LISTEN_ADDR` (default
/// `127.0.0.1:8080`). Returns the inbox receiver so the caller
/// can install it as a bevy resource. The pool is used by handlers
/// that need DB access (e.g. `session/create` loads the character
/// and its inventory before handing off to the world tick).
pub fn spawn_admin_server(pool: PgPool) -> mpsc::UnboundedReceiver<AdminCommand> {
    let (tx, rx) = mpsc::unbounded_channel::<AdminCommand>();
    let addr: SocketAddr = std::env::var("ADMIN_LISTEN_ADDR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1:8080".parse().expect("default admin addr parses"));
    let token = std::env::var("ADMIN_TOKEN").ok().map(Arc::new);
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
        .route("/api/admin/command", post(handle_command))
        .route("/api/admin/teleport", post(handle_teleport))
        .route("/api/admin/spawn", post(handle_spawn))
        .route("/api/admin/world/pause", post(handle_pause_world))
        .route("/api/admin/world/unpause", post(handle_unpause_world))
        .route("/api/admin/world/tick", post(handle_tick_world))
        .with_state(state)
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
    if got == expected.as_str() {
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
    state
        .tx
        .send(AdminCommand { request, reply: tx })
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "world tick channel closed".into()))?;
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
        AdminRequest::SessionCreate { player_name, user, character, items, abilities, aliases } => {
            session_create(world, &player_name, &user, &character, &items, &abilities, &aliases)
        }
        AdminRequest::SessionDestroy { player_name } => session_destroy(world, &player_name),
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
    }
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
        "posture": posture,
        "effects": effects,
    }))
}

#[allow(clippy::too_many_arguments)]
fn session_create(
    world: &mut World,
    player_name: &str,
    user: &User,
    character: &CharacterRow,
    items: &[character_items::CharacterItemRow],
    abilities: &[mud_db::character_abilities::CharacterAbilityRow],
    aliases: &[mud_db::character_aliases::CharacterAliasRow],
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
    let known_abilities = mud_world::KnownAbilities {
        entries: abilities
            .iter()
            .map(|r| (r.ability_id, r.proficiency, r.known))
            .collect(),
    };
    let ability_count = known_abilities.entries.len();
    let alias_set = mud_world::Aliases {
        entries: aliases
            .iter()
            .map(|r| (r.alias.clone(), r.command.clone()))
            .collect(),
    };
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
                },
                Posture(PostureKind::Standing),
            ));
            if let Some(keys) = trigger_keys {
                em.insert(AttachedTriggers(keys));
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
