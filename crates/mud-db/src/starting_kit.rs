//! `StartingKit` — what a new character is handed in their pack (legacy
//! `give_newbie_eq`: a common pack plus per-class gear). Builders edit the
//! rows; the game hands them out when a character is created.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartingKitRow {
    /// Class the row is limited to; `None` applies to every class.
    pub class_id: Option<i32>,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub quantity: i32,
}

/// Every kit row in `id` order (the order items are handed out), for the
/// boot-time `StartingKit` resource.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<StartingKitRow>> {
    sqlx::query_as!(
        StartingKitRow,
        r#"
        SELECT class_id, object_zone_id, object_id, quantity
        FROM "StartingKit"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
