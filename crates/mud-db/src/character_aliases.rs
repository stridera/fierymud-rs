//! Player-defined command aliases. Each row is `(alias, command)` —
//! when the player types `<alias> <args>`, the dispatcher rewrites it
//! to `<command> <args>` before lookup. v1 is plain prefix replacement;
//! positional substitution (`$1`, `$*` legacy-style) can land later.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterAliasRow {
    pub alias: String,
    pub command: String,
}

pub async fn list_for(pool: &PgPool, character_id: &str) -> sqlx::Result<Vec<CharacterAliasRow>> {
    sqlx::query_as!(
        CharacterAliasRow,
        r#"
        SELECT alias, command
        FROM "CharacterAliases"
        WHERE character_id = $1
        ORDER BY alias
        "#,
        character_id,
    )
    .fetch_all(pool)
    .await
}

/// Replace the character's full `CharacterAliases` set with `rows`.
/// Multi-query helper — caller passes a `&mut PgConnection` and is
/// responsible for atomicity (wrap in a transaction if needed).
/// Empty `rows` clears every alias for the character.
pub async fn save_for(
    conn: &mut sqlx::PgConnection,
    character_id: &str,
    rows: &[CharacterAliasRow],
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"DELETE FROM "CharacterAliases" WHERE character_id = $1"#,
        character_id
    )
    .execute(&mut *conn)
    .await?;
    for r in rows {
        sqlx::query!(
            r#"
            INSERT INTO "CharacterAliases" (character_id, alias, command)
            VALUES ($1, $2, $3)
            "#,
            character_id,
            r.alias,
            r.command,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}
