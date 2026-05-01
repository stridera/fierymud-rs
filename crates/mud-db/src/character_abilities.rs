//! What abilities a character knows. The schema also tracks proficiency
//! (0–1000 in legacy `CircleMUD` style; some imports cap at higher), the
//! known/learning flag (`known = false` means "trained but not yet
//! mastered"), and a `last_used` timestamp.
//!
//! The runtime today reads just the (`ability_id`, proficiency, known)
//! tuple per character. Save is deferred until commands that mutate
//! proficiency land (`practice`, `study`).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterAbilityRow {
    pub ability_id: i32,
    pub known: bool,
    pub proficiency: i32,
}

pub async fn list_for(pool: &PgPool, character_id: &str) -> sqlx::Result<Vec<CharacterAbilityRow>> {
    sqlx::query_as!(
        CharacterAbilityRow,
        r#"
        SELECT ability_id, known, proficiency
        FROM "CharacterAbilities"
        WHERE character_id = $1
        ORDER BY ability_id
        "#,
        character_id,
    )
    .fetch_all(pool)
    .await
}

/// Replace the character's full `CharacterAbilities` set with `rows`.
/// Wraps DELETE + bulk INSERT in a single transaction — matches the
/// `character_items::save_for` pattern. Empty `rows` means "wipe
/// every learned ability" which is rare but valid (e.g. a respec).
pub async fn save_for(
    pool: &PgPool,
    character_id: &str,
    rows: &[CharacterAbilityRow],
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        r#"DELETE FROM "CharacterAbilities" WHERE character_id = $1"#,
        character_id
    )
    .execute(&mut *tx)
    .await?;
    for r in rows {
        sqlx::query!(
            r#"
            INSERT INTO "CharacterAbilities"
                (character_id, ability_id, known, proficiency)
            VALUES ($1, $2, $3, $4)
            "#,
            character_id,
            r.ability_id,
            r.known,
            r.proficiency,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
