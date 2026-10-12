use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialRow {
    pub id: i32,
    pub name: String,
    pub hide: bool,
    /// `Social.staff_only` — players cannot reach it by abbreviation.
    pub staff_only: bool,
    pub char_no_arg: Option<String>,
    pub others_no_arg: Option<String>,
    pub char_found: Option<String>,
    pub others_found: Option<String>,
    pub vict_found: Option<String>,
    pub not_found: Option<String>,
    pub char_auto: Option<String>,
    pub others_auto: Option<String>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<SocialRow>> {
    sqlx::query_as!(
        SocialRow,
        r#"
        SELECT
            id,
            name,
            hide,
            staff_only,
            char_no_arg,
            others_no_arg,
            char_found,
            others_found,
            vict_found,
            not_found,
            char_auto,
            others_auto
        FROM "Social"
        ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await
}

/// [`list_all`] for a database that predates `"Social".staff_only`
/// (`fierylib/data/sql/2026-10-12-social-staff-only.sql`): every social reads as
/// not staff-only. Only the loader's missing-column fallback should call this
/// (it then applies the built-in staff-only list).
pub async fn list_all_without_staff_only(pool: &PgPool) -> sqlx::Result<Vec<SocialRow>> {
    sqlx::query_as!(
        SocialRow,
        r#"
        SELECT
            id,
            name,
            hide,
            false AS "staff_only!",
            char_no_arg,
            others_no_arg,
            char_found,
            others_found,
            vict_found,
            not_found,
            char_auto,
            others_auto
        FROM "Social"
        ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await
}
