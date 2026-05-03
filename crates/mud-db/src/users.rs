use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};

use crate::enums::UserRole;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub password_hash: Option<String>,
    pub role: UserRole,
}

pub async fn find_by_email(pool: &PgPool, email: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as!(
        User,
        r#"
        SELECT
            id,
            email,
            display_name,
            password_hash,
            role AS "role: UserRole"
        FROM "Users"
        WHERE email = $1 AND deleted_at IS NULL
        "#,
        email
    )
    .fetch_optional(pool)
    .await
}

pub async fn find_by_id(pool: &PgPool, id: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as!(
        User,
        r#"
        SELECT
            id,
            email,
            display_name,
            password_hash,
            role AS "role: UserRole"
        FROM "Users"
        WHERE id = $1 AND deleted_at IS NULL
        "#,
        id
    )
    .fetch_optional(pool)
    .await
}

/// INSERT a fresh `Users` row from the creation flow. Generates
/// the id via Postgres' `gen_random_uuid()` (matching the
/// existing seeded rows), defaults role to `PLAYER`, and
/// sets `updated_at = now()`. Returns the new id so the caller
/// can stash it on the next pipeline stage.
///
/// Takes any `PgExecutor` so callers can pass either `&pool`
/// for a stand-alone INSERT or `&mut *tx` for a transactional
/// pair with the matching `Characters` INSERT.
///
/// Email + `display_name` uniqueness is enforced by the table's
/// indexes; collisions surface as `sqlx::Error::Database` and
/// the caller should re-prompt the user.
pub async fn create<'e, E: PgExecutor<'e>>(
    executor: E,
    email: &str,
    display_name: &str,
    password_hash: &str,
) -> sqlx::Result<String> {
    let row = sqlx::query!(
        r#"
        INSERT INTO "Users" (id, email, display_name, password_hash, role, updated_at)
        VALUES (gen_random_uuid()::text, $1, $2, $3, 'PLAYER'::"UserRole", NOW())
        RETURNING id
        "#,
        email,
        display_name,
        password_hash,
    )
    .fetch_one(executor)
    .await?;
    Ok(row.id)
}
