//! Device-code ("log in from the website") rows in `"GameLoginCode"`.
//!
//! The telnet login inserts a PENDING row with a short code; the
//! player approves it on the website (which sets `status = APPROVED`,
//! `approvedAt`, `approvedByUserId`); the game then atomically flips
//! it to CONSUMED. All timestamps are `timestamp(3) without time
//! zone` written as UTC by both sides (Prisma convention), so every
//! time value here is a UTC `NaiveDateTime` bound explicitly rather
//! than left to the session-timezone `CURRENT_TIMESTAMP` default.

use chrono::NaiveDateTime;
use sqlx::{PgExecutor, PgPool};

/// Fields for a new PENDING code.
#[derive(Debug, Clone)]
pub struct NewGameLoginCode<'a> {
    /// 8 chars, no hyphen, uppercase, safe alphabet.
    pub code: &'a str,
    pub character_name: &'a str,
    pub user_id: Option<&'a str>,
    pub client_ip: &'a str,
    pub client_port: Option<i32>,
    pub tls: bool,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
}

/// Current state of a code row as the game sees it.
#[derive(Debug, Clone)]
pub struct CodeState {
    /// `GameLoginCodeStatus` label: PENDING / APPROVED / DENIED /
    /// CONSUMED / EXPIRED.
    pub status: String,
    pub approved_by_user_id: Option<String>,
}

/// INSERT a PENDING code, returning the row id. A unique-violation on
/// `code` surfaces as `sqlx::Error::Database`; the caller regenerates
/// and retries.
pub async fn insert<'e, E: PgExecutor<'e>>(
    executor: E,
    new: &NewGameLoginCode<'_>,
) -> sqlx::Result<String> {
    let row = sqlx::query!(
        r#"
        INSERT INTO "GameLoginCode"
            (id, code, "characterName", "userId", "clientIp", "clientPort", tls,
             status, "createdAt", "expiresAt")
        VALUES
            (gen_random_uuid()::text, $1, $2, $3, $4, $5, $6,
             'PENDING', $7, $8)
        RETURNING id
        "#,
        new.code,
        new.character_name,
        new.user_id,
        new.client_ip,
        new.client_port,
        new.tls,
        new.created_at,
        new.expires_at,
    )
    .fetch_one(executor)
    .await?;
    Ok(row.id)
}

/// Read the current status (and approver) of a code row. `None` if
/// the row has been deleted.
pub async fn state(pool: &PgPool, id: &str) -> sqlx::Result<Option<CodeState>> {
    let row = sqlx::query!(
        r#"
        SELECT status::text AS "status!", "approvedByUserId" AS approved_by_user_id
        FROM "GameLoginCode"
        WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| CodeState {
        status: r.status,
        approved_by_user_id: r.approved_by_user_id,
    }))
}

/// Atomically consume an APPROVED code: only succeeds when the row is
/// still APPROVED and was approved by `expected_user_id`. Returns
/// `true` when exactly one row was flipped to CONSUMED.
pub async fn consume(
    pool: &PgPool,
    id: &str,
    expected_user_id: &str,
    now: NaiveDateTime,
) -> sqlx::Result<bool> {
    let row = sqlx::query!(
        r#"
        UPDATE "GameLoginCode"
        SET status = 'CONSUMED', "consumedAt" = $3
        WHERE id = $1
          AND status = 'APPROVED'
          AND "approvedByUserId" = $2
        RETURNING id
        "#,
        id,
        expected_user_id,
        now,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Mark a still-PENDING code EXPIRED (cancel / timeout). A no-op for
/// rows already approved, denied or consumed.
pub async fn expire_pending(pool: &PgPool, id: &str) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "GameLoginCode" SET status = 'EXPIRED' WHERE id = $1 AND status = 'PENDING'"#,
        id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a code EXPIRED unless it has already been consumed (used when
/// an APPROVED code fails the server-side checks and must never be
/// usable afterwards).
pub async fn expire_unconsumed(pool: &PgPool, id: &str) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "GameLoginCode" SET status = 'EXPIRED' WHERE id = $1 AND status IN ('PENDING', 'APPROVED')"#,
        id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Best-effort delete of a row by id (tests only use this to clean up
/// after themselves).
pub async fn delete(pool: &PgPool, id: &str) -> sqlx::Result<()> {
    sqlx::query!(r#"DELETE FROM "GameLoginCode" WHERE id = $1"#, id)
        .execute(pool)
        .await?;
    Ok(())
}
