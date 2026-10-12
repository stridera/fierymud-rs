//! Boot loading of the small content tables (`StatusFlagValue`,
//! `SpellSyllable`, `SystemMessage`, `Ability.prompt_letter`): the rows seeded
//! by `fierylib/data/sql/2026-10-09-content-tables.sql` reach their ECS
//! resources, and a database without the tables still boots. Needs the dev
//! database; skipped when it is unreachable or the patch is not applied.

use std::time::Duration;

use bevy_ecs::prelude::World;
use mud_db::PoolSettings;
use mud_db::sqlx::PgPool;
use mud_world::{
    FlagPrevents, PromptLetters, ReservedNames, SpellSyllables, StatusFlagValues, SystemMessages,
};

use crate::commands::test_support::{
    db_test_lock, db_test_pool_settings, seeded_status_flag_values,
};

fn url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into())
}

async fn pool(settings: PoolSettings) -> Option<PgPool> {
    tokio::time::timeout(
        Duration::from_secs(3),
        mud_db::connect_with(&url(), settings),
    )
    .await
    .ok()?
    .ok()
}

/// Dev-database pool, `None` (test skipped) when the content-tables patch has
/// not been applied.
async fn seeded_pool() -> Option<PgPool> {
    let pool = pool(db_test_pool_settings()).await?;
    mud_db::sqlx::query("SELECT flag FROM \"StatusFlagValue\" LIMIT 1")
        .execute(&pool)
        .await
        .ok()?;
    mud_db::sqlx::query("SELECT prompt_letter FROM \"Ability\" LIMIT 1")
        .execute(&pool)
        .await
        .ok()?;
    mud_db::sqlx::query("SELECT name FROM \"ReservedNames\" LIMIT 1")
        .execute(&pool)
        .await
        .ok()?;
    Some(pool)
}

#[tokio::test]
async fn seeded_rows_reach_their_resources() {
    let _lock = db_test_lock().await;
    let Some(pool) = seeded_pool().await else {
        eprintln!("skipped: dev database without the content-tables patch");
        return;
    };
    let mut world = World::new();
    mud_world::load_content_tables(&mut world, &pool)
        .await
        .expect("load content tables");

    let flags = world.resource::<StatusFlagValues>();
    assert!(!flags.by_flag.is_empty());
    assert_eq!(flags.value("a_flag_nobody_seeded"), 0);
    // `2026-10-12-status-flag-prevents.sql` fills the prevents_* columns; a
    // dev database that has the columns but not the rows yet skips this part.
    if flags.prevents("webbed").movement {
        let expected = seeded_status_flag_values();
        for (flag, want) in &expected.prevents {
            assert_eq!(flags.prevents(flag), *want, "{flag}");
        }
        assert_eq!(flags.prevents("blind"), FlagPrevents::default());
        assert_eq!(
            flags.prevents("a_flag_nobody_seeded"),
            FlagPrevents::default()
        );
    } else {
        eprintln!("skipped prevents check: dev database without the status-flag-prevents patch");
    }

    assert!(!world.resource::<SpellSyllables>().rows.is_empty());

    let reserved = world.resource::<ReservedNames>();
    assert!(!reserved.names.is_empty());
    assert!(reserved.contains("Guard"), "seeded curated word");
    assert!(
        !reserved.contains("someone"),
        "command-grammar words stay in code, not the table"
    );

    let messages = world.resource::<SystemMessages>();
    assert_eq!(messages.get("exp_progress").len(), 11);
    assert_eq!(messages.get("month_names").len(), 16);
    assert_eq!(messages.get("insult_lines").len(), 8);
    assert!(!messages.get("weather_change_rain").is_empty());
    assert_ne!(messages.month_name(1), "an unknown month");

    let letters = world.resource::<PromptLetters>();
    assert!(
        letters
            .by_letter
            .iter()
            .any(|(l, ids)| *l == 'b' && ids.len() == 5)
    );
}

/// A database that has none of the tables (prod before the patch is applied)
/// must not fail the boot: every resource still exists, empty.
#[tokio::test]
async fn missing_tables_degrade_to_empty_resources() {
    let _lock = db_test_lock().await;
    // One connection, search_path pointed at an empty schema: every content
    // table (and `Ability`) is "undefined" for it.
    let Some(pool) = pool(PoolSettings {
        max_connections: 1,
        acquire_timeout: Duration::from_secs(60),
    })
    .await
    else {
        eprintln!("skipped: dev database unreachable");
        return;
    };
    mud_db::sqlx::query("CREATE SCHEMA IF NOT EXISTS content_tables_empty")
        .execute(&pool)
        .await
        .expect("create schema");
    mud_db::sqlx::query("SET search_path TO content_tables_empty")
        .execute(&pool)
        .await
        .expect("set search_path");

    let mut world = World::new();
    mud_world::load_content_tables(&mut world, &pool)
        .await
        .expect("a missing table must not fail the boot");

    assert!(world.resource::<StatusFlagValues>().by_flag.is_empty());
    assert!(world.resource::<StatusFlagValues>().prevents.is_empty());
    assert!(world.resource::<SpellSyllables>().rows.is_empty());
    assert!(world.resource::<ReservedNames>().names.is_empty());
    assert!(world.resource::<SystemMessages>().by_key.is_empty());
    assert!(world.resource::<PromptLetters>().by_letter.is_empty());

    mud_db::sqlx::query("DROP SCHEMA content_tables_empty")
        .execute(&pool)
        .await
        .expect("drop schema");
}
