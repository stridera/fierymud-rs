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
    /// `Slot::from_label_warn`. Unknown slots log one warning per label and the
    /// item is carried instead of worn.
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

/// One item nested inside a container a mob carries or wears (legacy `G` /
/// `E` followed by `P`). Hangs off a [`MobResetEquipment`] row and nests
/// through `parent_content_id` exactly like
/// [`crate::object_reset_contents::ObjectResetContent`] does for room
/// containers. Table `MobResetEquipmentContents`; created by the Muditor schema
/// / `fierylib/data/sql/2026-10-11-mob-gear-contents.sql`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobResetEquipmentContent {
    pub id: i32,
    /// FK to `MobResetEquipment.id` — the carried/worn container.
    pub equipment_id: i32,
    /// FK to another content row when nested deeper than the equipment item.
    pub parent_content_id: Option<i32>,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub quantity: i32,
    /// Legacy `P` max: world-wide cap on live copies of this object.
    pub max_instances: i32,
}

pub async fn list_contents(pool: &PgPool) -> sqlx::Result<Vec<MobResetEquipmentContent>> {
    sqlx::query_as!(
        MobResetEquipmentContent,
        r#"
        SELECT
            id,
            equipment_id,
            parent_content_id,
            object_zone_id,
            object_id,
            quantity,
            max_instances
        FROM "MobResetEquipmentContents"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
