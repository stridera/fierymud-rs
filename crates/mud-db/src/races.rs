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
