//! Effects that fire when a consumable is eaten/drunk. Two binding
//! shapes share the same `Effect`-pointing structure:
//!
//! - Per-object (specific potion proto): `object_zone_id`/`object_id`
//!   are set, `liquid_id` is null. Rare, used for one-of-a-kind
//!   magical items.
//! - Per-liquid (any container holding this liquid): `liquid_id` is
//!   set, the object FK pair is null. The common case for ales,
//!   potions of healing, poisoned cocktails, etc.
//!
//! `ConsumableEffects` is empty in fierydev today — this module
//! exists so the `eat`/`drink`/`quaff` runtime is wired the moment
//! content lands.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsumableEffectRow {
    pub effect_id: i32,
    /// 0.0–1.0 probability gate; the runtime rolls per-row before
    /// spawning the effect.
    pub chance: f64,
    /// Caster-equivalent skill level for formula evaluation.
    pub level: i32,
    /// Duration in seconds; None means use the Effect catalog's
    /// `default_params` duration.
    pub duration: Option<i32>,
}

/// Effects bound to a specific Object proto (via composite zone+id).
pub async fn list_for_object(
    pool: &PgPool,
    zone_id: i32,
    object_id: i32,
) -> sqlx::Result<Vec<ConsumableEffectRow>> {
    sqlx::query_as!(
        ConsumableEffectRow,
        r#"
        SELECT effect_id, chance, level, duration
        FROM "ConsumableEffects"
        WHERE object_zone_id = $1 AND object_id = $2
        "#,
        zone_id,
        object_id,
    )
    .fetch_all(pool)
    .await
}

/// Effects bound to a specific Liquid (any drink container holding
/// it triggers them on drink/sip).
pub async fn list_for_liquid(
    pool: &PgPool,
    liquid_id: i32,
) -> sqlx::Result<Vec<ConsumableEffectRow>> {
    sqlx::query_as!(
        ConsumableEffectRow,
        r#"
        SELECT effect_id, chance, level, duration
        FROM "ConsumableEffects"
        WHERE liquid_id = $1
        "#,
        liquid_id,
    )
    .fetch_all(pool)
    .await
}
