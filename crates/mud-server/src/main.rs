mod admin;
mod combat;
mod commands;
mod effects;
mod login;
mod memorize;
mod regen;
mod respawn;
mod syslog;
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
    if !tick.0.is_multiple_of(750) {
        return;
    }
    clock.hour += 1;
    if clock.hour >= 24 {
        clock.hour = 0;
        clock.day += 1;
    }
    if clock.day > 30 {
        clock.day = 1;
        clock.month += 1;
    }
    if clock.month > 12 {
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
    world.insert_resource(mud_world::MudClock::default());
    world.insert_resource(mud_script::LuaHost::default());

    if let Err(e) = mud_world::load_from_db(&mut world, &pool).await {
        error!(error = %e, "world load failed");
        return;
    }

    combat::seed_test_mobs(&mut world);
    combat::seed_test_items(&mut world);
    commands::validate_registry();
    // Fire LOAD-flagged triggers for every spawned mob now that the
    // world is fully populated (catalogs, prototypes, mob entities,
    // their AttachedTriggers). Bodies typically grant abilities or
    // emit greeting flavor text; running them up-front matches the
    // legacy "trigger on creation" semantics.
    triggers::fire_load_for_all_mobs(&mut world);

    let listen_addr =
        std::env::var("MUD_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:4003".into());
    let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<Inbound>();
    let listen_addr_for_task = listen_addr.clone();
    let inbound_tx_plain = inbound_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = mud_net::serve(&listen_addr_for_task, inbound_tx_plain).await {
            error!(addr = %listen_addr_for_task, error = %e, "listener stopped");
        }
    });

    // Optional TLS listener — enabled when both TLS_CERT_PATH and
    // TLS_KEY_PATH point at PEM files. Cert is a chain (server cert
    // first, then intermediates); key is PKCS#8 / RSA / SEC1 PEM.
    if let (Ok(cert_path), Ok(key_path)) =
        (std::env::var("TLS_CERT_PATH"), std::env::var("TLS_KEY_PATH"))
    {
        let tls_addr = std::env::var("MUD_TLS_LISTEN_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:4443".into());
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
            )
            .await
            {
                error!(addr = %tls_addr_for_task, error = %e, "TLS listener stopped");
            }
        });
    } else {
        info!(
            "TLS disabled — set TLS_CERT_PATH and TLS_KEY_PATH to enable on \
             $MUD_TLS_LISTEN_ADDR (default 0.0.0.0:4443)"
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

    let mut router = ConnRouter::new();
    let mut schedule = Schedule::default();
    // drain_admin_requests is intentionally OUTSIDE the schedule so
    // pause/unpause/tick admin requests can still flow through while
    // the rest of the world is frozen. Everything else stops on pause.
    schedule.add_systems(
        (
            advance_tick,
            mud_clock_tick,
            combat::combat_tick,
            combat::corpse_decay_tick,
            effects::effects_tick,
            regen::regen_tick,
            regen::hunger_thirst_tick,
            regen::light_fuel_tick,
            weather::weather_tick,
            memorize::memorize_tick,
            respawn::respawn_tick,
            triggers::lua_coroutine_tick,
            log_heartbeat,
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
                }
                // After all systems for this tick have run, refresh
                // prompts for anyone who received output (combat hits,
                // effect fades, broadcasts, etc.).
                commands::flush_prompts(&world);
            }
            msg = inbound_rx.recv() => {
                let Some(msg) = msg else {
                    error!("inbound channel closed; shutting down");
                    break;
                };
                match msg.kind {
                    InboundKind::Connected { peer, outbound } => {
                        info!(conn_id = msg.conn, %peer, "client connected");
                        router.on_connect(msg.conn, outbound);
                    }
                    InboundKind::Line(text) => {
                        router.on_line(msg.conn, text, &pool, &mut world).await;
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

    info!(
        final_tick = world.resource::<TickCount>().0,
        live_connections = router.live_connections(),
        "fierymud-rs stopped"
    );
}
