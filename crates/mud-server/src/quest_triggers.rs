//! Quest trigger dispatchers (Wave 4.1).
//!
//! Wires `Quest.trigger_type` columns (`MOB` is handled directly
//! by the existing `qaccept` flow; this module covers the others:
//! `LEVEL` / `ITEM` / `ROOM` / `SKILL` / `EVENT` / `AUTO`).
//!
//! Every entry point follows the same pattern:
//! 1. Snapshot `(character_id, level)` from the world.
//! 2. Spawn a tokio task that queries `Quests` for matching rows.
//! 3. For each row, decide whether to `auto_accept` immediately or
//!    just inform the player it's available (a future `qaccept`
//!    will pick it up).
//!
//! The handlers are deliberately async + fire-and-forget — they
//! piggy-back on the existing `bump_*_quest_progress` async path
//! and read back through the player's `Outbound`.
//!
//! `MANUAL` is intentionally absent: those quests are only assigned
//! by admin tooling (`qgive` / `qload`), which already lives in
//! `commands/quests.rs`.

#![allow(clippy::doc_markdown)]

use bevy_ecs::prelude::*;
use mud_world::{Account, Online, Player, Profile};

use crate::commands::{Connection, DbPool};

/// Dispatch LEVEL-trigger quests for `player` at the moment their
/// `Profile.level` becomes `new_level`. Fired from
/// `combat::check_level_up` after the level field is bumped.
pub(crate) fn dispatch_level_trigger(world: &mut World, player: Entity, new_level: i32) {
    let Some(cid) = world.get::<Account>(player).map(|a| a.character_id.clone()) else {
        return;
    };
    let Some(out) = world.get::<Connection>(player).map(|c| c.0.clone()) else {
        return;
    };
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    tokio::spawn(async move {
        let quests = match mud_db::quests::list_by_trigger_level(&pool, new_level).await {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "level-trigger quest lookup failed");
                return;
            }
        };
        for q in quests {
            if q.hidden {
                continue;
            }
            grant_or_offer(&pool, &cid, &out, new_level, &q).await;
        }
    });
}

/// Dispatch ITEM-trigger quests when `player` picks up an item
/// with prototype `(item_zone, item_id)`. Fired from `cmd_get`
/// alongside `bump_collect_quest_progress`.
pub(crate) fn dispatch_item_trigger(
    world: &mut World,
    player: Entity,
    item_zone: i32,
    item_id: i32,
) {
    let Some(cid) = world.get::<Account>(player).map(|a| a.character_id.clone()) else {
        return;
    };
    let Some(out) = world.get::<Connection>(player).map(|c| c.0.clone()) else {
        return;
    };
    let level = world.get::<Profile>(player).map_or(1, |p| p.level);
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    tokio::spawn(async move {
        let quests = match mud_db::quests::list_by_trigger_item(&pool, item_zone, item_id).await {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "item-trigger quest lookup failed");
                return;
            }
        };
        for q in quests {
            if q.hidden {
                continue;
            }
            grant_or_offer(&pool, &cid, &out, level, &q).await;
        }
    });
}

/// Dispatch ROOM-trigger quests when `player` enters a room with
/// prototype `(room_zone, room_id)`. Fired from `mark_room_visited`.
pub(crate) fn dispatch_room_trigger(
    world: &mut World,
    player: Entity,
    room_zone: i32,
    room_id: i32,
) {
    let Some(cid) = world.get::<Account>(player).map(|a| a.character_id.clone()) else {
        return;
    };
    let Some(out) = world.get::<Connection>(player).map(|c| c.0.clone()) else {
        return;
    };
    let level = world.get::<Profile>(player).map_or(1, |p| p.level);
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    tokio::spawn(async move {
        let quests = match mud_db::quests::list_by_trigger_room(&pool, room_zone, room_id).await {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "room-trigger quest lookup failed");
                return;
            }
        };
        for q in quests {
            if q.hidden {
                continue;
            }
            grant_or_offer(&pool, &cid, &out, level, &q).await;
        }
    });
}

