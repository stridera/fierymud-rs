use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Effect {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    pub effect_type: String,
    pub tags: Vec<String>,
    pub presence_override: Option<String>,
    /// JSONB blob of the effect's default parameters (duration, amount,
    /// target field, etc.). The `AbilityEffect` row's `override_params`
    /// takes precedence; this is the secondary fallback before the
    /// runtime's hardcoded global default.
    pub default_params: serde_json::Value,
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
            tags AS "tags!: Vec<String>",
            presence_override,
            default_params
        FROM "Effect"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
