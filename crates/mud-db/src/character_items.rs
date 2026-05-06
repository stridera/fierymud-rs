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

/// Insert payload — one row per item the character is carrying, wearing,
/// or has stashed inside another item. The runtime owns the
/// `(zone_id, id)` prototype key, the optional slot label, and an
/// optional `parent_idx` pointing at this Vec's earlier slot when this
/// item lives inside a container. Topological constraint: any
/// `parent_idx` must be strictly less than the row's own index, so
/// parents are inserted before children and `save_for` can resolve
/// `container_id` via the returned id of the parent row.
#[derive(Debug, Clone)]
pub struct NewCharacterItem {
    pub object_zone_id: i32,
    pub object_id: i32,
    pub equipped_location: Option<String>,
    /// Position of this item's container in the input Vec. None means
    /// this item is directly carried/equipped by the character.
    pub parent_idx: Option<usize>,
}

/// One row from the `pscan` admin lookup — a player + an item
/// proto they own. Ordered by player name then item name in
/// the query, but admin renderers are free to re-sort.
#[derive(Debug, Clone)]
pub struct OwnerHit {
    pub character_id: String,
    pub character_name: String,
    pub level: i32,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub object_name: String,
    pub equipped_location: Option<String>,
}

/// Search every persisted character's inventory for items whose
/// proto name matches `needle` (case-insensitive substring).
/// Returns one row per match — same character can show up
/// multiple times if they're carrying duplicates. Capped at
/// 200 rows server-side to avoid surprising big-result floods.
pub async fn pscan_owners_by_item(
    pool: &PgPool,
    needle: &str,
) -> sqlx::Result<Vec<OwnerHit>> {
    let pattern = format!("%{}%", needle.to_lowercase());
    sqlx::query_as!(
        OwnerHit,
        r#"
        SELECT
            c.id              AS character_id,
            c.name            AS character_name,
            c.level           AS level,
            o.zone_id         AS object_zone_id,
            o.id              AS object_id,
            o.name            AS object_name,
            ci.equipped_location AS equipped_location
        FROM "CharacterItems" ci
        JOIN "Characters" c ON c.id = ci.character_id
        JOIN "Objects" o
          ON o.zone_id = ci.object_zone_id
         AND o.id = ci.object_id
        WHERE LOWER(o.name) LIKE $1
        ORDER BY c.name, o.name
        LIMIT 200
        "#,
        pattern,
    )
    .fetch_all(pool)
    .await
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

/// Replace the entire `CharacterItems` set for a character. Inserts run
/// in input order so each row's `parent_idx` (when set) can be resolved
/// to the previously-inserted parent's auto-generated `id` via a
/// running index→id map. Single transaction — partial failure rolls
/// back cleanly.
pub async fn save_for(
    pool: &PgPool,
    character_id: &str,
    items: &[NewCharacterItem],
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    // Wipe every row for this character (top-level AND nested), since
    // the runtime now reconstructs the full chain.
    sqlx::query!(
        r#"DELETE FROM "CharacterItems" WHERE character_id = $1"#,
        character_id,
    )
    .execute(&mut *tx)
    .await?;
    let mut inserted_ids: Vec<i32> = Vec::with_capacity(items.len());
    for it in items {
        let container_id: Option<i32> = it.parent_idx.and_then(|idx| inserted_ids.get(idx).copied());
        let row = sqlx::query!(
            r#"
            INSERT INTO "CharacterItems"
                (character_id, object_zone_id, object_id, equipped_location, container_id, updated_at)
            VALUES ($1, $2, $3, $4, $5, NOW())
            RETURNING id
            "#,
            character_id,
            it.object_zone_id,
            it.object_id,
            it.equipped_location.as_deref(),
            container_id,
        )
        .fetch_one(&mut *tx)
        .await?;
        inserted_ids.push(row.id);
    }
    tx.commit().await?;
    Ok(())
}