/// Dispatch SKILL-trigger quests when `player` first successfully
/// uses ability `ability_id`. Fired from `bump_use_skill_quest_progress`'s
/// caller.
pub(crate) fn dispatch_skill_trigger(world: &mut World, player: Entity, ability_id: i32) {
    let Some(cid) = world.get::<Account>(player).map(|a| a.character_id.clone()) else {
        return;
    };
    let Some(out) = world.get::<Connection>(player).map(|c| c.0.clone()) else {
        return;
    };
    let level = world.get::<Profile>(player).map_or(1, |p| p.level);
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    tokio::spawn(async move {
        let quests = match mud_db::quests::list_by_trigger_ability(&pool, ability_id).await {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "skill-trigger quest lookup failed");
                return;
            }
        };
        for q in quests {
            if q.hidden {
                continue;
            }
            grant_or_offer(&pool, &cid, &out, level, &q).await;
        }
    });
}

/// Dispatch EVENT-trigger quests when game event `event_id` fires.
/// Called by the `events::drain_events_inbox` tick on the off → on
/// edge of an `Events.active` flip — see `events.rs` for the
/// polling / edge-detection contract.
pub(crate) fn dispatch_event_trigger(world: &mut World, event_id: i32) {
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    // Snapshot every online player so the spawn doesn't have to walk
    // ECS state from the tokio task.
    let recipients: Vec<(String, mud_net::Outbound, i32)> = {
        let mut q = world.query_filtered::<(&Account, &Connection, &Profile), (With<Player>, With<Online>)>();
        q.iter(world)
            .map(|(a, c, p)| (a.character_id.clone(), c.0.clone(), p.level))
            .collect()
    };
    tokio::spawn(async move {
        let quests = match mud_db::quests::list_by_trigger_event(&pool, event_id).await {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "event-trigger quest lookup failed");
                return;
            }
        };
        if quests.is_empty() {
            return;
        }
        for (cid, out, level) in recipients {
            for q in &quests {
                if q.hidden {
                    continue;
                }
                grant_or_offer(&pool, &cid, &out, level, q).await;
            }
        }
    });
}

/// Try to advance a quest dialogue when `player` says/asks `topic`
/// to mob with prototype `(mob_zone, mob_id)`. Spawns async to
/// fetch active TALK_TO_NPC objectives targeting that mob, then
/// looks up the per-objective dialogue binding in the in-memory
/// `DialogueCatalog`. On match, the NPC's response is sent back
/// through the player's `Connection` and (if the matching response
/// completes the objective) progress is bumped via the standard
/// upsert. (Wave 4.11.)
pub(crate) fn dispatch_dialogue_attempt(
    world: &mut World,
    player: Entity,
    mob_zone: i32,
    mob_id: i32,
    topic: &str,
) {
    let Some(cid) = world.get::<Account>(player).map(|a| a.character_id.clone()) else {
        return;
    };
    let Some(out) = world.get::<Connection>(player).map(|c| c.0.clone()) else {
        return;
    };
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    // Snapshot the catalog by clone — small (one entry per
    // dialogue-bound objective, dozens at most in normal content).
    let catalog = world
        .get_resource::<crate::quest_dialogue::DialogueCatalog>()
        .cloned()
        .unwrap_or_default();
    let topic = topic.to_string();
    let player_bits = player.to_bits();
    tokio::spawn(async move {
        let rows = match mud_db::quest_objectives::list_talk_to_npc_progress(
            &pool, &cid, mob_zone, mob_id, true,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "talk-to-npc objective lookup failed for dialogue");
                return;
            }
        };
        for row in rows {
            let Some(binding) = catalog.lookup_objective(
                row.quest_zone_id,
                row.quest_id,
                row.phase_id,
                row.objective_id,
            ) else {
                continue;
            };
            // Match against the binding's keywords.
            if !crate::quest_dialogue::matches(
                &topic,
                &binding.match_type,
                &binding.match_keywords,
            ) {
                continue;
            }
            // Emit the NPC's response. If the binding has a linked
            // tree, the root's `npc_message` is the reply; future
            // utterances by this player will walk the tree (see
            // the `try_advance_dialogue` path) — that path needs
            // the active tracker, which is mutated here too via
            // a pending update.
            let reply = match binding.dialogue_tree_id {
                Some(tree_id) => catalog
                    .root_of(tree_id)
                    .map(|n| n.npc_message.clone()),
                None => Some(binding.npc_message.clone()),
            };
            if let Some(msg) = reply {
                let _ = out.try_send(format!("{msg}\r\n").into_bytes());
            }
            // Bump objective progress (one per matched dialogue).
            let new_count = (row.current_count + 1).min(row.required_count);
            let completed = new_count >= row.required_count;
            if let Err(e) = mud_db::quest_objectives::upsert_progress(
                &pool,
                &row.character_quest_id,
                row.quest_zone_id,
                row.quest_id,
                row.phase_id,
                row.objective_id,
                new_count,
                completed,
            )
            .await
            {
                tracing::warn!(error = %e, "dialogue talk-progress upsert failed");
                continue;
            }
            if completed {
                let _ = mud_db::quest_objectives::try_advance_phase(
                    &pool,
                    &row.character_quest_id,
                )
                .await;
                let _ = out.try_send(
                    format!("Quest objective complete: {}\r\n", row.player_description)
                        .into_bytes(),
                );
            }
            let _ = player_bits;
            // Only consume one dialogue per utterance; later
            // active-tree-walk depends on knowing exactly which
            // tree the player is in.
            return;
        }
    });
}

