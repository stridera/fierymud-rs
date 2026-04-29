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
