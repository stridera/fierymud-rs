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

/// Look quests up in the background and hand the visible ones to the
/// world thread as trigger candidates. The world thread decides what
/// the character may actually be offered ([`offer_candidates`]): that
/// needs the Lua host for availability requirements.
fn spawn_candidate_lookup<F, Fut>(world: &World, player: Entity, what: &'static str, lookup: F)
where
    F: FnOnce(mud_db::sqlx::PgPool) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = mud_db::sqlx::Result<Vec<mud_db::quests::QuestRow>>>
        + Send
        + 'static,
{
    let Some(cid) = world.get::<Account>(player).map(|a| a.character_id.clone()) else {
        return;
    };
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let Some(tx) = world
        .get_resource::<crate::commands::PlayerUpdateTx>()
        .map(|t| t.0.clone())
    else {
        return;
    };
    tokio::spawn(async move {
        let quests = match lookup(pool).await {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "{what} quest lookup failed");
                return;
            }
        };
        let quests: Vec<_> = quests.into_iter().filter(|q| !q.hidden).collect();
        if quests.is_empty() {
            return;
        }
        let _ = tx
            .send(crate::commands::PendingPlayerUpdate::TriggerCandidates {
                character_id: cid,
                quests,
            })
            .await;
    });
}

/// World-thread half of every trigger: gate the candidate quests by
/// their availability requirement (the same check `qaccept` runs;
/// fails closed on a script error), then offer or auto-accept the
/// survivors in the background.
pub(crate) fn offer_candidates(
    world: &mut World,
    player: Entity,
    quests: Vec<mud_db::quests::QuestRow>,
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
    let level = world.get::<Profile>(player).map_or(1, |p| p.level);
    let update_tx = world
        .get_resource::<crate::commands::PlayerUpdateTx>()
        .map(|t| t.0.clone());
    let mut allowed = Vec::new();
    for q in quests {
        if q.hidden {
            continue;
        }
        if let Some(expr) = q
            .availability_requirement
            .as_deref()
            .filter(|e| !e.trim().is_empty())
            && !crate::commands::quests::eval_quest_availability(
                world,
                player,
                expr,
                &format!(
                    "quest ({}, {}) availability requirement (trigger)",
                    q.zone_id, q.id
                ),
            )
        {
            continue;
        }
        allowed.push(q);
    }
    if allowed.is_empty() {
        return;
    }
    tokio::spawn(async move {
        for q in &allowed {
            grant_or_offer(&pool, &cid, &out, level, q, update_tx.as_ref()).await;
        }
    });
}

/// Dispatch LEVEL-trigger quests for `player` at the moment their
/// `Profile.level` becomes `new_level`. Fired from
/// `combat::check_level_up` after the level field is bumped.
pub(crate) fn dispatch_level_trigger(world: &mut World, player: Entity, new_level: i32) {
    spawn_candidate_lookup(world, player, "level-trigger", move |pool| async move {
        mud_db::quests::list_by_trigger_level(&pool, new_level).await
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
    spawn_candidate_lookup(world, player, "item-trigger", move |pool| async move {
        mud_db::quests::list_by_trigger_item(&pool, item_zone, item_id).await
    });
}

/// Dispatch ROOM-trigger quests when `player` enters a room with
/// prototype `(room_zone, room_id)`. Fired from `note_room_entry` on every entry.
pub(crate) fn dispatch_room_trigger(
    world: &mut World,
    player: Entity,
    room_zone: i32,
    room_id: i32,
) {
    spawn_candidate_lookup(world, player, "room-trigger", move |pool| async move {
        mud_db::quests::list_by_trigger_room(&pool, room_zone, room_id).await
    });
}

/// Dispatch SKILL-trigger quests when `player` first successfully
/// uses ability `ability_id`. Fired from `bump_use_skill_quest_progress`'s
/// caller.
pub(crate) fn dispatch_skill_trigger(world: &mut World, player: Entity, ability_id: i32) {
    spawn_candidate_lookup(world, player, "skill-trigger", move |pool| async move {
        mud_db::quests::list_by_trigger_ability(&pool, ability_id).await
    });
}

/// Dispatch EVENT-trigger quests when game event `event_id` fires.
/// Called by the `events::drain_events_inbox` tick on the off → on
/// edge of an `Events.active` flip — see `events.rs` for the
/// polling / edge-detection contract.
pub(crate) fn dispatch_event_trigger(world: &mut World, event_id: i32) {
    let players: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world).collect()
    };
    for player in players {
        spawn_candidate_lookup(world, player, "event-trigger", move |pool| async move {
            mud_db::quests::list_by_trigger_event(&pool, event_id).await
        });
    }
}

