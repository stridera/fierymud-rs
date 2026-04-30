//! Per-ability multi-element damage split. Each row is one element
//! contributing a percentage-weighted damage formula to the total
//! damage of the cast. Used by spells like `CONE_OF_COLD`
//! (90% COLD, 10% FORCE) where resistances would later eat per-element.
//!
//! 32 rows in the schema today, covering ~16 spells. The runtime
//! sums each component's evaluated formula scaled by `percentage / 100`
//! to derive total damage; per-element resistance application is a
//! follow-up that needs Resistances components on entities first.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityDamageComponentRow {
    pub ability_id: i32,
    /// Schema enum label (FIRE / COLD / SHOCK / etc). Held as raw
    /// text since the runtime doesn't model resistance per element
    /// yet.
    pub element: String,
    pub damage_formula: String,
    /// 0..=100. Total contribution to overall damage:
    /// `evaluate(damage_formula) * percentage / 100`.
    pub percentage: i32,
    /// Display ordering and tie-breaker for animation timing.
    pub sequence: i32,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<AbilityDamageComponentRow>> {
    sqlx::query_as!(
        AbilityDamageComponentRow,
        r#"
        SELECT
            ability_id,
            element::text AS "element!: String",
            damage_formula,
            percentage,
            sequence
        FROM "AbilityDamageComponent"
        ORDER BY ability_id, sequence
        "#
    )
    .fetch_all(pool)
    .await
}