/// Dispatch AUTO-trigger quests at character creation time. Should
/// be called once per new character. (For login of an existing
/// character, AUTO quests are already on the row from prior runs.)
pub(crate) fn dispatch_auto_trigger(world: &mut World, player: Entity) {
    let Some(cid) = world.get::<Account>(player).map(|a| a.character_id.clone()) else {
        return;
    };
    let Some(out) = world.get::<Connection>(player).map(|c| c.0.clone()) else {
        return;
    };
    let level = world.get::<Profile>(player).map_or(1, |p| p.level);
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    tokio::spawn(async move {
        let quests = match mud_db::quests::list_auto_trigger(&pool).await {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "auto-trigger quest lookup failed");
                return;
            }
        };
        for q in quests {
            grant_or_offer(&pool, &cid, &out, level, &q).await;
        }
    });
}

/// Per-row dispatch: if the quest has `auto_accept = true`, run the
/// full `accept_for_player` path (which honors prereqs / level /
/// exclusive groups / cooldown). Otherwise emit a one-line "quest
/// available — type `qaccept <z> <id>` to take it" prompt.
async fn grant_or_offer(
    pool: &mud_db::sqlx::PgPool,
    cid: &str,
    out: &mud_net::Outbound,
    level: i32,
    q: &mud_db::quests::QuestRow,
) {
    // Skip if the player is already on (or has finished) this quest.
    if let Ok(Some((_id, status))) =
        mud_db::quests::find_character_quest(pool, cid, q.zone_id, q.id).await
    {
        // IN_PROGRESS / COMPLETED — nothing to do. ABANDONED rows
        // would re-eligible, but we keep auto-grant out of that
        // path so a player who abandons doesn't keep getting
        // re-grabbed by triggers.
        if status != "ABANDONED" {
            return;
        }
    }
    if q.auto_accept {
        match mud_db::quests::accept_for_player(pool, cid, level, q.zone_id, q.id).await {
            Ok(mud_db::quests::AcceptOutcome::Accepted) => {
                let line = format!(
                    "*** New quest: {} ({}, {}) — type `quests` to view. ***\r\n",
                    q.plain_name, q.zone_id, q.id
                );
                let _ = out.try_send(line.into_bytes());
            }
            Ok(_) => {
                // Refused (level, cooldown, exclusive, prereq).
                // No player-visible message; triggers shouldn't
                // nag.
            }
            Err(e) => {
                tracing::warn!(error = %e, zone = q.zone_id, id = q.id, "auto-accept failed");
            }
        }
    } else {
        let line = format!(
            "*** Quest available: {} ({}, {}) — type `qaccept {} {}` to take it. ***\r\n",
            q.plain_name, q.zone_id, q.id, q.zone_id, q.id
        );
        let _ = out.try_send(line.into_bytes());
    }
}

