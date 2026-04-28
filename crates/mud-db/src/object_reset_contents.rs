//! Nested-content rows for `ObjectResets`. Each row says: when the
//! container spawned by `reset_id` (or another content row referenced
//! via `parent_content_id`) materializes, also spawn this object inside
//! it. `quantity` allows multi-copy nesting like "chest contains 3
//! arrows".
//!
//! The `parent_content_id` chain forms a tree rooted at the
//! `ObjectResets` entry. fierydev today has 106 rows with max depth 2 —
//! a simple two-pass spawn (top-level first, nested second) is enough.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectResetContent {
    pub id: i32,
    /// FK to `ObjectResets.id` — the top-level reset this content belongs to.
    pub reset_id: i32,
    /// FK to another `ObjectResetContents.id` — when set, the parent is
    /// a content row rather than the reset's container directly.
    pub parent_content_id: Option<i32>,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub quantity: i32,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<ObjectResetContent>> {
    sqlx::query_as!(
        ObjectResetContent,
        r#"
        SELECT
            id,
            reset_id,
            parent_content_id,
            object_zone_id,
            object_id,
            quantity
        FROM "ObjectResetContents"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
