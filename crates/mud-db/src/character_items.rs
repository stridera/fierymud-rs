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

use std::collections::{HashMap, HashSet};

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
///   carried) → UPDATE only the runtime-owned columns
///   (`equipped_location`, `container_id`, `charges`, `liquid_remaining`,
///   `liquid_type`, `updated_at`). Other columns (`condition`,
///   `instance_flags`, `custom_name`, etc.) are untouched.
/// * Snapshot entries with `persisted_id = None` (newly acquired this
///   session) → INSERT.
///
/// Returns `idx → assigned_id` for each INSERT so the caller can stamp
/// `PersistedItemId` back onto the spawned entity. Inserts run in input
/// order so a new item placed inside a new container can resolve its
/// `container_id` from this run's prior insert.
///
/// Single transaction — partial failure rolls back cleanly.
pub async fn save_inventory_diff(
    pool: &PgPool,
    character_id: &str,
    items: &[CharacterItemSnap],
) -> sqlx::Result<HashMap<usize, i32>> {
    let mut tx = pool.begin().await?;

    // 1. DELETE rows for this character whose id isn't in the snapshot.
    //    Empty `keep` means delete everything (player gave up every
    //    item this session).
    let keep: Vec<i32> = items.iter().filter_map(|s| s.persisted_id).collect();
    if keep.is_empty() {
        sqlx::query!(
            r#"DELETE FROM "CharacterItems" WHERE character_id = $1"#,
            character_id,
        )
        .execute(&mut *tx)
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
        .execute(&mut *tx)
        .await?;
    }

    // 2. UPDATE existing rows. Container resolution at this point can
    //    use any `parent_persisted_id` directly; if the parent is a new
    //    insert (still in step 3 below) `parent_persisted_id` is None
    //    and `container_id` will be set in step 3 — but a loaded item
    //    can't have a not-yet-inserted parent because the parent would
    //    have to be loaded too. So we never need to retro-update an
    //    UPDATE row's container after a later INSERT.
    for snap in items {
        let Some(id) = snap.persisted_id else {
            continue;
        };
        sqlx::query!(
            r#"
            UPDATE "CharacterItems"
            SET equipped_location = $1,
                container_id = $2,
                charges = $3,
                liquid_remaining = $4,
                liquid_type = $5,
                updated_at = NOW()
            WHERE id = $6
            "#,
            snap.equipped_location.as_deref(),
            snap.parent_persisted_id,
            snap.charges.unwrap_or(-1),
            snap.liquid_remaining.unwrap_or(0),
            snap.liquid_type.as_deref(),
            id,
        )
        .execute(&mut *tx)
        .await?;
    }

    // 3. INSERT new items. For each, resolve container_id by either
    //    `parent_persisted_id` (parent was a loaded row) or
    //    `parent_idx` (parent is also a new insert earlier in `items`).
    //    The latter requires the parent's INSERT to have run first —
    //    `parent_idx` is constrained to be strictly less than the
    //    row's own index by the BFS that builds the snapshot.
    let mut assigned: HashMap<usize, i32> = HashMap::new();
    let inserted_idxs: HashSet<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, s)| s.persisted_id.is_none())
        .map(|(i, _)| i)
        .collect();
    for (idx, snap) in items.iter().enumerate() {
        if !inserted_idxs.contains(&idx) {
            continue;
        }
        let container_id: Option<i32> = snap.parent_persisted_id.or_else(|| {
            snap.parent_idx
                .and_then(|p_idx| assigned.get(&p_idx).copied())
        });
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
        .fetch_one(&mut *tx)
        .await?;
        assigned.insert(idx, row.id);
    }

    tx.commit().await?;
    Ok(assigned)
}
