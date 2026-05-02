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

/// Insert a placed item, returning the new row id so the runtime
/// can attach it to the spawned ECS entity for later removal.
pub async fn place_item(
    pool: &PgPool,
    room_id: i32,
    object_zone_id: i32,
    object_id: i32,
) -> sqlx::Result<i32> {
    let row = sqlx::query!(
        r#"
        INSERT INTO player_house_items (room_id, object_zone_id, object_id)
        VALUES ($1, $2, $3)
        RETURNING id
        "#,
        room_id,
        object_zone_id,
        object_id,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.id)
}

/// Delete one placed item by primary key. Returns the number of
/// rows actually removed (0 if the row was already gone).
pub async fn remove_item(pool: &PgPool, item_id: i32) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        r#"
        DELETE FROM player_house_items WHERE id = $1
        "#,
        item_id,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Create a fresh `PlayerHouse` row for a character that doesn't
/// own one yet. Also seeds a single foyer (`local_index = 0`) so
/// the house has at least one room. Returns `(house_id, foyer_id)`.
/// `Err` from a unique-constraint violation means the character
/// already owns a house.
pub async fn create_house(
    pool: &PgPool,
    character_id: &str,
    entrance_zone_id: i32,
    entrance_room_id: i32,
) -> sqlx::Result<(i32, i32)> {
    let mut tx = pool.begin().await?;
    let house_row = sqlx::query!(
        r#"
        INSERT INTO player_houses
            (character_id, entrance_room_zone_id, entrance_room_id)
        VALUES ($1, $2, $3)
        RETURNING id
        "#,
        character_id,
        entrance_zone_id,
        entrance_room_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    let foyer_row = sqlx::query!(
        r#"
        INSERT INTO player_house_rooms (house_id, local_index, name, description)
        VALUES ($1, 0, 'Your Foyer', 'A simple foyer welcomes you home.')
        RETURNING id
        "#,
        house_row.id,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((house_row.id, foyer_row.id))
}

/// Delete a house. Cascades remove rooms / items / guests / exits
/// via the FK constraints. Returns rows-affected on the parent
/// (0 if the house didn't exist).
pub async fn delete_house(pool: &PgPool, house_id: i32) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        r#"DELETE FROM player_houses WHERE id = $1"#,
        house_id,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Rename a house room (and optionally update its description).
/// Either field can be passed as `None` to leave it untouched.
pub async fn rename_room(
    pool: &PgPool,
    room_id: i32,
    new_name: Option<&str>,
    new_description: Option<&str>,
) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        r#"
        UPDATE player_house_rooms
        SET name        = COALESCE($1, name),
            description = COALESCE($2, description)
        WHERE id = $3
        "#,
        new_name,
        new_description,
        room_id,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Add a guest entry to a house. Idempotent on
/// `(house_id, character_id)` — re-adding the same guest is a
/// no-op if their `can_place` flag matches; otherwise the flag
/// is updated.
pub async fn add_guest(
    pool: &PgPool,
    house_id: i32,
    character_id: &str,
    can_place: bool,
) -> sqlx::Result<i32> {
    let row = sqlx::query!(
        r#"
        INSERT INTO player_house_guests (house_id, character_id, can_place)
        VALUES ($1, $2, $3)
        ON CONFLICT (house_id, character_id)
        DO UPDATE SET can_place = EXCLUDED.can_place
        RETURNING id
        "#,
        house_id,
        character_id,
        can_place,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.id)
}

/// Remove a guest from a house's access list. Returns the number
/// of rows deleted (0 if the character wasn't on the list).
pub async fn remove_guest(
    pool: &PgPool,
    house_id: i32,
    character_id: &str,
) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        r#"
        DELETE FROM player_house_guests
        WHERE house_id = $1 AND character_id = $2
        "#,
        house_id,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}