/// Period of the expiry / custom-lua sweep, in ticks. `TICK_HZ`
/// is 10 Hz, so 600 ticks = one minute.
const SWEEP_PERIOD_TICKS: u64 = 600;

/// Bevy-system wrapper for the periodic expiry sweep + CUSTOM_LUA
/// re-evaluation. Both fire every `SWEEP_PERIOD_TICKS` ticks.
/// Keeping them on the same cadence keeps the DB query load low
/// — most worlds will have zero or one timed quest at a time.
pub(crate) fn quest_sweep_tick(world: &mut World) {
    let tick = world.resource::<crate::TickCount>().0;
    if !tick.is_multiple_of(SWEEP_PERIOD_TICKS) {
        return;
    }
    quest_expiry_tick(world);
    quest_custom_lua_tick(world);
}

/// Tick the expiry sweeper (Wave 4.2). Scan the DB for IN_PROGRESS
/// quests past their `expires_at` and flip them to FAILED. Notify
/// online holders. Cheap when no rows match.
pub(crate) fn quest_expiry_tick(world: &mut World) {
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    // Snapshot online players for the notification map.
    let online: std::collections::HashMap<String, mud_net::Outbound> = {
        let mut q = world.query_filtered::<(&Account, &Connection), (With<Player>, With<Online>)>();
        q.iter(world)
            .map(|(a, c)| (a.character_id.clone(), c.0.clone()))
            .collect()
    };
    tokio::spawn(async move {
        let expired = match mud_db::quests::fail_expired_quests(&pool).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "expiry sweep failed");
                return;
            }
        };
        for row in expired {
            if let Some(out) = online.get(&row.character_id) {
                let line = format!(
                    "*** Quest expired: {} ({}, {}) ***\r\n",
                    row.quest_name, row.quest_zone_id, row.quest_id
                );
                let _ = out.try_send(line.into_bytes());
            }
        }
    });
}

/// Pass 1 of the CUSTOM_LUA sweep (Wave 4.5). For each online
/// player, spawn an async DB read that pushes matching rows into
/// the `CustomLuaSweepRx` channel. `quest_custom_lua_drain`
/// evaluates them next tick on the world thread.
///
/// **Strategy:** per-minute sweep (see migration plan parking lot).
/// Slow-paced objectives only ("be at full HP for 60s"); granular
/// per-action hooks are a future iteration.
pub(crate) fn quest_custom_lua_tick(world: &mut World) {
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let players: Vec<(u32, String)> = {
        let mut q = world.query_filtered::<(Entity, &Account), (With<Player>, With<Online>)>();
        q.iter(world)
            .map(|(e, a)| (e.to_bits() as u32, a.character_id.clone()))
            .collect()
    };
    // Tokio tasks can't hold `&World` to push directly into a Bevy
    // resource, so the sync inbox is an mpsc channel that the
    // drain (running on the world thread) pulls from.
    let sender = world
        .get_resource::<CustomLuaSweepSender>()
        .map(|s| s.0.clone());
    let Some(sender) = sender else {
        return;
    };
    for (bits, cid) in players {
        let pool = pool.clone();
        let sender = sender.clone();
        tokio::spawn(async move {
            let rows = match mud_db::quest_objectives::list_custom_lua_for_character(
                &pool, &cid,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "CUSTOM_LUA list failed");
                    return;
                }
            };
            for row in rows {
                let _ = sender.send((bits, row)).await;
            }
        });
    }
}

/// Sender side of the CUSTOM_LUA sweep channel. Set up at startup
/// alongside `CustomLuaSweepRx`.
#[derive(Resource, Clone)]
pub(crate) struct CustomLuaSweepSender(
    pub(crate)
        tokio::sync::mpsc::Sender<(u32, mud_db::quest_objectives::CustomLuaObjective)>,
);

