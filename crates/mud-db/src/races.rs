//! Per-race defaults from the schema's `Race` table. The runtime
//! reads the full row (`list_all`) into a `RaceCatalog` at boot —
//! stat caps, height/weight ranges, XP/HP/damage/coin factors,
//! enter/leave verb overrides, and per-race resistance JSON all
//! land there. The two narrow lookups (`list_default_sizes` /
//! `list_start_rooms`) are kept for callers that only need those
//! mappings without paying the cost of the full row materialization.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// Map of `Race` enum text key (`HUMAN` / `ELF` / ...) to its
/// `default_size` column (`MEDIUM` / `LARGE` / ... — the schema's
/// `Size` enum, kept as raw text for display). Empty when the
/// `Race` table has no rows yet (fresh DB).
pub async fn list_default_sizes(pool: &PgPool) -> sqlx::Result<HashMap<String, String>> {
    let rows = sqlx::query!(
        r#"
        SELECT
            race::text AS "race!",
            default_size::text AS "default_size!"
        FROM "Races"
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.race, r.default_size)).collect())
}

/// Map of `Race` enum text key → `(start_room_zone_id, start_room_id)`
/// for races that have a starting room set. Skips rows with NULL
/// columns so the caller can simply check `.get()` to know if a race
/// has a meaningful spawn fallback.
pub async fn list_start_rooms(pool: &PgPool) -> sqlx::Result<HashMap<String, (i32, i32)>> {
    let rows = sqlx::query!(
        r#"
        SELECT
            race::text AS "race!",
            start_room_zone_id AS "zone_id!",
            start_room_id AS "room_id!"
        FROM "Races"
        WHERE start_room_zone_id IS NOT NULL
          AND start_room_id IS NOT NULL
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.race, (r.zone_id, r.room_id)))
        .collect())
}

/// One full `Races` row loaded for the runtime `RaceCatalog`. Enum
/// columns (`race`, `race_align`, `default_size`, `default_lifeforce`)
/// round-trip as raw text — the runtime renders them directly and
/// keeps the door open for new schema variants without code-side
/// churn. JSON columns (`resistances`) stay as `serde_json::Value`
/// and are interpreted at catalog hydration time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaceRow {
    /// Raw `Race` enum text — the catalog key (`HUMAN` / `ELF` / ...).
    pub race: String,
    /// Display name, may carry XML-Lite color tags.
    pub name: String,
    /// Plain-text name without color tags; the catalog reverse-
    /// lookup key for `cmd_examine` / score display.
    pub plain_name: String,
    /// Space-separated keyword list (matches the legacy storage
    /// shape — no array column on the schema).
    pub keywords: String,
    pub playable: bool,
    pub humanoid: bool,
    pub magical: bool,
    /// `RaceAlign` enum text (`GOOD` / `NEUTRAL` / `EVIL`).
    pub race_align: String,
    pub default_alignment: i32,
    /// `Size` enum text (`MEDIUM` / `LARGE` / ...).
    pub default_size: String,
    pub focus_bonus: i32,
    /// `LifeForce` enum text (`LIFE` / `UNDEAD` / `MAGIC` / ...).
    pub default_lifeforce: String,
    pub male_weight_low: i32,
    pub male_weight_high: i32,
    pub male_height_low: i32,
    pub male_height_high: i32,
    pub female_weight_low: i32,
    pub female_weight_high: i32,
    pub female_height_low: i32,
    pub female_height_high: i32,
    pub max_strength: i32,
    pub max_dexterity: i32,
    pub max_intelligence: i32,
    pub max_wisdom: i32,
    pub max_constitution: i32,
    pub max_charisma: i32,
    pub exp_factor: i32,
    pub hp_factor: i32,
    pub hit_damage_factor: i32,
    pub damage_dice_factor: i32,
    pub copper_factor: i32,
    /// Overrides the default "arrives from ..." movement broadcast
    /// (flying / swimming creatures use distinct verbs). `None`
    /// falls through to the generic verb in `cmd_move`.
    pub enter_verb: Option<String>,
    /// Override for "leaves ..." — paired with `enter_verb`.
    pub leave_verb: Option<String>,
    pub start_room_zone_id: Option<i32>,
    pub start_room_id: Option<i32>,
    /// JSON map of element name → mitigation percent
    /// (`{ "FIRE": 25, "COLD": -25 }`). Folds into the wearer's
    /// `Resistances` map at spawn time.
    pub resistances: serde_json::Value,
}

/// Load every `Races` row for the runtime catalog. Race-text /
/// enum-text columns are cast to plain `text` so the
/// `query_as!`/`query!` macros don't try to map them to compiled
/// Rust enum types (`Race` / `RaceAlign` / `Size` / `LifeForce`)
/// the runtime intentionally doesn't model — keeping them as
/// strings keeps the catalog open to new schema variants without
/// code-side churn.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<RaceRow>> {
    sqlx::query_as!(
        RaceRow,
        r#"
        SELECT
            race::text AS "race!",
            name,
            plain_name,
            keywords,
            playable,
            humanoid,
            magical,
            race_align::text AS "race_align!",
            default_alignment,
            default_size::text AS "default_size!",
            focus_bonus,
            default_lifeforce::text AS "default_lifeforce!",
            male_weight_low,
            male_weight_high,
            male_height_low,
            male_height_high,
            female_weight_low,
            female_weight_high,
            female_height_low,
            female_height_high,
            max_strength,
            max_dexterity,
            max_intelligence,
            max_wisdom,
            max_constitution,
            max_charisma,
            exp_factor,
            hp_factor,
            hit_damage_factor,
            damage_dice_factor,
            copper_factor,
            enter_verb,
            leave_verb,
            start_room_zone_id,
            start_room_id,
            resistances AS "resistances!: serde_json::Value"
        FROM "Races"
        "#,
    )
    .fetch_all(pool)
    .await
}
