//! Per-zone weather drift tick. Updates `WeatherCatalog` every
//! `WEATHER_TICK_TICKS` real-time ticks (≈one game-quarter-day) by
//! nudging each zone's `WeatherState` one band toward a random
//! climate-bounded target. Fully ephemeral — restart re-derives
//! state from each zone's `Climate`.

use bevy_ecs::prelude::*;
use mud_db::enums::Climate;
use mud_world::{
    PrecipKind, TempBand, WeatherCatalog, WeatherState, ZoneClimate,
};

use crate::TickCount;

/// One real-time minute (six game-hours). Loose enough that small
/// random drifts feel weather-like rather than chaotic, tight enough
/// that a player's session sees it change.
const WEATHER_TICK_TICKS: u64 = 600;

pub fn weather_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(WEATHER_TICK_TICKS) {
        return;
    }
    // Snapshot zone_id → Climate. WeatherCatalog stores by zone_id;
    // climate lives on a Zone entity's ZoneClimate component.
    let climates: Vec<(i32, Climate)> = {
        let mut q = world.query_filtered::<(&mud_world::WorldKey, &ZoneClimate), With<mud_world::Zone>>();
        q.iter(world).map(|(wk, c)| (wk.zone, c.0)).collect()
    };
    // Track zones whose precip changed so the post-tick pass can
    // broadcast "the sky shifts" lines to outdoor players in them.
    let mut precip_changes: Vec<(i32, PrecipKind, PrecipKind)> = Vec::new();
    {
        let mut weather = world.resource_mut::<WeatherCatalog>();
        for (zone_id, climate) in climates {
            let prev = weather
                .by_zone
                .entry(zone_id)
                .or_insert_with(|| mud_world::default_weather_for_climate(climate))
                .precip;
            let entry = weather.by_zone.get_mut(&zone_id).unwrap();
            entry.temp = drift_temp(entry.temp, climate);
            entry.precip = drift_precip(entry.precip, climate);
            if entry.precip != prev {
                precip_changes.push((zone_id, prev, entry.precip));
            }
        }
    }
    // Broadcast precip changes. Snapshot players in outdoor rooms
    // for each affected zone, then send a transition flavor line.
    for (zone_id, _prev, new_precip) in precip_changes {
        let outdoor_recipients: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &mud_world::Located), (
                With<mud_world::Player>,
                With<mud_world::Online>,
            )>();
            q.iter(world)
                .filter(|(_, l)| {
                    let room = l.0;
                    let zone_match = world
                        .get::<mud_world::WorldKey>(room)
                        .is_some_and(|k| k.zone == zone_id);
                    let outdoor = world
                        .get::<mud_world::RoomSector>(room)
                        .is_some_and(|s| crate::commands::sector_is_outdoor_for_weather(s.0));
                    zone_match && outdoor
                })
                .map(|(e, _)| e)
                .collect()
        };
        let line = transition_line(new_precip);
        for r in outdoor_recipients {
            crate::commands::send_to(world, r, format!("\r\n{line}\r\n"));
        }
    }
}

/// One-line atmospheric flavor for a precip transition. Generic
/// (no per-from/per-to combinatorics) — players see the new
/// state, not the delta. Good enough for v1.
fn transition_line(new_precip: PrecipKind) -> &'static str {
    match new_precip {
        PrecipKind::Clear => "The clouds part; the sky brightens.",
        PrecipKind::Cloudy => "Clouds gather overhead.",
        PrecipKind::Drizzle => "A light drizzle begins to fall.",
        PrecipKind::Rain => "The rain picks up — a steady downpour.",
        PrecipKind::Storm => "The wind howls; thunder rumbles in the distance.",
        PrecipKind::Snow => "Snowflakes begin to fall.",
        PrecipKind::Blizzard => "The snow thickens into a blinding blizzard.",
    }
}

