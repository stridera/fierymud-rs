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
    AppliedTo, EffectInstance, Exits, Health, Item, Located, Mob, Named, Online, Player, Posture,
    Profile, Stamina, WorldKey, WorldKeyIndex,
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
    },
    SessionDestroy { player_name: String },
    Command { executor: String, command: String },
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
        .route("/api/admin/session/create", post(handle_session_create))
        .route("/api/admin/session/destroy", post(handle_session_destroy))
        .route("/api/admin/command", post(handle_command))
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
    json_ok(
        enqueue(
            &state,
            AdminRequest::SessionCreate {
                player_name: body.player_name,
                user: Box::new(user),
                character: Box::new(character),
                items,
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
        AdminRequest::SessionCreate { player_name, user, character, items } => {
            session_create(world, &player_name, &user, &character, &items)
        }
        AdminRequest::SessionDestroy { player_name } => session_destroy(world, &player_name),
        AdminRequest::Command { executor, command } => run_command(world, &executor, &command),
    }
}

fn world_status(world: &mut World) -> Value {
    let tick = world.resource::<crate::TickCount>().0;
    let players = world
        .query_filtered::<(), (With<Player>, With<Online>)>()
        .iter(world)
        .count();
    let mobs = world.query_filtered::<(), With<Mob>>().iter(world).count();
    let items = world.query_filtered::<(), With<Item>>().iter(world).count();
    json!({
        "paused": false,
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

fn session_create(
    world: &mut World,
    player_name: &str,
    user: &User,
    character: &CharacterRow,
    items: &[character_items::CharacterItemRow],
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
