//! Per-ability saving-throw rules. Each row sets the save type
//! (`FORTITUDE` / `REFLEX` / `WILL`), a `dc_formula` evaluated from
//! the caster's perspective, and an `on_save_action` enum value
//! describing what happens to the effects on a successful save
//! (`NEGATE` / `HALF_DURATION` / etc).
//!
//! Today only 2 rows exist (`BASH` FORTITUDE, `TRIP_UP` REFLEX); the
//! runtime evaluator supports the small subset they use and falls
//! through to "save fails / effects apply" for unknown action labels.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilitySavingThrowRow {
    pub ability_id: i32,
    /// `FORTITUDE` / `REFLEX` / `WILL` — held as the raw text so
    /// the runtime can interpret incrementally.
    pub save_type: String,
    /// Formula evaluated from caster's `FormulaCtx`. Standard shape
    /// is `10 + skill / 5 + str_bonus` (or `dex_bonus` for REFLEX).
    pub dc_formula: String,
    /// Action label as a JSON value. Schema stores a string like
    /// `"NEGATE"` or `"HALF_DURATION"`; some content uses richer
    /// objects so we keep the raw `serde_json::Value`.
    pub on_save_action: serde_json::Value,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<AbilitySavingThrowRow>> {
    sqlx::query_as!(
        AbilitySavingThrowRow,
        r#"
        SELECT
            ability_id,
            "saveType"::text AS "save_type!: String",
            dc_formula,
            on_save_action AS "on_save_action!: serde_json::Value"
        FROM "AbilitySavingThrow"
        ORDER BY ability_id
        "#
    )
    .fetch_all(pool)
    .await
}
