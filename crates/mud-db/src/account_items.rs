//! `AccountItems` round-trip — the account-shared chest. Any character
//! on the same `Users` row can deposit / withdraw here, which is the
//! key improvement over the per-character storage in `CharacterItems`.
//!
//! Unlike the character-side persistence which diff-writes a whole
//! inventory on save, the chest moves one row at a time: a `deposit`
//! call INSERTs a fresh row, a `withdraw` call DELETEs an existing
//! one. The runtime never carries a long-lived in-memory snapshot of
//! the chest — it's loaded on demand by the listing command and
//! consumed transactionally by the take command. That keeps the
//! cross-character "char A deposited, char B sees it" semantics easy
//! to reason about: the DB is the only source of truth.
//!
//! `custom_data` is the per-instance state worth preserving across
//! deposit/withdraw. v1 stores a small JSON envelope with the
//! mutable runtime fields the character_items path also persists:
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
pub async fn deposit(
    pool: &PgPool,
    user_id: &str,
    object_zone_id: i32,
    object_id: i32,
    quantity: i32,
    custom_data: Option<&serde_json::Value>,
    stored_by_character_id: Option<&str>,
) -> sqlx::Result<i32> {
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
    .fetch_one(pool)
    .await?;
    Ok(row.id)
}

/// Take a row out of the account chest. Returns the row that was
/// removed (so the caller can spawn a fresh entity with the
/// preserved `custom_data`) or `None` when the row is already gone
/// (race / double-withdraw — the runtime should surface "not
/// found" to the player without erroring out the command).
pub async fn withdraw(pool: &PgPool, item_id: i32) -> sqlx::Result<Option<AccountItemRow>> {
    sqlx::query_as!(
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
    .fetch_optional(pool)
    .await
}
