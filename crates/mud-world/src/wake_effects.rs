//! Rest / repose: Wake Effect attachment loader.
//!
//! Pulls rows from the `RoomWakeEffects` and `ObjectWakeEffects`
//! junction tables once at boot into a [`WakeEffectCatalog`] resource
//! (R7). The wake consumer runs inside ECS systems on a
//! current-thread runtime, so it must never block on the DB. Called by the first-XP-gain wake
//! consumer in mud-server when a player's `restSource` is consumed:
//!
//! - `INN` source → `room_wake_effects(zone, id, restTier)` against
//!   the room where the player logged off, filtered to entries with
//!   no `minTier` cap or a cap ≤ the rented tier.
//! - `CAMP` source → `object_wake_effects(zone, id)` against the
//!   consumed kit's `(zone, id)`. The kit reference is cached on a
//!   transient component at camp completion so the post-consume
//!   wake path doesn't need to remember the despawned entity.
//! - `HOUSE` source → reserved; the housing schema hasn't landed yet
//!   (TODO(housing)). When it does, this is the right query.
//!
//! The shape mirrors the design doc §"First XP gain after login": the
//! caller spawns one `EffectInstance` per returned `WakeRow`, linking
//! to the `effectId` row in the catalog and respecting the per-row
//! `duration` / `modifierData`.
//!
//! Both queries return an empty vector on miss; the caller treats
//! "no wake attachments authored" the same as "source has no extras"
//! and only the universal Refreshed Effect lands.

use std::collections::HashMap;

use bevy_ecs::prelude::Resource;
use serde_json::Value;
use sqlx::PgPool;

/// One Wake Effect attachment row. The shape collapses the schema's
/// composite-PK junction tables (`RoomWakeEffects`,
/// `ObjectWakeEffects`) into the three fields the wake-consume path
/// actually uses: which `Effect` to spawn, the per-attachment
/// modifier payload, and how long the spawned `EffectInstance`
/// should last on the target.
///
/// `RoomWakeEffects` rows additionally carry a `minTier` filter; the
/// `room_wake_effects` query applies that filter at the SQL layer so
/// callers get a uniform `Vec<WakeRow>` shape regardless of source.
#[derive(Debug, Clone)]
pub struct WakeRow {
    /// FK to `Effect.id` — the Effect catalog entry to spawn on the
    /// waking character.
    pub effect_id: i32,
    /// JSON payload that lands as the spawned `EffectInstance`'s
    /// `modifier_data` equivalent (today: passed through to per-effect
    /// hooks via the `EffectInstance.kind` lookup; future: drives the
    /// `ModifyDelta` companion for `effectType="modify"` rows).
    pub modifier_data: Value,
    /// Seconds the granted Effect lasts on the character. The wake
    /// consumer uses this verbatim for the spawned `EffectInstance`'s
    /// `remaining_secs`.
    pub duration: i32,
}

/// A `RoomWakeEffects` row plus its `minTier` gate.
#[derive(Debug, Clone)]
pub struct RoomWakeRow {
    /// Rented tier required for this row to fire; `None` = always.
    pub min_tier: Option<i32>,
    pub row: WakeRow,
}

/// Boot-time cache of the wake-effect junction tables. Content is
/// builder-authored, so it lives in the DB and is loaded into this
/// resource by [`load_wake_effect_catalog`]; runtime lookups are pure
/// in-memory reads.
#[derive(Resource, Debug, Default)]
pub struct WakeEffectCatalog {
    /// Keyed by room `(zone, id)`.
    pub by_room: HashMap<(i32, i32), Vec<RoomWakeRow>>,
    /// Keyed by object proto `(zone, id)`.
    pub by_object: HashMap<(i32, i32), Vec<WakeRow>>,
}

impl WakeEffectCatalog {
    /// Rows for `(zone, id)` whose `minTier` is null or <= `rest_tier`.
    /// Empty when the room has no authored wake attachments.
    #[must_use]
    pub fn room_rows(&self, zone: i32, id: i32, rest_tier: i32) -> Vec<WakeRow> {
        self.by_room
            .get(&(zone, id))
            .map(|rows| {
                rows.iter()
                    .filter(|r| r.min_tier.is_none_or(|t| t <= rest_tier))
                    .map(|r| r.row.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Rows for the object proto `(zone, id)` (camp kit, future bed).
    #[must_use]
    pub fn object_rows(&self, zone: i32, id: i32) -> Vec<WakeRow> {
        self.by_object.get(&(zone, id)).cloned().unwrap_or_default()
    }
}

/// Load every `RoomWakeEffects` and `ObjectWakeEffects` row.
///
/// # Errors
/// Returns the underlying sqlx error if either query fails.
pub async fn load_wake_effect_catalog(pool: &PgPool) -> sqlx::Result<WakeEffectCatalog> {
    let mut catalog = WakeEffectCatalog::default();
    let room_rows = sqlx::query!(
        r#"
        SELECT
            room_zone_id,
            room_id,
            min_tier,
            effect_id,
            modifier_data AS "modifier_data!: Value",
            duration
        FROM "RoomWakeEffects"
        "#,
    )
    .fetch_all(pool)
    .await?;
    for r in room_rows {
        catalog
            .by_room
            .entry((r.room_zone_id, r.room_id))
            .or_default()
            .push(RoomWakeRow {
                min_tier: r.min_tier,
                row: WakeRow {
                    effect_id: r.effect_id,
                    modifier_data: r.modifier_data,
                    duration: r.duration,
                },
            });
    }
    let object_rows = sqlx::query!(
        r#"
        SELECT
            object_zone_id,
            object_id,
            effect_id,
            modifier_data AS "modifier_data!: Value",
            duration
        FROM "ObjectWakeEffects"
        "#,
    )
    .fetch_all(pool)
    .await?;
    for r in object_rows {
        catalog
            .by_object
            .entry((r.object_zone_id, r.object_id))
            .or_default()
            .push(WakeRow {
                effect_id: r.effect_id,
                modifier_data: r.modifier_data,
                duration: r.duration,
            });
    }
    Ok(catalog)
}
