//! `CharacterItems` round-trip — what a character is carrying, wearing, or has
//! stashed in containers.
//!
//! The schema column set is rich (instance flags, custom names, liquid state,
//! charges, condition). The runtime owns a subset that mutates during play —
//! `charges`, `liquid_remaining`, `liquid_type` — and round-trips those.
//! Other columns (`condition`, `custom_name`, `custom_examine_description`,
//! `custom_values`, `instance_flags`, `liquid_effects`, `liquid_identified`)
//! aren't yet read or written by any runtime command, so the save path
//! UPDATEs only the runtime-owned columns and leaves the rest untouched.
//! That preserves admin/editor edits to those fields across player saves.
//!
//! `equipped_location` is a free-text column historically. The runtime maps
//! known slot names to its Slot enum on load and writes back the canonical
//! upper-case form on save. Unknown slot strings are treated as inventory
//! (no equipped slot) — better than dropping the row entirely.

use std::collections::HashMap;

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
    /// Per-instance charge count for wand/staff/limited-use items.
    /// Schema default is `-1` (unlimited / no Charges component
    /// active). Runtime hydrates `Charges(n)` from this value when
    /// `>= 0`, otherwise falls back to the proto's binding charges.
    pub charges: i32,
    /// Per-instance liquid level. Hydrates `LiquidContainer.remaining`
    /// when the row has a `liquid_type` set; otherwise the proto's
    /// initial-fill value is used.
    pub liquid_remaining: i32,
    /// Schema's `Liquid` enum label — `WATER` / `WINE` / etc. NULL
    /// means "no override" — runtime uses the proto's liquid kind.
    /// Set when a player fills/pours and the container's contents
    /// have changed from the proto default.
    pub liquid_type: Option<String>,
}

/// Save-side per-item snapshot. Mirrors what the runtime knows about
/// each carried/worn/contained item at save time. `persisted_id` is
/// `Some` for items that came from the DB (UPDATE candidates) and
/// `None` for items acquired during the session (INSERT candidates).
/// `parent_idx` is only consulted when both this item and its
/// container are new (both INSERTs in the same save) — the diff
/// resolves the new container's id from the inserted-ids map.
#[derive(Debug, Clone)]
pub struct CharacterItemSnap {
    pub persisted_id: Option<i32>,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub equipped_location: Option<String>,
    /// Container resolution: prefer `parent_persisted_id` when the
    /// parent already has a row; fall back to `parent_idx` when the
    /// parent is also a new insert in this save.
    pub parent_persisted_id: Option<i32>,
    pub parent_idx: Option<usize>,
    /// `None` means "no Charges component on the entity" — the column
    /// stays at the schema default `-1`. `Some(n)` writes the value.
    pub charges: Option<i32>,
    /// Set together when the entity has a `LiquidContainer` whose
    /// state has been touched. `None` for non-liquid items or for
    /// liquid items that still match the proto's spawn defaults.
    pub liquid_remaining: Option<i32>,
    pub liquid_type: Option<String>,
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
pub async fn pscan_owners_by_item(pool: &PgPool, needle: &str) -> sqlx::Result<Vec<OwnerHit>> {
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
            equipped_location,
            charges,
            liquid_remaining,
            liquid_type
        FROM "CharacterItems"
        WHERE character_id = $1
        ORDER BY id
        "#,
        character_id,
    )
    .fetch_all(pool)
    .await
}

