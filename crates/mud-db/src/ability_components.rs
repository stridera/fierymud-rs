//! Per-ability material reagent rows. Each row binds an
//! `ability_id` to an `object_id` (proto) the caster must carry to
//! cast — `required=true` makes it a hard gate, `consumed=true`
//! removes one instance on success.
//!
//! Rare in fierydev today (a handful of spells reference reagents),
//! but the table is wired so content can flesh it out without
//! runtime changes.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityComponentRow {
    pub id: i32,
    pub ability_id: i32,
    /// The Object proto id (FK to Objects.id). Note this is the
    /// non-zone-scoped legacy id; runtime resolution walks
    /// `ObjectPrototypes` for any proto whose `id` matches.
    pub object_id: i32,
    pub consumed: bool,
    pub required: bool,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<AbilityComponentRow>> {
    sqlx::query_as!(
        AbilityComponentRow,
        r#"
        SELECT
            id,
            ability_id,
            object_id,
            consumed,
            required
        FROM "AbilityComponent"
        ORDER BY ability_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
