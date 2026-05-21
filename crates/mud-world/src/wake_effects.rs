//! Rest / repose: Wake Effect attachment loader.
//!
//! Pulls rows from the `RoomWakeEffects` and `ObjectWakeEffects`
//! junction tables on demand (R7). Called by the first-XP-gain wake
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
    /// consumer uses this verbatim for the spawned EffectInstance's
    /// `remaining_secs`.
    pub duration: i32,
}

/// Load every `RoomWakeEffects` row for `(zone, id)` whose `minTier`
/// is null or ≤ `rest_tier`. The SQL filter is `min_tier IS NULL OR
/// min_tier <= $3` so the runtime caller doesn't have to post-filter.
///
/// Returns an empty vec when the room has no authored wake attachments
/// or every row's `minTier` exceeds the rented tier. Both are normal
/// states — the caller still spawns the universal Refreshed Effect on
/// `INN` source regardless.
pub async fn room_wake_effects(
    pool: &PgPool,
    zone: i32,
    id: i32,
    rest_tier: i32,
) -> sqlx::Result<Vec<WakeRow>> {
    let rows = sqlx::query!(
        r#"
        SELECT
            effect_id,
            modifier_data AS "modifier_data!: Value",
            duration
        FROM "RoomWakeEffects"
        WHERE room_zone_id = $1
          AND room_id      = $2
          AND (min_tier IS NULL OR min_tier <= $3)
        "#,
        zone,
        id,
        rest_tier,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| WakeRow {
            effect_id: r.effect_id,
            modifier_data: r.modifier_data,
            duration: r.duration,
        })
        .collect())
}

/// Load every `ObjectWakeEffects` row for `(zone, id)`. Used by the
/// `CAMP` and (future) `HOUSE` wake paths: a camp kit's rows fire on
/// the wake event after the kit is consumed; a HOUSE-source bed's
/// rows would fire on the same path once housing lands.
///
/// Returns empty when no rows are authored — the caller falls back
/// to the universal Refreshed Effect only.
pub async fn object_wake_effects(
    pool: &PgPool,
    zone: i32,
    id: i32,
) -> sqlx::Result<Vec<WakeRow>> {
    let rows = sqlx::query!(
        r#"
        SELECT
            effect_id,
            modifier_data AS "modifier_data!: Value",
            duration
        FROM "ObjectWakeEffects"
        WHERE object_zone_id = $1
          AND object_id      = $2
        "#,
        zone,
        id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| WakeRow {
            effect_id: r.effect_id,
            modifier_data: r.modifier_data,
            duration: r.duration,
        })
        .collect())
}
