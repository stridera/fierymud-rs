use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::{Climate, Hemisphere, ResetMode};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Zone {
    pub id: i32,
    pub name: String,
    pub lifespan: i32,
    pub reset_mode: ResetMode,
    pub hemisphere: Hemisphere,
    pub climate: Climate,
    /// `Zones.is_god_zone` — staff-only content: hidden from mortals, grants
    /// no exploration credit, never a random teleport destination.
    pub is_god_zone: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

pub async fn list_zones(pool: &PgPool) -> sqlx::Result<Vec<Zone>> {
    sqlx::query_as!(
        Zone,
        r#"
        SELECT
            id,
            name,
            lifespan,
            "resetMode" AS "reset_mode: ResetMode",
            hemisphere AS "hemisphere: Hemisphere",
            climate AS "climate: Climate",
            is_god_zone,
            "created_at" AS "created_at: DateTime<Utc>",
            "updated_at" AS "updated_at: DateTime<Utc>",
            "deleted_at" AS "deleted_at: DateTime<Utc>"
        FROM "Zones"
        WHERE "deleted_at" IS NULL
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}

/// [`list_zones`] for a database that predates `Zones.is_god_zone` (the column
/// is created by the muditor schema / `muditor/deploy/prod/sql/090-zones-is-god-zone.sql`,
/// which prod can apply after the game deploy): every zone reads as not a god
/// zone. Only the loader's missing-column fallback should call this.
pub async fn list_zones_without_god_flag(pool: &PgPool) -> sqlx::Result<Vec<Zone>> {
    sqlx::query_as!(
        Zone,
        r#"
        SELECT
            id,
            name,
            lifespan,
            "resetMode" AS "reset_mode: ResetMode",
            hemisphere AS "hemisphere: Hemisphere",
            climate AS "climate: Climate",
            false AS "is_god_zone!",
            "created_at" AS "created_at: DateTime<Utc>",
            "updated_at" AS "updated_at: DateTime<Utc>",
            "deleted_at" AS "deleted_at: DateTime<Utc>"
        FROM "Zones"
        WHERE "deleted_at" IS NULL
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
