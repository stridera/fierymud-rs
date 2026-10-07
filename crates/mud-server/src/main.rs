mod admin;
mod aggression;
mod autosave;
mod camp;
mod casting;
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
mod layout;
mod login;
mod memorize;
mod prompt;
mod quest_dialogue;
mod quest_progress;
mod quest_triggers;
mod quest_vars;
mod regen;
mod respawn;
mod rest;
mod room_access;
mod shops;
mod sleep;
mod syslog;
mod terminal;
mod tick_stats;
mod triggers;
mod wander;
mod weather;

use std::io::IsTerminal;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use mud_net::{Inbound, InboundKind};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Notify, mpsc};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info, info_span, warn};
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
///     `SHOW_DICE_ROLLS` flag — every swing surfaces its dice tail.
///
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
    let within_hour = i64::try_from(tick.0 % 750).unwrap_or(0);
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

/// Route panics through `tracing` (ERROR, target `panic`) with message,
/// location, thread and a forced backtrace, then run the default hook so the
/// usual stderr report is kept. Must run after the subscriber is installed.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_string());
        let location = info.location().map_or_else(
            || "unknown".to_string(),
            |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
        );
        let thread = std::thread::current();
        let backtrace = std::backtrace::Backtrace::force_capture();
        error!(
            target: "panic",
            location = %location,
            thread = thread.name().unwrap_or("<unnamed>"),
            backtrace = %backtrace,
            "{message}"
        );
        default_hook(info);
    }));
}

/// Record a finished tick and emit the (rate-limited) slow-tick WARN naming
/// the slowest phase.
fn note_tick_finished(world: &mut World, tick_start: Instant) {
    let now = Instant::now();
    let total = now.saturating_duration_since(tick_start);
    if let Some(r) = world
        .resource_mut::<tick_stats::TickStats>()
        .finish_tick(now, total)
    {
        warn!(
            phase = r.phase,
            duration_ms = u64::try_from(r.duration.as_millis()).unwrap_or(u64::MAX),
            suppressed_since_last_warn = r.suppressed,
            "slow tick (>= {} ms); slowest phase shown",
            tick_stats::SLOW_TURN.as_millis()
        );
    }
}

/// Same for loop turns that aren't the tick (inbound commands, auth
/// completions): they block the single-threaded loop just the same.
fn note_slow_turn(world: &mut World, phase: &'static str, turn_start: Instant) {
    let now = Instant::now();
    let d = now.saturating_duration_since(turn_start);
    if let Some(r) = world
        .resource_mut::<tick_stats::TickStats>()
        .report_slow(now, phase, d)
    {
        warn!(
            phase = r.phase,
            duration_ms = u64::try_from(r.duration.as_millis()).unwrap_or(u64::MAX),
            suppressed_since_last_warn = r.suppressed,
            "slow loop turn (>= {} ms)",
            tick_stats::SLOW_TURN.as_millis()
        );
    }
}

/// Wrap a schedule system so the time it takes is credited to its name in
/// [`tick_stats::TickStats`]. Relies on the schedule being fully chained:
/// the marker runs immediately after the system, so the gap since the
/// previous marker is exactly that system's runtime.
macro_rules! timed {
    ($sys:path) => {
        ($sys, tick_stats::lap_after(stringify!($sys))).chain()
    };
}

