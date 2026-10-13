//! The weather lines and spell-circle times moved from Rust to the database
//! (`"WeatherText"`, `"SpellCircle"`). The seeded rows must reproduce the
//! built-in tables exactly, so a database loaded from the seed patch behaves
//! like one without the tables.

use mud_db::enums::Climate;
use mud_world::weather_text::BUILTIN;
use mud_world::{CIRCLE_RECOVER_TIME, SpellSlotData, WeatherText};

#[tokio::test]
#[ignore = "requires live fierydev DB; run with: cargo test -p mud-server -- --ignored"]
async fn seeded_tables_reproduce_the_builtin_weather_lines_and_circle_times() {
    let _ = dotenvy::from_path("../../.env");
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = mud_db::connect(&url).await.expect("connect");

    let rows = mud_db::weather_text::list_all(&pool)
        .await
        .expect("weather");
    for (climate, daypart, text) in BUILTIN {
        let row = rows
            .iter()
            .find(|r| r.climate == climate && r.daypart == daypart)
            .unwrap_or_else(|| panic!("missing WeatherText {climate}/{daypart}"));
        assert_eq!(row.text.as_deref(), Some(text), "{climate}/{daypart}");
    }
    let db = WeatherText::from_rows(rows.into_iter().map(|r| (r.climate, r.daypart, r.text)));
    let builtin = WeatherText::default();
    for climate in [
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
    ] {
        for hour in 0..24 {
            assert_eq!(db.line(climate, hour), builtin.line(climate, hour));
        }
    }

    let circles = mud_db::spell_circles::list_all(&pool)
        .await
        .expect("circles");
    let mut data = SpellSlotData::default();
    for r in circles {
        if let Some(secs) = r.recover_secs {
            data.recover_secs.insert(r.circle, secs);
        }
    }
    for (circle, secs) in CIRCLE_RECOVER_TIME.iter().enumerate().skip(1) {
        let circle = i32::try_from(circle).unwrap();
        assert_eq!(
            data.recover_secs.get(&circle),
            Some(secs),
            "SpellCircle {circle}"
        );
        assert_eq!(data.circle_recover_secs(circle), *secs);
    }
}

/// The seeded `"MountType"` rows must pick and roll exactly like the built-in tier table.
#[tokio::test]
#[ignore = "requires live fierydev DB; run with: cargo test -p mud-server -- --ignored"]
async fn seeded_mount_types_reproduce_the_builtin_tiers() {
    use mud_world::MountTypes;

    let _ = dotenvy::from_path("../../.env");
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = mud_db::connect(&url).await.expect("connect");

    let rows = mud_db::mount_types::list_all(&pool)
        .await
        .expect("mount types");
    assert_eq!(rows.len(), 8, "four tiers x two sides");
    let db = MountTypes::from_rows(rows);
    let builtin = MountTypes::default();
    assert_eq!(db, builtin);
    for anti in [false, true] {
        for level in 0..=105 {
            for step in -21..=21 {
                let alignment = step * 50;
                assert_eq!(
                    db.plan(anti, level, alignment),
                    builtin.plan(anti, level, alignment),
                    "anti={anti} level={level} alignment={alignment}"
                );
            }
        }
    }
}
