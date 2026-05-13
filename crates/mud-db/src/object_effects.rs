//! `ObjectEffects` — per-proto effect grants triggered by wearing or
//! wielding the item (e.g. a *ring of invisibility* spawning an INVIS
//! `EffectInstance` on the wearer for as long as it's worn).
//! `wear_location` optionally restricts the grant to a specific slot
//! (a "ring of haste" only fires when worn on a finger, not when
//! held). `modifier_data` is free-form per-effect parameters merged
//! into the spawned EffectInstance.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::WearFlag;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectEffectRow {
    pub object_zone_id: i32,
    pub object_id: i32,
    /// FK into `Effect.id`. Resolved through `EffectCatalog` at
    /// apply time.
    pub effect_id: i32,
    pub strength: i32,
    pub modifier_data: serde_json::Value,
    pub wear_location: Option<WearFlag>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<ObjectEffectRow>> {
    sqlx::query_as!(
        ObjectEffectRow,
        r#"
        SELECT
            object_zone_id,
            object_id,
            effect_id,
            strength,
            modifier_data AS "modifier_data!: serde_json::Value",
            wear_location AS "wear_location?: WearFlag"
        FROM "ObjectEffects"
        ORDER BY object_zone_id, object_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
