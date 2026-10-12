//! Per-command policy flags (`"Command".allowed_while_casting` /
//! `allowed_while_paralyzed` / `keeps_hidden` / `held_by_cast_lag`). Builders with the
//! CODER role edit them in Muditor; `fierylib/data/sql/2026-10-12-command-policy-flags.sql`
//! seeds the values of the lists the server used to hard-code.
//!
//! Kept apart from `command_help` so a database that has the help columns but not these
//! (or the reverse) still loads the half it has.

use sqlx::PgPool;

#[allow(clippy::struct_excessive_bools)] // one column per flag
#[derive(Debug, Clone)]
pub struct CommandPolicyRow {
    /// Canonical (first) command name.
    pub name: String,
    /// Runs while the caster winds up a spell (legacy `CMD_CAST`).
    pub allowed_while_casting: bool,
    /// Runs while paralyzed (legacy `CMD_MINOR_PARA`).
    pub allowed_while_paralyzed: bool,
    /// Does not reveal a hiding character (legacy `CMD_HIDE`).
    pub keeps_hidden: bool,
    /// Held back while a broken-off cast's lag runs.
    pub held_by_cast_lag: bool,
}

/// Every `"Command"` row.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<CommandPolicyRow>> {
    sqlx::query_as!(
        CommandPolicyRow,
        r#"
        SELECT name AS "name!", allowed_while_casting, allowed_while_paralyzed,
               keeps_hidden, held_by_cast_lag
        FROM "Command"
        ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await
}
