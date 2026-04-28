use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Effect {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    pub effect_type: String,
    pub tags: Vec<String>,
}

pub async fn list_effects(pool: &PgPool) -> sqlx::Result<Vec<Effect>> {
    sqlx::query_as!(
        Effect,
        r#"
        SELECT
            id,
            name,
            description,
            "effectType" AS effect_type,
            tags AS "tags!: Vec<String>"
        FROM "Effect"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
