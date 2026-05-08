//! Per-stage login flow text — banners, prompts, error messages,
//! creation steps. The schema's `LoginMessage` table keys rows by
//! `(stage, variant)`; the `default` variant is used unless a
//! caller asks for something else (theme/A-B-test). Loaded once at
//! boot into a runtime resource. Missing rows fall back to a
//! compile-time default at the call site so an empty DB still
//! produces a working login screen.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginMessageRow {
    /// `LoginStage` enum text — `"WELCOME_BANNER"`, `"EMAIL_PROMPT"`,
    /// `"PASSWORD_PROMPT"`, etc. Returned as the raw enum label so
    /// callers can match against the same stable strings the schema
    /// uses, without needing a Rust enum mirror.
    pub stage: String,
    pub variant: String,
    pub message: String,
}

pub async fn list_active(pool: &PgPool) -> sqlx::Result<Vec<LoginMessageRow>> {
    sqlx::query_as!(
        LoginMessageRow,
        r#"
        SELECT
            stage::text AS "stage!",
            variant AS "variant!",
            message AS "message!"
        FROM "LoginMessage"
        WHERE is_active = TRUE
        ORDER BY stage, variant
        "#,
    )
    .fetch_all(pool)
    .await
}
