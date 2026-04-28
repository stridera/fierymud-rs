//! `CharacterItems` round-trip — what a character is carrying, wearing, or has
//! stashed in containers.
//!
//! The schema column set is rich (instance flags, custom names, liquid state,
//! charges) but for the runtime's first-pass round-trip we only need the four
//! fields that determine where the item lives at login: the prototype key
//! `(object_zone_id, object_id)`, the optional `equipped_location` slot
//! string, and the optional `container_id` for items inside a container.
//!
//! `equipped_location` is a free-text column historically. The runtime maps
//! known slot names to its Slot enum on load and writes back the canonical
//! upper-case form on save. Unknown slot strings are treated as inventory
//! (no equipped slot) — better than dropping the row entirely.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterItemRow {
    pub id: i32,
    pub character_id: String,
    pub object_zone_id: i32,
    pub object_id: i32,
    /// References another row in this table when the item is inside a
    /// container the character is carrying. Resolved by the runtime
    /// loader after all rows are spawned.
    pub container_id: Option<i32>,
    /// Free-text slot name when worn. Translated by the runtime against
    /// `mud_world::Slot`.
    pub equipped_location: Option<String>,
}

/// Insert payload — one row per item the character is carrying or wearing.
/// The runtime owns the `(zone_id, id)` prototype key and the optional slot
/// label; everything else (charges, condition, custom_*) is left to the
/// schema's defaults for a fresh save.
#[derive(Debug, Clone)]
pub struct NewCharacterItem {
    pub object_zone_id: i32,
    pub object_id: i32,
    pub equipped_location: Option<String>,
}

/// Read every item row for a character. Ordered by `id` (insertion order)
/// so the runtime sees items in a deterministic shape.
pub async fn list_for(pool: &PgPool, character_id: &str) -> sqlx::Result<Vec<CharacterItemRow>> {
    sqlx::query_as!(
        CharacterItemRow,
        r#"
        SELECT
            id,
            character_id,
            object_zone_id,
            object_id,
            container_id,
            equipped_location
        FROM "CharacterItems"
        WHERE character_id = $1
        ORDER BY id
        "#,
        character_id,
    )
    .fetch_all(pool)
    .await
}

/// Replace the top-level item rows for a character (every row whose
/// `container_id IS NULL` — i.e. items the runtime materializes as
/// inventory or equipped). Rows nested inside containers stay untouched:
/// the runtime doesn't yet rehydrate them and we don't want to wipe
/// stashed inventory just because we don't model it yet.
///
/// Side effect of the DELETE: any row whose `container_id` pointed to a
/// row we just deleted has its `container_id` set to NULL via the schema's
/// `ON DELETE SET NULL` cascade. Those orphans become top-level on the
/// next login and get loaded into the player's inventory — graceful
/// degradation rather than silent loss. (Once the runtime grows real
/// container commands and parent-aware save logic, this stops mattering.)
///
/// All in one transaction so a failure mid-save doesn't leave the
/// character with a partial inventory.
pub async fn save_for(
    pool: &PgPool,
    character_id: &str,
    items: &[NewCharacterItem],
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        r#"DELETE FROM "CharacterItems" WHERE character_id = $1 AND container_id IS NULL"#,
        character_id,
    )
    .execute(&mut *tx)
    .await?;
    for it in items {
        sqlx::query!(
            r#"
            INSERT INTO "CharacterItems"
                (character_id, object_zone_id, object_id, equipped_location, updated_at)
            VALUES ($1, $2, $3, $4, NOW())
            "#,
            character_id,
            it.object_zone_id,
            it.object_id,
            it.equipped_location.as_deref(),
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
