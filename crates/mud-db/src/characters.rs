use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::{Permission, PlayerFlag};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterRow {
    pub id: String,
    pub name: String,
    pub user_id: Option<String>,
    pub level: i32,
    pub hit_points: i32,
    pub hit_points_max: i32,
    pub stamina: i32,
    pub stamina_max: i32,
    pub hit_roll: i32,
    pub damage_roll: i32,
    pub armor_class: i32,
    pub alignment: i32,
    pub permissions: Vec<Permission>,
    pub player_flags: Vec<PlayerFlag>,
    pub prompt: String,
    pub current_room_zone_id: Option<i32>,
    pub current_room_id: Option<i32>,
    pub recall_room_zone_id: Option<i32>,
    pub recall_room_id: Option<i32>,
    /// FK to `Class.id`. Optional in the schema; characters created
    /// before class selection has no class yet.
    pub class_id: Option<i32>,
    /// Schema enum (HUMAN / ELF / GNOME / ...) — kept as the raw text
    /// label since the runtime only displays it.
    pub race: String,
    pub experience: i32,
    /// Player-set "the Wanderer" / "Slayer of Kobolds" line shown after
    /// the character name on `who`. None when unset.
    pub title: Option<String>,
    /// Free-form prose shown to anyone using `examine` on this
    /// character. None when unset; rendered with XML-Lite color tags
    /// like room descriptions.
    pub description: Option<String>,
}

// One UPDATE-per-column-set is the simplest call site for this many fields;
// a SaveState struct would just shuffle the names. Revisit if it grows.
#[allow(clippy::too_many_arguments)]
pub async fn save_state(
    pool: &PgPool,
    character_id: &str,
    hit_points: i32,
    stamina: i32,
    current_room_zone_id: Option<i32>,
    current_room_id: Option<i32>,
    recall_room_zone_id: Option<i32>,
    recall_room_id: Option<i32>,
    player_flags: &[PlayerFlag],
    prompt: &str,
    title: Option<&str>,
    description: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        UPDATE "Characters"
        SET hit_points = $1,
            stamina = $2,
            current_room_zone_id = $3,
            current_room_id = $4,
            recall_room_zone_id = $5,
            recall_room_id = $6,
            player_flags = $7,
            prompt = $8,
            title = $9,
            description = $10,
            last_login = NOW()
        WHERE id = $11
        "#,
        hit_points,
        stamina,
        current_room_zone_id,
        current_room_id,
        recall_room_zone_id,
        recall_room_id,
        player_flags as &[PlayerFlag],
        prompt,
        title,
        description,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_for_user(pool: &PgPool, user_id: &str) -> sqlx::Result<Vec<CharacterRow>> {
    sqlx::query_as!(
        CharacterRow,
        r#"
        SELECT
            id,
            name,
            user_id,
            level,
            hit_points,
            hit_points_max,
            stamina,
            stamina_max,
            hit_roll,
            damage_roll,
            armor_class,
            alignment,
            permissions AS "permissions!: Vec<Permission>",
            player_flags AS "player_flags!: Vec<PlayerFlag>",
            prompt,
            current_room_zone_id,
            current_room_id,
            recall_room_zone_id,
            recall_room_id,
            class_id,
            race::text AS "race!: String",
            experience,
            title,
            description
        FROM "Characters"
        WHERE user_id = $1
        ORDER BY level DESC, name
        "#,
        user_id
    )
    .fetch_all(pool)
    .await
}
