mod admin;
mod camp;
mod combat;
mod commands;
mod corpses;
mod drowning;
mod effects;
mod entity_vars;
mod equip_apply;
mod events;
mod idle;
mod item_decay;
mod login;
mod memorize;
mod regen;
mod respawn;
mod shops;
mod sleep;
mod syslog;
mod wander;
mod quest_dialogue;
mod quest_triggers;
mod quest_vars;
mod triggers;
mod weather;

use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use mud_net::{Inbound, InboundKind};
use tokio::signal;
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info, info_span};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::login::ConnRouter;

pub(crate) const TICK_HZ: u64 = 10;

#[derive(Resource, Default)]
pub(crate) struct TickCount(pub(crate) u64);

#[derive(Resource)]
pub(crate) struct ServerStart(pub(crate) Instant);

/// Server-wide "dev mode" toggle for open playtest servers.
/// When ON:
///   - Every connected player is treated as Implementor for permission
///     checks (``is_staff`` returns true regardless of account role).
///   - ``show_dice_for`` returns true regardless of the per-player
///     SHOW_DICE_ROLLS flag — every swing surfaces its dice tail.
/// Enabled at boot via env var ``MUD_DEV_MODE=1``; flipped at runtime
/// via the ``devmode`` admin command. NEVER ship to prod with this on.
#[derive(Resource, Default)]
pub(crate) struct DevMode(pub(crate) bool);

// Bevy systems take their resources by value (Res<T> is a smart-pointer
// wrapper); clippy::needless_pass_by_value doesn't know the API.
#[allow(clippy::needless_pass_by_value)]
fn advance_tick(mut tick: ResMut<TickCount>) {
    tick.0 += 1;
}

#[allow(clippy::needless_pass_by_value)]
fn log_heartbeat(tick: Res<TickCount>) {
    if tick.0.is_multiple_of(600) {
        info!(tick = tick.0, "heartbeat");
    }
}

/// Advance the in-game clock. One game hour every 750 ticks
/// (~75s real time at 10 Hz = 1.25 minutes per game hour, ~32
/// game days per real hour). Wraps month → year on the 30th day,
/// year on the 12th month.
#[allow(clippy::needless_pass_by_value)]
fn mud_clock_tick(tick: Res<TickCount>, mut clock: ResMut<mud_world::MudClock>) {
    // Refresh wall-clock stamp every tick — cheap and lets Lua
    // `time.stamp` reads stay current without a separate system.
    clock.stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
        .unwrap_or(0);
    // Minute granularity is derived from the within-hour tick
    // position so a 75-second game hour resolves into 60 minute
    // boundaries (12.5 ticks per game minute averaged). The hour
    // advance below pins this back to 0 at the boundary.
    let within_hour = (tick.0 % 750) as i64;
    clock.minute = i32::try_from(within_hour * 60 / 750).unwrap_or(0);
    if !tick.0.is_multiple_of(750) {
        return;
    }
    clock.hour += 1;
    clock.minute = 0;
    if clock.hour >= 24 {
        clock.hour = 0;
        clock.day += 1;
    }
    if clock.day > 30 {
        clock.day = 1;
        clock.month += 1;
    }
    // 16-month calendar — four thematic months per season, see
    // `MudClock::month_name` / `Season`.
    if clock.month > 16 {
        clock.month = 1;
        clock.year += 1;
    }
}

