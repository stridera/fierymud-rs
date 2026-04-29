//! Mapping from abilities to the effects they apply. Each row says
//! "ability X spawns effect Y when triggered, optionally with parameter
//! overrides". Triggers and chances let one ability fan out to multiple
//! effects with different conditions.
//!
//! 395 rows in `fierydev` cover most spells, chants, and a handful of
//! effect-bearing skills. The runtime today reads them just for
//! display in the cast/chant/perform metadata; real application will
//! consume `override_params`, `trigger`, `chance_pct`, and `condition`
//! when the casting pipeline lands.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityEffectRow {
    pub ability_id: i32,
    pub effect_id: i32,
    /// JSONB blob of per-mapping overrides (damage formula, area flag,
    /// component breakdown, etc.). Kept opaque until the casting
    /// pipeline interprets it.
    pub override_params: Option<serde_json::Value>,
    /// Within-ability ordering (matters when an ability emits multiple
    /// effects in sequence).
    pub order: i32,
    /// Free-text trigger label — historically `on_cast`, `on_hit`, etc.
    pub trigger: Option<String>,
    /// Probability percent that this mapping fires (default 100).
    pub chance_pct: i32,
    /// Free-text condition — interpreted by the casting pipeline.
    pub condition: Option<String>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<AbilityEffectRow>> {
    sqlx::query_as!(
        AbilityEffectRow,
        r#"
        SELECT
            ability_id,
            effect_id,
            override_params,
            "order",
            trigger,
            chance_pct,
            condition
        FROM "AbilityEffect"
        ORDER BY ability_id, "order"
        "#
    )
    .fetch_all(pool)
    .await
}
