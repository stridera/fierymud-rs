//! `LevelDefinition` reader — drives the XP curve for the `level`
//! readout command. Each row maps a level to the cumulative
//! experience needed to enter it, plus per-level HP/stamina gains
//! that the eventual leveling system can read.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::Permission;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelRow {
    pub level: i32,
    pub name: Option<String>,
    pub exp_required: i32,
    pub hp_gain: i32,
    pub stamina_gain: i32,
    pub is_immortal: bool,
    /// Permissions granted on reaching this level (B5). Empty for
    /// most mortal levels; populated for staff tiers (Build, Admin,
    /// God, etc). On level-up the runtime unions these into the
    /// character's `permissions` array.
    pub permissions: Vec<Permission>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<LevelRow>> {
    sqlx::query_as!(
        LevelRow,
        r#"
        SELECT
            level,
            name,
            exp_required,
            hp_gain,
            stamina_gain,
            is_immortal,
            permissions AS "permissions!: Vec<Permission>"
        FROM "LevelDefinition"
        ORDER BY level
        "#
    )
    .fetch_all(pool)
    .await
}
