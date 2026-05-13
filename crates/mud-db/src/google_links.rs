//! Per-user Google account links.
//!
//! `GoogleLink` rows pin a Google identity (OAuth `sub`, email,
//! display name, avatar) to a Muditor `Users` row. The link is
//! established on the Muditor side via the Google OAuth callback;
//! the Rust runtime reads it for identity-display commands. There's
//! no MUD-side write path for fresh links — Muditor calls `link`
//! directly out of its OAuth handler — but `unlink` is exposed so
//! the in-game `account unlink google` command can detach a binding
//! without round-tripping through the web.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleLinkRow {
    pub user_id: String,
    pub google_id: String,
    pub google_email: String,
    pub google_name: Option<String>,
    pub avatar_url: Option<String>,
}

/// Look up the Google link for `user_id`, or `None` if absent.
/// One link per user (unique on `user_id`).
pub async fn for_user(
    pool: &PgPool,
    user_id: &str,
) -> sqlx::Result<Option<GoogleLinkRow>> {
    sqlx::query_as!(
        GoogleLinkRow,
        r#"
        SELECT
            user_id      AS "user_id!: String",
            google_id    AS "google_id!: String",
            google_email AS "google_email!: String",
            google_name  AS "google_name: String",
            avatar_url   AS "avatar_url: String"
        FROM google_links
        WHERE user_id = $1
        "#,
        user_id,
    )
    .fetch_optional(pool)
    .await
}

/// Insert or refresh the Google link for `user_id`. Called by
/// Muditor's OAuth callback handler; included here for symmetry +
/// tests. Latest write wins on `user_id` conflict.
pub async fn link(
    pool: &PgPool,
    user_id: &str,
    google_id: &str,
    google_email: &str,
    google_name: Option<&str>,
    avatar_url: Option<&str>,
) -> sqlx::Result<String> {
    let row = sqlx::query!(
        r#"
        INSERT INTO google_links
            (id, user_id, google_id, google_email, google_name, avatar_url)
        VALUES
            (gen_random_uuid()::text, $1, $2, $3, $4, $5)
        ON CONFLICT (user_id) DO UPDATE
        SET google_id    = EXCLUDED.google_id,
            google_email = EXCLUDED.google_email,
            google_name  = EXCLUDED.google_name,
            avatar_url   = EXCLUDED.avatar_url,
            linked_at    = NOW()
        RETURNING id
        "#,
        user_id,
        google_id,
        google_email,
        google_name,
        avatar_url,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.id)
}

/// Remove the Google link for `user_id`. Returns the number of rows
/// deleted (0 if no link existed).
pub async fn unlink(pool: &PgPool, user_id: &str) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        r#"
        DELETE FROM google_links
        WHERE user_id = $1
        "#,
        user_id,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}
