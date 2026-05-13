//! Per-user Discord account links.
//!
//! `DiscordLink` rows attach a verified Discord identity to a
//! Muditor `Users` row, one row per user. The runtime reads the
//! link for identity-display commands (`account`) and the eventual
//! bot-side dispatch path (gossip channel mirror, admin
//! notifications). Writes happen through `link` (claim a pending
//! Discord ID), `unlink` (remove the binding), and `mark_verified`
//! (flip the verification flag once the bot confirms the
//! verification code came from the same Discord account).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordLinkRow {
    pub user_id: String,
    pub discord_id: String,
    pub discord_name: String,
    pub verified: bool,
}

/// Look up the Discord link for `user_id`, or `None` if the user
/// hasn't linked yet. One link per user (unique on `user_id`).
pub async fn for_user(
    pool: &PgPool,
    user_id: &str,
) -> sqlx::Result<Option<DiscordLinkRow>> {
    sqlx::query_as!(
        DiscordLinkRow,
        r#"
        SELECT
            user_id      AS "user_id!: String",
            discord_id   AS "discord_id!: String",
            discord_name AS "discord_name!: String",
            verified     AS "verified!: bool"
        FROM discord_links
        WHERE user_id = $1
        "#,
        user_id,
    )
    .fetch_optional(pool)
    .await
}

/// Insert a fresh link, or replace the existing one if the user
/// re-claims (latest write wins). `verified` starts at `false`; the
/// bot side calls `mark_verified` once the player's verification
/// code lands in the gossip channel. Returns the row id.
pub async fn link(
    pool: &PgPool,
    user_id: &str,
    discord_id: &str,
    discord_name: &str,
) -> sqlx::Result<String> {
    // Conflict on either the user_id unique or the discord_id unique.
    // We collapse both onto user_id-targeted upsert: if the same user
    // re-links to a different Discord ID we update; if a DIFFERENT user
    // tries to claim a Discord ID already on file, the discord_id
    // unique fires and the caller sees the violation.
    let row = sqlx::query!(
        r#"
        INSERT INTO discord_links
            (id, user_id, discord_id, discord_name, verified)
        VALUES
            (gen_random_uuid()::text, $1, $2, $3, false)
        ON CONFLICT (user_id) DO UPDATE
        SET discord_id   = EXCLUDED.discord_id,
            discord_name = EXCLUDED.discord_name,
            verified     = false,
            linked_at    = NOW()
        RETURNING id
        "#,
        user_id,
        discord_id,
        discord_name,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.id)
}

/// Remove the Discord link for `user_id`. Returns the number of rows
/// deleted (0 if no link existed).
pub async fn unlink(pool: &PgPool, user_id: &str) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        r#"
        DELETE FROM discord_links
        WHERE user_id = $1
        "#,
        user_id,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Flip `verified = true` on the user's link. Called from the bot
/// side after the player echoes the one-time verification code into
/// the gossip channel. Returns the number of rows updated (0 if no
/// link exists for the user).
pub async fn mark_verified(pool: &PgPool, user_id: &str) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        r#"
        UPDATE discord_links
        SET verified = true
        WHERE user_id = $1
        "#,
        user_id,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Reverse lookup — find the link by its Discord-side identifier.
/// Used by the bot ingress path: an incoming Discord message tags
/// the discord_id, and we want the linked Muditor user_id.
pub async fn for_discord_id(
    pool: &PgPool,
    discord_id: &str,
) -> sqlx::Result<Option<DiscordLinkRow>> {
    sqlx::query_as!(
        DiscordLinkRow,
        r#"
        SELECT
            user_id      AS "user_id!: String",
            discord_id   AS "discord_id!: String",
            discord_name AS "discord_name!: String",
            verified     AS "verified!: bool"
        FROM discord_links
        WHERE discord_id = $1
        "#,
        discord_id,
    )
    .fetch_optional(pool)
    .await
}
