use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailRow {
    pub id: i32,
    pub sender_user_id: String,
    /// Display name of the sender at fetch time (joined from `Users`).
    /// Listings can render this without a second query.
    pub sender_display_name: String,
    pub subject: String,
    pub body: String,
    pub sent_at: chrono::NaiveDateTime,
    pub read_at: Option<chrono::NaiveDateTime>,
}

/// Send a mail. `recipient_user_id` is the User who owns the addressed
/// character; mail is account-scoped, so any character on that account
/// will see it in their `mailbox`. Broadcast (null recipient) is
/// reserved for admin tooling.
pub async fn send(
    pool: &PgPool,
    sender_user_id: &str,
    recipient_user_id: &str,
    subject: &str,
    body: &str,
) -> sqlx::Result<i32> {
    let row = sqlx::query!(
        r#"
        INSERT INTO "AccountMail" (sender_user_id, recipient_user_id, subject, body, updated_at)
        VALUES ($1, $2, $3, $4, NOW())
        RETURNING id
        "#,
        sender_user_id,
        recipient_user_id,
        subject,
        body,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.id)
}

/// List inbound (non-deleted) mail for a user, newest first. Joined
/// with `Users` to pull the sender's display name into each row so
/// listings don't need a follow-up query.
pub async fn inbox_for(pool: &PgPool, recipient_user_id: &str) -> sqlx::Result<Vec<MailRow>> {
    sqlx::query_as!(
        MailRow,
        r#"
        SELECT
            m.id,
            m.sender_user_id,
            u.display_name AS "sender_display_name!: String",
            m.subject,
            m.body,
            m.sent_at AS "sent_at!: chrono::NaiveDateTime",
            m.read_at AS "read_at: chrono::NaiveDateTime"
        FROM "AccountMail" m
        JOIN "Users" u ON u.id = m.sender_user_id
        WHERE m.recipient_user_id = $1
          AND m.is_deleted = FALSE
        ORDER BY m.sent_at DESC
        "#,
        recipient_user_id,
    )
    .fetch_all(pool)
    .await
}

/// Mark a mail row as read. No-op if the row is missing or already
/// read; safe to call repeatedly.
pub async fn mark_read(pool: &PgPool, mail_id: i32) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        UPDATE "AccountMail"
        SET read_at = NOW(), updated_at = NOW()
        WHERE id = $1 AND read_at IS NULL
        "#,
        mail_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Soft-delete a mail row from the recipient's inbox view. The row
/// stays in the table (audit trail); `is_deleted = true` hides it
/// from `inbox_for`.
pub async fn soft_delete(pool: &PgPool, mail_id: i32) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        UPDATE "AccountMail"
        SET is_deleted = TRUE, updated_at = NOW()
        WHERE id = $1
        "#,
        mail_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Look up the User ID that owns a character with the given name
/// (case-insensitive). Returns `(user_id, canonical_name)`. Used by
/// `mail <name>` to resolve the recipient before opening the
/// composition buffer.
pub async fn user_for_character_name(
    pool: &PgPool,
    name: &str,
) -> sqlx::Result<Option<(String, String)>> {
    let row = sqlx::query!(
        r#"
        SELECT user_id, name
        FROM "Characters"
        WHERE LOWER(name) = LOWER($1)
        LIMIT 1
        "#,
        name,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.user_id.map(|uid| (uid, r.name))))
}
