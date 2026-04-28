use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterRow {
    pub id: String,
    pub name: String,
    pub user_id: Option<String>,
    pub level: i32,
    pub current_room_zone_id: Option<i32>,
    pub current_room_id: Option<i32>,
    pub recall_room_zone_id: Option<i32>,
    pub recall_room_id: Option<i32>,
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
