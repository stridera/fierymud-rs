use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialRow {
    pub id: i32,
    pub name: String,
    pub hide: bool,
    pub char_no_arg: Option<String>,
    pub others_no_arg: Option<String>,
    pub char_found: Option<String>,
    pub others_found: Option<String>,
    pub vict_found: Option<String>,
    pub not_found: Option<String>,
    pub char_auto: Option<String>,
    pub others_auto: Option<String>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<SocialRow>> {
    sqlx::query_as!(
        SocialRow,
        r#"
        SELECT
            id,
            name,
            hide,
            char_no_arg,
            others_no_arg,
            char_found,
            others_found,
            vict_found,
            not_found,
            char_auto,
            others_auto
        FROM "Social"
        ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await
}
