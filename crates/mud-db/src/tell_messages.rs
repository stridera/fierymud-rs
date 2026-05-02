//! Persistent tell history.
//!
//! Backs the `lasttells` command. Inbound tells INSERT one row per
//! delivery into `tell_message`; on login we read the most recent
//! N rows for the recipient and seed `TellLog` so the history
//! survives a restart.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TellMessageRow {
    pub id: i32,
    pub recipient_id: String,
    pub sender_name: String,
    pub body: String,
    pub sent_at: chrono::NaiveDateTime,
}

/// Fire-and-forget INSERT of a delivered tell. The runtime calls
/// this from `cmd_tell` after the in-memory broadcast.
pub async fn record(
    pool: &PgPool,
    recipient_id: &str,
    sender_name: &str,
    body: &str,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        INSERT INTO tell_message (recipient_id, sender_name, body)
        VALUES ($1, $2, $3)
        "#,
        recipient_id,
        sender_name,
        body,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Most recent `limit` tells received by `recipient_id`, newest
/// first. Login uses this to hydrate the `TellLog` component so
/// `lasttells` shows continuity across reconnects.
pub async fn recent_for(
    pool: &PgPool,
    recipient_id: &str,
    limit: i64,
) -> sqlx::Result<Vec<TellMessageRow>> {
    sqlx::query_as!(
        TellMessageRow,
        r#"
        SELECT id, recipient_id, sender_name, body, sent_at
        FROM tell_message
        WHERE recipient_id = $1
        ORDER BY sent_at DESC
        LIMIT $2
        "#,
        recipient_id,
        limit,
    )
    .fetch_all(pool)
    .await
}
