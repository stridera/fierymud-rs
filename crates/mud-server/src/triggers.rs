//! Lua trigger event dispatcher.
//!
//! Walks `AttachedTriggers` on an entity, filters by event flag against
//! `TriggerCatalog`, and executes each matching body via `LuaHost`.
//! After each fire, drains the `LuaOutbox` so any `room.send` calls
//! reach players in the room.
//!
//! v1 only fires `LOAD` (at mob spawn). Other events (GREET / SPEECH /
//! DEATH / FIGHT / etc.) hook in incrementally as the relevant systems
//! gain dispatch points.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_world::{
    AttachedTriggers, Located, Mob, Room, ScriptError, ScriptErrorLog, TriggerCatalog,
    TriggerEvent, WorldKey,
};
use tracing::warn;

use crate::commands::drain_lua_outbox;

/// Aggregate fire counters by event type. Per-event keys use the
/// `Debug` form ("Speech" / "Greet" / "Load" / …) so the JSON
/// surfaces match what `record_failure` already writes for errors.
/// Reset on process restart — pure runtime telemetry.
#[derive(Resource, Debug, Default)]
pub struct TriggerStats {
    pub total_fired: u64,
    pub total_succeeded: u64,
    pub total_failed: u64,
    pub by_event: HashMap<String, EventCounters>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct EventCounters {
    pub fired: u64,
    pub succeeded: u64,
    pub failed: u64,
}

/// Increment the per-event counters for one trigger fire and
/// append a per-entity history row. Inserted everywhere a script
/// body executes against `LuaHost`. The history feeds the
/// `trighistory <target>` builder command — bounded ring buffer
/// per `TriggerHistoryLog::CAP` so a chatty trigger can't bleed
/// memory.
fn record_fire(
    world: &mut World,
    listener: Entity,
    zone: i32,
    id: i32,
    event: TriggerEvent,
    ok: bool,
) {
    if !world.contains_resource::<TriggerStats>() {
        world.insert_resource(TriggerStats::default());
    }
    {
        let mut stats = world.resource_mut::<TriggerStats>();
        stats.total_fired += 1;
        if ok {
            stats.total_succeeded += 1;
        } else {
            stats.total_failed += 1;
        }
        let key = format!("{event:?}");
        let counter = stats.by_event.entry(key).or_default();
        counter.fired += 1;
        if ok {
            counter.succeeded += 1;
        } else {
            counter.failed += 1;
        }
    }

    if !world.contains_resource::<mud_world::TriggerHistoryLog>() {
        world.insert_resource(mud_world::TriggerHistoryLog::default());
    }
    let tick = world.get_resource::<crate::TickCount>().map_or(0, |t| t.0);
    world
        .resource_mut::<mud_world::TriggerHistoryLog>()
        .push(mud_world::TriggerHistoryEntry {
            at: std::time::SystemTime::now(),
            tick,
            listener,
            trigger_zone: zone,
            trigger_id: id,
            event: format!("{event:?}"),
            ok,
        });
}

/// Push a fire failure into the in-memory `ScriptErrorLog` and emit
/// the matching tracing warn. Called from every event dispatcher's
/// error arm.
fn record_failure(
    world: &mut World,
    zone: i32,
    id: i32,
    name: &str,
    event: &str,
    message: &str,
) {
    warn!(zone, id, name = %name, event = %event, error = %message, "trigger fire failed");
    if !world.contains_resource::<ScriptErrorLog>() {
        world.insert_resource(ScriptErrorLog::default());
    }
    world.resource_mut::<ScriptErrorLog>().push(ScriptError {
        at: std::time::SystemTime::now(),
        trigger_zone: zone,
        trigger_id: id,
        trigger_name: name.to_string(),
        event: event.to_string(),
        message: message.to_string(),
    });
    // Fire-and-forget DB persistence into `script_error_log`.
    // mlua errors are mostly runtime today; mark `runtime` until
    // the dispatcher learns to differentiate compile-vs-runtime.
    if let Some(pool) = world
        .get_resource::<crate::commands::DbPool>()
        .map(|p| p.0.clone())
    {
        let context = serde_json::json!({
            "trigger_name": name,
            "event": event,
        });
        let message_owned = message.to_string();
        tokio::spawn(async move {
            if let Err(e) = mud_db::script_errors::record(
                &pool,
                zone,
                id,
                "runtime",
                &message_owned,
                &context,
            )
            .await
            {
                tracing::warn!(error = %e, "script_error_log persist failed");
            }
        });
    }
}

/// Fire every trigger attached to `entity` whose flags include `event`.
/// Each fire takes a fresh `&mut World` (via `resource_scope` on
/// `LuaHost`); errors are logged at warn level — a broken trigger
/// shouldn't crash a spawn or respawn.
pub fn fire_event(world: &mut World, entity: Entity, event: TriggerEvent) {
    // Snapshot the (zone, id) keys + bodies+flags BEFORE entering
    // resource_scope. Cloning the bodies avoids re-borrowing the
    // catalog mid-execution.
    let to_fire: Vec<(i32, i32, String, String)> = {
        let Some(at) = world.get::<AttachedTriggers>(entity) else {
            return;
        };
        let keys = at.0.clone();
        let catalog = world.resource::<TriggerCatalog>();
        keys.into_iter()
            .filter_map(|(zone, id)| {
                let def = catalog.by_key.get(&(zone, id))?;
                if def.flags.contains(&event) {
                    Some((zone, id, def.name.clone(), def.commands.clone()))
                } else {
                    None
                }
            })
            .collect()
    };

    if to_fire.is_empty() {
        return;
    }

    for (zone, id, name, body) in to_fire {
        let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
            host.exec_for_actor(world, entity, &body)
        });
        drain_lua_outbox(world);
        record_fire(world, entity, zone, id, event, result.is_ok());
        if let Err(e) = result {
            record_failure(world, zone, id, &name, &format!("{event:?}"), &e);
        }
    }
}

