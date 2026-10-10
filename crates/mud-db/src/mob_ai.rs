//! Mob spellcasting / skill AI rows: `ClassAiRules` (per class), `RaceAiRules`
//! (per race: dragon breath and the like), `MobAbilities` (per-mob rules, used
//! when `Mobs.ai_mode = CUSTOM`) and the non-default `Mobs.ai_mode` values.
//!
//! These objects come from `fierylib/data/sql/2026-10-10-mob-ai-schema.sql`
//! and `2026-10-10-mob-ai-race.sql`, which prod applies by hand, so every
//! query lives in its own function and the runtime loader treats any failure
//! (missing table / column, undecodable row) as "no rows" (see
//! `mud_world::loader::load_mob_ai_data`). The loader reads these tables at
//! every boot; nothing *uses* them while the `mob_ai.enabled` `GameConfig`
//! flag is off.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::MobAiMode;

/// One enabled `ClassAiRules` row joined with its ability's type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassAiRuleRow {
    pub class_id: i32,
    pub ability_id: i32,
    /// `Ability.ability_type`: `SPELL`, `SKILL`, `SONG` or `CHANT`.
    pub ability_type: String,
    pub priority: i32,
    pub chance_pct: i32,
    pub cooldown_s: i32,
    pub conditions: Option<serde_json::Value>,
    pub target: String,
    pub min_level: Option<i32>,
}

/// Enabled class rules, ordered by class then priority (lower first).
pub async fn list_class_rules(pool: &PgPool) -> sqlx::Result<Vec<ClassAiRuleRow>> {
    sqlx::query_as!(
        ClassAiRuleRow,
        r#"
        SELECT
            r.class_id,
            r.ability_id,
            a."abilityType" AS "ability_type!",
            r.priority,
            r.chance_pct,
            r.cooldown_s,
            r.conditions,
            r.target,
            r.min_level
        FROM "ClassAiRules" r
        JOIN "Ability" a ON a.id = r.ability_id
        WHERE r.enabled
        ORDER BY r.class_id, r.priority, r.id
        "#,
    )
    .fetch_all(pool)
    .await
}

/// One enabled `RaceAiRules` row joined with its ability's type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaceAiRuleRow {
    /// `Race` enum text (`DRAGON_FIRE`).
    pub race: String,
    pub ability_id: i32,
    pub ability_type: String,
    pub priority: i32,
    pub chance_pct: i32,
    pub cooldown_s: i32,
    pub conditions: Option<serde_json::Value>,
    pub target: String,
    /// Mob level gate; `None` = none.
    pub min_level: Option<i32>,
}

/// Enabled race rules, ordered by race then priority (lower first).
pub async fn list_race_rules(pool: &PgPool) -> sqlx::Result<Vec<RaceAiRuleRow>> {
    sqlx::query_as!(
        RaceAiRuleRow,
        r#"
        SELECT
            r.race::text AS "race!: String",
            r.ability_id,
            a."abilityType" AS "ability_type!",
            r.priority,
            r.chance_pct,
            r.cooldown_s,
            r.conditions,
            r.target,
            r.min_level
        FROM "RaceAiRules" r
        JOIN "Ability" a ON a.id = r.ability_id
        WHERE r.enabled
        ORDER BY r.race::text, r.priority, r.id
        "#,
    )
    .fetch_all(pool)
    .await
}

/// One known `MobAbilities` row joined with its ability's type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobAiRuleRow {
    pub mob_zone_id: i32,
    pub mob_id: i32,
    pub ability_id: i32,
    pub ability_type: String,
    pub priority: i32,
    pub chance_pct: i32,
    pub cooldown_s: i32,
    pub conditions: Option<serde_json::Value>,
    /// `None` = let the engine pick from the ability's own targeting.
    pub target: Option<String>,
}

/// Per-mob rules (`known` rows only), ordered by mob then priority.
pub async fn list_mob_rules(pool: &PgPool) -> sqlx::Result<Vec<MobAiRuleRow>> {
    sqlx::query_as!(
        MobAiRuleRow,
        r#"
        SELECT
            m.mob_zone_id,
            m.mob_id,
            m.ability_id,
            a."abilityType" AS "ability_type!",
            m.priority,
            m.chance_pct,
            m.cooldown_s,
            m.conditions,
            m.target
        FROM "MobAbilities" m
        JOIN "Ability" a ON a.id = m.ability_id
        WHERE m.known
        ORDER BY m.mob_zone_id, m.mob_id, m.priority, m.id
        "#,
    )
    .fetch_all(pool)
    .await
}

/// A mob prototype whose `ai_mode` is not the default `CLASS`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MobAiModeRow {
    pub zone_id: i32,
    pub id: i32,
    pub ai_mode: MobAiMode,
}

/// Every mob prototype with `ai_mode` `CUSTOM` or `NONE`.
pub async fn list_non_class_modes(pool: &PgPool) -> sqlx::Result<Vec<MobAiModeRow>> {
    sqlx::query_as!(
        MobAiModeRow,
        r#"
        SELECT zone_id, id, ai_mode AS "ai_mode!: MobAiMode"
        FROM "Mobs"
        WHERE ai_mode <> 'CLASS'
        ORDER BY zone_id, id
        "#,
    )
    .fetch_all(pool)
    .await
}
