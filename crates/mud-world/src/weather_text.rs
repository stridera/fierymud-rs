//! Flavour lines of the `weather` command, per zone climate and time of day.
//!
//! The lines are data: the `"WeatherText"` table (`climate`, `daypart`,
//! `text`), editable in Muditor and seeded by
//! `fierylib/data/sql/2026-10-12-weather-text.sql`. [`BUILTIN`] is the
//! fallback for any key the table has no line for (missing row, NULL text, or
//! a database that predates the table), so behaviour is identical with or
//! without the patch.
//!
//! Lookup: the hour maps to a [`Daypart`] band, which tries its own key first
//! (`dawn`, `midday`, `evening`), then its group (`day` = day + midday;
//! `night` = night + evening + dawn), then `any`. Each step checks the
//! database, then the built-in table, before moving on.

use std::collections::HashMap;

use bevy_ecs::prelude::Resource;
use mud_db::enums::Climate;

/// `(climate, daypart, text)`: the lines shipped with the game. Climate is
/// the lowercase `Climate` label; `none` also serves rooms in a zone with no
/// climate.
pub const BUILTIN: [(&str, &str, &str); 20] = [
    (
        "arid",
        "day",
        "Heat shimmers off the parched ground; the air is dry as bone.",
    ),
    (
        "arid",
        "night",
        "The desert chill cuts through cloaks; stars wheel sharply overhead.",
    ),
    (
        "semiarid",
        "day",
        "Dust dances on a warm wind; the sun bears down without mercy.",
    ),
    (
        "semiarid",
        "night",
        "The scrub cools rapidly; far-off coyotes call.",
    ),
    (
        "tropical",
        "day",
        "Humid air clings to your skin; a green-tinted sun hangs heavy.",
    ),
    (
        "tropical",
        "night",
        "Frogs and night-birds compete in the dripping dark.",
    ),
    (
        "subtropical",
        "day",
        "Warm gusts carry distant rain; cumulus clouds tower in lazy stacks.",
    ),
    (
        "subtropical",
        "night",
        "Crickets thicken the air; warm mist drifts through the night.",
    ),
    (
        "temperate",
        "dawn",
        "A cool breeze rustles the leaves; dew glistens on every blade.",
    ),
    (
        "temperate",
        "day",
        "Mild sun and a clean breeze; pleasant traveling weather.",
    ),
    (
        "temperate",
        "night",
        "Stars glitter through clear, cool air.",
    ),
    (
        "oceanic",
        "day",
        "Salt spray rides a steady wind; gulls wheel and complain.",
    ),
    (
        "oceanic",
        "night",
        "Distant surf and sea-mist mute the night.",
    ),
    (
        "subarctic",
        "day",
        "Pale sun glints off stubborn frost; your breath fogs the air.",
    ),
    (
        "subarctic",
        "night",
        "Bitter cold seeps through every seam; aurora flickers overhead.",
    ),
    (
        "arctic",
        "day",
        "Wind-driven snow blurs the horizon; the sun is a pale disc.",
    ),
    (
        "arctic",
        "night",
        "The cold is absolute; ice creaks in the dark.",
    ),
    (
        "alpine",
        "day",
        "Thin, sharp air bites at your lungs; the sun is fierce off the snow.",
    ),
    (
        "alpine",
        "night",
        "The wind howls down the slopes; stars are knife-bright at altitude.",
    ),
    ("none", "any", "The air is still and unremarkable."),
];

/// Lowercase `Climate` label used as the table key. A zone without a
/// climate reads as `none`.
#[must_use]
pub fn climate_key(climate: Option<Climate>) -> &'static str {
    match climate {
        None | Some(Climate::None) => "none",
        Some(Climate::Semiarid) => "semiarid",
        Some(Climate::Arid) => "arid",
        Some(Climate::Oceanic) => "oceanic",
        Some(Climate::Temperate) => "temperate",
        Some(Climate::Subtropical) => "subtropical",
        Some(Climate::Tropical) => "tropical",
        Some(Climate::Subarctic) => "subarctic",
        Some(Climate::Arctic) => "arctic",
        Some(Climate::Alpine) => "alpine",
    }
}

/// Time-of-day band of an in-game hour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Daypart {
    Night,
    Dawn,
    Day,
    Midday,
    Evening,
}

impl Daypart {
    #[must_use]
    pub fn from_hour(hour: i32) -> Self {
        match hour {
            0..=4 | 21..=23 => Self::Night,
            5..=8 => Self::Dawn,
            9..=11 | 14..=17 => Self::Day,
            12..=13 => Self::Midday,
            _ => Self::Evening,
        }
    }

    /// Keys tried in order: the band itself, its group, then `any`.
    #[must_use]
    pub fn candidates(self) -> &'static [&'static str] {
        match self {
            Self::Night => &["night", "any"],
            Self::Dawn => &["dawn", "night", "any"],
            Self::Day => &["day", "any"],
            Self::Midday => &["midday", "day", "any"],
            Self::Evening => &["evening", "night", "any"],
        }
    }
}