/// Fire SPEECH-flagged triggers for every entity in `room` (other
/// than the speaker themselves) that carries `AttachedTriggers`.
/// Each fire binds `speech` (the spoken text, lowercased) as a Lua
/// global so trigger bodies can keyword-match against it.
///
/// SPEECH bodies do their own keyword filtering — the dispatcher
/// fires every SPEECH trigger and lets the body decide whether to
/// react. ~6900 corpus refs across `SPEECH`/`SPEECH_TO` triggers.
/// Fire SPEECH-flagged triggers on a single `listener` (vs the
/// whole room). Used by `ask <mob> <topic>` to address one NPC
/// without inviting every adjacent mob to chime in. `actor`
/// binds to the speaker; `speech` (lowercased) carries the
/// keyword.
pub fn fire_speech_at(world: &mut World, listener: Entity, speaker: Entity, text: &str) {
    let to_fire: Vec<(i32, i32, String, String)> = {
        let Some(at) = world.get::<AttachedTriggers>(listener) else {
            return;
        };
        let keys = at.0.clone();
        let catalog = world.resource::<TriggerCatalog>();
        keys.into_iter()
            .filter_map(|(zone, id)| {
                let def = catalog.by_key.get(&(zone, id))?;
                if def.flags.contains(&TriggerEvent::Speech) {
                    Some((zone, id, def.name.clone(), def.commands.clone()))
                } else {
                    None
                }
            })
            .collect()
    };
    if to_fire.is_empty() {
        return;
    }
    let lowered = text.to_ascii_lowercase();
    for (zone, id, name, body) in to_fire {
        let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
            host.exec_for_listener_with_extras(
                world,
                listener,
                speaker,
                &body,
                &[("speech", &lowered)],
            )
        });
        drain_lua_outbox(world);
        record_fire(world, listener, zone, id, TriggerEvent::Speech, result.is_ok());
        if let Err(e) = result {
            record_failure(world, zone, id, &name, "SPEECH", &e);
        }
    }
}

