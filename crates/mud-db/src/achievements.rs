#![allow(clippy::doc_markdown)]
//! Achievement catalog + per-character unlock state.
//!
//! `Achievement` is the catalog (loaded once at boot); referenced
//! by stable `code` string from event hooks ("first_kill",
//! "zone_30_cleared", etc). `CharacterAchievement` stores the
//! per-player unlock + optional progress JSON for multi-step ones.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::AchievementCategory;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AchievementRow {
    pub id: i32,
    pub code: String,
    pub title: String,
    pub description: String,
    pub category: AchievementCategory,
    pub hidden: bool,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterAchievementRow {
    pub character_id: String,
    pub achievement_id: i32,
    pub unlocked_at: chrono::NaiveDateTime,
    pub progress: Option<serde_json::Value>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<AchievementRow>> {
    sqlx::query_as!(
        AchievementRow,
        r#"
        SELECT
            id,
            code,
            title,
            description,
            category AS "category: AchievementCategory",
            hidden,
            sort_order
        FROM achievement
        ORDER BY category, sort_order, id
        "#
    )
    .fetch_all(pool)
    .await
}

pub async fn unlocked_for(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<Vec<CharacterAchievementRow>> {
    sqlx::query_as!(
        CharacterAchievementRow,
        r#"
        SELECT character_id, achievement_id, unlocked_at, progress
        FROM character_achievement
        WHERE character_id = $1
        "#,
        character_id
    )
    .fetch_all(pool)
    .await
}

/// Insert (or no-op on duplicate) the unlock row. Returns
/// `Ok(true)` when the row was newly inserted, `Ok(false)` when
/// the player already had it unlocked.
pub async fn grant(
    pool: &PgPool,
    character_id: &str,
    achievement_id: i32,
    progress: Option<&serde_json::Value>,
) -> sqlx::Result<bool> {
    let res = sqlx::query!(
        r#"
        INSERT INTO character_achievement (character_id, achievement_id, progress)
        VALUES ($1, $2, $3)
        ON CONFLICT (character_id, achievement_id) DO NOTHING
        "#,
        character_id,
        achievement_id,
        progress
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Update the `progress` JSON for a partial-completion achievement
/// the player hasn't unlocked yet. Insert with the supplied
/// progress on first call so the runtime can write progress
/// without an unlock.
pub async fn upsert_progress(
    pool: &PgPool,
    character_id: &str,
    achievement_id: i32,
    progress: &serde_json::Value,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        INSERT INTO character_achievement (character_id, achievement_id, progress)
        VALUES ($1, $2, $3)
        ON CONFLICT (character_id, achievement_id)
        DO UPDATE SET progress = EXCLUDED.progress
        "#,
        character_id,
        achievement_id,
        progress,
    )
    .execute(pool)
    .await?;
    Ok(())
}
