//! `ObjectResistance` — per-element resistance modifier granted while
//! the item is worn / wielded. `value` is a signed percentage:
//! positive = mitigate (+25 = take 75% damage from this element),
//! negative = vulnerable (-25 = take 125% damage). The schema allows
//! values past 100 only when `allow_absorption = true`; otherwise the
//! combat tick clamps total resistance at 100 (no infinite-immunity
//! stacks).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::ElementType;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectResistanceRow {
    pub object_zone_id: i32,
    pub object_id: i32,
    pub element: ElementType,
    pub value: i32,
    pub allow_absorption: bool,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<ObjectResistanceRow>> {
    sqlx::query_as!(
        ObjectResistanceRow,
        r#"
        SELECT
            object_zone_id,
            object_id,
            element AS "element: ElementType",
            value,
            allow_absorption
        FROM "ObjectResistance"
        ORDER BY object_zone_id, object_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
