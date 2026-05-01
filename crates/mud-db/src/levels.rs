//! `LevelDefinition` reader — drives the XP curve for the `level`
//! readout command. Each row maps a level to the cumulative
//! experience needed to enter it, plus per-level HP/stamina gains
//! that the eventual leveling system can read.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelRow {
    pub level: i32,
    pub name: Option<String>,
    pub exp_required: i32,
    pub hp_gain: i32,
    pub stamina_gain: i32,
    pub is_immortal: bool,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<LevelRow>> {
    sqlx::query_as!(
        LevelRow,
        r#"
        SELECT level, name, exp_required, hp_gain, stamina_gain, is_immortal
        FROM "LevelDefinition"
        ORDER BY level
        "#
    )
    .fetch_all(pool)
    .await
}
