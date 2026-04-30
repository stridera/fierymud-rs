//! Per-ability target validation rules. Each row is keyed by
//! `ability_id` (UNIQUE) and lists which `TargetType`s are acceptable
//! plus the scope (SINGLE / AREA / etc), max targets, and LOS flag.
//!
//! 9 of 408 abilities have a row today. Abilities without a row fall
//! through to the dispatcher's existing target-resolution logic.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityTargetingRow {
    pub ability_id: i32,
    /// Acceptable `TargetType` labels (e.g. `ENEMY_PC`, `ENEMY_NPC`,
    /// `CORPSE`, `OBJECT_INV`, `RIDER`, `UNCONSCIOUS`). Held as
    /// strings so the runtime can grow per-type interpretation
    /// incrementally without touching the schema query.
    pub valid_targets: Vec<String>,
    pub scope: String,
    pub scope_pattern: Option<String>,
    pub max_targets: i32,
    pub range: i32,
    pub require_los: bool,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<AbilityTargetingRow>> {
    sqlx::query_as!(
        AbilityTargetingRow,
        r#"
        SELECT
            ability_id,
            valid_targets::text[] AS "valid_targets!: Vec<String>",
            scope::text AS "scope!: String",
            scope_pattern,
            max_targets,
            range,
            require_los
        FROM "AbilityTargeting"
        ORDER BY ability_id
        "#
    )
    .fetch_all(pool)
    .await
}
