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

/// Ambient flavor cadence — every 20 real-time seconds, sample
/// outdoor players for a sky/wildlife line keyed off the current
/// per-zone precip. Tighter than the drift tick so the world feels
/// alive even when the band hasn't changed; looser than the combat
/// tick so it isn't spammy.
const AMBIENT_TICK_TICKS: u64 = 200;
/// Per-tick chance any given outdoor player hears an ambient line.
/// 1 in 4 means roughly one line every 80s on average — present
/// without being noisy.
const AMBIENT_CHANCE_DENOM: u32 = 4;

/// Minimum quiet window after a player's last command before another
/// ambient line is allowed to fire. Without this the ambient tick
/// can land in the gap between a command's response and the next
/// prompt, producing the disorienting "I cast a spell and immediately
/// got a weather line" reading. Two seconds is comfortably longer
/// than any normal command's response cycle but short enough that
/// ambient flavor still surfaces between actions.
const AMBIENT_MIN_QUIET_SECS: u64 = 2;

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
        let mut q = world.query_filtered::<(&mud_world::WorldKey, &ZoneClimate), With<mud_world::Zone>>();
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
    {
        let mut weather = world.resource_mut::<WeatherCatalog>();
        for (zone_id, climate) in climates {
            let prev = weather
                .by_zone
                .entry(zone_id)
                .or_insert_with(|| mud_world::default_weather_for_climate(climate))
                .precip;
            let entry = weather.by_zone.get_mut(&zone_id).unwrap();
            entry.temp = drift_temp(entry.temp, climate, season);
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

/// Ambient weather chatter — light, repeating flavor for each
/// outdoor player keyed off the per-zone precip. Runs more often
/// than the drift tick so the world feels alive even when the
/// band hasn't shifted; per-player probability gate keeps the
/// stream readable. No-op for indoor/cave/plane sectors.
pub fn ambient_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(AMBIENT_TICK_TICKS) {
        return;
    }
    let now = std::time::Instant::now();
    let quiet_floor = std::time::Duration::from_secs(AMBIENT_MIN_QUIET_SECS);
    let candidates: Vec<(Entity, Entity)> = {
        let mut q = world.query_filtered::<(
            Entity,
            &mud_world::Located,
            Option<&mud_world::LastInputAt>,
        ), (
            With<mud_world::Player>,
            With<mud_world::Online>,
        )>();
        q.iter(world)
            .filter(|(_, l, _)| {
                world
                    .get::<mud_world::RoomSector>(l.0)
                    .is_some_and(|s| crate::commands::sector_is_outdoor_for_weather(s.0))
            })
            .filter(|(_, _, last_in)| {
                // Skip a player whose last command landed within
                // AMBIENT_MIN_QUIET_SECS — the ambient line would
                // otherwise crash into their command's output.
                last_in.is_none_or(|li| now.duration_since(li.0) >= quiet_floor)
            })
            .map(|(e, l, _)| (e, l.0))
            .collect()
    };
    for (player, room) in candidates {
        if rand::random_range(0..AMBIENT_CHANCE_DENOM) != 0 {
            continue;
        }
        let zone = world.get::<mud_world::WorldKey>(room).map(|k| k.zone);
        let state = zone.and_then(|z| {
            world
                .resource::<WeatherCatalog>()
                .by_zone
                .get(&z)
                .copied()
        });
        let Some(state) = state else { continue };
        // Cascade: precip first (the most vivid cue when present),
        // then temperature extreme on quiet-sky days, then terrain
        // flavor for fully mild outdoor rooms. The first hit fires;
        // missing all three stays silent.
        let sector = world.get::<mud_world::RoomSector>(room).map(|s| s.0);
        let line = ambient_line(state.precip)
            .or_else(|| ambient_temp_line(state.temp))
            .or_else(|| sector.and_then(ambient_terrain_line));
        if let Some(line) = line {
            crate::commands::send_to(world, player, format!("\r\n{line}\r\n"));
        }
    }
}

/// Terrain-keyed flavor for mild outdoor rooms where neither
/// precip nor temperature extreme has anything to say. Returns
/// None for sectors whose surroundings don't have an obvious
/// ambient sound (City streets, Roads — too varied to flavor
/// generically).
fn ambient_terrain_line(sector: mud_db::enums::Sector) -> Option<&'static str> {
    use mud_db::enums::Sector;
    // Color palette: <green> for forest/grass/swamp life,
    // <cyan> for water/sea, <dim> for stone/wind, <b:yellow>
    // for warm-band flashes. Lines stay one-color so render
    // cost is trivial and the eye can scan them quickly.
    let pool: &[&str] = match sector {
        Sector::Forest => &[
            "<green>Leaves rustle in the canopy</> overhead.",
            "Somewhere unseen, <green>a bird calls</>.",
            "<dim>A branch snaps in the distance.</>",
        ],
        Sector::Hills => &[
            "<dim>Wind whispers across the slopes.</>",
            "<green>Grass bends</> in the breeze.",
        ],
        Sector::Mountain => &[
            "<dim>A distant rock clatters down the slope.</>",
            "<dim>Wind howls between the peaks.</>",
            "<b:yellow>A raptor's cry</> echoes off the stone.",
        ],
        Sector::Field | Sector::Grasslands => &[
            "<green>Tall grass whispers</> in the wind.",
            "<green>Insects hum</> in the undergrowth.",
        ],
        Sector::Beach | Sector::Shallows | Sector::Water => &[
            "<cyan>Waves wash against the shore.</>",
            "<b:cyan>A distant gull cries.</>",
            "<cyan>Salt spray rides the wind.</>",
        ],
        Sector::Swamp => &[
            "<green>Frogs croak</> from the murky pools.",
            "<cyan>Something splashes nearby.</>",
            "<dim>Mosquitoes whine past your ear.</>",
        ],
        Sector::Ruins => &[
            "<dim>Old stone settles with a creak.</>",
            "<dim>Wind whistles through cracked walls.</>",
        ],
        _ => return None,
    };
    let pick = rand::random_range(0..pool.len());
    Some(pool[pick])
}

