//! `RoomExtraDescriptions` — keyword-addressable flavor text on
//! a room ("look fountain" / "look painting" / "look at the
//! tapestry"). Builders use these to add detail without modeling
//! the thing as a real Object entity.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomExtra {
    pub room_zone_id: i32,
    pub room_id: i32,
    pub keywords: Vec<String>,
    pub description: String,
}

pub async fn list_extras(pool: &PgPool) -> sqlx::Result<Vec<RoomExtra>> {
    sqlx::query_as!(
        RoomExtra,
        r#"
        SELECT
            room_zone_id,
            room_id,
            keywords AS "keywords!: Vec<String>",
            description
        FROM "RoomExtraDescriptions"
        ORDER BY room_zone_id, room_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
