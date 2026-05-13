//! Persistent K/V backing for Lua trigger entity variables.
//! Keyed on (entity_type, zone, id, key). Values are arbitrary JSON.
//!
//! Read path: at world boot we pull every row via `list_all` and hydrate
//! the in-memory `EntityVariableCache` resource. Spawned mobs / objects /
//! rooms then read from that cache (no per-spawn DB round trip).
//!
//! Write path: Lua `self:setvar(name, value)` updates the in-memory bag
//! and marks the (type, zone, id) tuple dirty. A periodic flush tick in
//! mud-server drains the dirty set via `upsert_many` so a trigger that
//! sets five vars in one fire only takes one DB round-trip per entity.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::EntityType;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityVariableRow {
    pub entity_type: EntityType,
    pub zone_id: i32,
    pub entity_id: i32,
    pub key: String,
    pub value: serde_json::Value,
}

/// All rows for one entity (type, zone, id). Returns an empty vec when
/// the entity has no persisted state — callers don't need to special-case
/// missing entities.
pub async fn list_for_entity(
    pool: &PgPool,
    entity_type: EntityType,
    zone_id: i32,
    entity_id: i32,
) -> sqlx::Result<Vec<EntityVariableRow>> {
    sqlx::query_as!(
        EntityVariableRow,
        r#"
        SELECT
            entity_type AS "entity_type: EntityType",
            zone_id,
            entity_id,
            key,
            value AS "value!: serde_json::Value"
        FROM entity_variables
        WHERE entity_type = $1 AND zone_id = $2 AND entity_id = $3
        ORDER BY key
        "#,
        entity_type as EntityType,
        zone_id,
        entity_id,
    )
    .fetch_all(pool)
    .await
}

/// Bulk read at boot — returns every row in the table. The caller (the
/// trigger catalog loader) groups rows by `(entity_type, zone_id,
/// entity_id)` into a `HashMap` and inserts as the
/// `EntityVariableCache` resource, so individual entity spawns can read
/// without touching the DB.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<EntityVariableRow>> {
    sqlx::query_as!(
        EntityVariableRow,
        r#"
        SELECT
            entity_type AS "entity_type: EntityType",
            zone_id,
            entity_id,
            key,
            value AS "value!: serde_json::Value"
        FROM entity_variables
        ORDER BY entity_type, zone_id, entity_id, key
        "#
    )
    .fetch_all(pool)
    .await
}

