//! Per-zone weather drift tick. Updates `WeatherCatalog` every
//! `WEATHER_TICK_TICKS` real-time ticks (≈one game-quarter-day) by
//! nudging each zone's `WeatherState` one band toward a random
//! climate-bounded target. Fully ephemeral — restart re-derives
//! state from each zone's `Climate`.

use bevy_ecs::prelude::*;
use mud_db::enums::Climate;
use mud_world::{PrecipKind, TempBand, WeatherCatalog, WeatherState, ZoneClimate};

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
    // Climate::None marks metaphysical / interior-only zones (the
    // Void, plane spaces) where weather doesn't make sense. Skip
    // them at collection time so they never get an entry in
    // `WeatherCatalog.by_zone` — that lets the look-room weather
    // hint and `look sky` cleanly read "no weather" via a missing
    // map key, with no per-call zone lookup.
    let climates: Vec<(i32, Climate)> = {
        let mut q =
            world.query_filtered::<(&mud_world::WorldKey, &ZoneClimate), With<mud_world::Zone>>();
        q.iter(world)
            .filter(|(_, c)| !matches!(c.0, Climate::None))
            .map(|(wk, c)| (wk.zone, c.0))
            .collect()
    };
    // Read the current season once — drift_temp uses it to shift the
    // climate's allowed band. Without this, a Temperate zone in deep
    // winter still drifted Cool..Warm, which looked weird next to a
    // "the snow thickens" precip line.
    let season = world.resource::<mud_world::MudClock>().season();
    // Track zones whose precip changed so the post-tick pass can
    // broadcast "the sky shifts" lines to outdoor players in them.
    let mut precip_changes: Vec<(i32, PrecipKind, PrecipKind)> = Vec::new();
    // Snapshot the lock map (zone -> expiry). Skip drift on any zone
    // whose lock is still in the future; opportunistically clear
    // expired locks at the same time so the map doesn't grow without
    // bound after a long-running session.
    let now = std::time::Instant::now();
    let locked_zones: std::collections::HashSet<i32> = {
        let mut locks = world.resource_mut::<mud_world::WeatherDriftLocks>();
        locks.by_zone.retain(|_, expiry| *expiry > now);
        locks.by_zone.keys().copied().collect()
    };
    {
        let mut weather = world.resource_mut::<WeatherCatalog>();
        for (zone_id, climate) in climates {
            let prev = weather
                .by_zone
                .entry(zone_id)
                .or_insert_with(|| mud_world::default_weather_for_climate(climate))
                .precip;
            if locked_zones.contains(&zone_id) {
                // CONTROL_WEATHER / RAIN are holding this zone — leave
                // both temp and precip alone for the rest of the lock.
                continue;
            }
            let entry = weather.by_zone.get_mut(&zone_id).unwrap();
            entry.temp = drift_temp(entry.temp, climate, season);
            entry.precip = drift_precip(entry.precip, climate);
            if entry.precip != prev {
                precip_changes.push((zone_id, prev, entry.precip));
            }
        }
    }
    // Broadcast precip changes (state transitions only) to awake
    // players standing outdoors in the affected zone.
    for (zone_id, _prev, new_precip) in precip_changes {
        broadcast_precip_change(world, zone_id, new_precip);
    }
}

/// Players who should hear a weather change in `zone_id`: online,
/// awake, standing in an outdoor room of that zone. Mirrors legacy
/// `cb_outdoor` (`AWAKE(ch) && CH_OUTSIDE(ch) && IN_ZONE_RNUM(ch)`),
/// plus the builder `IndoorRoom` shelter override that `look` honours.
fn weather_recipients(world: &mut World, zone_id: i32) -> Vec<Entity> {
    let mut q = world.query_filtered::<(
        Entity,
        &mud_world::Located,
        Option<&mud_world::Posture>,
    ), (
        With<mud_world::Player>,
        With<mud_world::Online>,
    )>();
    q.iter(world)
        .filter(|(_, l, posture)| {
            let room = l.0;
            let awake = posture.is_none_or(|p| p.0 != mud_world::PostureKind::Sleeping);
            let zone_match = world
                .get::<mud_world::WorldKey>(room)
                .is_some_and(|k| k.zone == zone_id);
            let outdoor = world
                .get::<mud_world::RoomSector>(room)
                .is_some_and(|s| crate::commands::sector_is_outdoor_for_weather(s.0))
                && world.get::<mud_world::IndoorRoom>(room).is_none();
            awake && zone_match && outdoor
        })
        .map(|(e, _, _)| e)
        .collect()
}

/// Send the transition line for `new_precip` to everyone who can
/// perceive it. Called only when a zone's precipitation changed.
fn broadcast_precip_change(world: &mut World, zone_id: i32, new_precip: PrecipKind) {
    let line = transition_line(new_precip);
    for r in weather_recipients(world, zone_id) {
        crate::commands::send_to(world, r, format!("\r\n{line}\r\n"));
    }
}