fn drift_temp(current: TempBand, climate: Climate) -> TempBand {
    let (lo, hi) = temp_range(climate);
    let lo_idx = i32::try_from(temp_idx(lo)).unwrap_or(0);
    let hi_idx = i32::try_from(temp_idx(hi)).unwrap_or(6);
    let cur_idx = i32::try_from(temp_idx(current)).unwrap_or(3);
    // 50% chance to stay, 25% drift up, 25% drift down — bounded.
    let delta: i32 = match rand::random_range(0..4) {
        0 => -1,
        1 => 1,
        _ => 0,
    };
    let new_idx = (cur_idx + delta).clamp(lo_idx, hi_idx);
    idx_to_temp(usize::try_from(new_idx).unwrap_or(3))
}

fn drift_precip(current: PrecipKind, climate: Climate) -> PrecipKind {
    let pool = precip_pool(climate);
    // 60% stay, 40% pick a random valid precip for this climate.
    if rand::random_range(0..5) < 3 {
        return current;
    }
    let pick = rand::random_range(0..pool.len());
    pool[pick]
}

fn temp_idx(t: TempBand) -> usize {
    match t {
        TempBand::Frigid => 0,
        TempBand::Cold => 1,
        TempBand::Cool => 2,
        TempBand::Mild => 3,
        TempBand::Warm => 4,
        TempBand::Hot => 5,
        TempBand::Sweltering => 6,
    }
}

fn idx_to_temp(i: usize) -> TempBand {
    [
        TempBand::Frigid,
        TempBand::Cold,
        TempBand::Cool,
        TempBand::Mild,
        TempBand::Warm,
        TempBand::Hot,
        TempBand::Sweltering,
    ][i.min(6)]
}

fn temp_range(climate: Climate) -> (TempBand, TempBand) {
    use TempBand::{Cold, Cool, Frigid, Hot, Mild, Sweltering, Warm};
    // Some climates intentionally share a (lo, hi) range — e.g.
    // Arid and Tropical both span Warm..Sweltering despite having
    // very different precipitation. clippy wants them collapsed,
    // but the climate-arm structure documents intent and lets us
    // diverge them later (different precip pools already do).
    #[allow(clippy::match_same_arms)]
    match climate {
        Climate::None => (Mild, Mild),
        Climate::Arid => (Warm, Sweltering),
        Climate::Semiarid => (Mild, Hot),
        Climate::Tropical => (Warm, Sweltering),
        Climate::Subtropical => (Mild, Hot),
        Climate::Temperate => (Cool, Warm),
        Climate::Oceanic => (Cool, Mild),
        Climate::Subarctic => (Frigid, Cool),
        Climate::Arctic => (Frigid, Cold),
        Climate::Alpine => (Cold, Cool),
    }
}

fn precip_pool(climate: Climate) -> &'static [PrecipKind] {
    use PrecipKind::{Blizzard, Cloudy, Clear, Drizzle, Rain, Snow, Storm};
    match climate {
        Climate::None => &[Clear],
        Climate::Arid | Climate::Semiarid => &[Clear, Cloudy],
        Climate::Tropical => &[Clear, Cloudy, Rain, Storm],
        Climate::Subtropical | Climate::Temperate => {
            &[Clear, Cloudy, Drizzle, Rain, Storm]
        }
        Climate::Oceanic => &[Cloudy, Drizzle, Rain, Storm],
        Climate::Subarctic => &[Cloudy, Snow, Blizzard],
        Climate::Arctic => &[Snow, Blizzard, Cloudy],
        Climate::Alpine => &[Cloudy, Snow, Clear],
    }
}

/// Render a one-line description for the given state, suitable for
/// the `weather` command and outdoor `look`. Pass by value — the
/// state is only two enum variants (~2 bytes total).
#[must_use]
pub fn describe(state: WeatherState) -> String {
    format!(
        "It is {} and {} here.",
        state.temp.label(),
        state.precip.label()
    )
}

/// Where the persisted weather snapshot lives. Relative path so
/// it follows the working directory the server's started from.
const WEATHER_SNAPSHOT_PATH: &str = "state/weather.json";

