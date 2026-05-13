//! Liquid catalog. Drink containers reference these by `liquid_type`
//! string on `CharacterItems` rows; the catalog answers "what does a
//! 'water' container look like" (color), "what does drinking it do"
//! (hunger/thirst/drunk deltas), and "what flavor text on examine."
//!
//! Every CircleMUD-derived MUD has a fixed list of liquid types
//! (water, ale, wine, firebreather, …); the legacy server hardcoded
//! them in C. Here they live in a `Liquids` table and are hydrated
//! into a runtime resource at boot.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidRow {
    pub id: i32,
    /// Display name — what an identified container shows. May contain
    /// spaces ("dark ale", "slime mold juice").
    pub name: String,
    /// Single-token keyword used in commands and item state — `name`
    /// with spaces collapsed to dashes ("dark-ale"). Unique.
    pub alias: String,
    /// Color word(s) shown when a container is unidentified
    /// ("clear", "brown", "thick red").
    pub color_desc: String,
    /// Per-sip intoxication delta. 0 for non-alcoholic drinks.
    pub drunk_effect: i32,
    /// Per-sip fullness delta (clamped at 0 / max).
    pub hunger_effect: i32,
    /// Per-sip quench delta (clamped at 0 / max).
    pub thirst_effect: i32,
    /// Optional flavor text rendered on examine of an identified
    /// container.
    pub description: Option<String>,
}

/// Load every Liquid row, ordered by id (stable for tests / display).
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<LiquidRow>> {
    sqlx::query_as!(
        LiquidRow,
        r#"
        SELECT id, name, alias, color_desc, drunk_effect, hunger_effect, thirst_effect, description
        FROM "Liquids"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
