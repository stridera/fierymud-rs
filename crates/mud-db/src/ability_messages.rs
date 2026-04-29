//! Per-ability templated message strings emitted during the ability
//! lifecycle (start / success / fail / wearoff). The runtime renders
//! these via simple placeholder substitution (`{actor.name}`,
//! `{target.name}`, etc.) rather than the per-handler
//! `format!(...)` calls every cmd_* used to use.
//!
//! Schema convention: optional everywhere — if a field is null, the
//! runtime falls back to its terse default rendering (e.g.
//! "you cast X — heal (+5 HP)"). 383 of 408 abilities have a row.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityMessageRow {
    pub ability_id: i32,
    pub start_to_caster: Option<String>,
    pub start_to_victim: Option<String>,
    pub start_to_room: Option<String>,
    pub success_to_caster: Option<String>,
    pub success_to_victim: Option<String>,
    pub success_to_room: Option<String>,
    /// Fired only when caster == target. Replaces the
    /// `success_to_caster` (and `success_to_room`) pair when the
    /// player self-targets.
    pub success_to_self: Option<String>,
    pub success_self_room: Option<String>,
    pub fail_to_caster: Option<String>,
    pub fail_to_victim: Option<String>,
    pub fail_to_room: Option<String>,
    /// Emitted when a duration-tracked effect expires on the target.
    pub wearoff_to_target: Option<String>,
    pub wearoff_to_room: Option<String>,
    /// Returned by `look at <effect>` / equivalent inspection.
    pub look_message: Option<String>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<AbilityMessageRow>> {
    sqlx::query_as!(
        AbilityMessageRow,
        r#"
        SELECT
            ability_id,
            start_to_caster,
            start_to_victim,
            start_to_room,
            success_to_caster,
            success_to_victim,
            success_to_room,
            success_to_self,
            success_self_room,
            fail_to_caster,
            fail_to_victim,
            fail_to_room,
            wearoff_to_target,
            wearoff_to_room,
            look_message
        FROM "AbilityMessages"
        ORDER BY ability_id
        "#
    )
    .fetch_all(pool)
    .await
}
