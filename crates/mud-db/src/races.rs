//! Per-race defaults from the schema's `Race` table. The runtime
//! treats race-as-text most places (gender / race strings round-
//! trip the same way), but a few display surfaces — score's "Size:
//! Medium" line, gear restrictions referring to size class — need
//! to resolve `(race) -> default_size`. This module loads just
//! that mapping; the full Race row carries far more (focusBonus /
//! defaultLifeforce / etc.) and lands here as needed.

use std::collections::HashMap;

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
