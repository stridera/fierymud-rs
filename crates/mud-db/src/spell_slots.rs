//! `SpellSlotProgression` + `ClassAbilityCircles` readers — drive
//! the per-class, per-level spell slot count for the `slots`
//! command and the eventual memorize/forget cycle.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SlotProgressionRow {
    pub level: i32,
    pub circle: i32,
    pub slots: i32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClassCircleRow {
    pub class_id: i32,
    pub circle: i32,
    pub min_level: i32,
}

pub async fn list_slot_progression(pool: &PgPool) -> sqlx::Result<Vec<SlotProgressionRow>> {
    sqlx::query_as!(
        SlotProgressionRow,
        r#"SELECT level, circle, slots FROM "SpellSlotProgression""#
    )
    .fetch_all(pool)
    .await
}

pub async fn list_class_circles(pool: &PgPool) -> sqlx::Result<Vec<ClassCircleRow>> {
    sqlx::query_as!(
        ClassCircleRow,
        r#"SELECT class_id, circle, min_level FROM "ClassAbilityCircles""#
    )
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClassAbilityRow {
    pub class_id: i32,
    pub ability_id: i32,
    pub circle: i32,
    pub proficiency_cap: i32,
}

pub async fn list_class_abilities(pool: &PgPool) -> sqlx::Result<Vec<ClassAbilityRow>> {
    sqlx::query_as!(
        ClassAbilityRow,
        r#"SELECT class_id, ability_id, circle, proficiency_cap FROM "ClassAbilities""#
    )
    .fetch_all(pool)
    .await
}
