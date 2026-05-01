//! Race-innate ability rows. Each `(race, ability_id)` says "members
//! of this race start with this ability at the given proficiency".
//! The runtime reads these for the `innate` listing — the ability
//! catalog still gates the actual runtime behavior.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaceAbilityRow {
    /// `Race` enum value as raw text (HUMAN / ELF / etc.).
    pub race: String,
    pub ability_id: i32,
    /// Display name from the joined `Ability.plain_name`.
    pub ability_name: String,
    /// `SkillCategory` (PRIMARY / SECONDARY / ...) as raw text.
    pub category: String,
    pub bonus: i32,
    pub proficiency_cap: i32,
}

/// Race-innate abilities for a single race, sorted by ability name.
pub async fn list_for_race(
    pool: &PgPool,
    race: &str,
) -> sqlx::Result<Vec<RaceAbilityRow>> {
    sqlx::query_as!(
        RaceAbilityRow,
        r#"
        SELECT
            ra.race::text       AS "race!: String",
            ra.ability_id,
            a.plain_name        AS "ability_name!: String",
            ra.category::text   AS "category!: String",
            ra.bonus,
            ra.proficiency_cap
        FROM "RaceAbilities" ra
        JOIN "Ability" a ON a.id = ra.ability_id
        WHERE ra.race::text = $1
        ORDER BY a.plain_name
        "#,
        race,
    )
    .fetch_all(pool)
    .await
}
