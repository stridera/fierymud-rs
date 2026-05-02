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
    let mut weather = world.resource_mut::<WeatherCatalog>();
    for (zone_id, climate) in climates {
        let entry = weather
            .by_zone
            .entry(zone_id)
            .or_insert_with(|| mud_world::default_weather_for_climate(climate));
        // Drift each axis with a small random nudge per tick. Bounds
        // come from the climate's typical range so e.g. an Arctic
        // zone never drifts into "sweltering."
        entry.temp = drift_temp(entry.temp, climate);
        entry.precip = drift_precip(entry.precip, climate);
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