/// Dispatch AUTO-trigger quests at login. Quests the character already
/// holds are skipped per row.
pub(crate) fn dispatch_auto_trigger(world: &mut World, player: Entity) {
    spawn_candidate_lookup(world, player, "auto-trigger", |pool| async move {
        mud_db::quests::list_auto_trigger(&pool).await
    });
}

/// Per-row dispatch: if the quest has `auto_accept = true`, run the
/// full `accept_for_player` path (which honors prereqs / level /
/// exclusive groups / cooldown). Otherwise emit a one-line "quest
/// available — type `qaccept <z> <id>` to take it" prompt.
pub(crate) async fn grant_or_offer(
    pool: &mud_db::sqlx::PgPool,
    cid: &str,
    out: &mud_net::Outbound,
    level: i32,
    q: &mud_db::quests::QuestRow,
    update_tx: Option<&tokio::sync::mpsc::Sender<crate::commands::PendingPlayerUpdate>>,
) {
    // A trigger only ever starts a quest from scratch: skip when the
    // character already has any record of it (in progress, completed,
    // failed, or abandoned - a player who abandons must not be
    // re-grabbed by the next trigger). Re-taking such a quest is a
    // deliberate `qaccept`.
    match mud_db::quests::find_character_quest(pool, cid, q.zone_id, q.id).await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(error = %e, zone = q.zone_id, id = q.id, "quest record lookup failed");
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
                // Reset cached variables and credit carried items.
                if let Some(tx) = update_tx {
                    let _ = tx
                        .send(crate::commands::PendingPlayerUpdate::QuestAccepted {
                            character_id: cid.to_string(),
                            quest_zone: q.zone_id,
                            quest_id: q.id,
                        })
                        .await;
                }
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
    let players = online_players(world);
    // Tokio tasks can't hold `&World` to push directly into a Bevy
    // resource, so the sync inbox is an mpsc channel that the
    // drain (running on the world thread) pulls from.
    let sender = world
        .get_resource::<CustomLuaSweepSender>()
        .map(|s| s.0.clone());
    let Some(sender) = sender else {
        return;
    };
    for (entity, cid) in players {
        let pool = pool.clone();
        let sender = sender.clone();
        tokio::spawn(async move {
            let rows =
                match mud_db::quest_objectives::list_custom_lua_for_character(&pool, &cid).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "CUSTOM_LUA list failed");
                        return;
                    }
                };
            for row in rows {
                let _ = sender.send((entity, row)).await;
            }
        });
    }
}

/// `(entity, character_id)` for every online player. Carries the full
/// `Entity` (index + generation) — truncating to 32 bits would drop the
/// generation and resolve to a stale/wrong entity after slot reuse.
fn online_players(world: &mut World) -> Vec<(Entity, String)> {
    let mut q = world.query_filtered::<(Entity, &Account), (With<Player>, With<Online>)>();
    q.iter(world)
        .map(|(e, a)| (e, a.character_id.clone()))
        .collect()
}

/// Sender side of the CUSTOM_LUA sweep channel. Set up at startup
/// alongside `CustomLuaSweepRx`.
#[derive(Resource, Clone)]
pub(crate) struct CustomLuaSweepSender(
    pub(crate) tokio::sync::mpsc::Sender<(Entity, mud_db::quest_objectives::CustomLuaObjective)>,
);

