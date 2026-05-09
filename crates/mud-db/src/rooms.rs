use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::Sector;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub zone_id: i32,
    pub id: i32,
    pub name: String,
    pub room_description: String,
    pub sector: Sector,
    pub base_light_level: i32,
    pub capacity: i32,
    /// Schema's `is_peaceful` flag — combat is refused in these
    /// rooms (sanctuaries, shop interiors, quest hubs). Loaded
    /// into a `PeacefulRoom` marker on the spawned room entity.
    pub is_peaceful: bool,
    /// Builder-authored mapper coordinates. Nullable per axis;
    /// when any are set the runtime threads them through to the
    /// `Room.Info` GMCP frame so client-side mappers (Mudlet,
    /// MUSHclient) can place rooms exactly where the builder
    /// laid them out instead of falling back to compass-walk
    /// auto-placement. Z defaults to 0 in the schema; X/Y are
    /// genuinely Option (rooms imported from legacy sources
    /// without coordinates carry NULL).
    pub layout_x: Option<i32>,
    pub layout_y: Option<i32>,
    pub layout_z: Option<i32>,
}

pub async fn list_rooms(pool: &PgPool) -> sqlx::Result<Vec<Room>> {
    sqlx::query_as!(
        Room,
        r#"
        SELECT
            zone_id,
            id,
            name,
            room_description,
            sector AS "sector: Sector",
            base_light_level,
            capacity,
            is_peaceful,
            layout_x,
            layout_y,
            layout_z
        FROM "Room"
        WHERE deleted_at IS NULL
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
