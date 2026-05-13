//! Periodic flush of the in-memory `EntityVariableCache` back to the
//! `entity_variables` Postgres table.
//!
//! Lua trigger `:setvar` / `:clearvar` writes hit only the cache (no
//! DB round-trip from the world tick — DB writes are async and the
//! tick is sync). This system drains the cache's dirty set every
//! `FLUSH_PERIOD_TICKS` ticks and spawns a tokio task per dirty
//! entity to upsert / delete the affected rows.
//!
//! A 10-second cadence trades durability for write volume: a crash
//! loses at most 10s of variable updates, which is acceptable for the
//! scripting use case (cooldown counters, quest stage advances —
//! re-driven by the next trigger fire). If a particular trigger
//! pattern needs harder durability the trigger body can call
//! `actor:save()` to force an immediate checkpoint of player-side
//! state, which is the existing path.

use bevy_ecs::prelude::*;
use mud_db::enums::EntityType;
use mud_db::sqlx;
use mud_world::EntityVariableCache;
use tracing::warn;

use crate::TickCount;
use crate::commands::DbPool;

/// Flush every 10 seconds. At TICK_HZ=10 that's 100 ticks. Tunable;
/// raise for heavier write loads (mass quest completion), lower for
/// tighter durability if it becomes a concern.
const FLUSH_PERIOD_TICKS: u64 = 100;

/// Drain dirty bags out of `EntityVariableCache` and persist each one
/// via `entity_variables::upsert_many` (sets) and per-key `delete`
/// (clears). The actual DB calls run on a tokio task so they don't
/// block the schedule; if a write fails we log a warning but the
/// in-memory cache stays consistent.
pub fn entity_var_flush_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(FLUSH_PERIOD_TICKS) {
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    if !world.contains_resource::<EntityVariableCache>() {
        return;
    }
    let drained = world
        .resource_mut::<EntityVariableCache>()
        .drain_dirty();
    if drained.is_empty() {
        return;
    }
    for (kind, zone, id, sets, clears) in drained {
        let pool = pool.clone();
        tokio::spawn(async move {
            flush_one(&pool, kind, zone, id, &sets, &clears).await;
        });
    }
}

async fn flush_one(
    pool: &sqlx::PgPool,
    kind: EntityType,
    zone: i32,
    id: i32,
    sets: &[(String, serde_json::Value)],
    clears: &[String],
) {
    if !sets.is_empty()
        && let Err(e) = mud_db::entity_variables::upsert_many(pool, kind, zone, id, sets).await
    {
        warn!(
            entity_type = kind.label(),
            zone,
            id,
            count = sets.len(),
            error = %e,
            "entity variable flush: upsert failed"
        );
    }
    for key in clears {
        if let Err(e) = mud_db::entity_variables::delete(pool, kind, zone, id, key).await {
            warn!(
                entity_type = kind.label(),
                zone,
                id,
                key,
                error = %e,
                "entity variable flush: delete failed"
            );
        }
    }
}
