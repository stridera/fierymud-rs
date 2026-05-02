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
    AttachedTriggers, Located, Mob, ScriptError, ScriptErrorLog, TriggerCatalog, TriggerEvent,
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

/// Increment the per-event counters for one trigger fire.
/// Inserted everywhere a script body executes against `LuaHost`.
fn record_fire(world: &mut World, event: TriggerEvent, ok: bool) {
    if !world.contains_resource::<TriggerStats>() {
        world.insert_resource(TriggerStats::default());
    }
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
        record_fire(world, event, result.is_ok());
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
        record_fire(world, TriggerEvent::Speech, result.is_ok());
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
            record_fire(world, TriggerEvent::Speech, result.is_ok());
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
        record_fire(world, event, result.is_ok());
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
            record_fire(world, TriggerEvent::Greet, result.is_ok());
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
        record_fire(world, event, result.is_ok());
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
        record_fire(world, event, result.is_ok());
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
        record_fire(world, TriggerEvent::Receive, result.is_ok());
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
            record_fire(world, TriggerEvent::Command, result.is_ok());
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