pub fn fire_speech_in_room(world: &mut World, speaker: Entity, room: Entity, text: &str) {
    let listeners: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located, &AttachedTriggers)>();
        q.iter(world)
            .filter(|(e, l, _)| *e != speaker && l.0 == room)
            .map(|(e, _, _)| e)
            .collect()
    };
    if listeners.is_empty() {
        return;
    }
    let lowered = text.to_ascii_lowercase();
    for listener in listeners {
        let to_fire: Vec<(i32, i32, String, String)> = {
            let Some(at) = world.get::<AttachedTriggers>(listener) else {
                continue;
            };
            let keys = at.0.clone();
            let catalog = world.resource::<TriggerCatalog>();
            keys.into_iter()
                .filter_map(|(zone, id)| {
                    let def = catalog.by_key.get(&(zone, id))?;
                    if def.flags.contains(&TriggerEvent::Speech) {
                        Some((zone, id, def.name.clone(), def.commands.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        };
        for (zone, id, name, body) in to_fire {
            let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
                host.exec_for_actor_with_extras(
                    world,
                    listener,
                    &body,
                    &[("speech", &lowered)],
                )
            });
            drain_lua_outbox(world);
            record_fire(world, listener, zone, id, TriggerEvent::Speech, result.is_ok());
            if let Err(e) = result {
                record_failure(world, zone, id, &name, "SPEECH", &e);
            }
        }
    }
}

/// Fire `PREENTRY` / `POSTENTRY` triggers attached to `room`. Both
/// fire from the room's perspective (`self` = room) with `actor`
/// bound to the entering player. PREENTRY fires before the player's
/// `Located` is changed; POSTENTRY fires after.
pub fn fire_room_entry(world: &mut World, room: Entity, entering: Entity, event: TriggerEvent) {
    let to_fire: Vec<(i32, i32, String, String)> = {
        let Some(at) = world.get::<AttachedTriggers>(room) else {
            return;
        };
        let keys = at.0.clone();
        let catalog = world.resource::<TriggerCatalog>();
        keys.into_iter()
            .filter_map(|(zone, id)| {
                let def = catalog.by_key.get(&(zone, id))?;
                if def.flags.contains(&event) {
                    Some((zone, id, def.name.clone(), def.commands.clone()))
                } else {
                    None
                }
            })
            .collect()
    };
    if to_fire.is_empty() {
        return;
    }
    for (zone, id, name, body) in to_fire {
        let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
            host.exec_for_listener_with_extras(world, room, entering, &body, &[])
        });
        drain_lua_outbox(world);
        record_fire(world, room, zone, id, event, result.is_ok());
        if let Err(e) = result {
            record_failure(world, zone, id, &name, &format!("{event:?}"), &e);
        }
    }
}

/// Fire `GREET` / `GREET_ALL` triggers for every entity in `room`
/// (other than the entering actor) that carries `AttachedTriggers`.
/// Used by the movement system after a player arrives in a new
/// room. Each fire binds `self` to the listener and `actor` to the
/// entering player.
pub fn fire_greet_in_room(world: &mut World, entering: Entity, room: Entity) {
    let listeners: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located, &AttachedTriggers)>();
        q.iter(world)
            .filter(|(e, l, _)| *e != entering && l.0 == room)
            .map(|(e, _, _)| e)
            .collect()
    };
    if listeners.is_empty() {
        return;
    }
    for listener in listeners {
        let to_fire: Vec<(i32, i32, String, String)> = {
            let Some(at) = world.get::<AttachedTriggers>(listener) else {
                continue;
            };
            let keys = at.0.clone();
            let catalog = world.resource::<TriggerCatalog>();
            keys.into_iter()
                .filter_map(|(zone, id)| {
                    let def = catalog.by_key.get(&(zone, id))?;
                    if def.flags.contains(&TriggerEvent::Greet)
                        || def.flags.contains(&TriggerEvent::GreetAll)
                    {
                        Some((zone, id, def.name.clone(), def.commands.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        };
        for (zone, id, name, body) in to_fire {
            let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
                host.exec_for_listener_with_extras(world, listener, entering, &body, &[])
            });
            drain_lua_outbox(world);
            record_fire(world, listener, zone, id, TriggerEvent::Greet, result.is_ok());
            if let Err(e) = result {
                record_failure(world, zone, id, &name, "GREET", &e);
            }
        }
    }
}

/// Fire an `event`-flagged trigger on `item` (an Item entity) with
/// `self` bound to the item and `actor` bound to the acting player.
/// Used by GET / DROP / WEAR / REMOVE / USE / CONSUME — every
/// object-attached event whose dispatch shape is "the item observed
/// the actor doing X to it."
pub fn fire_item_event(
    world: &mut World,
    item: Entity,
    actor: Entity,
    event: TriggerEvent,
) {
    let to_fire: Vec<(i32, i32, String, String)> = {
        let Some(at) = world.get::<AttachedTriggers>(item) else {
            return;
        };
        let keys = at.0.clone();
        let catalog = world.resource::<TriggerCatalog>();
        keys.into_iter()
            .filter_map(|(zone, id)| {
                let def = catalog.by_key.get(&(zone, id))?;
                if def.flags.contains(&event) {
                    Some((zone, id, def.name.clone(), def.commands.clone()))
                } else {
                    None
                }
            })
            .collect()
    };
    if to_fire.is_empty() {
        return;
    }
    for (zone, id, name, body) in to_fire {
        let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
            host.exec_for_listener_with_extras(world, item, actor, &body, &[])
        });
        drain_lua_outbox(world);
        record_fire(world, item, zone, id, event, result.is_ok());
        if let Err(e) = result {
            record_failure(world, zone, id, &name, &format!("{event:?}"), &e);
        }
    }
}