/// One-line atmospheric flavor for a precip transition. Generic
/// (no per-from/per-to combinatorics) — players see the new
/// state, not the delta. Good enough for v1.
fn transition_line(new_precip: PrecipKind) -> &'static str {
    // Same palette as `ambient_line`; transitions are the louder
    // moments the player should look up at, so accent words
    // ("rain", "thunder", "lightning") get the saturation.
    match new_precip {
        PrecipKind::Clear => "<b:yellow>The clouds part</>; the sky brightens.",
        PrecipKind::Cloudy => "<dim>Clouds gather overhead.</>",
        PrecipKind::Drizzle => "<cyan>A light drizzle</> begins to fall.",
        PrecipKind::Rain => "The <cyan>rain</> picks up — a steady <cyan>downpour</>.",
        PrecipKind::Storm => "<dim>The wind howls</>; <dim>thunder</> rumbles in the distance.",
        PrecipKind::Snow => "<b:white>Snowflakes</> begin to fall.",
        PrecipKind::Blizzard => "The snow thickens into a blinding <b:white>blizzard</>.",
    }
}

fn drift_temp(current: TempBand, climate: Climate, season: mud_world::Season) -> TempBand {
    let (lo, hi) = seasonal_temp_range(climate, season);
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

/// Climate's base band shifted by the calendar quarter. Winter pulls
/// the allowed range two bands cooler, summer two bands warmer; the
/// equinox seasons leave the climate alone. Bands clamp to
/// [Frigid, Sweltering] so subarctic in summer doesn't escape to a
/// nonsense `idx_to_temp(8)`.
fn seasonal_temp_range(climate: Climate, season: mud_world::Season) -> (TempBand, TempBand) {
    let (lo, hi) = temp_range(climate);
    if matches!(climate, Climate::None) {
        // No climate = no seasonal swing. Static dungeons / planes.
        return (lo, hi);
    }
    let shift: i32 = match season {
        mud_world::Season::Winter => -2,
        mud_world::Season::Summer => 2,
        mud_world::Season::Spring | mud_world::Season::Autumn => 0,
    };
    let lo_idx = (i32::try_from(temp_idx(lo)).unwrap_or(0) + shift).clamp(0, 6);
    let hi_idx = (i32::try_from(temp_idx(hi)).unwrap_or(6) + shift).clamp(0, 6);
    // Preserve invariant: lo <= hi after clamping (a single-band climate
    // shifted off the edge becomes a single-band climate at the edge).
    let (lo_idx, hi_idx) = if lo_idx <= hi_idx {
        (lo_idx, hi_idx)
    } else {
        (hi_idx, lo_idx)
    };
    (
        idx_to_temp(usize::try_from(lo_idx).unwrap_or(0)),
        idx_to_temp(usize::try_from(hi_idx).unwrap_or(6)),
    )
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
    use PrecipKind::{Blizzard, Clear, Cloudy, Drizzle, Rain, Snow, Storm};
    match climate {
        Climate::None => &[Clear],
        Climate::Arid | Climate::Semiarid => &[Clear, Cloudy],
        Climate::Tropical => &[Clear, Cloudy, Rain, Storm],
        Climate::Subtropical | Climate::Temperate => &[Clear, Cloudy, Drizzle, Rain, Storm],
        Climate::Oceanic => &[Cloudy, Drizzle, Rain, Storm],
        Climate::Subarctic => &[Cloudy, Snow, Blizzard],
        Climate::Arctic => &[Snow, Blizzard, Cloudy],
        Climate::Alpine => &[Cloudy, Snow, Clear],
    }
}

/// Render a one-line description for the given state, suitable for
/// the `weather` command and `look sky`. Pass by value — the
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
    // Snapshot the set of Climate::None zone ids before borrowing
    // the catalog mutably — those should never carry weather, so a
    // legacy on-disk entry for zone 0 (the Void) needs to drop on
    // restore rather than re-poison the runtime.
    let none_zones: std::collections::HashSet<i32> = {
        let mut q =
            world.query_filtered::<(&mud_world::WorldKey, &ZoneClimate), With<mud_world::Zone>>();
        q.iter(world)
            .filter(|(_, c)| matches!(c.0, Climate::None))
            .map(|(wk, _)| wk.zone)
            .collect()
    };
    let mut catalog = world.resource_mut::<WeatherCatalog>();
    let mut restored = 0usize;
    let mut skipped = 0usize;
    for (zone_id, state) in snapshot {
        if none_zones.contains(&zone_id) {
            skipped += 1;
            continue;
        }
        catalog.by_zone.insert(zone_id, state);
        restored += 1;
    }
    tracing::info!(
        zones = restored,
        skipped_none_climate = skipped,
        path = %WEATHER_SNAPSHOT_PATH,
        "weather snapshot loaded",
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Connection;
    use crate::commands::test_support::{Rx, drain};
    use mud_db::enums::Sector;
    use mud_world::{
        IndoorRoom, Located, MudClock, Named, Online, Player, Posture, PostureKind, Room,
        RoomSector, WeatherDriftLocks, WorldKey, Zone,
    };

    const ZONE: i32 = 10;

    fn world() -> World {
        let mut world = World::new();
        world.insert_resource(TickCount(0));
        world.insert_resource(MudClock::default());
        world.insert_resource(WeatherCatalog::default());
        world.insert_resource(WeatherDriftLocks::default());
        world.insert_resource(mud_world::ObjectPrototypes::default());
        world.spawn((
            Zone,
            WorldKey { zone: ZONE, id: 0 },
            Named {
                name: "Test".into(),
            },
            ZoneClimate(Climate::Temperate),
        ));
        world
    }

    fn room(world: &mut World, zone: i32, id: i32, sector: Sector) -> Entity {
        world
            .spawn((
                Room,
                WorldKey { zone, id },
                Named {
                    name: format!("Room {id}"),
                },
                RoomSector(sector),
                mud_world::Exits::default(),
            ))
            .id()
    }

    fn player(world: &mut World, room: Entity, posture: PostureKind) -> (Entity, Rx) {
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        let e = world
            .spawn((
                Player,
                Online,
                Named {
                    name: "Wanderer".into(),
                },
                Located(room),
                Posture(posture),
                Connection(tx),
            ))
            .id();
        (e, rx)
    }

    #[test]
    fn precip_change_reaches_only_awake_outdoor_players_in_the_zone() {
        let mut w = world();
        let field = room(&mut w, ZONE, 1, Sector::Field);
        let inside = room(&mut w, ZONE, 2, Sector::Structure);
        let shelter = room(&mut w, ZONE, 3, Sector::Field);
        w.entity_mut(shelter).insert(IndoorRoom);
        let elsewhere = room(&mut w, 99, 1, Sector::Field);
        let (_, mut awake) = player(&mut w, field, PostureKind::Standing);
        let (_, mut resting) = player(&mut w, field, PostureKind::Resting);
        let (_, mut asleep) = player(&mut w, field, PostureKind::Sleeping);
        let (_, mut indoors) = player(&mut w, inside, PostureKind::Standing);
        let (_, mut sheltered) = player(&mut w, shelter, PostureKind::Standing);
        let (_, mut other_zone) = player(&mut w, elsewhere, PostureKind::Standing);

        broadcast_precip_change(&mut w, ZONE, PrecipKind::Rain);

        assert!(drain(&mut awake).contains("rain"), "awake outdoor hears it");
        assert!(drain(&mut resting).contains("rain"), "resting is awake");
        assert!(drain(&mut asleep).is_empty(), "sleeper hears nothing");
        assert!(drain(&mut indoors).is_empty(), "indoor room hears nothing");
        assert!(drain(&mut sheltered).is_empty(), "IndoorRoom hears nothing");
        assert!(
            drain(&mut other_zone).is_empty(),
            "other zone hears nothing"
        );
    }

    #[test]
    fn no_unprompted_chatter_when_nothing_changes() {
        let mut w = world();
        let field = room(&mut w, ZONE, 1, Sector::Forest);
        let (_, mut rx) = player(&mut w, field, PostureKind::Standing);
        w.resource_mut::<WeatherCatalog>().by_zone.insert(
            ZONE,
            WeatherState {
                temp: TempBand::Frigid,
                precip: PrecipKind::Storm,
            },
        );
        // Off-cadence ticks do nothing at all.
        for tick in 1..WEATHER_TICK_TICKS {
            w.resource_mut::<TickCount>().0 = tick;
            weather_tick(&mut w);
        }
        // On-cadence tick with the zone held by CONTROL_WEATHER: no drift,
        // so no state change and no message.
        w.resource_mut::<WeatherDriftLocks>().by_zone.insert(
            ZONE,
            std::time::Instant::now() + std::time::Duration::from_secs(600),
        );
        w.resource_mut::<TickCount>().0 = WEATHER_TICK_TICKS;
        weather_tick(&mut w);
        assert_eq!(drain(&mut rx), "");
    }

    #[test]
    fn weather_tick_announces_each_precip_change_once() {
        let mut w = world();
        let field = room(&mut w, ZONE, 1, Sector::Field);
        let (_, mut rx) = player(&mut w, field, PostureKind::Standing);
        let mut changed = false;
        for _ in 0..500 {
            let before = w
                .resource::<WeatherCatalog>()
                .by_zone
                .get(&ZONE)
                .map(|s| s.precip);
            w.resource_mut::<TickCount>().0 = WEATHER_TICK_TICKS;
            weather_tick(&mut w);
            let after = w.resource::<WeatherCatalog>().by_zone[&ZONE].precip;
            let out = drain(&mut rx);
            // Never a message without a change; at most one per tick.
            if before.is_some_and(|b| b == after) {
                assert_eq!(out, "", "message without a precip change");
            } else if before.is_some() {
                assert!(!out.is_empty(), "precip changed silently");
                changed = true;
            }
        }
        assert!(changed, "a temperate zone should drift within 500 ticks");
    }
}
