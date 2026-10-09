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
    AttachedTriggers, DeferredRoomTriggerFire, DeferredRoomTriggerFires, Located, Mob, Room,
    ScriptError, ScriptErrorLog, TriggerCatalog, TriggerEvent, WorldKey, WorldKeyIndex,
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
fn record_failure(world: &mut World, zone: i32, id: i32, name: &str, event: &str, message: &str) {
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
            if let Err(e) =
                mud_db::script_errors::record(&pool, zone, id, "runtime", &message_owned, &context)
                    .await
            {
                tracing::warn!(error = %e, "script_error_log persist failed");
            }
        });
    }
}

/// Run `f` with the mob sleep / casting gate selected for `event`.
/// Legacy `script_driver` exempts only DEATH triggers; every other
/// mob trigger is aborted while the mob sleeps and paused while it
/// casts (legacy 54a61ba6). Room / object listeners are unaffected
/// (the host only gates `Mob` entities).
fn gated<R>(
    host: &mut mud_script::LuaHost,
    event: TriggerEvent,
    f: impl FnOnce(&mut mud_script::LuaHost) -> R,
) -> R {
    host.set_gate(if event == TriggerEvent::Death {
        mud_script::ScriptGate::Off
    } else {
        mud_script::ScriptGate::Mob
    });
    let out = f(host);
    host.set_gate(mud_script::ScriptGate::Off);
    out
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
            gated(&mut host, event, |h| h.exec_for_actor(world, entity, &body))
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
            gated(&mut host, TriggerEvent::Speech, |h| {
                h.exec_for_listener_with_extras(
                    world,
                    listener,
                    speaker,
                    &body,
                    &[("speech", &lowered)],
                )
            })
        });
        drain_lua_outbox(world);
        record_fire(
            world,
            listener,
            zone,
            id,
            TriggerEvent::Speech,
            result.is_ok(),
        );
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
                gated(&mut host, TriggerEvent::Speech, |h| {
                    h.exec_for_actor_with_extras(world, listener, &body, &[("speech", &lowered)])
                })
            });
            drain_lua_outbox(world);
            record_fire(
                world,
                listener,
                zone,
                id,
                TriggerEvent::Speech,
                result.is_ok(),
            );
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
                gated(&mut host, TriggerEvent::Greet, |h| {
                    h.exec_for_listener_with_extras(world, listener, entering, &body, &[])
                })
            });
            drain_lua_outbox(world);
            record_fire(
                world,
                listener,
                zone,
                id,
                TriggerEvent::Greet,
                result.is_ok(),
            );
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
pub fn fire_item_event(world: &mut World, item: Entity, actor: Entity, event: TriggerEvent) {
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
            gated(&mut host, event, |h| {
                h.exec_for_listener_with_extras(world, listener, acting, &body, &[])
            })
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
            gated(&mut host, TriggerEvent::Receive, |h| {
                h.exec_for_event(world, recipient, giver, Some(item), &body, &[])
            })
        });
        drain_lua_outbox(world);
        record_fire(
            world,
            recipient,
            zone,
            id,
            TriggerEvent::Receive,
            result.is_ok(),
        );
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
                gated(&mut host, TriggerEvent::Command, |h| {
                    h.exec_for_event_with_value(
                        world,
                        listener,
                        player,
                        None,
                        &body,
                        &[("cmd", cmd), ("args", args)],
                    )
                })
            });
            drain_lua_outbox(world);
            record_fire(
                world,
                listener,
                zone,
                id,
                TriggerEvent::Command,
                result.is_ok(),
            );
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

/// Fire every trigger attached to `room` with `self = room` and
/// `actor = caller` (falling back to `room` when the caller is
/// gone). Used by the deferred-fire drain to honor a Lua
/// `run_room_trigger(zone, id)` call from another script.
///
/// Unlike `fire_event`, we do NOT filter by event flag — the
/// legacy DG-Script `run_room_trigger` invokes the room's
/// script unconditionally, and the corpus's target triggers
/// (zones 117 / 123 / 163 / 185) are authored with probability
/// 0% so they never fire on their own; `run_room_trigger` is
/// the only entry that wakes them. Filtering by flag would
/// require choosing one (PREENTRY? RANDOM?) and miss the
/// others.
fn fire_all_room_triggers(world: &mut World, room: Entity, caller: Option<Entity>) {
    let to_fire: Vec<(i32, i32, String, String)> = {
        let Some(at) = world.get::<AttachedTriggers>(room) else {
            return;
        };
        let keys = at.0.clone();
        let catalog = world.resource::<TriggerCatalog>();
        keys.into_iter()
            .filter_map(|(zone, id)| {
                let def = catalog.by_key.get(&(zone, id))?;
                Some((zone, id, def.name.clone(), def.commands.clone()))
            })
            .collect()
    };
    if to_fire.is_empty() {
        return;
    }
    // If the caller is gone (despawned between enqueue and drain),
    // bind `actor` to the room itself — matches legacy "no actor"
    // semantics and keeps the body from seeing a stale entity.
    let acting = caller
        .filter(|&e| world.get_entity(e).is_ok())
        .unwrap_or(room);
    for (zone, id, name, body) in to_fire {
        let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
            host.exec_for_listener_with_extras(world, room, acting, &body, &[])
        });
        drain_lua_outbox(world);
        // Pick a representative event for the history row — RANDOM
        // is the closest legacy match for "fired manually with no
        // particular event context."
        record_fire(world, room, zone, id, TriggerEvent::Random, result.is_ok());
        if let Err(e) = result {
            record_failure(world, zone, id, &name, "RUN_ROOM_TRIGGER", &e);
        }
    }
}

