use std::collections::HashMap;
use std::time::Duration;

use bevy_ecs::prelude::*;
use mud_net::{ConnId, Inbound, InboundKind, Outbound};
use tokio::signal;
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info, info_span, warn};
use tracing_subscriber::EnvFilter;

#[derive(Resource, Default)]
struct TickCount(u64);

#[derive(Resource, Default)]
struct PendingCommands(Vec<(ConnId, String)>);

#[derive(Resource, Default)]
struct ConnRegistry {
    senders: HashMap<ConnId, Outbound>,
}

fn advance_tick(mut tick: ResMut<TickCount>) {
    tick.0 += 1;
}

fn echo_system(mut pending: ResMut<PendingCommands>, registry: Res<ConnRegistry>) {
    for (conn_id, text) in pending.0.drain(..) {
        if let Some(sender) = registry.senders.get(&conn_id)
            && sender.send(format!("echo: {text}\r\n")).is_err()
        {
            warn!(conn_id, "outbound send failed (client gone?)");
        }
    }
}

fn log_heartbeat(tick: Res<TickCount>, registry: Res<ConnRegistry>) {
    if tick.0 % 600 == 0 {
        info!(
            tick = tick.0,
            connections = registry.senders.len(),
            "heartbeat"
        );
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
    world.insert_resource(PendingCommands::default());
    world.insert_resource(ConnRegistry::default());

    if let Err(e) = mud_world::load_from_db(&mut world, &pool).await {
        error!(error = %e, "world load failed");
        return;
    }

    let listen_addr =
        std::env::var("MUD_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:4103".into());
    let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<Inbound>();
    let listen_addr_for_task = listen_addr.clone();
    tokio::spawn(async move {
        if let Err(e) = mud_net::serve(&listen_addr_for_task, inbound_tx).await {
            error!(addr = %listen_addr_for_task, error = %e, "listener stopped");
        }
    });

    let mut schedule = Schedule::default();
    schedule.add_systems((advance_tick, echo_system, log_heartbeat).chain());

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
                let pending_disconnects = drain_inbound(&mut world, &mut inbound_rx);
                {
                    let span = info_span!("tick");
                    let _g = span.enter();
                    schedule.run(&mut world);
                }
                // Process disconnects after the tick so the same tick's outbound
                // sends still find their sender in the registry.
                if !pending_disconnects.is_empty() {
                    let registry = &mut world.resource_mut::<ConnRegistry>().senders;
                    for conn_id in pending_disconnects {
                        info!(conn_id, "client disconnected");
                        registry.remove(&conn_id);
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
        "fierymud-rs stopped"
    );
}

/// Drain pending net events into the world. Connected/Line are applied
/// immediately; Disconnected IDs are returned so callers can process them
/// *after* the tick (otherwise the same-tick echo system loses its sender).
fn drain_inbound(world: &mut World, rx: &mut mpsc::UnboundedReceiver<Inbound>) -> Vec<ConnId> {
    let mut disconnects = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        match msg.kind {
            InboundKind::Connected { peer, outbound } => {
                info!(conn_id = msg.conn, %peer, "client connected");
                world
                    .resource_mut::<ConnRegistry>()
                    .senders
                    .insert(msg.conn, outbound);
            }
            InboundKind::Line(text) => {
                world
                    .resource_mut::<PendingCommands>()
                    .0
                    .push((msg.conn, text));
            }
            InboundKind::Disconnected => {
                disconnects.push(msg.conn);
            }
        }
    }
    disconnects
}
