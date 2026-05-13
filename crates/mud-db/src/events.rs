#![allow(clippy::doc_markdown)]
//! World-event catalog (parking-lot resolution: 2026-05-12).
//!
//! `Events` rows model server-wide holiday / festival / faction-shift
//! flags. Each row carries:
//!   - `active: bool` — manual operator override (immediate on/off)
//!   - `start_date / end_date` — optional date window for scheduled
//!     events (NULL = "manual only")
//!   - `recurring: bool` — repeats yearly (today: just a hint for the
//!     operator UI; the runtime is intentionally simple — admins flip
//!     `active` manually)
//!
//! The runtime hooks `QuestTriggerType::EVENT` quests through
//! `quest_triggers::dispatch_event_trigger(world, event_id)`. This
//! module surfaces the raw rows; the runtime catalog
//! (`mud-server::events::EventsCatalog`) holds the
//! active-edge bookkeeping so it only fires the dispatch on the
//! observed "off → on" transition.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRow {
    pub id: i32,
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub start_date: Option<chrono::NaiveDateTime>,
    pub end_date: Option<chrono::NaiveDateTime>,
    pub recurring: bool,
    pub active: bool,
}

/// Snapshot every Events row. The runtime catalog hydrates from this
/// once at boot and re-polls each `EVENT_POLL_PERIOD_TICKS` so an
/// admin flipping `active` from the Muditor UI is observed
/// server-side without a restart.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<EventRow>> {
    sqlx::query_as!(
        EventRow,
        r#"
        SELECT
            id AS "id!: i32",
            name AS "name!: String",
            display_name AS "display_name!: String",
            description,
            start_date AS "start_date: chrono::NaiveDateTime",
            end_date AS "end_date: chrono::NaiveDateTime",
            recurring AS "recurring!: bool",
            active AS "active!: bool"
        FROM "Events"
        ORDER BY id
        "#,
    )
    .fetch_all(pool)
    .await
}