/// Drain the `DeferredRoomTriggerFires` queue, firing each entry
/// against the target room. Called from `lua_coroutine_tick` so
/// the drain happens once per world tick, outside any Lua frame
/// (no re-entrancy hazard).
///
/// Entries whose target room doesn't resolve via `WorldKeyIndex`
/// are silently dropped — same shape as `get_room(zone, id)`
/// returning nil, which the corpus already tolerates.
pub fn drain_deferred_room_triggers(world: &mut World) {
    let pending: Vec<DeferredRoomTriggerFire> = {
        let Some(mut q) = world.get_resource_mut::<DeferredRoomTriggerFires>() else {
            return;
        };
        std::mem::take(&mut q.queue)
    };
    if pending.is_empty() {
        return;
    }
    for entry in pending {
        let key = (entry.room_zone, entry.room_id);
        let room_entity = world
            .get_resource::<WorldKeyIndex>()
            .and_then(|idx| idx.rooms.get(&key).copied());
        let Some(room_entity) = room_entity else {
            tracing::warn!(
                zone = entry.room_zone,
                id = entry.room_id,
                "run_room_trigger target room not found in WorldKeyIndex"
            );
            continue;
        };
        fire_all_room_triggers(world, room_entity, entry.caller);
    }
}

/// Tick system: advance the `LuaHost`'s view of the current tick, then
/// resume any parked threads whose `wait(N)` deadline has passed.
/// `LuaOutbox` is drained inline after the resume pass since
/// resumed bodies may emit `actor:send` / `room.send` lines.
///
/// After the resume pass we drain `DeferredRoomTriggerFires` —
/// any `run_room_trigger(zone, id)` calls made from triggers
/// during the tick get fired now, outside any Lua frame.
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
    drain_deferred_room_triggers(world);
}

/// One trigger whose body failed to compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptFailure {
    pub zone_id: i32,
    pub id: i32,
    pub name: String,
    pub error: String,
}

/// Result of a syntax-only pass over the trigger catalog.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScriptValidation {
    pub total: usize,
    pub failures: Vec<ScriptFailure>,
}

/// Compile-check every trigger body in `catalog` (optionally limited to one
/// zone) without running it. Uses a throwaway Lua state, so no trigger side
/// effects and no interaction with the live `LuaHost`. Failures are sorted by
/// `(zone, id)`.
#[must_use]
pub fn validate_catalog(catalog: &TriggerCatalog, zone: Option<i32>) -> ScriptValidation {
    let lua = mlua::Lua::new();
    let mut keys: Vec<&(i32, i32)> = catalog
        .by_key
        .keys()
        .filter(|(z, _)| zone.is_none_or(|only| only == *z))
        .collect();
    keys.sort();
    let mut out = ScriptValidation {
        total: keys.len(),
        failures: Vec::new(),
    };
    for key in keys {
        let def = &catalog.by_key[key];
        let chunk_name = format!("={}:{}", key.0, key.1);
        if let Err(e) = lua
            .load(def.commands.as_str())
            .set_name(chunk_name)
            .into_function()
        {
            out.failures.push(ScriptFailure {
                zone_id: key.0,
                id: key.1,
                name: def.name.clone(),
                error: e.to_string(),
            });
        }
    }
    out
}

#[cfg(test)]
mod validate_tests {
    use super::*;
    use mud_world::{TriggerAttach, TriggerDef};

    fn def(zone: i32, id: i32, body: &str) -> TriggerDef {
        TriggerDef {
            zone_id: zone,
            id,
            name: format!("t{zone}_{id}"),
            attach_type: TriggerAttach::Mob,
            commands: body.to_string(),
            flags: vec![],
            arg_list: vec![],
            num_args: 0,
        }
    }

    #[test]
    fn validate_catalog_reports_only_syntax_failures() {
        let mut cat = TriggerCatalog::default();
        cat.by_key
            .insert((1, 1), def(1, 1, "local x = 1\nreturn x"));
        cat.by_key.insert((1, 2), def(1, 2, "if then end end"));
        cat.by_key.insert((2, 1), def(2, 1, "x = = 2"));
        let all = validate_catalog(&cat, None);
        assert_eq!(all.total, 3);
        assert_eq!(all.failures.len(), 2);
        assert_eq!((all.failures[0].zone_id, all.failures[0].id), (1, 2));
        assert!(!all.failures[0].error.is_empty());
        let z2 = validate_catalog(&cat, Some(2));
        assert_eq!(z2.total, 1);
        assert_eq!(z2.failures.len(), 1);
        let z3 = validate_catalog(&cat, Some(3));
        assert_eq!(z3.total, 0);
        assert!(z3.failures.is_empty());
    }

