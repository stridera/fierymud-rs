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
    /// Failed-login throttle counter — increments on a wrong-password
    /// attempt, resets to 0 on a successful login. Compared against
    /// `security.max_login_attempts` to decide when to set
    /// `locked_until`.
    pub failed_login_attempts: i32,
    /// When set and in the future, refuses login attempts for this
    /// account regardless of credentials. Cleared on successful
    /// login or admin unlock.
    pub locked_until: Option<chrono::NaiveDateTime>,
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
            role AS "role: UserRole",
            failed_login_attempts,
            locked_until
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
            role AS "role: UserRole",
            failed_login_attempts,
            locked_until
        FROM "Users"
        WHERE id = $1 AND deleted_at IS NULL
        "#,
        id
    )
    .fetch_optional(pool)
    .await
}

/// Bump the failed-login counter and set `last_failed_login = NOW()`.
/// Optionally lock the account by setting `locked_until` ahead by the
/// supplied minutes — caller decides whether the new attempt count
/// crossed the threshold.
pub async fn record_failed_login(
    pool: &PgPool,
    user_id: &str,
    lock_for_minutes: Option<i32>,
) -> sqlx::Result<()> {
    if let Some(mins) = lock_for_minutes {
        sqlx::query!(
            r#"
            UPDATE "Users"
            SET failed_login_attempts = failed_login_attempts + 1,
                last_failed_login = NOW(),
                locked_until = NOW() + ($1::int || ' minutes')::interval,
                updated_at = NOW()
            WHERE id = $2
            "#,
            mins,
            user_id,
        )
        .execute(pool)
        .await?;
    } else {
        sqlx::query!(
            r#"
            UPDATE "Users"
            SET failed_login_attempts = failed_login_attempts + 1,
                last_failed_login = NOW(),
                updated_at = NOW()
            WHERE id = $1
            "#,
            user_id,
        )
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Reset the failed-login counter and clear any active lock.
/// Called on successful credential check.
pub async fn clear_failed_logins(pool: &PgPool, user_id: &str) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        UPDATE "Users"
        SET failed_login_attempts = 0,
            locked_until = NULL,
            updated_at = NOW()
        WHERE id = $1
        "#,
        user_id,
    )
    .execute(pool)
    .await?;
    Ok(())
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