fn builtin(climate: &str, daypart: &str) -> Option<&'static str> {
    BUILTIN
        .iter()
        .find(|(c, d, _)| *c == climate && *d == daypart)
        .map(|(_, _, t)| *t)
}

/// Lines loaded from `"WeatherText"`, keyed by lowercase `(climate, daypart)`.
/// `Default` is empty, so every lookup uses [`BUILTIN`].
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct WeatherText {
    lines: HashMap<(String, String), String>,
}

impl WeatherText {
    /// Build from `(climate, daypart, text)` rows. Blank texts (NULL / empty)
    /// are skipped so those keys keep the built-in line.
    #[must_use]
    pub fn from_rows<I>(rows: I) -> Self
    where
        I: IntoIterator<Item = (String, String, Option<String>)>,
    {
        let mut lines = HashMap::new();
        for (climate, daypart, text) in rows {
            let Some(text) = text else { continue };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            lines.insert(
                (
                    climate.trim().to_ascii_lowercase(),
                    daypart.trim().to_ascii_lowercase(),
                ),
                text.to_owned(),
            );
        }
        Self { lines }
    }

    /// Number of lines loaded from the database.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The flavour line for `climate` at `hour`, or `None` when neither the
    /// database nor the built-in table has one (cannot happen for the
    /// built-in climates).
    #[must_use]
    pub fn line(&self, climate: Option<Climate>, hour: i32) -> Option<String> {
        let climate = climate_key(climate);
        Daypart::from_hour(hour)
            .candidates()
            .iter()
            .find_map(|daypart| {
                self.lines
                    .get(&(climate.to_owned(), (*daypart).to_owned()))
                    .map(String::as_str)
                    .or_else(|| builtin(climate, daypart))
            })
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_CLIMATES: [Option<Climate>; 11] = [
        None,
        Some(Climate::None),
        Some(Climate::Semiarid),
        Some(Climate::Arid),
        Some(Climate::Oceanic),
        Some(Climate::Temperate),
        Some(Climate::Subtropical),
        Some(Climate::Tropical),
        Some(Climate::Subarctic),
        Some(Climate::Arctic),
        Some(Climate::Alpine),
    ];

    /// The `match` that `cmd_weather` used before the lines moved to the
    /// database, as the oracle for the new lookup.
    fn legacy_line(climate: Option<Climate>, hour: i32) -> &'static str {
        let day = match hour {
            0..=4 | 21..=23 => "night",
            5..=8 => "dawn",
            9..=11 | 14..=17 => "day",
            12..=13 => "midday",
            _ => "evening",
        };
        match (climate, day) {
            (Some(Climate::Arid), "day" | "midday") => {
                "Heat shimmers off the parched ground; the air is dry as bone."
            }
            (Some(Climate::Arid), _) => {
                "The desert chill cuts through cloaks; stars wheel sharply overhead."
            }
            (Some(Climate::Semiarid), "day" | "midday") => {
                "Dust dances on a warm wind; the sun bears down without mercy."
            }
            (Some(Climate::Semiarid), _) => "The scrub cools rapidly; far-off coyotes call.",
            (Some(Climate::Tropical), "day" | "midday") => {
                "Humid air clings to your skin; a green-tinted sun hangs heavy."
            }
            (Some(Climate::Tropical), _) => "Frogs and night-birds compete in the dripping dark.",
            (Some(Climate::Subtropical), "day" | "midday") => {
                "Warm gusts carry distant rain; cumulus clouds tower in lazy stacks."
            }
            (Some(Climate::Subtropical), _) => {
                "Crickets thicken the air; warm mist drifts through the night."
            }
            (Some(Climate::Temperate), "dawn") => {
                "A cool breeze rustles the leaves; dew glistens on every blade."
            }
            (Some(Climate::Temperate), "day" | "midday") => {
                "Mild sun and a clean breeze; pleasant traveling weather."
            }
            (Some(Climate::Temperate), _) => "Stars glitter through clear, cool air.",
            (Some(Climate::Oceanic), "day" | "midday") => {
                "Salt spray rides a steady wind; gulls wheel and complain."
            }
            (Some(Climate::Oceanic), _) => "Distant surf and sea-mist mute the night.",
            (Some(Climate::Subarctic), "day" | "midday") => {
                "Pale sun glints off stubborn frost; your breath fogs the air."
            }
            (Some(Climate::Subarctic), _) => {
                "Bitter cold seeps through every seam; aurora flickers overhead."
            }
            (Some(Climate::Arctic), "day" | "midday") => {
                "Wind-driven snow blurs the horizon; the sun is a pale disc."
            }
            (Some(Climate::Arctic), _) => "The cold is absolute; ice creaks in the dark.",
            (Some(Climate::Alpine), "day" | "midday") => {
                "Thin, sharp air bites at your lungs; the sun is fierce off the snow."
            }
            (Some(Climate::Alpine), _) => {
                "The wind howls down the slopes; stars are knife-bright at altitude."
            }
            (Some(Climate::None) | None, _) => "The air is still and unremarkable.",
        }
    }

