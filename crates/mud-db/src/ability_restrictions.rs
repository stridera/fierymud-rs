//! Per-ability requirement rules. Each row holds a JSONB array of rule
//! objects shaped like
//!   `{"type": "weapon_type", "weaponType": "piercing", "message": "..."}`
//! The `type` field drives runtime gating logic when real casting
//! lands. For now the runtime only consumes the human-readable
//! `message` strings to display in the cast/chant/perform output.
//!
//! 26 of the 408 abilities have a row here (most spells are
//! unrestricted at this level — class restrictions live in
//! `ClassAbilities` instead).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityRestrictionRow {
    pub ability_id: i32,
    /// Each entry is one rule object. We hold them as opaque JSON so
    /// the runtime can grow richer interpretation later without
    /// re-touching the schema query.
    pub requirements: Vec<serde_json::Value>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<AbilityRestrictionRow>> {
    sqlx::query_as!(
        AbilityRestrictionRow,
        r#"
        SELECT
            ability_id,
            requirements AS "requirements!: Vec<serde_json::Value>"
        FROM "AbilityRestrictions"
        ORDER BY ability_id
        "#
    )
    .fetch_all(pool)
    .await
}
