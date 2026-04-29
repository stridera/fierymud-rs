use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::{Direction, ExitState};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomExit {
    pub id: i32,
    pub room_zone_id: i32,
    pub room_id: i32,
    pub direction: Direction,
    pub to_zone_id: Option<i32>,
    pub to_room_id: Option<i32>,
    pub default_state: ExitState,
    /// Composite key for `unlock`. Both halves NULL means "no key".
    /// The pair points at an `Objects(zoneId, id)` row.
    pub key_zone_id: Option<i32>,
    pub key_id: Option<i32>,
}

pub async fn list_exits(pool: &PgPool) -> sqlx::Result<Vec<RoomExit>> {
    sqlx::query_as!(
        RoomExit,
        r#"
        SELECT
            id,
            room_zone_id,
            room_id,
            direction AS "direction: Direction",
            to_zone_id,
            to_room_id,
            default_state AS "default_state: ExitState",
            key_zone_id,
            key_id
        FROM "RoomExit"
        ORDER BY room_zone_id, room_id, direction
        "#
    )
    .fetch_all(pool)
    .await
}
