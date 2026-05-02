//! Persistence layer for the in-game admin audit log.
//!
//! The runtime keeps a 256-entry ring buffer in memory for fast
//! `show audit` reads, but every entry is also fire-and-forget
//! INSERT'd into the schema's existing `AuditLogs` table so the
//! trail survives a restart and can be queried alongside
//! Muditor's web-side audit events.

use sqlx::PgPool;

/// Insert one admin action into the schema's `AuditLogs` table.
/// Fire-and-forget — callers `tokio::spawn` this and ignore the
/// result. Errors land in `tracing::warn` from the caller.
///
/// Parameters map onto the schema as:
/// * `user_id` — caller's `Users.id`
/// * `verb` becomes `action` (e.g. "freeze", "transfer", "hgrant")
/// * `target_label` is a free-form identifier (player name, room
///   key, etc) stamped into `entity_id`
/// * `args` is preserved verbatim as the `new_values.args` JSON
pub async fn record(
    pool: &PgPool,
    user_id: &str,
    verb: &str,
    target_label: &str,
    args: &str,
) -> sqlx::Result<()> {
    let new_values = serde_json::json!({ "args": args });
    sqlx::query!(
        r#"
        INSERT INTO "AuditLogs"
            (id, action, entity_type, entity_id, new_values, user_id)
        VALUES
            (gen_random_uuid()::text, $1, 'AdminAction', $2, $3, $4)
        "#,
        verb,
        target_label,
        new_values,
        user_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}
