//! Object spawn-reset rows. Each `ObjectResets` row says "this object
//! `(zone_id, id)` should exist in this room `(zone_id, id)`". Same shape as
//! `MobResets`. Companion table `ObjectResetContents` adds nested-content
//! resets (chest contains scroll, etc.) — that loader is a follow-up.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectReset {
    pub id: i32,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub room_zone_id: i32,
    pub room_id: i32,
    pub max_instances: i32,
    pub probability: f64,
    pub reset_behavior: String,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<ObjectReset>> {
    sqlx::query_as!(
        ObjectReset,
        r#"
        SELECT
            id,
            object_zone_id,
            object_id,
            room_zone_id,
            room_id,
            max_instances,
            probability,
            reset_behavior
        FROM "ObjectResets"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
