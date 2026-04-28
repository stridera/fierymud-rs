//! Mob spawn-reset rows. Each `MobResets` row says "this mob `(zone_id, id)`
//! should exist in this room `(zone_id, id)`, capped at `max_instances`,
//! probabilistically every reset cycle". The runtime walks these on
//! startup to populate the world; eventually a tick system also re-spawns
//! after a death subject to the cycle and probability.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobReset {
    pub id: i32,
    pub mob_zone_id: i32,
    pub mob_id: i32,
    pub room_zone_id: i32,
    pub room_id: i32,
    /// Cap on simultaneous live instances of this reset.
    pub max_instances: i32,
    /// 0.0–1.0 probability that this reset fires on a given cycle.
    /// 1.0 means "always fires" (the most common case for permanent mobs).
    pub probability: f64,
    /// `PERSISTENT` (always exists) vs `ONCE` etc. — text in the schema,
    /// values aren't an enum yet. The runtime reads this verbatim and
    /// interprets the few values it cares about.
    pub reset_behavior: String,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<MobReset>> {
    sqlx::query_as!(
        MobReset,
        r#"
        SELECT
            id,
            mob_zone_id,
            mob_id,
            room_zone_id,
            room_id,
            max_instances,
            probability,
            reset_behavior
        FROM "MobResets"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