#[tokio::main(flavor = "current_thread")]
#[allow(clippy::too_many_lines)]
async fn main() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .with(syslog::SyslogLayer)
        .init();

    let _ = dotenvy::dotenv();

    info!("fierymud-rs starting");

    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        error!("DATABASE_URL not set; aborting");
        return;
    };

    let Ok(pool) = mud_db::connect(&database_url)
        .await
        .inspect_err(|e| error!(error = %e, "failed to connect to database"))
    else {
        return;
    };

    let mut world = World::new();
    world.insert_resource(TickCount::default());
    world.insert_resource(ServerStart(Instant::now()));
    // DevMode lives in the GameConfig `server.dev_mode` row so the
    // toggle persists across restarts. Boot initial value is `false`;
    // resolved against the DB once world load completes below.
    world.insert_resource(DevMode(false));
    world.insert_resource(mud_world::MudClock::default());
    world.insert_resource(mud_world::HousingIndex::default());
    world.insert_resource(respawn::MobRespawnTimers::default());
    world.insert_resource(mud_script::LuaHost::default());
    // Install the skill-dispatch shim. The Lua corpus calls
    // `skills.execute(actor, "kick", target)` from combat AI; the
    // host crate doesn't depend on mud-server, so we hand it a
    // fn-ptr that routes to `invoke_ability` with kind=Skill.
    world.insert_resource(mud_script::SkillExecutor(Some(commands::lua_invoke_skill)));
    world.insert_resource(mud_script::SpellExecutor(Some(commands::lua_invoke_spell)));
    world.insert_resource(mud_script::ChantExecutor(Some(commands::lua_invoke_chant)));
    world.insert_resource(mud_script::SongExecutor(Some(commands::lua_invoke_song)));
    world.insert_resource(mud_script::AttackAllExecutor(Some(commands::lua_attack_all)));

    if let Err(e) = mud_world::load_from_db(&mut world, &pool).await {
        error!(error = %e, "world load failed");
        return;
    }
    // Wire CUSTOM_LUA sweep channel + dialogue catalog defaults
    // (Wave 4.5 / 4.11). Must happen before the dialogue load so
    // the resource is in place when the loader fills it.
    quest_triggers::init_resources(&mut world);
    // Wire the world-event catalog + poll inbox so the first
    // `events_poll_tick` (fires on tick 0) has somewhere to send
    // its result and the `drain_events_inbox` has a catalog to
    // mutate. Catalog starts empty; the first poll fills it.
    events::init_resources(&mut world);
    // Per-quest variable cache (`quest:setvar` / `quest:getvar`
    // Lua bindings). Empty by default; `quest_var_flush_tick`
    // drains dirty rows to the DB every 10s. Hydration is
    // implicit — the first `quest:getvar` against a freshly-
    // accepted quest sees no cache entry and returns nil, which
    // is the same result as a DB row with `variables = '{}'`.
    world.insert_resource(mud_world::QuestVariableCache::default());
    if let Err(e) = quest_dialogue::load_catalog(&mut world, &pool).await {
        tracing::warn!(error = %e, "dialogue catalog load failed");
    }
    // After load_from_db spawned mobs (Pass 5) and equipped them
    // from MobResetEquipment (Pass 6), apply each equipped item's
    // ObjectAffects / ObjectEffects / ObjectResistance bonuses
    // onto its mob. Without this, an L99 mob wielding a +5 sword
    // wouldn't get the +5 hitroll.
    let mob_entities: Vec<bevy_ecs::prelude::Entity> = {
        let mut q = world.query_filtered::<bevy_ecs::prelude::Entity, bevy_ecs::prelude::With<mud_world::Mob>>();
        q.iter(&world).collect()
    };
    for mob in mob_entities {
        equip_apply::recompute_equipped_for(&mut world, mob);
    }

    // Overlay persisted weather (if any) on top of the climate-default
    // state the loader populated. Silent no-op on first boot.
    weather::load_snapshot(&mut world);
    // Same for the in-game clock — without this, every restart snaps
    // back to year 1 / month 1 / day 1 / hour 12.
    weather::load_clock_snapshot(&mut world);
    // Recreate any corpses that were on the floor at last shutdown.
    // Needs prototypes + WorldKeyIndex which load_from_db populated.
    corpses::load_snapshot(&mut world);
    // Restore shop stock deltas from last shutdown so a server
    // restart doesn't silently refill every depleted shelf.
    shops::load_snapshot(&mut world);

    // Test fixtures (training dummy + rusty sword + healing potion in
    // The Void). Useful for development; surprising in production. Gate
    // behind an explicit env flag so a prod boot doesn't quietly carry
    // dev-only props.
    let seed_test_content = std::env::var("MUD_SEED_TEST_CONTENT")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);
    if seed_test_content {
        info!("MUD_SEED_TEST_CONTENT=true — seeding training dummy + test items");
        combat::seed_test_mobs(&mut world);
        combat::seed_test_items(&mut world);
    }
    // Resolve persistent DevMode from GameConfig (`server.dev_mode`).
    // Loud WARN banner when on so a forgotten flag leaves a paper
    // trail in syslog at every boot. Set on this host by inserting
    // the row; absent row = default off everywhere else.
    let dev_mode_db = world
        .resource::<mud_world::RuntimeConfig>()
        .get_bool("server", "dev_mode", false);
    if dev_mode_db {
        tracing::warn!("┌─────────────────────────────────────────────────────────┐");
        tracing::warn!("│ GameConfig server.dev_mode=ON — players are Implementor│");
        tracing::warn!("│ Admin commands open to anyone. Dice rolls visible.     │");
        tracing::warn!("│ DO NOT RUN THIS IN PRODUCTION.                         │");
        tracing::warn!("└─────────────────────────────────────────────────────────┘");
        world.insert_resource(DevMode(true));
    }
    commands::validate_registry();
    // Fire LOAD-flagged triggers for every spawned mob now that the
    // world is fully populated (catalogs, prototypes, mob entities,
    // their AttachedTriggers). Bodies typically grant abilities or
    // emit greeting flavor text; running them up-front matches the
    // legacy "trigger on creation" semantics.
    triggers::fire_load_for_all_mobs(&mut world);

    // Listen address precedence: GameConfig > env > hardcoded default.
    // GameConfig is the operator-facing source of truth; env still
    // works as a dev-time override (`MUD_LISTEN_ADDR=...`); hardcoded
    // default is the last-resort fallback when neither is set.
    let listen_addr = {
        let cfg = world.resource::<mud_world::RuntimeConfig>();
        let port = cfg.get_i32("server", "port", 0);
        if port > 0 {
            format!("0.0.0.0:{port}")
        } else {
            std::env::var("MUD_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:4003".into())
        }
    };
    // Inbound command queue cap. Reads from
    // `server.max_command_queue_size` GameConfig (default 4096); a
    // non-positive value falls back to the hardcoded
    // INBOUND_QUEUE_CAP. Operators tune this to absorb burst input
    // without growing memory unboundedly when the tick is slow.
    let inbound_cap = {
        let cfg = world.resource::<mud_world::RuntimeConfig>();
        let raw = cfg.get_i32(
            "server",
            "max_command_queue_size",
            i32::try_from(mud_net::INBOUND_QUEUE_CAP).unwrap_or(4096),
        );
        if raw > 0 {
            usize::try_from(raw).unwrap_or(mud_net::INBOUND_QUEUE_CAP)
        } else {
            mud_net::INBOUND_QUEUE_CAP
        }
    };
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<Inbound>(inbound_cap);
    // Total accepted-and-still-open connection cap, summed across
    // both listeners. Reads from `server.max_connections` GameConfig
    // (default 200, matching the existing imported row); a non-
    // positive value or missing row falls back to usize::MAX (no
    // limit) for dev / unrestricted operator override.
    let max_connections = {
        let cfg = world.resource::<mud_world::RuntimeConfig>();
        let raw = cfg.get_i32("server", "max_connections", 200);
        if raw > 0 {
            usize::try_from(raw).unwrap_or(usize::MAX)
        } else {
            usize::MAX
        }
    };
    let listen_addr_for_task = listen_addr.clone();
    let inbound_tx_plain = inbound_tx.clone();
    tokio::spawn(async move {
        if let Err(e) =
            mud_net::serve(&listen_addr_for_task, inbound_tx_plain, max_connections).await
        {
            error!(addr = %listen_addr_for_task, error = %e, "listener stopped");
        }
    });

    // Optional TLS listener — enabled when both TLS_CERT_PATH and
    // TLS_KEY_PATH point at PEM files AND `security.enable_tls` isn't
    // explicitly false. Cert is a chain (server cert first, then
    // intermediates); key is PKCS#8 / RSA / SEC1 PEM.
    let enable_tls = world
        .resource::<mud_world::RuntimeConfig>()
        .get_bool("security", "enable_tls", true);
    if !enable_tls {
        info!("TLS listener disabled by `security.enable_tls=false`");
    } else if let (Ok(cert_path), Ok(key_path)) =
        (std::env::var("TLS_CERT_PATH"), std::env::var("TLS_KEY_PATH"))
    {
        // TLS port: same precedence chain as plain telnet.
        let tls_addr = {
            let cfg = world.resource::<mud_world::RuntimeConfig>();
            let port = cfg.get_i32("server", "tls_port", 0);
            if port > 0 {
                format!("0.0.0.0:{port}")
            } else {
                std::env::var("MUD_TLS_LISTEN_ADDR")
                    .unwrap_or_else(|_| "0.0.0.0:4443".into())
            }
        };
        // Required by rustls 0.23+: install a default crypto provider
        // before any ServerConfig is built.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        info!(tls_addr = %tls_addr, cert = %cert_path, "TLS listener configured");
        let inbound_tx_tls = inbound_tx.clone();
        let tls_addr_for_task = tls_addr.clone();
        let cert_path_for_task = cert_path.clone();
        let key_path_for_task = key_path.clone();
        tokio::spawn(async move {
            if let Err(e) = mud_net::serve_tls(
                &tls_addr_for_task,
                &cert_path_for_task,
                &key_path_for_task,
                inbound_tx_tls,
                max_connections,
            )
            .await
            {
                error!(addr = %tls_addr_for_task, error = %e, "TLS listener stopped");
            }
        });
    } else {
        info!(
            "TLS disabled — set TLS_CERT_PATH and TLS_KEY_PATH to enable on \
             the configured tls_port (`server.tls_port` GameConfig row, \
             then $MUD_TLS_LISTEN_ADDR, then 0.0.0.0:4443)"
        );
    }
    drop(inbound_tx);

    // Spawn the admin HTTP listener and install its inbox + virtual
    // session table as resources so the world tick can drain pending
    // requests synchronously each frame.
    let admin_rx = admin::spawn_admin_server(pool.clone());
    world.insert_resource(admin::AdminInbox(std::sync::Mutex::new(admin_rx)));
    world.insert_resource(admin::VirtualSessions::default());
    world.insert_resource(admin::WorldPause::default());
    // Pool as a resource so sync command handlers can fire-and-forget
    // DB writes via tokio::spawn (e.g. `bug` / `idea` / `typo` reports
    // through any dispatch path, including the admin port's sync path
    // that doesn't go through try_dispatch_async).
    world.insert_resource(commands::DbPool(pool.clone()));
    // Channel for async tasks to push live ECS deltas back to the
    // world (quest reward grants, etc.). Tick drains the inbox.
    // Bounded so a flood of background tasks (e.g. mass quest
    // completion) can't grow memory without a cap; on overflow the
    // sending task awaits until the tick drains a slot.
    let (player_update_tx, player_update_rx) = tokio::sync::mpsc::channel::<
        commands::PendingPlayerUpdate,
    >(commands::PLAYER_UPDATE_QUEUE_CAP);
    world.insert_resource(commands::PlayerUpdateTx(player_update_tx));
    world.insert_resource(commands::PlayerUpdateInbox(std::sync::Mutex::new(
        player_update_rx,
    )));

    let mut router = ConnRouter::new();
    let mut schedule = Schedule::default();
    // drain_admin_requests is intentionally OUTSIDE the schedule so
    // pause/unpause/tick admin requests can still flow through while
    // the rest of the world is frozen. Everything else stops on pause.
    schedule.add_systems(
        (
            (
                advance_tick,
                mud_clock_tick,
                combat::combat_tick,
                combat::corpse_decay_tick,
                item_decay::item_decay_tick,
                effects::effects_tick,
                regen::regen_tick,
                regen::hunger_thirst_tick,
                regen::light_fuel_tick,
                regen::drunkenness_tick,
                drowning::drowning_tick,
                weather::weather_tick,
                weather::ambient_tick,
                sleep::mob_sleep_tick,
            )
                .chain(),
            (
                wander::wander_tick,
                wander::scavenger_tick,
                idle::idle_kick_tick,
                camp::camp_tick,
                memorize::memorize_tick,
                respawn::respawn_tick,
                triggers::lua_coroutine_tick,
                entity_vars::entity_var_flush_tick,
                quest_vars::quest_var_flush_tick,
                commands::drain_player_updates,
                quest_triggers::quest_sweep_tick,
                quest_triggers::quest_custom_lua_drain,
                events::events_poll_tick,
                events::drain_events_inbox,
                log_heartbeat,
            )
                .chain(),
        )
            .chain(),
    );

    let mut ticker = interval(Duration::from_millis(1000 / TICK_HZ));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    info!(
        rate_hz = TICK_HZ,
        listen_addr = %listen_addr,
        "tick loop running; Ctrl-C to stop"
    );

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let span = info_span!("tick");
                let _g = span.enter();
                // Always drain admin requests first — pause/unpause/
                // tick must flow even while the rest of the world is
                // frozen. The drain consumes any forced-tick budget
                // posted by /api/admin/world/tick.
                admin::drain_admin_requests(&mut world);
                let run_world = {
                    let mut p = world.resource_mut::<admin::WorldPause>();
                    if !p.paused {
                        true
                    } else if p.forced_ticks > 0 {
                        p.forced_ticks -= 1;
                        true
                    } else {
                        false
                    }
                };
                if run_world {
                    schedule.run(&mut world);
                    // Periodic autosave. Cadence reads from
                    // `server.auto_save_interval_seconds` GameConfig
                    // (default 300s = 5 min). Cheap insurance against
                    // crashes — a SIGKILL or a power loss would skip
                    // the graceful shutdown save_all_online path
                    // entirely. Done out-of-band of the schedule so
                    // any save_player work doesn't get re-entered by
                    // the schedule's effects/regen ticks.
                    {
                        let tick = world.resource::<TickCount>().0;
                        let autosave_secs = world
                            .resource::<mud_world::RuntimeConfig>()
                            .get_i32("server", "auto_save_interval_seconds", 300)
                            .max(10); // floor to avoid pathological config
                        let autosave_ticks = u64::from(u32::try_from(autosave_secs).unwrap_or(300))
                            * TICK_HZ;
                        if tick > 0 && tick.is_multiple_of(autosave_ticks) {
                            router.save_all_online(&mut world, &pool).await;
                            info!(tick, autosave_ticks, "periodic autosave");
                        }
                    }
                    // Periodic expiry of in-memory Discord-link
                    // verification codes. Cadence: every 30 simulated
                    // seconds (300 ticks at 10 Hz). A stalled link
                    // request shouldn't leave a code reservable
                    // forever — this is the only persistent cost of
                    // the link flow that needs aging.
                    //
                    // The LoginRequests-based approval flow that used
                    // to live on this tick was removed in favor of
                    // the per-character `Characters.name_approved`
                    // gate; that flag is staff-resolved through
                    // `approve_name` / `reject_name` and needs no
                    // periodic sweep.
                    {
                        let tick = world.resource::<TickCount>().0;
                        let expire_ticks = 30u64 * TICK_HZ;
                        if tick > 0 && tick.is_multiple_of(expire_ticks) {
                            let now = std::time::Instant::now();
                            let dropped = world
                                .resource_mut::<mud_world::PendingDiscordLinks>()
                                .expire_old(now);
                            if dropped > 0 {
                                info!(dropped, "pending discord-link codes aged off");
                            }
                        }
                    }
                    // Lua-requested saves: triggers can call
                    // `actor:save()` to checkpoint progress mid-tick.
                    // The Lua callback inserts a `PendingSave` marker
                    // since async DB writes can't run inline; we drain
                    // the markers here, save each player, and clear the
                    // marker. Plays well with the post-tick autosave
                    // above because every save_player is idempotent.
                    {
                        let pending_save: Vec<Entity> = {
                            let mut q = world
                                .query_filtered::<Entity, With<mud_world::PendingSave>>();
                            q.iter(&world).collect()
                        };
                        for e in pending_save {
                            if let Ok(mut em) = world.get_entity_mut(e) {
                                em.remove::<mud_world::PendingSave>();
                            }
                            // PendingSave is the autosave path —
                            // ignore the outcome (tracing::warn
                            // covers diagnostics).
                            let _ = login::save_player(&mut world, e, &pool).await;
                        }
                    }
                    // Drain idle-kick markers before flushing prompts
                    // so the kick notice lands ahead of the prompt
                    // refresh and the disconnect path runs cleanly
                    // through the canonical on_disconnect save flow.
                    let pending: Vec<Entity> = {
                        let mut q = world
                            .query_filtered::<Entity, With<idle::IdleKickPending>>();
                        q.iter(&world).collect()
                    };
                    for entity in pending {
                        if let Some(conn_id) = router.find_conn(entity) {
                            commands::send_to(
                                &world,
                                entity,
                                "\r\nYou have been idle for too long. Disconnecting.\r\n",
                            );
                            router.on_disconnect(&mut world, conn_id, &pool).await;
                        } else if let Ok(mut e) = world.get_entity_mut(entity) {
                            // Orphaned marker (e.g. the player despawned
                            // mid-tick somehow) — drop it so the next
                            // pass doesn't keep retrying.
                            e.remove::<idle::IdleKickPending>();
                        }
                    }
                }
                // Drain real-time syslog WARN+ events to subscribers
                // before the prompt flush so any pushed lines land
                // ahead of the next prompt refresh and appear cleanly
                // separated from gameplay output. Cheap when no one
                // is watching (early-out on empty subscriber list).
                commands::drain_syslog_to_watchers(&mut world);
                // After all systems for this tick have run, refresh
                // prompts for anyone who received output (combat hits,
                // effect fades, broadcasts, etc.).
                commands::flush_prompts(&mut world);
            }
            msg = inbound_rx.recv() => {
                let Some(msg) = msg else {
                    error!("inbound channel closed; shutting down");
                    break;
                };
                match msg.kind {
                    InboundKind::Connected { peer, outbound } => {
                        info!(conn_id = msg.conn, %peer, "client connected");
                        router.on_connect(msg.conn, outbound, &world);
                    }
                    InboundKind::Line(text) => {
                        router.on_line(msg.conn, text, &pool, &mut world).await;
                    }
                    InboundKind::WindowSize { cols, rows } => {
                        router.on_window_size(msg.conn, cols, rows, &mut world);
                    }
                    InboundKind::Terminal { index, value } => {
                        router.on_terminal(msg.conn, index, &value, &mut world);
                    }
                    InboundKind::Capability { name, on } => {
                        router.on_capability(msg.conn, name, on, &mut world);
                    }
                    InboundKind::Gmcp { package, payload } => {
                        router.on_gmcp(msg.conn, &package, &payload, &mut world).await;
                    }
                    InboundKind::Disconnected => {
                        info!(conn_id = msg.conn, "client disconnected");
                        router.on_disconnect(&mut world, msg.conn, &pool).await;
                    }
                }
            }
            _ = signal::ctrl_c() => {
                info!("shutdown signal received");
                break;
            }
        }
    }

    // Save every online player BEFORE persisting world snapshots —
    // their save path reads Located → WorldKey on the room they're
    // standing in, plus the items they're carrying. Doing this only
    // on `on_disconnect` meant Ctrl-C dropped the process without
    // ever firing the save and players lost progress.
    router.save_all_online(&mut world, &pool).await;

    // Persist weather state so the next boot picks up where we
    // left off instead of snapping back to climate defaults.
    weather::save_snapshot(&world);
    weather::save_clock_snapshot(&world);
    corpses::save_snapshot(&mut world);
    shops::save_snapshot(&world);

    info!(
        final_tick = world.resource::<TickCount>().0,
        live_connections = router.live_connections(),
        "fierymud-rs stopped"
    );
}