/// Fire `event`-flagged triggers on `listener` with a separate
/// `actor` binding for the acting entity. Used by FIGHT / ATTACK
/// where the listener is the target and the actor is the attacker.
pub fn fire_event_with_actor(
    world: &mut World,
    listener: Entity,
    acting: Entity,
    event: TriggerEvent,
) {
    let to_fire: Vec<(i32, i32, String, String)> = {
        let Some(at) = world.get::<AttachedTriggers>(listener) else {
            return;
        };
        let keys = at.0.clone();
        let catalog = world.resource::<TriggerCatalog>();
        keys.into_iter()
            .filter_map(|(zone, id)| {
                let def = catalog.by_key.get(&(zone, id))?;
                if def.flags.contains(&event) {
                    Some((zone, id, def.name.clone(), def.commands.clone()))
                } else {
                    None
                }
            })
            .collect()
    };
    if to_fire.is_empty() {
        return;
    }
    for (zone, id, name, body) in to_fire {
        let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
            host.exec_for_listener_with_extras(world, listener, acting, &body, &[])
        });
        drain_lua_outbox(world);
        record_fire(world, listener, zone, id, event, result.is_ok());
        if let Err(e) = result {
            record_failure(world, zone, id, &name, &format!("{event:?}"), &e);
        }
    }
}

/// Fire `RECEIVE`-flagged triggers on `recipient` when `giver` hands
/// them `item`. Each fire binds `self` to recipient, `actor` to giver,
/// `object` to the item. RECEIVE bodies typically inspect `object.id`
/// to handle quest item turn-ins.
pub fn fire_receive(world: &mut World, recipient: Entity, giver: Entity, item: Entity) {
    let to_fire: Vec<(i32, i32, String, String)> = {
        let Some(at) = world.get::<AttachedTriggers>(recipient) else {
            return;
        };
        let keys = at.0.clone();
        let catalog = world.resource::<TriggerCatalog>();
        keys.into_iter()
            .filter_map(|(zone, id)| {
                let def = catalog.by_key.get(&(zone, id))?;
                if def.flags.contains(&TriggerEvent::Receive) {
                    Some((zone, id, def.name.clone(), def.commands.clone()))
                } else {
                    None
                }
            })
            .collect()
    };
    if to_fire.is_empty() {
        return;
    }
    for (zone, id, name, body) in to_fire {
        let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
            host.exec_for_event(world, recipient, giver, Some(item), &body, &[])
        });
        drain_lua_outbox(world);
        record_fire(world, recipient, zone, id, TriggerEvent::Receive, result.is_ok());
        if let Err(e) = result {
            record_failure(world, zone, id, &name, "RECEIVE", &e);
        }
    }
}

