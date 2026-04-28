//! Equipment rows attached to a `MobResets` entry. Each row says: the mob
//! spawned by `reset_id` is wearing/holding object `(object_zone_id,
//! object_id)` in slot `wear_location`. Probability and `decorative`
//! mirror the `MobResets` semantics — decorative items can't be removed
//! by players, but we don't yet model that distinction at runtime.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobResetEquipment {
    pub id: i32,
    /// FK to `MobResets.id` — the mob this equipment belongs to.
    pub reset_id: i32,
    pub object_zone_id: i32,
    pub object_id: i32,
    /// Free-text slot name; the runtime maps to `mud_world::Slot` via
    /// `Slot::from_label`. Unknown slots are skipped on load.
    pub wear_location: Option<String>,
    pub max_instances: i32,
    pub probability: f64,
    /// Decorative items are flavor-only and shouldn't be removable by
    /// players. The runtime doesn't yet enforce this — currently treated
    /// the same as a normal equipped item.
    pub decorative: bool,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<MobResetEquipment>> {
    sqlx::query_as!(
        MobResetEquipment,
        r#"
        SELECT
            id,
            reset_id,
            object_zone_id,
            object_id,
            wear_location,
            max_instances,
            probability,
            decorative
        FROM "MobResetEquipment"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
