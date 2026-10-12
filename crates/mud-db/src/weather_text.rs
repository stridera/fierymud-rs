//! Weather flavour lines (`"WeatherText"`). The `weather` command prints one
//! line per zone climate and time of day; builders edit them in Muditor and
//! `fierylib/data/sql/2026-10-12-weather-text.sql` seeds the defaults.

use sqlx::PgPool;

#[derive(Debug, Clone)]
pub struct WeatherTextRow {
    /// Lowercase `Climate` label ("arid", "none").
    pub climate: String,
    /// dawn / day / midday / evening / night / any.
    pub daypart: String,
    /// XML-Lite line. `None` means the built-in line for that key.
    pub text: Option<String>,
}

/// Every `"WeatherText"` row.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<WeatherTextRow>> {
    sqlx::query_as!(
        WeatherTextRow,
        r#"
        SELECT LOWER(climate::text) AS "climate!", daypart AS "daypart!", text
        FROM "WeatherText"
        ORDER BY climate, daypart
        "#
    )
    .fetch_all(pool)
    .await
}
