//! Persistence for trigger / Lua execution failures.
//!
//! The runtime keeps an in-memory ring buffer (`ScriptErrorLog`)
//! for fast `scripterrors` reads, but every entry is also fire-
//! and-forget INSERT'd into the schema's `script_error_log` table
//! so the trail survives restart and surfaces in Muditor.

use sqlx::PgPool;

/// Insert one trigger fire failure. `error_type` is free-form but
/// the schema's docs suggest `"compilation"`, `"runtime"`, or
/// `"timeout"`. `script_line` is optional (the runtime today
/// doesn't extract it from `mlua` errors). `context_info` carries
/// trigger name + event as a small JSON object.
pub async fn record(
    pool: &PgPool,
    trigger_zone_id: i32,
    trigger_id: i32,
    error_type: &str,
    error_message: &str,
    context: &serde_json::Value,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        INSERT INTO script_error_log
            (trigger_zone_id, trigger_id, error_type, error_message, context_info)
        VALUES ($1, $2, $3, $4, $5)
        "#,
        trigger_zone_id,
        trigger_id,
        error_type,
        error_message,
        context,
    )
    .execute(pool)
    .await?;
    Ok(())
}
