//! Help entry catalog. Builder-authored articles keyed by an array
//! of keyword strings (`["FIREBALL", "FIRE BALL"]`). The `help`
//! command looks up by exact keyword match (case-insensitive) and
//! falls back to title-prefix matches when no keyword hits.
//!
//! `min_level` gates entries — staff-only articles (e.g. wizard
//! tooling) stay invisible until the viewer is at least that level.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelpEntryRow {
    pub id: i32,
    pub keywords: Vec<String>,
    pub title: String,
    pub content: String,
    pub min_level: i32,
    pub category: Option<String>,
    pub usage: Option<String>,
    pub duration: Option<String>,
    pub sphere: Option<String>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<HelpEntryRow>> {
    sqlx::query_as!(
        HelpEntryRow,
        r#"
        SELECT
            id,
            keywords AS "keywords!: Vec<String>",
            title,
            content,
            min_level,
            category,
            usage,
            duration,
            sphere
        FROM "HelpEntry"
        ORDER BY title
        "#
    )
    .fetch_all(pool)
    .await
}