/// Receiver side of the CUSTOM_LUA sweep channel.
#[derive(Resource)]
pub(crate) struct CustomLuaSweepRx(
    pub(crate)
        std::sync::Mutex<
            tokio::sync::mpsc::Receiver<(u32, mud_db::quest_objectives::CustomLuaObjective)>,
        >,
);

/// One-time setup helper. Call from main.rs after `World::new()`
/// to wire the channel + inbox.
pub(crate) fn init_resources(world: &mut World) {
    let (tx, rx) = tokio::sync::mpsc::channel::<(
        u32,
        mud_db::quest_objectives::CustomLuaObjective,
    )>(1024);
    world.insert_resource(CustomLuaSweepSender(tx));
    world.insert_resource(CustomLuaSweepRx(std::sync::Mutex::new(rx)));
    // Wire the dialogue catalog (Wave 4.11) if not present. The
    // loader fills it; default-empty here keeps test worlds happy.
    world.insert_resource(crate::quest_dialogue::DialogueCatalog::default());
    world.insert_resource(crate::quest_dialogue::ActiveQuestDialogues::default());
}

/// Per-tick Lua evaluation pass for queued CUSTOM_LUA rows
/// (Wave 4.5). Runs synchronously on the world thread so the Lua
/// host can borrow `&mut World`. Truthy expr → bump progress and
/// (when complete) advance the phase asynchronously.
pub(crate) fn quest_custom_lua_drain(world: &mut World) {
    let pending: Vec<(u32, mud_db::quest_objectives::CustomLuaObjective)> = {
        let Some(rx) = world.get_resource::<CustomLuaSweepRx>() else {
            return;
        };
        let Ok(mut rx) = rx.0.lock() else {
            return;
        };
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            out.push(msg);
        }
        out
    };
    if pending.is_empty() {
        return;
    }
    let pool = world.get_resource::<DbPool>().map(|p| p.0.clone());
    for (entity_bits, row) in pending {
        let entity = Entity::from_bits(u64::from(entity_bits));
        let body = format!("return ({})", row.lua_expression);
        let vars_text = serde_json::to_string(&row.variables)
            .unwrap_or_else(|_| "{}".to_string());
        let extras: Vec<(&str, &str)> = vec![("quest_vars_json", &vars_text)];
        let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
            host.exec_for_event_with_value(world, entity, entity, None, &body, &extras)
        });
        let truthy = match result {
            Ok((_o, Some(b))) => b,
            Ok((_o, None)) => false,
            Err(e) => {
                tracing::warn!(error = %e, expr = %row.lua_expression, "CUSTOM_LUA eval failed");
                false
            }
        };
        if !truthy {
            continue;
        }
        let Some(pool) = pool.clone() else { continue };
        let Some(out) = world.get::<Connection>(entity).map(|c| c.0.clone()) else {
            continue;
        };
        let new_count = (row.current_count + 1).min(row.required_count);
        let completed = new_count >= row.required_count;
        let cqid = row.character_quest_id.clone();
        let desc = row.player_description.clone();
        let show = row.show_progress;
        let zone = row.quest_zone_id;
        let qid = row.quest_id;
        let pid = row.phase_id;
        let oid = row.objective_id;
        let req = row.required_count;
        tokio::spawn(async move {
            if let Err(e) = mud_db::quest_objectives::upsert_progress(
                &pool, &cqid, zone, qid, pid, oid, new_count, completed,
            )
            .await
            {
                tracing::warn!(error = %e, "CUSTOM_LUA upsert failed");
                return;
            }
            if completed {
                let _ = mud_db::quest_objectives::try_advance_phase(&pool, &cqid).await;
                let _ = out.try_send(
                    format!("Quest objective complete: {desc}\r\n").into_bytes(),
                );
            } else if show {
                let _ = out.try_send(
                    format!("Quest objective: {desc} ({new_count}/{req})\r\n")
                        .into_bytes(),
                );
            }
        });
    }
}