    /// What the seed patch puts in the table: every built-in line.
    fn seeded() -> WeatherText {
        WeatherText::from_rows(
            BUILTIN
                .iter()
                .map(|(c, d, t)| ((*c).to_owned(), (*d).to_owned(), Some((*t).to_owned()))),
        )
    }

    #[test]
    fn builtin_lines_match_the_old_match_for_every_climate_and_hour() {
        let empty = WeatherText::default();
        for climate in ALL_CLIMATES {
            for hour in -2..=26 {
                assert_eq!(
                    empty.line(climate, hour).as_deref(),
                    Some(legacy_line(climate, hour)),
                    "builtin {climate:?} hour {hour}"
                );
            }
        }
    }

    #[test]
    fn seeded_table_gives_the_same_lines_as_the_builtin_fallback() {
        let (db, empty) = (seeded(), WeatherText::default());
        assert_eq!(db.len(), BUILTIN.len());
        for climate in ALL_CLIMATES {
            for hour in -2..=26 {
                assert_eq!(
                    db.line(climate, hour),
                    empty.line(climate, hour),
                    "{climate:?} hour {hour}"
                );
            }
        }
    }

    #[test]
    fn builtin_keys_are_unique_and_cover_every_climate() {
        let mut keys: Vec<_> = BUILTIN.iter().map(|(c, d, _)| (*c, *d)).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), BUILTIN.len());
        for climate in ALL_CLIMATES {
            assert!(
                WeatherText::default().line(climate, 12).is_some(),
                "{climate:?}"
            );
        }
    }

    #[test]
    fn a_database_line_overrides_only_its_own_key() {
        let rows = vec![(
            "ARID".to_owned(),
            "Day".to_owned(),
            Some("  Custom heat.  ".to_owned()),
        )];
        let db = WeatherText::from_rows(rows);
        // day and midday share the "day" line; the other bands are untouched.
        assert_eq!(
            db.line(Some(Climate::Arid), 10).as_deref(),
            Some("Custom heat.")
        );
        assert_eq!(
            db.line(Some(Climate::Arid), 12).as_deref(),
            Some("Custom heat.")
        );
        assert_eq!(
            db.line(Some(Climate::Arid), 22).as_deref(),
            Some(legacy_line(Some(Climate::Arid), 22))
        );
        assert_eq!(
            db.line(Some(Climate::Alpine), 10).as_deref(),
            Some(legacy_line(Some(Climate::Alpine), 10))
        );
    }

    #[test]
    fn exact_band_rows_win_over_their_group() {
        let db = WeatherText::from_rows(vec![
            ("arid".into(), "midday".into(), Some("Noon glare.".into())),
            (
                "arid".into(),
                "dawn".into(),
                Some("Cold sand at dawn.".into()),
            ),
            ("arid".into(), "evening".into(), Some("Dusk wind.".into())),
        ]);
        assert_eq!(
            db.line(Some(Climate::Arid), 12).as_deref(),
            Some("Noon glare.")
        );
        assert_eq!(
            db.line(Some(Climate::Arid), 7).as_deref(),
            Some("Cold sand at dawn.")
        );
        assert_eq!(
            db.line(Some(Climate::Arid), 19).as_deref(),
            Some("Dusk wind.")
        );
        // Plain day / night keep the built-in lines.
        assert_eq!(
            db.line(Some(Climate::Arid), 10).as_deref(),
            Some(legacy_line(Some(Climate::Arid), 10))
        );
    }

    #[test]
    fn null_and_blank_texts_keep_the_builtin_line() {
        let db = WeatherText::from_rows(vec![
            ("arid".into(), "day".into(), None),
            ("arid".into(), "night".into(), Some("   ".into())),
        ]);
        assert!(db.is_empty());
        assert_eq!(
            db.line(Some(Climate::Arid), 10).as_deref(),
            Some(legacy_line(Some(Climate::Arid), 10))
        );
        assert_eq!(
            db.line(Some(Climate::Arid), 23).as_deref(),
            Some(legacy_line(Some(Climate::Arid), 23))
        );
    }

    #[test]
    fn a_dawn_row_does_not_leak_into_other_bands() {
        // Builtin temperate dawn is its own line; a database "night" row must not
        // replace it, because dawn looks at its own key (database, then builtin) first.
        let db = WeatherText::from_rows(vec![(
            "temperate".into(),
            "night".into(),
            Some("Custom night.".into()),
        )]);
        assert_eq!(
            db.line(Some(Climate::Temperate), 6).as_deref(),
            Some(legacy_line(Some(Climate::Temperate), 6))
        );
        assert_eq!(
            db.line(Some(Climate::Temperate), 23).as_deref(),
            Some("Custom night.")
        );
        // Every other climate's dawn is its night line, so it follows the database.
        let db = WeatherText::from_rows(vec![(
            "arid".into(),
            "night".into(),
            Some("Custom.".into()),
        )]);
        assert_eq!(db.line(Some(Climate::Arid), 6).as_deref(), Some("Custom."));
    }
}
