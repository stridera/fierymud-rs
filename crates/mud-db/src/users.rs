use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};

use crate::enums::UserRole;

/// Deliberately has no password field: `Users.password_hash` is the
/// WEBSITE password and the game never reads it. Game logins verify
/// `Characters.password_hash` or a website-approved device code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub display_name: String,
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
    /// Account-shared bank balance, in copper. Pooled across every
    /// character on this user account; the per-character
    /// `Characters.bank_wealth` is independent. Wired by the
    /// `account_balance` / `account_deposit` / `account_withdraw`
    /// command family. Schema default is 0.
    pub account_wealth: i64,
}

pub async fn find_by_email(pool: &PgPool, email: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as!(
        User,
        r#"
        SELECT
            id,
            email,
            display_name,
            role AS "role: UserRole",
            failed_login_attempts,
            locked_until,
            account_wealth
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
            role AS "role: UserRole",
            failed_login_attempts,
            locked_until,
            account_wealth
        FROM "Users"
        WHERE id = $1 AND deleted_at IS NULL
        "#,
        id
    )
    .fetch_optional(pool)
    .await
}

/// Persist the account-shared bank balance. Used by the
/// `account_deposit` / `account_withdraw` commands and the save-player
/// path when one character on the account adjusted the shared pool
/// in-memory. Mirrors `characters::save_bank_wealth` for the per-
/// character balance.
pub async fn save_account_wealth<'e, E: PgExecutor<'e>>(
    executor: E,
    user_id: &str,
    amount: i64,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Users" SET account_wealth = $1, updated_at = NOW() WHERE id = $2"#,
        amount,
        user_id,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Read the account-shared bank balance only. Cheaper than the full
/// `find_by_id` and useful for online-character sync where every
/// other character on the account needs its `AccountWealth` component
/// refreshed after a transfer.
pub async fn load_account_wealth(pool: &PgPool, user_id: &str) -> sqlx::Result<i64> {
    let row = sqlx::query!(
        r#"SELECT account_wealth FROM "Users" WHERE id = $1"#,
        user_id,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.account_wealth)
}

/// Outcome of [`record_failed_login`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailedLogin {
    /// Failed-attempt count after this failure.
    pub attempts: i32,
    /// This failure left the account locked (`locked_until` in the future).
    pub locked: bool,
}

/// Record one failed password. The attempt count is incremented, and
/// the lock decided, atomically in a single statement from the value
/// stored in the row (not from a count read earlier at name entry):
///
/// - a lock that has already expired (`locked_until <= now`) restarts
///   the count at 1 and is cleared, so one wrong password after expiry
///   doesn't re-lock immediately;
/// - when the new count reaches `max_attempts` (`<= 0` disables),
///   `locked_until` is set `lock_minutes` ahead.
///
/// These columns are `timestamp without time zone` and every reader
/// compares them as naive UTC, so the values are computed in Rust and
/// bound; the pool also pins the session timezone to UTC.
pub async fn record_failed_login(
    pool: &PgPool,
    user_id: &str,
    max_attempts: i32,
    lock_minutes: i32,
) -> sqlx::Result<FailedLogin> {
    let now = chrono::Utc::now().naive_utc();
    let lock_until = now + chrono::Duration::minutes(i64::from(lock_minutes));
    let row = sqlx::query!(
        r#"
        UPDATE "Users"
        SET failed_login_attempts =
                CASE WHEN locked_until IS NOT NULL AND locked_until <= $1
                     THEN 1 ELSE failed_login_attempts + 1 END,
            locked_until =
                CASE WHEN $2 > 0 AND
                          (CASE WHEN locked_until IS NOT NULL AND locked_until <= $1
                                THEN 1 ELSE failed_login_attempts + 1 END) >= $2
                     THEN $3::timestamp
                     WHEN locked_until IS NOT NULL AND locked_until <= $1
                     THEN NULL
                     ELSE locked_until END,
            last_failed_login = $1,
            updated_at = $1
        WHERE id = $4
        RETURNING failed_login_attempts, locked_until
        "#,
        now,
        max_attempts,
        lock_until,
        user_id,
    )
    .fetch_one(pool)
    .await?;
    Ok(FailedLogin {
        attempts: row.failed_login_attempts,
        locked: row.locked_until.is_some_and(|t| t > now),
    })
}

/// Reset the failed-login counter and clear any active lock.
/// Called on successful credential check.
pub async fn clear_failed_logins(pool: &PgPool, user_id: &str) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        UPDATE "Users"
        SET failed_login_attempts = 0,
            locked_until = NULL,
            updated_at = $2
        WHERE id = $1
        "#,
        user_id,
        chrono::Utc::now().naive_utc(),
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// INSERT a fresh `Users` row from the creation flow. Generates
/// the id via Postgres' `gen_random_uuid()` (matching the
/// existing seeded rows), defaults role to `PLAYER`, and
/// sets `updated_at = now()`. `password_hash` is left NULL: the
/// website password is set through the website, never the game.
/// Returns the new id so the caller can stash it on the next
/// pipeline stage.
///
/// Takes any `PgExecutor` so callers can pass either `&pool`
/// for a stand-alone INSERT or `&mut *tx` for a transactional
/// pair with the matching `Characters` INSERT.
///
/// The email was only typed at the game prompt, never proven, so the
/// row is created with `preferences.emailVerified = false`. Muditor's
/// Google login refuses to auto-link by email to such a row (otherwise
/// anyone could pre-register a victim's address in game and inherit
/// their website account when they later sign in with Google). There is
/// no `emailVerified` column; the flag lives in the existing
/// `preferences` JSON so no schema change is needed. Rows without the
/// key (website registrations, imports) count as verified.
///
/// Email + `display_name` uniqueness is enforced by the table's
/// indexes; collisions surface as `sqlx::Error::Database` and
/// the caller should re-prompt the user.
pub async fn create<'e, E: PgExecutor<'e>>(
    executor: E,
    email: &str,
    display_name: &str,
) -> sqlx::Result<String> {
    let row = sqlx::query!(
        r#"
        INSERT INTO "Users" (id, email, display_name, role, preferences, updated_at)
        VALUES (gen_random_uuid()::text, $1, $2, 'PLAYER'::"UserRole",
                '{"emailVerified": false}'::jsonb, NOW())
        RETURNING id
        "#,
        email,
        display_name,
    )
    .fetch_one(executor)
    .await?;
    Ok(row.id)
}