/// Diff-write the character's inventory. Compares the in-memory snapshot
/// against the DB row set:
///
/// * Rows whose `id` is no longer in the snapshot (item dropped, sold,
///   given) → DELETE.
/// * Snapshot entries with `persisted_id = Some` (loaded items still
///   carried) → UPDATE the runtime-owned columns (`equipped_location`,
///   `container_id`, `charges`, `liquid_remaining`, `liquid_type`,
///   `updated_at`) and re-home the row to this character
///   (`character_id`), so an item handed over from another character is
///   claimed rather than deleted by the previous owner's save. Other
///   columns (`condition`, `instance_flags`, `custom_name`, etc.) are
///   untouched. If the row no longer exists the item is inserted.
/// * Snapshot entries with `persisted_id = None` (newly acquired this
///   session) → INSERT.
///
/// Returns `idx → assigned_id` for each INSERT (including re-inserts of
/// a vanished persisted row) so the caller can stamp `PersistedItemId`
/// back onto the spawned entity. Entries are processed in input order so
/// an item placed inside a new container resolves its `container_id`
/// from this run's prior insert.
///
/// Multi-query helper — caller passes a `&mut PgConnection` and is
/// responsible for atomicity (wrap in a transaction if the work
/// should commit-or-rollback as a unit; `save_player` does this).
pub async fn save_inventory_diff(
    conn: &mut sqlx::PgConnection,
    character_id: &str,
    items: &[CharacterItemSnap],
) -> sqlx::Result<HashMap<usize, i32>> {
    // 1. DELETE rows for this character whose id isn't in the snapshot.
    //    Empty `keep` means delete everything (player gave up every
    //    item this session).
    let keep: Vec<i32> = items.iter().filter_map(|s| s.persisted_id).collect();
    if keep.is_empty() {
        sqlx::query!(
            r#"DELETE FROM "CharacterItems" WHERE character_id = $1"#,
            character_id,
        )
        .execute(&mut *conn)
        .await?;
    } else {
        sqlx::query!(
            r#"
            DELETE FROM "CharacterItems"
            WHERE character_id = $1 AND NOT (id = ANY($2))
            "#,
            character_id,
            &keep,
        )
        .execute(&mut *conn)
        .await?;
    }

    // 2. Upsert every snapshot entry in input order (parents precede
    //    children). A snapshot entry with a `persisted_id` is UPDATEd
    //    *and re-homed* (`character_id = $cid`): the item may have been
    //    handed over from another character whose row we now claim. Once
    //    the row carries our id, the previous owner's delete step (scoped
    //    to `character_id = <them>`) no longer matches it, so whichever
    //    owner saves first the item ends up as exactly one row owned by
    //    its current holder. If the UPDATE matches nothing (the previous
    //    owner's save already deleted the row, or it was never ours to
    //    begin with) the item is INSERTed fresh and the new id returned.
    //
    //    `ids[idx]` is the row id each entry ended up with; it resolves a
    //    child's `container_id` even when its parent was re-inserted under
    //    a new id in this same pass.
    let mut assigned: HashMap<usize, i32> = HashMap::new();
    let mut ids: Vec<i32> = Vec::with_capacity(items.len());
    let mut remapped: HashMap<i32, i32> = HashMap::new();
    for (idx, snap) in items.iter().enumerate() {
        let container_id: Option<i32> = snap
            .parent_idx
            .and_then(|p_idx| ids.get(p_idx).copied())
            .or_else(|| {
                snap.parent_persisted_id
                    .map(|pid| remapped.get(&pid).copied().unwrap_or(pid))
            });
        if let Some(id) = snap.persisted_id {
            let updated = sqlx::query!(
                r#"
                UPDATE "CharacterItems"
                SET character_id = $1,
                    equipped_location = $2,
                    container_id = $3,
                    charges = $4,
                    liquid_remaining = $5,
                    liquid_type = $6,
                    updated_at = NOW()
                WHERE id = $7
                RETURNING id
                "#,
                character_id,
                snap.equipped_location.as_deref(),
                container_id,
                snap.charges.unwrap_or(-1),
                snap.liquid_remaining.unwrap_or(0),
                snap.liquid_type.as_deref(),
                id,
            )
            .fetch_optional(&mut *conn)
            .await?;
            if updated.is_some() {
                ids.push(id);
                continue;
            }
        }
        let row = sqlx::query!(
            r#"
            INSERT INTO "CharacterItems"
                (character_id, object_zone_id, object_id,
                 equipped_location, container_id,
                 charges, liquid_remaining, liquid_type, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
            RETURNING id
            "#,
            character_id,
            snap.object_zone_id,
            snap.object_id,
            snap.equipped_location.as_deref(),
            container_id,
            snap.charges.unwrap_or(-1),
            snap.liquid_remaining.unwrap_or(0),
            snap.liquid_type.as_deref(),
        )
        .fetch_one(&mut *conn)
        .await?;
        if let Some(old) = snap.persisted_id {
            remapped.insert(old, row.id);
        }
        assigned.insert(idx, row.id);
        ids.push(row.id);
    }

    Ok(assigned)
}
