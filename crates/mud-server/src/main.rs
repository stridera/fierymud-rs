use std::time::Duration;

use bevy_ecs::prelude::*;
use tokio::signal;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, info_span};
use tracing_subscriber::EnvFilter;

#[derive(Resource, Default)]
struct TickCount(u64);

fn advance_tick(mut tick: ResMut<TickCount>) {
    tick.0 += 1;
}

fn log_heartbeat(tick: Res<TickCount>) {
    if tick.0 % 10 == 0 {
        info!(tick = tick.0, "heartbeat");
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    info!("fierymud-rs starting");

    let mut world = World::new();
    world.insert_resource(TickCount::default());

    let mut schedule = Schedule::default();
    schedule.add_systems((advance_tick, log_heartbeat).chain());

    const TICK_HZ: u64 = 10;
    let mut ticker = interval(Duration::from_millis(1000 / TICK_HZ));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    info!(rate_hz = TICK_HZ, "tick loop running; Ctrl-C to stop");

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let span = info_span!("tick");
                let _g = span.enter();
                schedule.run(&mut world);
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
