//! Read-side queries for player housing. The schema models a
//! tree: one `PlayerHouse` per character, N `PlayerHouseRoom`
//! children, M `PlayerHouseExit` connecting them, items placed
//! per-room, and a guest list.
//!
//! Runtime use: on `home` / `house` commands, fetch the house
//! and synthesize ECS `Room` entities for the interior. Each room
//! is reset-free (not part of `MobResets` / `ObjectResets`); the
//! housing system is responsible for re-spawning placed items
//! from `PlayerHouseItem` on first enter.
#![allow(clippy::doc_markdown)]

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerHouseRow {
    pub id: i32,
    pub character_id: String,
    pub return_room_zone_id: Option<i32>,
    pub return_room_id: Option<i32>,
    pub entrance_room_zone_id: i32,
    pub entrance_room_id: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerHouseRoomRow {
    pub id: i32,
    pub house_id: i32,
    pub local_index: i32,
    pub name: String,
    pub description: String,
    pub is_peaceful: bool,
    pub base_light_level: i32,
    pub capacity: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerHouseExitRow {
    pub id: i32,
    pub from_room_id: i32,
    pub to_room_id: i32,
    /// Schema enum `Direction` — kept as text for now since the
    /// runtime mirrors it locally and the conversion is one
    /// match arm at the dispatch site.
    pub direction: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerHouseItemRow {
    pub id: i32,
    pub room_id: i32,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub condition: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerHouseGuestRow {
    pub id: i32,
    pub house_id: i32,
    pub character_id: String,
    pub can_place: bool,
}

/// Fetch the single PlayerHouse row for `character_id` if it
/// exists. Returns Ok(None) for characters who don't own a
/// house — every operation should branch on that without a
/// db query failure.
pub async fn for_character(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<Option<PlayerHouseRow>> {
    sqlx::query_as!(
        PlayerHouseRow,
        r#"
        SELECT
            id,
            character_id,
            return_room_zone_id,
            return_room_id,
            entrance_room_zone_id,
            entrance_room_id
        FROM player_houses
        WHERE character_id = $1
        "#,
        character_id
    )
    .fetch_optional(pool)
    .await
}

/// All rooms in a house, ordered by `local_index`.
pub async fn rooms_for_house(
    pool: &PgPool,
    house_id: i32,
) -> sqlx::Result<Vec<PlayerHouseRoomRow>> {
    sqlx::query_as!(
        PlayerHouseRoomRow,
        r#"
        SELECT
            id,
            house_id,
            local_index,
            name,
            description,
            is_peaceful,
            base_light_level,
            capacity
        FROM player_house_rooms
        WHERE house_id = $1
        ORDER BY local_index
        "#,
        house_id
    )
    .fetch_all(pool)
    .await
}

/// All exits among a house's rooms. Caller resolves
/// from_room_id / to_room_id back into local indexes for the
/// runtime adjacency table.
pub async fn exits_for_house(
    pool: &PgPool,
    house_id: i32,
) -> sqlx::Result<Vec<PlayerHouseExitRow>> {
    sqlx::query_as!(
        PlayerHouseExitRow,
        r#"
        SELECT e.id, e.from_room_id, e.to_room_id, e.direction::text AS "direction!: String"
        FROM player_house_exits e
        JOIN player_house_rooms r ON r.id = e.from_room_id
        WHERE r.house_id = $1
        "#,
        house_id
    )
    .fetch_all(pool)
    .await
}

/// All items placed in any room of a house.
pub async fn items_for_house(
    pool: &PgPool,
    house_id: i32,
) -> sqlx::Result<Vec<PlayerHouseItemRow>> {
    sqlx::query_as!(
        PlayerHouseItemRow,
        r#"
        SELECT i.id, i.room_id, i.object_zone_id, i.object_id, i.condition
        FROM player_house_items i
        JOIN player_house_rooms r ON r.id = i.room_id
        WHERE r.house_id = $1
        "#,
        house_id
    )
    .fetch_all(pool)
    .await
}

/// All guests for a house.
pub async fn guests_for_house(
    pool: &PgPool,
    house_id: i32,
) -> sqlx::Result<Vec<PlayerHouseGuestRow>> {
    sqlx::query_as!(
        PlayerHouseGuestRow,
        r#"
        SELECT id, house_id, character_id, can_place
        FROM player_house_guests
        WHERE house_id = $1
        "#,
        house_id
    )
    .fetch_all(pool)
    .await
}
