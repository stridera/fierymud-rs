//! In-game `help <command>` text (`"Command".help_summary` / `help_text`).
//! Builders edit it in Muditor; `fierylib/data/sql/2026-10-12-command-help-text.sql`
//! seeds the defaults from the built-in registry strings.

use sqlx::PgPool;

#[derive(Debug, Clone)]
pub struct CommandHelpRow {
    /// Canonical (first) command name.
    pub name: String,
    /// One-line summary. `None` / empty means the built-in string.
    pub help_summary: Option<String>,
    /// Long paragraph. `None` / empty means the built-in string.
    pub help_text: Option<String>,
}

/// Every `"Command"` row.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<CommandHelpRow>> {
    sqlx::query_as!(
        CommandHelpRow,
        r#"
        SELECT name AS "name!", help_summary, help_text
        FROM "Command"
        ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await
}