/// Fire `COMMAND`-flagged triggers for every entity in the player's
/// room (skipping the player themselves) that carries
/// `AttachedTriggers`. Each fire binds `cmd` (command word) and
/// `args` (rest of input) as Lua globals. Returns `true` if any
/// trigger explicitly returned `false`, signaling the caller to
/// stop dispatch (the command was consumed by the trigger).
pub fn fire_command_in_room(
    world: &mut World,
    player: Entity,
    room: Entity,
    cmd: &str,
    args: &str,
) -> bool {
    let listeners: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located, &AttachedTriggers)>();
        q.iter(world)
            .filter(|(e, l, _)| *e != player && l.0 == room)
            .map(|(e, _, _)| e)
            .collect()
    };
    if listeners.is_empty() {
        return false;
    }
    let mut consumed = false;
    for listener in listeners {
        let to_fire: Vec<(i32, i32, String, String)> = {
            let Some(at) = world.get::<AttachedTriggers>(listener) else {
                continue;
            };
            let keys = at.0.clone();
            let catalog = world.resource::<TriggerCatalog>();
            keys.into_iter()
                .filter_map(|(zone, id)| {
                    let def = catalog.by_key.get(&(zone, id))?;
                    if def.flags.contains(&TriggerEvent::Command) {
                        Some((zone, id, def.name.clone(), def.commands.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        };
        for (zone, id, name, body) in to_fire {
            let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
                host.exec_for_event_with_value(
                    world,
                    listener,
                    player,
                    None,
                    &body,
                    &[("cmd", cmd), ("args", args)],
                )
            });
            drain_lua_outbox(world);
            record_fire(world, listener, zone, id, TriggerEvent::Command, result.is_ok());
            match result {
                Ok((_out, Some(false))) => {
                    consumed = true;
                }
                Ok(_) => {}
                Err(e) => record_failure(world, zone, id, &name, "COMMAND", &e),
            }
        }
        if consumed {
            break;
        }
    }
    consumed
}

/// Bulk-fire `LOAD` triggers for every Mob in the world that carries
/// `AttachedTriggers`. Used once at boot after `load_from_db` so
/// proto-attached mob triggers (e.g. `skills.set_level`) run before
/// the first player connects.
/// Counts surfaced after a `treload` / admin reload so callers can
/// format a status message. All zero is a valid response — empty
/// catalog, no rooms refreshed.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReloadStats {
    pub total: usize,
    pub mob_links: usize,
    pub object_links: usize,
    pub room_links: usize,
    pub rooms_with_triggers: usize,
}

/// Atomically swap the world's `TriggerCatalog` resource for the
/// new one and refresh every `Room` entity's `AttachedTriggers`
/// from the new catalog's `room_attachments` map. Mob/object
/// instance attachments stay put — next respawn picks up catalog
/// edits on those naturally.
///
/// Centralized here so the HTTP admin endpoint and the in-game
/// `treload` command share one path; neither has to redo the
/// per-room rewire.
pub fn apply_reloaded_catalog(world: &mut World, new: TriggerCatalog) -> ReloadStats {
    let mut stats = ReloadStats {
        total: new.by_key.len(),
        mob_links: new.mob_attachments.len(),
        object_links: new.object_attachments.len(),
        room_links: new.room_attachments.len(),
        rooms_with_triggers: 0,
    };

    let rooms_to_refresh: Vec<(Entity, (i32, i32))> = {
        let mut q = world.query_filtered::<(Entity, &WorldKey), With<Room>>();
        q.iter(world).map(|(e, k)| (e, (k.zone, k.id))).collect()
    };
    for (room, key) in rooms_to_refresh {
        let attached = new.room_attachments.get(&key).cloned();
        if let Ok(mut em) = world.get_entity_mut(room) {
            em.remove::<AttachedTriggers>();
            if let Some(list) = attached
                && !list.is_empty()
            {
                em.insert(AttachedTriggers(list));
                stats.rooms_with_triggers += 1;
            }
        }
    }

    world.insert_resource(new);
    tracing::info!(
        total = stats.total,
        mob_links = stats.mob_links,
        object_links = stats.object_links,
        room_links = stats.room_links,
        rooms_with_triggers = stats.rooms_with_triggers,
        "trigger catalog reloaded",
    );
    stats
}

pub fn fire_load_for_all_mobs(world: &mut World) {
    let mobs: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Mob>, With<AttachedTriggers>)>();
        q.iter(world).collect()
    };
    let count = mobs.len();
    for e in mobs {
        fire_event(world, e, TriggerEvent::Load);
    }
    tracing::info!(mobs = count, "fired LOAD triggers for spawned mobs");
}

/// Tick system: advance the `LuaHost`'s view of the current tick, then
/// resume any parked threads whose `wait(N)` deadline has passed.
/// `LuaOutbox` is drained inline after the resume pass since
/// resumed bodies may emit `actor:send` / `room.send` lines.
pub fn lua_coroutine_tick(world: &mut World) {
    let tick = world.resource::<crate::TickCount>().0;
    let (resumed, parked) = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
        host.set_current_tick(tick);
        let n = host.tick_yielded(world);
        (n, host.yielded_count())
    });
    if resumed > 0 {
        crate::commands::drain_lua_outbox(world);
        tracing::info!(resumed, parked, "lua_coroutine_tick resumed parked threads");
    }
}