/// Companion path for the in-game clock. Same persistence shape:
/// a JSON snapshot read on boot, written on graceful shutdown.
const CLOCK_SNAPSHOT_PATH: &str = "state/clock.json";

/// Load `MudClock` state from `state/clock.json`. First boot or
/// parse failures fall through silently — the resource keeps its
/// `Default` value (year 2025, month 1, day 1, hour 12).
pub fn load_clock_snapshot(world: &mut World) {
    let bytes = match std::fs::read(CLOCK_SNAPSHOT_PATH) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(error = %e, "clock snapshot read failed");
            return;
        }
    };
    let snapshot: mud_world::MudClock = match serde_json::from_slice(&bytes) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "clock snapshot parse failed");
            return;
        }
    };
    let year = snapshot.year;
    let month = snapshot.month;
    let day = snapshot.day;
    let hour = snapshot.hour;
    world.insert_resource(snapshot);
    tracing::info!(
        year,
        month,
        day,
        hour,
        path = %CLOCK_SNAPSHOT_PATH,
        "clock snapshot loaded",
    );
}

/// Persist the in-game `MudClock` to `state/clock.json` on graceful
/// shutdown. Mirrors `save_snapshot` for weather.
pub fn save_clock_snapshot(world: &World) {
    if let Some(parent) = std::path::Path::new(CLOCK_SNAPSHOT_PATH).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(error = %e, "couldn't create clock snapshot dir");
        return;
    }
    let clock = world.resource::<mud_world::MudClock>();
    let bytes = match serde_json::to_vec_pretty(clock) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "clock snapshot serialize failed");
            return;
        }
    };
    if let Err(e) = std::fs::write(CLOCK_SNAPSHOT_PATH, bytes) {
        tracing::warn!(error = %e, "clock snapshot write failed");
        return;
    }
    tracing::info!(
        hour = clock.hour,
        day = clock.day,
        path = %CLOCK_SNAPSHOT_PATH,
        "clock snapshot saved",
    );
}

/// Try to overlay the `WeatherCatalog` with a saved snapshot from
/// `state/weather.json`. Silent no-op when the file doesn't exist
/// (first boot) or the parse fails (corrupt file). Climate-default
/// state from world load remains for any zone the snapshot doesn't
/// cover, so adding a new zone post-snapshot Just Works.
pub fn load_snapshot(world: &mut World) {
    let bytes = match std::fs::read(WEATHER_SNAPSHOT_PATH) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(error = %e, "weather snapshot read failed");
            return;
        }
    };
    let snapshot: std::collections::HashMap<i32, WeatherState> =
        match serde_json::from_slice(&bytes) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "weather snapshot parse failed");
                return;
            }
        };
    let mut catalog = world.resource_mut::<WeatherCatalog>();
    let restored = snapshot.len();
    for (zone_id, state) in snapshot {
        catalog.by_zone.insert(zone_id, state);
    }
    tracing::info!(zones = restored, path = %WEATHER_SNAPSHOT_PATH, "weather snapshot loaded");
}

/// Persist the current `WeatherCatalog` to `state/weather.json`.
/// Creates the parent directory if missing. Called from the main
/// shutdown handler.
pub fn save_snapshot(world: &World) {
    if let Some(parent) = std::path::Path::new(WEATHER_SNAPSHOT_PATH).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(error = %e, "couldn't create weather snapshot dir");
        return;
    }
    let catalog = world.resource::<WeatherCatalog>();
    let bytes = match serde_json::to_vec_pretty(&catalog.by_zone) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "weather snapshot serialize failed");
            return;
        }
    };
    if let Err(e) = std::fs::write(WEATHER_SNAPSHOT_PATH, bytes) {
        tracing::warn!(error = %e, "weather snapshot write failed");
        return;
    }
    tracing::info!(zones = catalog.by_zone.len(), path = %WEATHER_SNAPSHOT_PATH, "weather snapshot saved");
}
