//! Periodic flush of the in-memory `QuestVariableCache` back to the
//! `CharacterQuests.variables` Postgres column (parking-lot
//! resolution).
//!
//! Lua-side trigger writes (`quest:setvar` / `quest:clearvar`) hit
//! the cache; this system drains the dirty set every
//! `QUEST_FLUSH_PERIOD_TICKS` ticks and spawns a tokio task per dirty
//! quest to upsert / delete the affected JSON keys.
//!
//! Cadence: same 10-second cadence as `entity_vars::flush_tick`. The
//! tradeoff is identical — a crash loses up to 10s of variable
//! updates, which is acceptable for the quest-scripting use case
//! (progress counters, stage markers; re-driven by the next trigger
//! fire). Hard-durability writes go through `accept_for_player` and
//! `try_advance_phase`, which round-trip the DB synchronously and
//! aren't routed through this cache.

use bevy_ecs::prelude::*;
use mud_db::sqlx;
use mud_world::QuestVariableCache;
use tracing::warn;

use crate::TickCount;
use crate::commands::DbPool;

/// 10 seconds at TICK_HZ=10. Mirrors `entity_vars` cadence so the
/// two flush systems coalesce into the same db-write spike.
const QUEST_FLUSH_PERIOD_TICKS: u64 = 100;

/// Drain dirty quest-variable bags out of `QuestVariableCache` and
/// persist each one via `mud_db::quests::set_quest_variable`. The
/// DB calls run in a per-quest tokio task so they don't block the
/// schedule.
///
/// `set_quest_variable` keys by the CharacterQuest *row id*, not
/// `(character_id, quest_zone, quest_id)`. The id is resolved per
/// flush via `find_character_quest`. A missing row means the
/// player hasn't accepted the quest yet (or has abandoned it); the
/// write is silently skipped. The cache entry is dropped anyway by
/// `drain_dirty`, so a re-accept later starts with a clean bag.
pub fn quest_var_flush_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(QUEST_FLUSH_PERIOD_TICKS) {
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    if !world.contains_resource::<QuestVariableCache>() {
        return;
    }
    let drained = world
        .resource_mut::<QuestVariableCache>()
        .drain_dirty();
    if drained.is_empty() {
        return;
    }
    for (cid, qz, qid, sets, clears) in drained {
        let pool = pool.clone();
        tokio::spawn(async move {
            flush_one(&pool, &cid, qz, qid, &sets, &clears).await;
        });
    }
}

async fn flush_one(
    pool: &sqlx::PgPool,
    character_id: &str,
    quest_zone: i32,
    quest_id: i32,
    sets: &[(String, serde_json::Value)],
    clears: &[String],
) {
    // Resolve the CharacterQuest row id. None → player isn't on
    // this quest (no accept yet, or abandoned + cleaned). Skip
    // silently — the next quest:setvar after accept will mark
    // the row dirty again with the fresh cache state.
    let cqid = match mud_db::quests::find_character_quest(pool, character_id, quest_zone, quest_id)
        .await
    {
        Ok(Some((id, _status))) => id,
        Ok(None) => return,
        Err(e) => {
            warn!(
                character_id,
                quest_zone,
                quest_id,
                error = %e,
                "quest var flush: find_character_quest failed"
            );
            return;
        }
    };
    for (key, value) in sets {
        if let Err(e) =
            mud_db::quests::set_quest_variable(pool, &cqid, key, value).await
        {
            warn!(
                character_id,
                quest_zone,
                quest_id,
                key,
                error = %e,
                "quest var flush: set failed"
            );
        }
    }
    for key in clears {
        if let Err(e) =
            mud_db::quests::set_quest_variable(pool, &cqid, key, &serde_json::Value::Null).await
        {
            warn!(
                character_id,
                quest_zone,
                quest_id,
                key,
                error = %e,
                "quest var flush: clear failed"
            );
        }
    }
}
