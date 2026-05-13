//! Player classes — Sorcerer, Cleric, Warrior, Bard, etc. Loaded once
//! at boot into the runtime `ClassCatalog` (`mud-world::resources`).
//! Identity fields (`name`, `plain_name`, sub-class hierarchy) drive
//! the score / who readouts; the parity columns (`description`,
//! `hit_dice`, `primary_stat`, `hp_per_level`, `resistances`) round-
//! trip so the runtime can render them and apply per-class HP / damage
//! scaling once those systems land.
//!
//! Names commonly carry color tags (`<b:magenta>Sorcerer</>`) so the
//! display path renders them like every other XML-Lite text.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassRow {
    pub id: i32,
    pub name: String,
    pub plain_name: String,
    pub is_subclass: bool,
    pub parent_class_id: Option<i32>,
    /// Long-form builder-authored prose shown on the class info /
    /// help page. `None` when the row hasn't been authored yet.
    pub description: Option<String>,
    /// Hit-dice expression in standard `NdM` form (`"1d8"`,
    /// `"1d10"`, ...). Used for natural HP rolls / level-up gains
    /// per class. Schema default is `"1d8"`.
    pub hit_dice: String,
    /// Primary attribute label (`"STR"` / `"DEX"` / ...). `None`
    /// when the class doesn't designate a primary stat.
    pub primary_stat: Option<String>,
    /// Flat HP gain per level (added on top of `LevelDefinition.hp_gain`
    /// once the consumer wires it). Schema default `10`.
    pub hp_per_level: i32,
    /// JSON map of element name → mitigation percent. Folds into
    /// the wearer's `Resistances` at spawn time alongside race
    /// resistances and gear bookkeeping.
    pub resistances: serde_json::Value,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<ClassRow>> {
    sqlx::query_as!(
        ClassRow,
        r#"
        SELECT
            id,
            name,
            plain_name,
            is_subclass,
            parent_class_id,
            description,
            hit_dice,
            primary_stat,
            hp_per_level,
            COALESCE(resistances, '{}'::jsonb) AS "resistances!: serde_json::Value"
        FROM "Class"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
