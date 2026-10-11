//! `MobResetEquipmentContents` against the dev database: a database without
//! the table still boots (the loader logs an ERROR and carries on with no
//! rows), and a full boot puts the key inside the cocoon the canopy spider
//! carries (legacy `M 2308`, `G 2303`, `P 2302 2303`). Skipped when the dev
//! database is unreachable or the table / patch is not applied.

use std::time::Duration;

use bevy_ecs::prelude::{Entity, With, World};
use mud_db::PoolSettings;
use mud_db::sqlx::PgPool;
use mud_world::{Item, Located, Mob, WorldKey};

use crate::commands::test_support::{db_test_lock, db_test_pool_settings};

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

/// A database lacking the table must not fail the boot.
#[tokio::test]
async fn missing_gear_contents_table_degrades_to_no_rows() {
    let _lock = db_test_lock().await;
    let Some(pool) = pool(PoolSettings {
        max_connections: 1,
        acquire_timeout: Duration::from_secs(60),
    })
    .await
    else {
        eprintln!("skipped: dev database unreachable");
        return;
    };
    mud_db::sqlx::query("CREATE SCHEMA IF NOT EXISTS mob_gear_contents_empty")
        .execute(&pool)
        .await
        .expect("create schema");
    mud_db::sqlx::query("SET search_path TO mob_gear_contents_empty")
        .execute(&pool)
        .await
        .expect("set search_path");

    let rows = mud_world::load_gear_contents(&pool)
        .await
        .expect("a missing table must not fail the boot");
    assert!(rows.is_empty());

    mud_db::sqlx::query("DROP SCHEMA mob_gear_contents_empty")
        .execute(&pool)
        .await
        .expect("drop schema");
}

fn held_by(world: &mut World, holder: Entity, zone: i32, id: i32) -> Vec<Entity> {
    let mut q = world.query_filtered::<(Entity, &WorldKey, &Located), With<Item>>();
    q.iter(world)
        .filter(|(_, k, l)| l.0 == holder && k.zone == zone && k.id == id)
        .map(|(e, _, _)| e)
        .collect()
}

/// Boot the dev world: mob 23/8 carries cocoon 23/3 with key 23/2 inside.
#[tokio::test]
async fn booted_canopy_spider_carries_the_cocoon_with_the_key() {
    let _lock = db_test_lock().await;
    let Some(pool) = pool(db_test_pool_settings()).await else {
        eprintln!("skipped: dev database unreachable");
        return;
    };
    let Ok(rows) = mud_db::mob_reset_equipment::list_contents(&pool).await else {
        eprintln!("skipped: dev database without MobResetEquipmentContents");
        return;
    };
    if !rows
        .iter()
        .any(|r| r.object_zone_id == 23 && r.object_id == 2)
    {
        eprintln!("skipped: 2026-10-11-mob-gear-contents.sql not applied");
        return;
    }
    let mut world = World::new();
    mud_world::load_from_db(&mut world, &pool)
        .await
        .expect("load world");

    let spiders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &WorldKey), With<Mob>>();
        q.iter(&world)
            .filter(|(_, k)| k.zone == 23 && k.id == 8)
            .map(|(e, _)| e)
            .collect()
    };
    assert!(!spiders.is_empty(), "canopy spider 23/8 spawned");
    let mut keys = 0;
    for spider in spiders {
        for cocoon in held_by(&mut world, spider, 23, 3) {
            keys += held_by(&mut world, cocoon, 23, 2).len();
        }
    }
    assert!(keys >= 1, "the key sits inside a carried cocoon");
}
