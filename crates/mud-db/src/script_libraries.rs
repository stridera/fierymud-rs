//! Importable Lua libraries (`"ScriptLibrary"`). Scripts load them with
//! `require("<name>")`; builders edit them on the Muditor Scripts page (Libraries tab) and
//! `fierylib/data/script_libraries/*.lua` seeds the committed ones. The table is created by
//! the Prisma schema (`prisma db push`) or `fierylib/data/sql/2026-10-14-script-library.sql`.

use sqlx::PgPool;

#[derive(Debug, Clone)]
pub struct ScriptLibraryRow {
    /// Unique, lowercase; what scripts pass to `require`.
    pub name: String,
    /// Owning zone (organisation / permission scope only), if any.
    pub zone_id: Option<i32>,
    /// Lua source.
    pub code: String,
}

/// Every `"ScriptLibrary"` row.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<ScriptLibraryRow>> {
    sqlx::query_as!(
        ScriptLibraryRow,
        r#"
        SELECT name, zone_id, code
        FROM "ScriptLibrary"
        ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await
}
