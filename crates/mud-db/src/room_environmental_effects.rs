//! Junction table reads for `RoomEnvironmentalEffect`.
//!
//! Every row is a (room composite key, effect id) link. The
//! runtime hydrates a resource at boot and applies each linked
//! effect to anyone walking into the room.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RoomEnvironmentalEffectRow {
    pub room_zone_id: i32,
    pub room_id: i32,
    pub effect_id: i32,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<RoomEnvironmentalEffectRow>> {
    sqlx::query_as!(
        RoomEnvironmentalEffectRow,
        r#"
        SELECT room_zone_id, room_id, effect_id
        FROM "RoomEnvironmentalEffect"
        "#,
    )
    .fetch_all(pool)
    .await
}