/// Temperature-extreme flavor for the quiet-precip days where
/// `ambient_line` returns None. Mild bands stay silent — only
/// the ends of the scale (Frigid / Sweltering) get chatter.
fn ambient_temp_line(temp: TempBand) -> Option<&'static str> {
    // Cold ends paint <b:cyan> / <b:white>; hot ends paint
    // <red> / <b:yellow> for an immediate "hot vs cold" read.
    let pool: &[&str] = match temp {
        TempBand::Frigid => &[
            "<b:cyan>Your breath fogs</> in the bitter cold.",
            "<b:cyan>The cold gnaws</> at every exposed inch of skin.",
        ],
        TempBand::Sweltering => &[
            "<red>Heat shimmers</> off every surface.",
            "<red>Sweat beads on your brow</>; the air feels heavy.",
        ],
        _ => return None,
    };
    let pick = rand::random_range(0..pool.len());
    Some(pool[pick])
}

/// Returns a flavor line for the given precip, or None for the
/// quiet bands (Clear / Cloudy) where chatter would just be noise.
/// Multiple variants per band; picked uniformly at random so the
/// same band doesn't fire the same line every time.
fn ambient_line(precip: PrecipKind) -> Option<&'static str> {
    // <cyan> for rain/water, <b:white> for snow,
    // <b:yellow> for lightning bursts, <dim> for thunder /
    // wind that should sit *behind* the louder hits visually.
    let pool: &[&str] = match precip {
        PrecipKind::Clear | PrecipKind::Cloudy => return None,
        PrecipKind::Drizzle => &[
            "<cyan>A fine mist</> drifts past your face.",
            "<cyan>Drops patter softly</> on stone.",
        ],
        PrecipKind::Rain => &[
            "<cyan>Rain hisses</> against the ground.",
            "<dim>Wet wind tugs at your cloak.</>",
            "<cyan>A puddle ripples</> nearby.",
        ],
        PrecipKind::Storm => &[
            "<dim>Thunder rumbles</> in the distance.",
            "<b:yellow>Lightning splits the sky</> for an instant.",
            "<dim>The wind shrieks through the trees.</>",
        ],
        PrecipKind::Snow => &[
            "<b:white>Snowflakes settle</> on your shoulders.",
            "<b:white>The snow muffles every sound.</>",
        ],
        PrecipKind::Blizzard => &[
            "<b:white>The blizzard howls</>; visibility shrinks.",
            "<b:white>Stinging snow</> whips past your face.",
        ],
    };
    let pick = rand::random_range(0..pool.len());
    Some(pool[pick])
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

fn drift_temp(
    current: TempBand,
    climate: Climate,
    season: mud_world::Season,
) -> TempBand {
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
fn seasonal_temp_range(
    climate: Climate,
    season: mud_world::Season,
) -> (TempBand, TempBand) {
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
    let lo_idx =
        (i32::try_from(temp_idx(lo)).unwrap_or(0) + shift).clamp(0, 6);
    let hi_idx =
        (i32::try_from(temp_idx(hi)).unwrap_or(6) + shift).clamp(0, 6);
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
    // Snapshot the set of Climate::None zone ids before borrowing
    // the catalog mutably — those should never carry weather, so a
    // legacy on-disk entry for zone 0 (the Void) needs to drop on
    // restore rather than re-poison the runtime.
    let none_zones: std::collections::HashSet<i32> = {
        let mut q = world.query_filtered::<(&mud_world::WorldKey, &ZoneClimate), With<mud_world::Zone>>();
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
