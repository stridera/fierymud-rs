//! Per-sphere display colours (`"SphereStyle"`). The spells / skills
//! listings and the `<fire>`-style tag aliases both read the colour of an
//! ability's sphere; builders edit it in Muditor and
//! `fierylib/data/sql/2026-10-12-sphere-colors.sql` seeds the defaults.

use sqlx::PgPool;

#[derive(Debug, Clone)]
pub struct SphereStyleRow {
    /// Lowercase `SpellSphere` label ("fire", "healing"), the form
    /// `Ability.sphere` is loaded in.
    pub sphere: String,
    /// XML-Lite tag body without brackets ("red", "b:cyan"). `None` means
    /// the built-in default for that sphere.
    pub color: Option<String>,
}

/// Every `"SphereStyle"` row.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<SphereStyleRow>> {
    sqlx::query_as!(
        SphereStyleRow,
        r#"
        SELECT LOWER(sphere::text) AS "sphere!", color
        FROM "SphereStyle"
        ORDER BY sphere
        "#
    )
    .fetch_all(pool)
    .await
}