/// Upsert one row (INSERT … ON CONFLICT DO UPDATE). Used by the
/// occasional one-off write site that can't batch (e.g. admin tool).
/// Normal Lua-driven writes go through `upsert_many` from the flush
/// tick.
pub async fn upsert(
    pool: &PgPool,
    entity_type: EntityType,
    zone_id: i32,
    entity_id: i32,
    key: &str,
    value: &serde_json::Value,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        INSERT INTO entity_variables
            (entity_type, zone_id, entity_id, key, value, updated_at)
        VALUES ($1, $2, $3, $4, $5, NOW())
        ON CONFLICT (entity_type, zone_id, entity_id, key)
        DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()
        "#,
        entity_type as EntityType,
        zone_id,
        entity_id,
        key,
        value,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Batch upsert — caller passes `(key, value)` pairs for one entity. The
/// periodic flush tick uses this so a Lua body that does
/// `self:setvar("a", 1); self:setvar("b", 2)` takes one round-trip.
///
/// Empty `pairs` is a no-op (returns immediately). Issues one query per
/// row inside a single transaction so a mid-flight crash leaves the bag
/// consistent.
pub async fn upsert_many(
    pool: &PgPool,
    entity_type: EntityType,
    zone_id: i32,
    entity_id: i32,
    pairs: &[(String, serde_json::Value)],
) -> sqlx::Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for (key, value) in pairs {
        sqlx::query!(
            r#"
            INSERT INTO entity_variables
                (entity_type, zone_id, entity_id, key, value, updated_at)
            VALUES ($1, $2, $3, $4, $5, NOW())
            ON CONFLICT (entity_type, zone_id, entity_id, key)
            DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()
            "#,
            entity_type as EntityType,
            zone_id,
            entity_id,
            key,
            value,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Delete one row. Lua `self:clearvar(name)` queues this through the
/// flush tick (see `EntityVariableCache::drain_dirty` — keys whose
/// cache value is `None` get translated into a delete on flush).
/// `Ok(true)` when the row existed and was removed; `Ok(false)` when
/// no row matched the key.
pub async fn delete(
    pool: &PgPool,
    entity_type: EntityType,
    zone_id: i32,
    entity_id: i32,
    key: &str,
) -> sqlx::Result<bool> {
    let res = sqlx::query!(
        r#"
        DELETE FROM entity_variables
        WHERE entity_type = $1 AND zone_id = $2 AND entity_id = $3 AND key = $4
        "#,
        entity_type as EntityType,
        zone_id,
        entity_id,
        key,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    //! Integration tests against a live `fierydev`. Ignored by default;
    //! run with `cargo test -p mud-db -- --ignored`. Each test scopes
    //! itself to a never-used (zone, id) tuple and cleans up after
    //! itself so re-runs are idempotent.

    use super::*;
    use crate::connect;
    use crate::enums::EntityType;

    async fn pool() -> PgPool {
        let _ = dotenvy::from_path("../../.env");
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        connect(&url).await.expect("connect to fierydev")
    }

    /// Use (zone, id) tuples far above any real content. Different
    /// tests use different ids so the parallel test harness can't
    /// step on a sibling's cleanup mid-flight.
    const TEST_ZONE: i32 = 99_999;
    const TEST_ID_ROUND_TRIP: i32 = 99_900;
    const TEST_ID_BATCH: i32 = 99_901;

    async fn cleanup(p: &PgPool, id: i32) {
        let _ = sqlx::query!(
            r#"DELETE FROM entity_variables WHERE zone_id = $1 AND entity_id = $2"#,
            TEST_ZONE,
            id,
        )
        .execute(p)
        .await;
    }

    #[tokio::test]
    #[ignore = "requires live fierydev DB; run with: cargo test -p mud-db -- --ignored"]
    async fn round_trips_single_var() {
        let p = pool().await;
        let id = TEST_ID_ROUND_TRIP;
        cleanup(&p, id).await;
        let val = serde_json::json!({"count": 7});
        upsert(&p, EntityType::Mob, TEST_ZONE, id, "hello", &val)
            .await
            .expect("upsert");
        let rows = list_for_entity(&p, EntityType::Mob, TEST_ZONE, id)
            .await
            .expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "hello");
        assert_eq!(rows[0].value, val);
        // overwrite
        let new_val = serde_json::json!({"count": 8});
        upsert(&p, EntityType::Mob, TEST_ZONE, id, "hello", &new_val)
            .await
            .expect("upsert overwrite");
        let rows = list_for_entity(&p, EntityType::Mob, TEST_ZONE, id)
            .await
            .expect("list2");
        assert_eq!(rows.len(), 1, "upsert must not duplicate");
        assert_eq!(rows[0].value, new_val);
        assert!(delete(&p, EntityType::Mob, TEST_ZONE, id, "hello")
            .await
            .expect("del"));
        let rows = list_for_entity(&p, EntityType::Mob, TEST_ZONE, id)
            .await
            .expect("list3");
        assert!(rows.is_empty());
        cleanup(&p, id).await;
    }

    #[tokio::test]
    #[ignore = "requires live fierydev DB"]
    async fn batch_upsert_writes_each_pair() {
        let p = pool().await;
        let id = TEST_ID_BATCH;
        cleanup(&p, id).await;
        let pairs = vec![
            ("k1".to_string(), serde_json::json!(1)),
            ("k2".to_string(), serde_json::json!("two")),
            ("k3".to_string(), serde_json::json!({"x": true})),
        ];
        upsert_many(&p, EntityType::Object, TEST_ZONE, id, &pairs)
            .await
            .expect("batch upsert");
        let rows = list_for_entity(&p, EntityType::Object, TEST_ZONE, id)
            .await
            .expect("list");
        assert_eq!(rows.len(), 3);
        cleanup(&p, id).await;
    }
}
