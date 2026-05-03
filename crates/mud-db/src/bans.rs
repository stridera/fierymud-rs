//! Account ban records.
//!
//! `BanRecords` rows are independent of `Characters`; bans
//! attach to a `Users` account so all of an account's
//! characters share a ban. The login flow consults `is_banned`
//! before issuing the password challenge.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveBanRow {
    pub reason: String,
    pub banned_by: String,
    pub banned_at: chrono::NaiveDateTime,
    pub expires_at: Option<chrono::NaiveDateTime>,
}

/// Active ban for `user_id` if any. Filters expired bans
/// (`expires_at < NOW()`) so temporary bans auto-release.
pub async fn active_for(
    pool: &PgPool,
    user_id: &str,
) -> sqlx::Result<Option<ActiveBanRow>> {
    sqlx::query_as!(
        ActiveBanRow,
        r#"
        SELECT
            reason,
            banned_by AS "banned_by!: String",
            banned_at AS "banned_at!: chrono::NaiveDateTime",
            expires_at AS "expires_at: chrono::NaiveDateTime"
        FROM "BanRecords"
        WHERE user_id = $1
          AND active = true
          AND (expires_at IS NULL OR expires_at > NOW())
        ORDER BY banned_at DESC
        LIMIT 1
        "#,
        user_id,
    )
    .fetch_optional(pool)
    .await
}

/// Insert a fresh ban. `duration_secs = None` means permanent.
pub async fn ban(
    pool: &PgPool,
    user_id: &str,
    banned_by: &str,
    reason: &str,
    duration_secs: Option<i64>,
) -> sqlx::Result<String> {
    let expires_at = duration_secs.map(|secs| {
        let naive = chrono::Utc::now()
            + chrono::Duration::seconds(secs);
        naive.naive_utc()
    });
    let row = sqlx::query!(
        r#"
        INSERT INTO "BanRecords"
            (id, user_id, banned_by, reason, expires_at, active)
        VALUES (gen_random_uuid()::text, $1, $2, $3, $4, true)
        RETURNING id
        "#,
        user_id,
        banned_by,
        reason,
        expires_at,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.id)
}

/// Lift the most recent active ban on `user_id`. Returns the
/// number of rows updated (0 if no active ban existed).
pub async fn unban(
    pool: &PgPool,
    user_id: &str,
    unbanned_by: &str,
) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        r#"
        UPDATE "BanRecords"
        SET active = false,
            unbanned_at = NOW(),
            unbanned_by = $2
        WHERE user_id = $1 AND active = true
        "#,
        user_id,
        unbanned_by,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}
