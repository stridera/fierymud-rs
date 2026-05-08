//! Authored static screens — MOTD, news, credits, policies, imotd,
//! and any other long-form text the runtime serves on demand. The
//! schema's `SystemText` table is keyed by a stable string (e.g.
//! `"motd"`, `"news"`) so repository callers and the in-game lookups
//! agree on names. Builders edit rows via Muditor; the runtime
//! loads all active rows once at boot into a resource and reads
//! from there. A missing row falls back to a compile-time default
//! at the call site, so an empty DB still boots cleanly.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemTextRow {
    pub key: String,
    /// Category enum text — `"LOGIN"`, `"SYSTEM"`, `"COMBAT"`,
    /// `"IMMORTAL"`. Returned as raw text so callers don't need a
    /// Rust enum mirror; the runtime treats it as a tag for
    /// filtering/grouping if needed.
    pub category: String,
    pub title: Option<String>,
    pub content: String,
    /// Minimum viewer level to display. `0` = visible to all
    /// (default for player-facing text). Higher values gate
    /// staff-only screens (e.g. `imotd`).
    pub min_level: i32,
}

/// All currently-active system texts. Inactive rows are filtered
/// at the SQL layer so the runtime resource only ever holds live
/// content — toggling `is_active` in Muditor effectively unpublishes
/// without deleting.
pub async fn list_active(pool: &PgPool) -> sqlx::Result<Vec<SystemTextRow>> {
    sqlx::query_as!(
        SystemTextRow,
        r#"
        SELECT
            key AS "key!",
            category::text AS "category!",
            title,
            content AS "content!",
            min_level AS "min_level!"
        FROM "SystemText"
        WHERE is_active = TRUE
        ORDER BY key
        "#,
    )
    .fetch_all(pool)
    .await
}
