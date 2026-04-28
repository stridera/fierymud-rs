use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub password_hash: Option<String>,
}

pub async fn find_by_email(pool: &PgPool, email: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as!(
        User,
        r#"
        SELECT id, email, display_name, password_hash
        FROM "Users"
        WHERE email = $1 AND deleted_at IS NULL
        "#,
        email
    )
    .fetch_optional(pool)
    .await
}
