//! `MobDefaultEffects` — effects every spawned instance of a mob proto
//! carries from birth (e.g. a wolf that always `detect_invisible`s).
//! `modifier_data` may carry a `flag` that overrides the `Effect` row's
//! own `default_params.flag`.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobDefaultEffectRow {
    pub mob_zone_id: i32,
    pub mob_id: i32,
    /// FK into `Effect.id`, resolved through `EffectCatalog` at spawn.
    pub effect_id: i32,
    pub strength: i32,
    pub modifier_data: serde_json::Value,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<MobDefaultEffectRow>> {
    sqlx::query_as!(
        MobDefaultEffectRow,
        r#"
        SELECT
            mob_zone_id,
            mob_id,
            effect_id,
            strength,
            modifier_data AS "modifier_data!: serde_json::Value"
        FROM "MobDefaultEffects"
        ORDER BY mob_zone_id, mob_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
