mod combat;
mod commands;
mod login;

use std::time::Duration;

use bevy_ecs::prelude::*;
use mud_net::{Inbound, InboundKind};
use tokio::signal;
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info, info_span};
use tracing_subscriber::EnvFilter;

use crate::login::ConnRouter;

#[derive(Resource, Default)]
pub(crate) struct TickCount(pub(crate) u64);

fn advance_tick(mut tick: ResMut<TickCount>) {
    tick.0 += 1;
}

fn log_heartbeat(tick: Res<TickCount>) {
    if tick.0 % 600 == 0 {
        info!(tick = tick.0, "heartbeat");
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let _ = dotenvy::dotenv();

    info!("fierymud-rs starting");

    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            error!("DATABASE_URL not set; aborting");
            return;
        }
    };

    let pool = match mud_db::connect(&database_url).await {
        Ok(p) => p,
        Err(e) => {
            error!(error = %e, "failed to connect to database");
            return;
        }
    };

    let mut world = World::new();
    world.insert_resource(TickCount::default());

    if let Err(e) = mud_world::load_from_db(&mut world, &pool).await {
        error!(error = %e, "world load failed");
        return;
    }

    combat::seed_test_mobs(&mut world);
    commands::validate_registry();

    let listen_addr =
        std::env::var("MUD_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:4003".into());
    let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<Inbound>();
    let listen_addr_for_task = listen_addr.clone();
    tokio::spawn(async move {
        if let Err(e) = mud_net::serve(&listen_addr_for_task, inbound_tx).await {
            error!(addr = %listen_addr_for_task, error = %e, "listener stopped");
        }
    });

    let mut router = ConnRouter::new();
    let mut schedule = Schedule::default();
    schedule.add_systems((advance_tick, combat::combat_tick, log_heartbeat).chain());

    const TICK_HZ: u64 = 10;
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
                schedule.run(&mut world);
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
                        router.on_disconnect(&mut world, msg.conn);
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
