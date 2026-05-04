//! K/V tunables loaded from `GameConfig` at boot. Replaces the
//! per-callsite `pub(crate) const FOO_COST` constants scattered
//! through `mud-server`. The runtime stores the loaded rows in a
//! `RuntimeConfig` resource (see `mud-world`); call sites do
//! `world.resource::<RuntimeConfig>().get_i32("category", "key",
//! default)` and the default is the legacy compile-time value, so
//! a missing row degrades to today's behavior.
//!
//! Schema: `(category, key)` is the unique pair. Values are stored
//! as text and tagged with `ConfigValueType` so the loader can
//! parse to the right runtime type. Out-of-range / parse-fail rows
//! log a warning and fall back to the call-site default.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameConfigRow {
    pub category: String,
    pub key: String,
    pub value: String,
    pub value_type: String,
    pub description: Option<String>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<GameConfigRow>> {
    sqlx::query_as!(
        GameConfigRow,
        r#"
        SELECT
            category AS "category!",
            key AS "key!",
            value AS "value!",
            value_type::text AS "value_type!",
            description
        FROM "GameConfig"
        ORDER BY category, key
        "#,
    )
    .fetch_all(pool)
    .await
}