    #[test]
    fn validate_catalog_does_not_execute_bodies() {
        // A body that would error at runtime (nil call) but compiles fine.
        let mut cat = TriggerCatalog::default();
        cat.by_key
            .insert((1, 1), def(1, 1, "undefined_function_xyz()"));
        assert!(validate_catalog(&cat, None).failures.is_empty());
    }
}

#[cfg(test)]
mod sleep_gate_tests {
    use super::*;
    use mud_db::enums::EntityType;
    use mud_world::{
        EntityVariableCache, Health, Mob, Named, Posture, PostureKind, TriggerAttach, TriggerDef,
        WorldKey,
    };

    const BODY: &str = "self:setvar('ran', 1)";

    struct Fixture {
        world: World,
        room: Entity,
        mob: Entity,
        player: Entity,
    }

    fn fixture(event: TriggerEvent) -> Fixture {
        let mut world = World::new();
        let mut catalog = TriggerCatalog::default();
        catalog.by_key.insert(
            (99, 1),
            TriggerDef {
                zone_id: 99,
                id: 1,
                name: "t".to_string(),
                attach_type: TriggerAttach::Mob,
                commands: BODY.to_string(),
                flags: vec![event],
                arg_list: vec![],
                num_args: 0,
            },
        );
        world.insert_resource(catalog);
        world.insert_resource(mud_script::LuaHost::new());
        let room = world.spawn_empty().id();
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "sleeper".to_string(),
                },
                Health { hp: 10, max: 10 },
                WorldKey { zone: 99, id: 1 },
                Located(room),
                Posture(PostureKind::Standing),
                AttachedTriggers(vec![(99, 1)]),
            ))
            .id();
        let player = world
            .spawn((
                Named {
                    name: "visitor".to_string(),
                },
                Located(room),
            ))
            .id();
        Fixture {
            world,
            room,
            mob,
            player,
        }
    }

    fn ran(world: &mut World) -> bool {
        let seen = world
            .get_resource::<EntityVariableCache>()
            .is_some_and(|c| c.get(EntityType::Mob, 99, 1, "ran").is_some());
        // Reset between phases so each assertion starts clean.
        world.insert_resource(EntityVariableCache::default());
        seen
    }

    fn set_posture(f: &mut Fixture, p: PostureKind) {
        f.world.entity_mut(f.mob).insert(Posture(p));
    }

    #[test]
    fn sleeping_mob_ignores_speech_until_awake() {
        let mut f = fixture(TriggerEvent::Speech);
        set_posture(&mut f, PostureKind::Sleeping);
        fire_speech_in_room(&mut f.world, f.player, f.room, "hello");
        assert!(!ran(&mut f.world), "asleep: speech trigger suppressed");
        set_posture(&mut f, PostureKind::Standing);
        fire_speech_in_room(&mut f.world, f.player, f.room, "hello");
        assert!(ran(&mut f.world), "awake: speech trigger fires again");
    }

    #[test]
    fn sleeping_mob_ignores_greet_until_awake() {
        let mut f = fixture(TriggerEvent::Greet);
        set_posture(&mut f, PostureKind::Sleeping);
        fire_greet_in_room(&mut f.world, f.player, f.room);
        assert!(!ran(&mut f.world));
        set_posture(&mut f, PostureKind::Standing);
        fire_greet_in_room(&mut f.world, f.player, f.room);
        assert!(ran(&mut f.world));
    }

    #[test]
    fn sleeping_mob_ignores_fight_events() {
        let mut f = fixture(TriggerEvent::Fight);
        set_posture(&mut f, PostureKind::Sleeping);
        fire_event(&mut f.world, f.mob, TriggerEvent::Fight);
        assert!(!ran(&mut f.world));
    }

    #[test]
    fn death_trigger_fires_while_asleep() {
        let mut f = fixture(TriggerEvent::Death);
        set_posture(&mut f, PostureKind::Sleeping);
        fire_event(&mut f.world, f.mob, TriggerEvent::Death);
        assert!(ran(&mut f.world), "death triggers are immune to the gate");
    }

    #[test]
    fn gate_is_cleared_after_dispatch() {
        // A gated dispatch must not leak the gate into later,
        // ungated exec calls (effects, admin lua).
        let mut f = fixture(TriggerEvent::Speech);
        fire_speech_in_room(&mut f.world, f.player, f.room, "hi");
        set_posture(&mut f, PostureKind::Sleeping);
        f.world
            .resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
                host.exec_for_actor(world, f.mob, BODY).unwrap();
            });
        assert!(ran(&mut f.world));
    }
}