#[tokio::main(flavor = "current_thread")]
#[allow(clippy::too_many_lines)]
async fn main() {
    // Start from RUST_LOG (or info), then *always* append a clamp on
    // rustls's handshake module to error. A public TLS port gets
    // scanners that send illegal handshakes (IP-as-SNI, corrupt
    // ClientHellos), and rustls logs each at WARN — library-internal
    // noise about clients doing illegal things, not operator-
    // actionable. Appending (rather than only defaulting) means the
    // clamp survives the deploy env setting RUST_LOG=info.
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive(
            "rustls::msgs::handshake=error"
                .parse()
                .expect("static rustls log directive is valid"),
        );
    tracing_subscriber::registry()
        .with(env_filter)
        // Text format `<ts> <LEVEL> [span:] <target>: <message> key=value ...`.
        // ANSI colours only on a real terminal so journald gets plain text
        // (the error digest parser strips ANSI regardless, for old lines).
        .with(tracing_subscriber::fmt::layer().with_ansi(std::io::stdout().is_terminal()))
        .with(syslog::SyslogLayer)
        .init();
    install_panic_hook();

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
    world.insert_resource(tick_stats::TickStats::default());
    world.insert_resource(autosave::SaveCoordinator::default());
    world.insert_resource(ServerStart(Instant::now()));
    // DevMode lives in the GameConfig `server.dev_mode` row so the
    // toggle persists across restarts. Boot initial value is `false`;
    // resolved against the DB once world load completes below.
    world.insert_resource(DevMode(false));
    world.insert_resource(mud_world::MudClock::default());
    world.insert_resource(mud_world::HousingIndex::default());
    world.insert_resource(respawn::MobRespawnTimers::default());
    world.insert_resource(aggression::AggressionFormulaCache::default());
    world.insert_resource(mud_script::LuaHost::default());
    // Install the skill-dispatch shim. The Lua corpus calls
    // `skills.execute(actor, "kick", target)` from combat AI; the
    // host crate doesn't depend on mud-server, so we hand it a
    // fn-ptr that routes to `invoke_ability` with kind=Skill.
    world.insert_resource(mud_script::SkillExecutor(Some(commands::lua_invoke_skill)));
    world.insert_resource(mud_script::SpellExecutor(Some(commands::lua_invoke_spell)));
    world.insert_resource(mud_script::ChantExecutor(Some(commands::lua_invoke_chant)));
    world.insert_resource(mud_script::SongExecutor(Some(commands::lua_invoke_song)));
    world.insert_resource(mud_script::AttackAllExecutor(Some(
        commands::lua_attack_all,
    )));

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
    // Lua bindings), hydrated from `CharacterQuest.variables` by the
    // world loader; `quest_var_flush_tick` drains dirty rows back to
    // the DB every 10s. Only fall back to an empty cache when the
    // loader did not install one.
    if !world.contains_resource::<mud_world::QuestVariableCache>() {
        world.insert_resource(mud_world::QuestVariableCache::default());
    }
    if let Err(e) = quest_dialogue::load_catalog(&mut world, &pool).await {
        tracing::warn!(error = %e, "dialogue catalog load failed");
    }
    // After load_from_db spawned mobs (Pass 5) and equipped them
    // from MobResetEquipment (Pass 6), apply each equipped item's
    // ObjectAffects / ObjectEffects / ObjectResistance bonuses
    // onto its mob. Without this, an L99 mob wielding a +5 sword
    // wouldn't get the +5 hitroll.
    let mob_entities: Vec<bevy_ecs::prelude::Entity> = {
        let mut q = world
            .query_filtered::<bevy_ecs::prelude::Entity, bevy_ecs::prelude::With<mud_world::Mob>>();
        q.iter(&world).collect()
    };
    for mob in mob_entities {
        equip_apply::recompute_equipped_for(&mut world, mob);
    }

    // K4 dead-spell audit: walk every SPELL in the catalog and warn
    // about content gaps (zero AbilityEffect rows OR an effect_type
    // with no dispatcher arm). Runs once at boot so the gap surfaces
    // in syslog without per-cast noise.
    commands::audit_dead_spells(&world);

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
    // Per-source-IP open-connection cap so one host can't fill every
    // slot above. `server.max_connections_per_ip` (default 10); non-
    // positive means unlimited.
    let max_per_ip = {
        let cfg = world.resource::<mud_world::RuntimeConfig>();
        let raw = cfg.get_i32("server", "max_connections_per_ip", 10);
        if raw > 0 {
            usize::try_from(raw).unwrap_or(usize::MAX)
        } else {
            usize::MAX
        }
    };
    let net_limits = mud_net::Limits::new(max_connections, max_per_ip);
    let listen_addr_for_task = listen_addr.clone();
    let inbound_tx_plain = inbound_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = mud_net::serve(&listen_addr_for_task, inbound_tx_plain, net_limits).await {
            error!(addr = %listen_addr_for_task, error = %e, "listener stopped");
        }
    });

    // Optional TLS listener — enabled when both TLS_CERT_PATH and
    // TLS_KEY_PATH point at PEM files AND `security.enable_tls` isn't
    // explicitly false. Cert is a chain (server cert first, then
    // intermediates); key is PKCS#8 / RSA / SEC1 PEM.
    let enable_tls =
        world
            .resource::<mud_world::RuntimeConfig>()
            .get_bool("security", "enable_tls", true);
    if !enable_tls {
        info!("TLS listener disabled by `security.enable_tls=false`");
    } else if let (Ok(cert_path), Ok(key_path)) = (
        std::env::var("TLS_CERT_PATH"),
        std::env::var("TLS_KEY_PATH"),
    ) {
        // TLS port: same precedence chain as plain telnet.
        let tls_addr = {
            let cfg = world.resource::<mud_world::RuntimeConfig>();
            let port = cfg.get_i32("server", "tls_port", 0);
            if port > 0 {
                format!("0.0.0.0:{port}")
            } else {
                std::env::var("MUD_TLS_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:4443".into())
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
                net_limits,
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
    // Completion channel for off-thread password hashing / verification
    // (bcrypt runs on the blocking pool; see login.rs `AuthDone`).
    let mut auth_rx = router
        .take_auth_rx()
        .expect("auth receiver is taken exactly once");
    let mut schedule = Schedule::default();
    // drain_admin_requests is intentionally OUTSIDE the schedule so
    // pause/unpause/tick admin requests can still flow through while
    // the rest of the world is frozen. Everything else stops on pause.
    schedule.add_systems(
        (
            (
                timed!(advance_tick),
                timed!(mud_clock_tick),
                timed!(casting::casting_tick),
                timed!(commands::info::pending_summon_tick),
                timed!(combat::combat_tick),
                timed!(combat::corpse_decay_tick),
                timed!(item_decay::item_decay_tick),
                timed!(effects::effects_tick),
                timed!(regen::regen_tick),
                timed!(regen::hunger_thirst_tick),
                timed!(regen::light_fuel_tick),
                timed!(regen::drunkenness_tick),
                timed!(drowning::drowning_tick),
                timed!(weather::weather_tick),
                timed!(weather::ambient_tick),
                timed!(sleep::mob_sleep_tick),
            )
                .chain(),
            (
                timed!(wander::wander_tick),
                timed!(wander::scavenger_tick),
                timed!(idle::idle_kick_tick),
                timed!(camp::camp_tick),
                timed!(memorize::memorize_tick),
                timed!(respawn::respawn_tick),
                timed!(triggers::lua_coroutine_tick),
                timed!(entity_vars::entity_var_flush_tick),
                timed!(quest_vars::quest_var_flush_tick),
                timed!(commands::drain_player_updates),
                timed!(combat::level_sweep_tick),
                timed!(quest_triggers::quest_sweep_tick),
                timed!(quest_triggers::quest_custom_lua_drain),
                timed!(events::events_poll_tick),
                timed!(events::drain_events_inbox),
                timed!(log_heartbeat),
            )
                .chain(),
        )
            .chain(),
    );

    let mut ticker = interval(Duration::from_millis(1000 / TICK_HZ));
    let mut last_auth_sync = std::time::Instant::now();
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // One long-lived listener for SIGINT and SIGTERM. Handlers are
    // installed here (before the loop) and `Notify::notify_one` stores
    // a permit, so a signal that lands mid-tick is still seen the next
    // time the loop polls `shutdown.notified()`.
    let shutdown = Arc::new(Notify::new());
    {
        let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        let shutdown = Arc::clone(&shutdown);
        tokio::spawn(async move {
            tokio::select! {
                _ = sigint.recv() => info!("SIGINT received"),
                _ = sigterm.recv() => info!("SIGTERM received"),
            }
            shutdown.notify_one();
        });
    }

    info!(
        rate_hz = TICK_HZ,
        listen_addr = %listen_addr,
        "tick loop running; Ctrl-C / SIGTERM to stop"
    );

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let span = info_span!("tick");
                let _g = span.enter();
                let tick_start = Instant::now();
                world.resource_mut::<tick_stats::TickStats>().begin_turn(tick_start);
                // Always drain admin requests first — pause/unpause/
                // tick must flow even while the rest of the world is
                // frozen. The drain consumes any forced-tick budget
                // posted by /api/admin/world/tick.
                admin::drain_admin_requests(&mut world);
                tick_stats::lap(&mut world, "admin_drain");
                // Tell mud-net which connections have finished login
                // so they leave the pre-login timeout. Wall-clock
                // paced (not tick-paced) so a paused world still
                // syncs; once a second is far inside the 120s
                // pre-login idle window.
                if last_auth_sync.elapsed() >= Duration::from_secs(1) {
                    last_auth_sync = std::time::Instant::now();
                    idle::sync_authenticated(&mut world, &router);
                    tick_stats::lap(&mut world, "auth_sync");
                }
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
                    // any save work doesn't get re-entered by the
                    // schedule's effects/regen ticks.
                    //
                    // Never awaits the database: once a second it
                    // snapshots the (at most two) characters whose own
                    // save is older than the interval and spawns their
                    // writes (see `autosave.rs`). The old code saved
                    // every online player serially, awaited inline, so
                    // the whole world froze for the full duration.
                    {
                        let tick = world.resource::<TickCount>().0;
                        if tick.is_multiple_of(TICK_HZ) {
                            let autosave_secs = world
                                .resource::<mud_world::RuntimeConfig>()
                                .get_i32("server", "auto_save_interval_seconds", 300)
                                .max(10); // floor to avoid pathological config
                            router.autosave_tick(
                                &mut world,
                                &pool,
                                Duration::from_secs(u64::from(
                                    u32::try_from(autosave_secs).unwrap_or(300),
                                )),
                            );
                        }
                    }
                    tick_stats::lap(&mut world, "autosave");
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
                    // the markers here and hand each player to the same
                    // background writer the autosave uses (so a script
                    // calling save() can't stall the tick). The marker
                    // survives until a write is handed off and is
                    // re-armed if that write fails (see
                    // `login::drain_pending_saves`).
                    login::drain_pending_saves(&mut world, &pool);
                    tick_stats::lap(&mut world, "pending_save");
                    // Drain idle-kick markers before flushing prompts
                    // so the kick notice lands ahead of the prompt
                    // refresh and the disconnect path runs cleanly
                    // through the canonical on_disconnect save flow.
                    idle::drain_idle_kicks(&mut world, &mut router, &pool).await;
                    tick_stats::lap(&mut world, "idle_kicks");
                }
                // Drain real-time syslog WARN+ events to subscribers
                // before the prompt flush so any pushed lines land
                // ahead of the next prompt refresh and appear cleanly
                // separated from gameplay output. Cheap when no one
                // is watching (early-out on empty subscriber list).
                commands::drain_syslog_to_watchers(&mut world);
                tick_stats::lap(&mut world, "syslog_drain");
                // After all systems for this tick have run, refresh
                // prompts for anyone who received output (combat hits,
                // effect fades, broadcasts, etc.).
                commands::flush_prompts(&mut world);
                tick_stats::lap(&mut world, "flush_prompts");
                note_tick_finished(&mut world, tick_start);
                // Admin `shutdown`: announce the countdown and, once it
                // expires, leave the loop so the save-everyone path below
                // runs before the process exits.
                if commands::shutdown_poll(&mut world) {
                    info!("admin shutdown requested; leaving tick loop");
                    break;
                }
            }
            Some(done) = auth_rx.recv() => {
                let turn_start = Instant::now();
                router.on_auth_done(done, &pool, &mut world).await;
                note_slow_turn(&mut world, "auth_done", turn_start);
            }
            msg = inbound_rx.recv() => {
                let Some(msg) = msg else {
                    error!("inbound channel closed; shutting down");
                    break;
                };
                let turn_start = Instant::now();
                let turn_name = match &msg.kind {
                    InboundKind::Connected { .. } => "inbound:connect",
                    InboundKind::Line(_) => "inbound:line",
                    InboundKind::Disconnected => "inbound:disconnect",
                    InboundKind::Gmcp { .. } => "inbound:gmcp",
                    _ => "inbound:other",
                };
                match msg.kind {
                    InboundKind::Connected { peer, outbound, output } => {
                        info!(conn_id = msg.conn, %peer, "client connected");
                        router.on_connect_with(msg.conn, outbound, Some(peer), output, &world);
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
                note_slow_turn(&mut world, turn_name, turn_start);
            }
            () = shutdown.notified() => {
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
    // Give connection writer tasks a moment to flush the last
    // announcements (e.g. the shutdown notice) before the process exits.
    tokio::time::sleep(Duration::from_millis(250)).await;

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
