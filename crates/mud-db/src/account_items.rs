//! `AccountItems` round-trip — the account-shared chest. Any character
//! on the same `Users` row can deposit / withdraw here, which is the
//! key improvement over the per-character storage in `CharacterItems`.
//!
//! Unlike the character-side persistence which diff-writes a whole
//! inventory on save, the chest moves one row at a time: a `deposit`
//! call INSERTs a fresh row (and deletes the item's inventory row in the
//! same transaction), a `withdraw_to_inventory` call DELETEs an existing
//! one (and inserts the inventory row in the same transaction). The runtime never carries a long-lived in-memory snapshot of
//! the chest — it's loaded on demand by the listing command and
//! consumed transactionally by the take command. That keeps the
//! cross-character "char A deposited, char B sees it" semantics easy
//! to reason about: the DB is the only source of truth.
//!
//! `custom_data` is the per-instance state worth preserving across
//! deposit/withdraw. v1 stores a small JSON envelope with the
//! mutable runtime fields the `character_items` path also persists:
//! `charges`, `liquid_remaining`, `liquid_type`. Future passes can
//! extend the shape — `serde_json::Value` keeps that open without a
//! schema change.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountItemRow {
    pub id: i32,
    pub user_id: String,
    /// Display-order index in the chest. Currently assigned as
    /// `max(slot)+1` on each deposit so the chest reads as a stable
    /// "newest-on-bottom" list; eventually the player could choose.
    pub slot: i32,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub quantity: i32,
    pub custom_data: Option<serde_json::Value>,
    pub stored_by_character_id: Option<String>,
    pub stored_at: chrono::NaiveDateTime,
}

/// Read every row in this user's account chest. Ordered by `slot`,
/// then `id` as a tiebreak — keeps the on-screen list stable even
/// when two rows happen to share a slot (shouldn't happen, but the
/// schema doesn't enforce uniqueness on `slot`).
pub async fn list_for_user(pool: &PgPool, user_id: &str) -> sqlx::Result<Vec<AccountItemRow>> {
    sqlx::query_as!(
        AccountItemRow,
        r#"
        SELECT
            id,
            user_id,
            slot,
            object_zone_id,
            object_id,
            quantity,
            custom_data,
            stored_by_character_id,
            stored_at
        FROM account_items
        WHERE user_id = $1
        ORDER BY slot, id
        "#,
        user_id,
    )
    .fetch_all(pool)
    .await
}

/// INSERT a fresh row into the account chest and return its id.
/// The new row's `slot` is `max(slot)+1` for this user (or 0 if the
/// chest is empty) — keeps the listing append-only by default
/// without needing the caller to compute it. `stored_at` defaults to
/// `NOW()` via the schema.
///
/// `inventory_row_id` is the depositing character's `CharacterItems`
/// row for the item (its `PersistedItemId`), if it has one. It is
/// deleted in the same transaction as the INSERT so a crash between
/// the deposit and the character's next save can never leave the item
/// in both the chest and the inventory. Not scoped to a character: only
/// one entity can hold a given id, and an item handed over since its last
/// save may still sit in the previous holder's row.
#[allow(clippy::too_many_arguments)]
pub async fn deposit(
    pool: &PgPool,
    user_id: &str,
    object_zone_id: i32,
    object_id: i32,
    quantity: i32,
    custom_data: Option<&serde_json::Value>,
    stored_by_character_id: Option<&str>,
    inventory_row_id: Option<i32>,
) -> sqlx::Result<i32> {
    let mut tx = pool.begin().await?;
    if let Some(row_id) = inventory_row_id {
        sqlx::query!(r#"DELETE FROM "CharacterItems" WHERE id = $1"#, row_id)
            .execute(&mut *tx)
            .await?;
    }
    let row = sqlx::query!(
        r#"
        INSERT INTO account_items
            (user_id, slot, object_zone_id, object_id, quantity,
             custom_data, stored_by_character_id)
        VALUES (
            $1,
            COALESCE((SELECT MAX(slot) + 1 FROM account_items WHERE user_id = $1), 0),
            $2, $3, $4, $5, $6
        )
        RETURNING id
        "#,
        user_id,
        object_zone_id,
        object_id,
        quantity,
        custom_data,
        stored_by_character_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row.id)
}

/// Take a row out of the account chest into a character's inventory, in
/// ONE transaction: the chest row is deleted and the `CharacterItems` row
/// inserted together, so a crash can neither lose the item nor leave it in
/// both places. Returns the removed chest row (so the caller can spawn an
/// entity with the preserved `custom_data`) and the new inventory row's id
/// (to stamp on that entity as its `PersistedItemId`), or `None` when the
/// chest row is already gone (race / double-withdraw — the runtime should
/// surface "not found" to the player without erroring out the command).
///
/// `charges`, `liquid_remaining`, `liquid_type` and the `curse` delta are
/// carried over from `custom_data`; the other per-instance fields have no inventory column.
pub async fn withdraw_to_inventory(
    pool: &PgPool,
    item_id: i32,
    character_id: &str,
) -> sqlx::Result<Option<(AccountItemRow, i32)>> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query_as!(
        AccountItemRow,
        r#"
        DELETE FROM account_items
        WHERE id = $1
        RETURNING
            id,
            user_id,
            slot,
            object_zone_id,
            object_id,
            quantity,
            custom_data,
            stored_by_character_id,
            stored_at
        "#,
        item_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let field = |key: &str| row.custom_data.as_ref().and_then(|v| v.get(key));
    let int = |key: &str| {
        field(key)
            .and_then(serde_json::Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
    };
    let charges = int("charges").unwrap_or(-1);
    let liquid_remaining = int("liquid_remaining").unwrap_or(0);
    let liquid_type = field("liquid_type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    // A Curse's delta rides along as the `curse` key of `custom_values`
    // (see `character_items::ItemAlter`).
    let mut custom_values = serde_json::Map::new();
    if let Some(curse) = field("curse")
        .and_then(|v| serde_json::from_value::<crate::character_items::ItemAlter>(v.clone()).ok())
        .filter(|a| !a.is_empty())
        && let Ok(v) = serde_json::to_value(&curse)
    {
        custom_values.insert("curse".into(), v);
    }
    let custom_values = serde_json::Value::Object(custom_values);
    let inserted = sqlx::query!(
        r#"
        INSERT INTO "CharacterItems"
            (character_id, object_zone_id, object_id,
             charges, liquid_remaining, liquid_type, custom_values, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, NOW())
        RETURNING id
        "#,
        character_id,
        row.object_zone_id,
        row.object_id,
        charges,
        liquid_remaining,
        liquid_type,
        custom_values,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some((row, inserted.id)))
}
