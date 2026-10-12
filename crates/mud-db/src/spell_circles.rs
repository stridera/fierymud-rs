//! Per spell circle tuning (`"SpellCircle"`): the base seconds a spent slot
//! takes to recover. Builders edit it in Muditor and
//! `fierylib/data/sql/2026-10-12-spell-circle-recover.sql` seeds the defaults.

use sqlx::PgPool;

#[derive(Debug, Clone, Copy)]
pub struct SpellCircleRow {
    pub circle: i32,
    /// `None` means the built-in time for that circle.
    pub recover_secs: Option<i32>,
}

/// Every `"SpellCircle"` row.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<SpellCircleRow>> {
    sqlx::query_as!(
        SpellCircleRow,
        r#"
        SELECT circle, recover_secs
        FROM "SpellCircle"
        ORDER BY circle
        "#
    )
    .fetch_all(pool)
    .await
}