/// Receiver side of the CUSTOM_LUA sweep channel.
#[derive(Resource)]
pub(crate) struct CustomLuaSweepRx(
    pub(crate)  std::sync::Mutex<
        tokio::sync::mpsc::Receiver<(Entity, mud_db::quest_objectives::CustomLuaObjective)>,
    >,
);

/// One-time setup helper. Call from main.rs after `World::new()`
/// to wire the channel + inbox.
pub(crate) fn init_resources(world: &mut World) {
    let (tx, rx) =
        tokio::sync::mpsc::channel::<(Entity, mud_db::quest_objectives::CustomLuaObjective)>(1024);
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
    let pending: Vec<(Entity, mud_db::quest_objectives::CustomLuaObjective)> = {
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
    for (entity, row) in pending {
        // The sweep carries the full `Entity` (index + generation). If
        // the player despawned between sweep and drain, the generation
        // no longer matches and this skips instead of hitting a reused
        // slot.
        if world.get::<Account>(entity).is_none() {
            continue;
        }
        let body = format!("return ({})", row.lua_expression);
        let vars_text = serde_json::to_string(&row.variables).unwrap_or_else(|_| "{}".to_string());
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
        let Some(notify) = crate::quest_progress::Notifier::for_player(world, entity) else {
            continue;
        };
        let obj = crate::quest_progress::ObjectiveRef::from(&row);
        tokio::spawn(async move {
            crate::quest_progress::record_progress(&pool, &notify, &obj, "").await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::Account;

    fn account(cid: &str) -> Account {
        Account {
            user_id: "u".to_string(),
            character_id: cid.to_string(),
            role: mud_db::enums::UserRole::Player,
            account_role: mud_db::enums::UserRole::Player,
            perms: Vec::new(),
        }
    }

    fn objective(cid: &str) -> mud_db::quest_objectives::CustomLuaObjective {
        mud_db::quest_objectives::CustomLuaObjective {
            character_id: cid.to_string(),
            character_quest_id: "cq".to_string(),
            quest_zone_id: 1,
            quest_id: 1,
            phase_id: 1,
            objective_id: 1,
            required_count: 1,
            current_count: 0,
            lua_expression: "true".to_string(),
            player_description: "d".to_string(),
            show_progress: false,
            variables: serde_json::Value::Null,
        }
    }

    /// Spawn + despawn until a slot is reused with generation > 0, then
    /// return a live online player occupying it.
    fn reused_slot_player(world: &mut World) -> Entity {
        let first = world.spawn_empty().id();
        world.despawn(first);
        let reused = world.spawn((Player, Online, account("reused"))).id();
        assert_eq!(reused.index(), first.index(), "slot should be reused");
        assert_ne!(reused.generation(), first.generation());
        assert!(
            reused.to_bits() > u64::from(u32::MAX),
            "generation in high bits"
        );
        reused
    }

    #[test]
    fn online_players_keeps_generation() {
        let mut world = World::new();
        let reused = reused_slot_player(&mut world);
        let players = online_players(&mut world);
        assert_eq!(players.len(), 1);
        assert_eq!(players[0].0, reused);
        // The old truncating round-trip pointed at a different entity.
        #[allow(clippy::cast_possible_truncation)]
        let truncated = Entity::from_bits(u64::from(reused.to_bits() as u32));
        assert_ne!(truncated, reused);
        assert!(world.get::<Account>(truncated).is_none());
    }

    #[test]
    fn sweep_channel_round_trip_resolves_correct_entity() {
        let mut world = World::new();
        init_resources(&mut world);
        let reused = reused_slot_player(&mut world);
        let tx = world.resource::<CustomLuaSweepSender>().0.clone();
        tx.try_send((reused, objective("reused"))).unwrap();
        let received = {
            let rx = world.resource::<CustomLuaSweepRx>();
            let mut rx = rx.0.lock().unwrap();
            rx.try_recv().unwrap()
        };
        assert_eq!(received.0, reused);
        assert_eq!(
            world
                .get::<Account>(received.0)
                .map(|a| a.character_id.as_str()),
            Some("reused")
        );
    }
}
