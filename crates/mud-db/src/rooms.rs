use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::Sector;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub zone_id: i32,
    pub id: i32,
    pub name: String,
    pub sector: Sector,
    pub base_light_level: i32,
    pub capacity: i32,
}

pub async fn list_rooms(pool: &PgPool) -> sqlx::Result<Vec<Room>> {
    sqlx::query_as!(
        Room,
        r#"
        SELECT
            zone_id,
            id,
            name,
            sector AS "sector: Sector",
            base_light_level,
            capacity
        FROM "Room"
        WHERE deleted_at IS NULL
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
