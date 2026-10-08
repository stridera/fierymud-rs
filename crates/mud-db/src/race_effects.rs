//! `RaceEffects` — permanent innate effects every member of a race
//! carries (legacy `races[race].effect_flags`: elves see infravision,
//! dragons fly, ...). Like `MobDefaultEffects`, `modifier_data` carries
//! the status `flags` array (or a singular `flag`) the row grants.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaceEffectRow {
    /// `Race` enum value as raw text (ELF / DROW / ...).
    pub race: String,
    /// FK into `Effect.id`, resolved through `EffectCatalog` at apply time.
    pub effect_id: i32,
    pub strength: i32,
    pub modifier_data: serde_json::Value,
}

/// Every race-effect row, for the boot-time `RaceEffectCatalog` load.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<RaceEffectRow>> {
    sqlx::query_as!(
        RaceEffectRow,
        r#"
        SELECT
            race::text AS "race!: String",
            effect_id,
            strength,
            modifier_data AS "modifier_data!: serde_json::Value"
        FROM "RaceEffects"
        ORDER BY race::text, id
        "#
    )
    .fetch_all(pool)
    .await
}
