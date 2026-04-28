use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::Permission;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterRow {
    pub id: String,
    pub name: String,
    pub user_id: Option<String>,
    pub level: i32,
    pub hit_points: i32,
    pub hit_points_max: i32,
    pub hit_roll: i32,
    pub damage_roll: i32,
    pub armor_class: i32,
    pub alignment: i32,
    pub permissions: Vec<Permission>,
    pub current_room_zone_id: Option<i32>,
    pub current_room_id: Option<i32>,
    pub recall_room_zone_id: Option<i32>,
    pub recall_room_id: Option<i32>,
}

pub async fn save_state(
    pool: &PgPool,
    character_id: &str,
    hit_points: i32,
    current_room_zone_id: Option<i32>,
    current_room_id: Option<i32>,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        UPDATE "Characters"
        SET hit_points = $1,
            current_room_zone_id = $2,
            current_room_id = $3,
            last_login = NOW()
        WHERE id = $4
        "#,
        hit_points,
        current_room_zone_id,
        current_room_id,
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
            hit_roll,
            damage_roll,
            armor_class,
            alignment,
            permissions AS "permissions!: Vec<Permission>",
            current_room_zone_id,
            current_room_id,
            recall_room_zone_id,
            recall_room_id
        FROM "Characters"
        WHERE user_id = $1
        ORDER BY level DESC, name
        "#,
        user_id
    )
    .fetch_all(pool)
    .await
}
