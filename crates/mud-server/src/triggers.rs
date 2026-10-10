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
    AttachedTriggers, DeferredRoomTriggerFire, DeferredRoomTriggerFires, Mob, Room, ScriptError,
    ScriptErrorLog, TriggerCatalog, TriggerEvent, WorldKey, WorldKeyIndex,
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
    if crate::deferred_triggers::lua_busy(world) {
        crate::deferred_triggers::defer(world, move |w| fire_event(w, entity, event));
        return;
    }
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

/// Fire SPEECH-flagged triggers on a single `listener`. Used by
/// `ask <mob> <topic>` / `tell` to address one NPC without inviting
/// every adjacent mob to chime in, and per listener by
/// [`fire_speech_in_room`]. `self` binds to the listener and `actor`
/// to the speaker (legacy `speech_mtrigger`: `ADD_UID_VAR(.., actor)`);
/// `speech` (lowercased) carries the spoken text.
///
/// Legacy `speech_mtrigger` / `speech_to_mtrigger` test both
/// `MTRIG_SPEECH` and `MTRIG_SPEECHTO` on the listener, so a SPEECH_TO
/// script also answers plain `say`. The keyword filter lives in the
/// converted body, so every matching trigger is fired and the body
/// decides whether to react.
pub fn fire_speech_at(world: &mut World, listener: Entity, speaker: Entity, text: &str) {
    if crate::deferred_triggers::lua_busy(world) {
        let text = text.to_string();
        crate::deferred_triggers::defer(world, move |w| {
            fire_speech_at(w, listener, speaker, &text);
        });
        return;
    }
    if listener == speaker {
        return;
    }
    let to_fire: Vec<(i32, i32, String, String)> = {
        let Some(at) = world.get::<AttachedTriggers>(listener) else {
            return;
        };
        let keys = at.0.clone();
        let catalog = world.resource::<TriggerCatalog>();
        keys.into_iter()
            .filter_map(|(zone, id)| {
                let def = catalog.by_key.get(&(zone, id))?;
                if def.flags.contains(&TriggerEvent::Speech)
                    || def.flags.contains(&TriggerEvent::SpeechTo)
                {
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

/// Fire SPEECH-flagged triggers for every entity in `room` (other
/// than the speaker) that carries `AttachedTriggers`, then the room's
/// own. Each listener runs as `self` with `actor` bound to the speaker,
/// so the 681 corpus scripts that answer `actor` reply to whoever spoke.
pub fn fire_speech_in_room(world: &mut World, speaker: Entity, room: Entity, text: &str) {
    if crate::deferred_triggers::lua_busy(world) {
        let text = text.to_string();
        crate::deferred_triggers::defer(world, move |w| {
            fire_speech_in_room(w, speaker, room, &text);
        });
        return;
    }
    let listeners: Vec<Entity> = crate::room_index::contents_of(world, room)
        .filter(|&e| e != speaker && world.get::<AttachedTriggers>(e).is_some())
        .collect();
    for listener in listeners {
        fire_speech_at(world, listener, speaker, text);
    }
    // Legacy `do_say` calls `speech_wtrigger` right after
    // `speech_mtrigger`: the room's own SPEECH triggers (`self` = the
    // room) hear it too.
    if world.get::<AttachedTriggers>(room).is_some() {
        fire_speech_at(world, room, speaker, text);
    }
}

/// Fire `PREENTRY` / `POSTENTRY` triggers attached to `room`. Both
/// fire from the room's perspective (`self` = room) with `actor`
/// bound to the entering player. PREENTRY fires before the player's
/// `Located` is changed; POSTENTRY fires after.
pub fn fire_room_entry(world: &mut World, room: Entity, entering: Entity, event: TriggerEvent) {
    if crate::deferred_triggers::lua_busy(world) {
        crate::deferred_triggers::defer(world, move |w| fire_room_entry(w, room, entering, event));
        return;
    }
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
    if crate::deferred_triggers::lua_busy(world) {
        crate::deferred_triggers::defer(world, move |w| fire_greet_in_room(w, entering, room));
        return;
    }
    let listeners: Vec<Entity> = crate::room_index::contents_of(world, room)
        .filter(|&e| e != entering && world.get::<AttachedTriggers>(e).is_some())
        .collect();
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
    if crate::deferred_triggers::lua_busy(world) {
        crate::deferred_triggers::defer(world, move |w| fire_item_event(w, item, actor, event));
        return;
    }
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
    if crate::deferred_triggers::lua_busy(world) {
        crate::deferred_triggers::defer(world, move |w| {
            fire_event_with_actor(w, listener, acting, event);
        });
        return;
    }
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
    if crate::deferred_triggers::lua_busy(world) {
        crate::deferred_triggers::defer(world, move |w| fire_receive(w, recipient, giver, item));
        return;
    }
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

/// Legacy `command_wtrigger` / `command_mtrigger` / `command_otrigger`
/// do not run for staff: the interpreter gates them on
/// `GET_LEVEL(ch) < LVL_IMMORT`. They also skip a wizinvis actor
/// (`char_susceptible_to_triggers`).
fn command_triggers_exempt(world: &World, actor: Entity) -> bool {
    if world
        .get::<mud_world::WizInvis>(actor)
        .is_some_and(|w| w.0 > 0)
    {
        return true;
    }
    crate::room_access::is_immortal(world, actor)
        && world
            .get::<mud_world::Profile>(actor)
            .is_some_and(|p| p.level >= 100)
}

/// Fire `COMMAND`-flagged triggers for a command `player` just typed,
/// in the legacy interpreter order (`command_wtrigger ||
/// command_mtrigger || command_otrigger`):
///
/// 1. the room itself (WORLD triggers),
/// 2. mobs in the room,
/// 3. objects the player wears, then carries, then objects on the floor.
///
/// `cmd` is the typed word. `args` (also bound as `arg`, the name the
/// converted bodies use) is the rest of the line. Object triggers also
/// get `location` (`"equip"` / `"inventory"` / `"room"`), the legacy
/// `OCMD_*` mask the converter folds into a guard in the body.
///
/// Returns `true` when a trigger consumed the command, so the caller
/// stops dispatch. Legacy `script_driver` starts with `ret_val = 1`,
/// which blocks the command; only an explicit `return 0` lets it
/// through, and a script that reaches a `wait` has already returned
/// that default. So a run that ends `return true` (the Lua spelling of
/// `return 0`, and of "not my command") lets the command continue, and
/// anything else (`return false`, no return, a thread parked on
/// `wait`) consumes it.
pub fn fire_command_in_room(
    world: &mut World,
    player: Entity,
    room: Entity,
    cmd: &str,
    args: &str,
) -> bool {
    // A typed command cannot be replayed once the script that issued it is
    // done, so while a script runs the command is simply not intercepted.
    if crate::deferred_triggers::lua_busy(world) {
        return false;
    }
    if command_triggers_exempt(world, player) {
        return false;
    }
    let has_triggers = |world: &World, e: Entity| world.get::<AttachedTriggers>(e).is_some();
    let in_room: Vec<Entity> = crate::room_index::contents_of(world, room).collect();
    // (source entity, location label for object triggers)
    let mut sources: Vec<(Entity, Option<&'static str>)> = Vec::new();
    if has_triggers(world, room) {
        sources.push((room, None));
    }
    for &e in &in_room {
        if e != player && world.get::<Mob>(e).is_some() && has_triggers(world, e) {
            sources.push((e, None));
        }
    }
    let (worn, carried): (Vec<Entity>, Vec<Entity>) = crate::room_index::contents_of(world, player)
        .filter(|&e| world.get::<mud_world::Item>(e).is_some() && has_triggers(world, e))
        .partition(|&e| world.get::<mud_world::EquippedSlot>(e).is_some());
    sources.extend(worn.into_iter().map(|e| (e, Some("equip"))));
    sources.extend(carried.into_iter().map(|e| (e, Some("inventory"))));
    for &e in &in_room {
        if world.get::<mud_world::Item>(e).is_some() && has_triggers(world, e) {
            sources.push((e, Some("room")));
        }
    }

    for (listener, location) in sources {
        let to_fire = triggers_with(world, listener, &[TriggerEvent::Command]);
        if to_fire.is_empty() {
            continue;
        }
        // A sleeping mob's script never starts and a casting mob's is
        // parked: neither says anything about this command, so they
        // must not read as a verdict.
        if mud_script::mob_script_blocked(world, listener) {
            continue;
        }
        for (zone, id, name, body) in to_fire {
            let mut extras: Vec<(&str, &str)> = vec![("cmd", cmd), ("args", args), ("arg", args)];
            if let Some(loc) = location {
                extras.push(("location", loc));
            }
            let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
                gated(&mut host, TriggerEvent::Command, |h| {
                    h.exec_for_event_with_value(world, listener, player, None, &body, &extras)
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
                Ok((_out, Some(true))) => {}
                Ok(_) => return true,
                Err(e) => record_failure(world, zone, id, &name, "COMMAND", &e),
            }
        }
    }
    false
}

/// `(zone, id, name, body)` of every trigger attached to `entity` whose
/// flags include any of `events`, in attachment order.
fn triggers_with(
    world: &World,
    entity: Entity,
    events: &[TriggerEvent],
) -> Vec<(i32, i32, String, String)> {
    let Some(at) = world.get::<AttachedTriggers>(entity) else {
        return Vec::new();
    };
    let catalog = world.resource::<TriggerCatalog>();
    at.0.iter()
        .filter_map(|key| {
            let def = catalog.by_key.get(key)?;
            def.flags
                .iter()
                .any(|f| events.contains(f))
                .then(|| (key.0, key.1, def.name.clone(), def.commands.clone()))
        })
        .collect()
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

    // The new catalog goes in first: `AttachedTriggers`' insert hook
    // reads it to index entities with RANDOM triggers.
    let room_attachments = new.room_attachments.clone();
    world.insert_resource(new);

    let rooms_to_refresh: Vec<(Entity, (i32, i32))> = {
        let mut q = world.query_filtered::<(Entity, &WorldKey), With<Room>>();
        q.iter(world).map(|(e, k)| (e, (k.zone, k.id))).collect()
    };
    for (room, key) in rooms_to_refresh {
        let attached = room_attachments.get(&key).cloned();
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

    // Mob and object instances keep their attachments across a reload,
    // but an edited trigger may have gained or lost the RANDOM flag.
    reindex_random_triggers(world);
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
    // Fires a script's server calls had to queue (see `deferred_triggers`).
    crate::deferred_triggers::drain(world);
}

/// Legacy `PULSE_DG_SCRIPT`: RANDOM triggers are rolled every 13 real
/// seconds.
const RANDOM_PULSE_TICKS: u64 = 13 * crate::TICK_HZ;

/// Split the converter's leading probability gate off a trigger body:
///
/// ```lua
/// -- 25% chance to trigger
/// if not percent_chance(25) then
///     return true
/// end
/// ```
///
/// Returns the percentage and the body with the gate removed, or `None`
/// and the untouched body when it does not start with one (comment and
/// blank lines before it are fine). The converter folds the DG numeric
/// argument into this gate, but for several event types the argument is
/// not a chance to run: for HIT_PERCENT it is the HP% threshold. The
/// dispatcher reads it back out so it can apply the legacy meaning and,
/// for RANDOM, roll in Rust (sparing a Lua environment for the rolls
/// that fail).
fn split_leading_gate(body: &str) -> (Option<i64>, &str) {
    fn parse(mut rest: &str) -> Option<(i64, &str)> {
        while let Some(line_end) = rest.find('\n') {
            let line = rest[..line_end].trim();
            if line.is_empty() || line.starts_with("--") {
                rest = &rest[line_end + 1..];
            } else {
                break;
            }
        }
        let rest = rest.strip_prefix("if not percent_chance(")?;
        let (num, rest) = rest.split_once(')')?;
        let pct: i64 = num.trim().parse().ok()?;
        let rest = rest.trim_start().strip_prefix("then")?.trim_start();
        let rest = rest.strip_prefix("return true")?;
        // Optional trailing comment on the `return true` line.
        let (tail, rest) = rest.split_once('\n')?;
        if !(tail.trim().is_empty() || tail.trim_start().starts_with("--")) {
            return None;
        }
        let rest = rest.trim_start().strip_prefix("end")?;
        let rest = rest.strip_prefix('\n').unwrap_or(rest);
        Some((pct, rest))
    }
    match parse(body) {
        Some((pct, rest)) => (Some(pct), rest),
        None => (None, body),
    }
}

/// Recompute the [`mud_world::RandomTriggers`] marker for every
/// scripted entity against the current catalog. The marker is kept
/// current on insert by a component hook; this catches the entities
/// whose attachments survive a catalog reload.
fn reindex_random_triggers(world: &mut World) {
    let attached: Vec<(Entity, Vec<(i32, i32)>)> = {
        let mut q = world.query::<(Entity, &AttachedTriggers)>();
        q.iter(world).map(|(e, at)| (e, at.0.clone())).collect()
    };
    let scripted: Vec<(Entity, bool)> = {
        let catalog = world.resource::<TriggerCatalog>();
        attached
            .into_iter()
            .map(|(e, keys)| {
                let random = keys.iter().any(|key| {
                    catalog
                        .by_key
                        .get(key)
                        .is_some_and(|d| d.flags.contains(&TriggerEvent::Random))
                });
                (e, random)
            })
            .collect()
    };
    for (e, random) in scripted {
        if let Ok(mut em) = world.get_entity_mut(e) {
            if random {
                em.insert(mud_world::RandomTriggers);
            } else {
                em.remove::<mud_world::RandomTriggers>();
            }
        }
    }
}

/// Per-pulse RANDOM dispatch (legacy `script_trigger_check`). Every 13
/// seconds each scripted mob, object and room rolls its RANDOM triggers.
/// A mob or room only rolls while a player is in its zone, unless one of
/// its triggers carries the GLOBAL flag ("check even if zone empty");
/// objects roll everywhere. Only entities carrying the
/// [`mud_world::RandomTriggers`] marker are visited, so the cost tracks
/// the number of RANDOM-scripted entities, not the size of the world.
///
/// Legacy runs at most one RANDOM trigger per entity per pulse: the first
/// whose percent passes. Here the percent is read from the body's leading
/// gate and rolled before any Lua runs, and a trigger that passes runs
/// with the gate stripped so the chance is not rolled twice.
pub fn random_trigger_tick(world: &mut World) {
    let tick = world.resource::<crate::TickCount>().0;
    if tick == 0 || !tick.is_multiple_of(RANDOM_PULSE_TICKS) {
        return;
    }
    run_random_pulse(world);
}

/// One RANDOM pulse; split out so tests can run it without tick math.
pub(crate) fn run_random_pulse(world: &mut World) {
    let candidates: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, With<mud_world::RandomTriggers>>();
        q.iter(world).collect()
    };
    if candidates.is_empty() {
        return;
    }
    // Zones with a player in them, built once per pulse and only when a
    // mob or room needs the answer.
    let mut active_zones: Option<std::collections::HashSet<i32>> = None;
    let mut zone_is_active = |world: &mut World, zone: i32| -> bool {
        active_zones
            .get_or_insert_with(|| {
                let mut zones = std::collections::HashSet::new();
                let mut q = world.query_filtered::<&mud_world::Located, With<mud_world::Player>>();
                let rooms: Vec<Entity> = q.iter(world).map(|l| l.0).collect();
                for room in rooms {
                    if let Some(k) = world.get::<WorldKey>(room) {
                        zones.insert(k.zone);
                    }
                }
                zones
            })
            .contains(&zone)
    };
    for entity in candidates {
        let Ok(em) = world.get_entity(entity) else {
            continue;
        };
        let is_item = em.contains::<mud_world::Item>();
        let defs = triggers_with(world, entity, &[TriggerEvent::Random]);
        if defs.is_empty() {
            continue;
        }
        if !is_item {
            let global = {
                let catalog = world.resource::<TriggerCatalog>();
                world.get::<AttachedTriggers>(entity).is_some_and(|at| {
                    at.0.iter().any(|k| {
                        catalog
                            .by_key
                            .get(k)
                            .is_some_and(|d| d.flags.contains(&TriggerEvent::Global))
                    })
                })
            };
            if !global {
                // A mob's zone is its room's; a room's is its own.
                let room = if world.get::<Room>(entity).is_some() {
                    Some(entity)
                } else {
                    world.get::<mud_world::Located>(entity).map(|l| l.0)
                };
                let zone = room.and_then(|r| world.get::<WorldKey>(r)).map(|k| k.zone);
                match zone {
                    Some(z) if zone_is_active(world, z) => {}
                    _ => continue,
                }
            }
        }
        for (zone, id, name, body) in defs {
            let (gate, rest) = split_leading_gate(&body);
            if let Some(pct) = gate
                && rand::random_range(1i64..=100) > pct.clamp(0, 100)
            {
                continue;
            }
            let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
                gated(&mut host, TriggerEvent::Random, |h| {
                    h.exec_for_actor(world, entity, rest)
                })
            });
            drain_lua_outbox(world);
            record_fire(
                world,
                entity,
                zone,
                id,
                TriggerEvent::Random,
                result.is_ok(),
            );
            if let Err(e) = result {
                record_failure(world, zone, id, &name, "RANDOM", &e);
            }
            // Legacy: only the first RANDOM trigger that passes runs.
            break;
        }
    }
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
        EntityVariableCache, Health, Located, Mob, Named, Posture, PostureKind, TriggerAttach,
        TriggerDef, WorldKey,
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

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use mud_db::enums::EntityType;
    use mud_world::{
        EntityVariableCache, Health, Located, Mob, Named, Posture, PostureKind, TriggerAttach,
        TriggerDef, WorldKey,
    };

    /// Register trigger `(99, id)` with the given flags and body.
    fn add_trigger(world: &mut World, id: i32, flags: Vec<TriggerEvent>, body: &str) {
        let mut catalog = world
            .remove_resource::<TriggerCatalog>()
            .unwrap_or_default();
        catalog.by_key.insert(
            (99, id),
            TriggerDef {
                zone_id: 99,
                id,
                name: format!("t{id}"),
                attach_type: TriggerAttach::Mob,
                commands: body.to_string(),
                flags,
                arg_list: vec![],
                num_args: 0,
            },
        );
        world.insert_resource(catalog);
    }

    fn base_world() -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(TriggerCatalog::default());
        world.insert_resource(mud_script::LuaHost::new());
        let room = world
            .spawn((mud_world::Room, WorldKey { zone: 99, id: 0 }))
            .id();
        (world, room)
    }

    fn spawn_mob(world: &mut World, room: Entity, id: i32, triggers: Vec<(i32, i32)>) -> Entity {
        world
            .spawn((
                Mob,
                Named {
                    name: format!("mob{id}"),
                },
                Health { hp: 10, max: 10 },
                WorldKey { zone: 99, id },
                Located(room),
                Posture(PostureKind::Standing),
                AttachedTriggers(triggers),
            ))
            .id()
    }

    fn spawn_player(world: &mut World, room: Entity, name: &str) -> Entity {
        world
            .spawn((
                mud_world::Player,
                Named {
                    name: name.to_string(),
                },
                Located(room),
            ))
            .id()
    }

    fn var(world: &World, mob_id: i32, key: &str) -> Option<serde_json::Value> {
        world
            .get_resource::<EntityVariableCache>()
            .and_then(|c| c.get(EntityType::Mob, 99, mob_id, key).cloned())
    }

    #[test]
    fn speech_binds_actor_to_the_speaker_not_the_listener() {
        let (mut world, room) = base_world();
        add_trigger(
            &mut world,
            1,
            vec![TriggerEvent::Speech],
            "self:setvar('who', actor.name)\nself:setvar('heard', speech)",
        );
        let mob = spawn_mob(&mut world, room, 1, vec![(99, 1)]);
        let speaker = spawn_player(&mut world, room, "Alice");
        fire_speech_in_room(&mut world, speaker, room, "Hello There");
        assert_eq!(var(&world, 1, "who"), Some("Alice".into()));
        assert_eq!(var(&world, 1, "heard"), Some("hello there".into()));
        // `ask`/`tell` reach the same binding through `fire_speech_at`.
        world.insert_resource(EntityVariableCache::default());
        fire_speech_at(&mut world, mob, speaker, "topic");
        assert_eq!(var(&world, 1, "who"), Some("Alice".into()));
    }

    #[test]
    fn speech_to_scripts_answer_speech_too() {
        let (mut world, room) = base_world();
        add_trigger(
            &mut world,
            1,
            vec![TriggerEvent::SpeechTo],
            "self:setvar('who', actor.name)",
        );
        spawn_mob(&mut world, room, 1, vec![(99, 1)]);
        let speaker = spawn_player(&mut world, room, "Bob");
        fire_speech_in_room(&mut world, speaker, room, "hi");
        assert_eq!(var(&world, 1, "who"), Some("Bob".into()));
    }

    // ----- command triggers (rooms, carried objects, return value) -----

    fn spawn_item(
        world: &mut World,
        located: Entity,
        id: i32,
        triggers: Vec<(i32, i32)>,
    ) -> Entity {
        world
            .spawn((
                mud_world::Item,
                Named {
                    name: format!("item{id}"),
                },
                WorldKey { zone: 99, id },
                Located(located),
                AttachedTriggers(triggers),
            ))
            .id()
    }

    fn ran(world: &World, id: i32, kind: EntityType, key: &str) -> Option<serde_json::Value> {
        world
            .get_resource::<EntityVariableCache>()
            .and_then(|c| c.get(kind, 99, id, key).cloned())
    }

    #[test]
    fn room_command_trigger_fires_and_consumes() {
        let (mut world, room) = base_world();
        add_trigger(
            &mut world,
            1,
            vec![TriggerEvent::Command],
            "if cmd ~= 'push' then return true end\nself:setvar('arg', arg)\nreturn false",
        );
        world
            .entity_mut(room)
            .insert(AttachedTriggers(vec![(99, 1)]));
        let player = spawn_player(&mut world, room, "Pat");
        assert!(!fire_command_in_room(&mut world, player, room, "look", ""));
        assert!(fire_command_in_room(
            &mut world, player, room, "push", "ice"
        ));
        assert_eq!(ran(&world, 0, EntityType::Room, "arg"), Some("ice".into()));
    }

    #[test]
    fn carried_and_worn_object_command_triggers_fire_with_their_location() {
        let (mut world, room) = base_world();
        add_trigger(
            &mut world,
            1,
            vec![TriggerEvent::Command],
            "if cmd ~= 'light' then return true end\nself:setvar('loc', location)\nself:setvar('arg', arg)\nreturn false",
        );
        let player = spawn_player(&mut world, room, "Pat");
        let torch = spawn_item(&mut world, player, 9, vec![(99, 1)]);
        assert!(fire_command_in_room(
            &mut world, player, room, "light", "torch"
        ));
        assert_eq!(
            ran(&world, 9, EntityType::Object, "loc"),
            Some("inventory".into())
        );
        assert_eq!(
            ran(&world, 9, EntityType::Object, "arg"),
            Some("torch".into())
        );
        // Worn: the slot marker flips the location label.
        world.insert_resource(EntityVariableCache::default());
        world
            .entity_mut(torch)
            .insert(mud_world::EquippedSlot(mud_world::Slot::Hold));
        assert!(fire_command_in_room(
            &mut world, player, room, "light", "torch"
        ));
        assert_eq!(
            ran(&world, 9, EntityType::Object, "loc"),
            Some("equip".into())
        );
        // On the floor of the room.
        world.insert_resource(EntityVariableCache::default());
        world.entity_mut(torch).remove::<mud_world::EquippedSlot>();
        world.entity_mut(torch).insert(Located(room));
        assert!(fire_command_in_room(
            &mut world, player, room, "light", "torch"
        ));
        assert_eq!(
            ran(&world, 9, EntityType::Object, "loc"),
            Some("room".into())
        );
    }

    #[test]
    fn command_triggers_run_in_legacy_order_and_the_first_verdict_wins() {
        let (mut world, room) = base_world();
        let body = |tag: &str, ret: &str| {
            format!(
                "if cmd ~= 'go' then return true end\nglobals.x = 1\nself:setvar('{tag}', 1)\nreturn {ret}"
            )
        };
        add_trigger(
            &mut world,
            1,
            vec![TriggerEvent::Command],
            &body("hit", "true"),
        );
        add_trigger(
            &mut world,
            2,
            vec![TriggerEvent::Command],
            &body("hit", "false"),
        );
        add_trigger(
            &mut world,
            3,
            vec![TriggerEvent::Command],
            &body("hit", "true"),
        );
        let player = spawn_player(&mut world, room, "Pat");
        spawn_mob(&mut world, room, 20, vec![(99, 2)]);
        spawn_item(&mut world, player, 30, vec![(99, 3)]);
        spawn_item(&mut world, room, 31, vec![(99, 3)]);
        assert!(fire_command_in_room(&mut world, player, room, "go", ""));
        // The mob consumed it: carried and floor objects never ran.
        assert!(ran(&world, 20, EntityType::Mob, "hit").is_some());
        assert!(ran(&world, 30, EntityType::Object, "hit").is_none());
        assert!(ran(&world, 31, EntityType::Object, "hit").is_none());
    }

    #[test]
    fn command_trigger_that_waits_consumes_the_command() {
        // Shape of 519_65 (academy deposit): a quest branch sends a forced
        // `deposit` and waits; the typed command must not run as well.
        let (mut world, room) = base_world();
        let body = r#"
if not (cmd == "deposit") then
    return true  -- Not our command
end
local _return_value = false  -- legacy default
if arg == "d" then
    _return_value = true
    return _return_value
end
if string.find(arg, "1 gold") then
    self:setvar('forced', arg)
    wait(2)
    self:setvar('after_wait', 1)
end
if arg == "plain" then
    _return_value = true
end
return _return_value"#;
        add_trigger(&mut world, 1, vec![TriggerEvent::Command], body);
        spawn_mob(&mut world, room, 1, vec![(99, 1)]);
        let player = spawn_player(&mut world, room, "Pat");
        // Waits: consumed, and the body is parked mid-way.
        assert!(fire_command_in_room(
            &mut world,
            player,
            room,
            "deposit",
            "1 gold 1 silver"
        ));
        assert_eq!(var(&world, 1, "forced"), Some("1 gold 1 silver".into()));
        assert!(var(&world, 1, "after_wait").is_none(), "parked on wait");
        // Explicit allow paths and other commands pass through.
        assert!(!fire_command_in_room(
            &mut world, player, room, "deposit", "d"
        ));
        assert!(!fire_command_in_room(
            &mut world, player, room, "deposit", "plain"
        ));
        assert!(!fire_command_in_room(&mut world, player, room, "look", ""));
        // No `return 0` on the path: the legacy default blocks.
        assert!(fire_command_in_room(
            &mut world, player, room, "deposit", "x"
        ));
    }

    #[test]
    fn sleeping_mob_does_not_swallow_commands() {
        let (mut world, room) = base_world();
        add_trigger(
            &mut world,
            1,
            vec![TriggerEvent::Command],
            "if cmd ~= 'rest' then return true end\nreturn false",
        );
        let mob = spawn_mob(&mut world, room, 1, vec![(99, 1)]);
        let player = spawn_player(&mut world, room, "Pat");
        assert!(fire_command_in_room(&mut world, player, room, "rest", ""));
        world.entity_mut(mob).insert(Posture(PostureKind::Sleeping));
        assert!(
            !fire_command_in_room(&mut world, player, room, "rest", ""),
            "an asleep mob's aborted script is not a verdict"
        );
        assert!(!fire_command_in_room(&mut world, player, room, "look", ""));
    }

    #[test]
    fn room_speech_triggers_hear_say() {
        let (mut world, room) = base_world();
        add_trigger(
            &mut world,
            1,
            vec![TriggerEvent::Speech],
            "self:setvar('who', actor.name)",
        );
        world
            .entity_mut(room)
            .insert(AttachedTriggers(vec![(99, 1)]));
        let speaker = spawn_player(&mut world, room, "Dana");
        fire_speech_in_room(&mut world, speaker, room, "open sesame");
        assert_eq!(
            world
                .resource::<EntityVariableCache>()
                .get(EntityType::Room, 99, 0, "who")
                .cloned(),
            Some("Dana".into())
        );
    }

    // ----- RANDOM -----

    /// A room at `zone` (WorldKey) that is not the default zone-99 room.
    fn spawn_room_in(world: &mut World, zone: i32, id: i32) -> Entity {
        world.spawn((mud_world::Room, WorldKey { zone, id })).id()
    }

    #[test]
    fn split_leading_gate_reads_the_converter_header() {
        let body = "-- Trigger: x\n\n-- 25% chance to trigger\nif not percent_chance(25) then\n    return true\nend\nself:say('hi')\n";
        let (pct, rest) = split_leading_gate(body);
        assert_eq!(pct, Some(25));
        assert_eq!(rest, "self:say('hi')\n");
        // Trailing comment on the return line is fine.
        let (pct, rest) = split_leading_gate(
            "if not percent_chance(7) then\n    return true  -- nope\nend\nbody()",
        );
        assert_eq!((pct, rest), (Some(7), "body()"));
        // No gate, or code before it: untouched.
        for b in [
            "self:say('hi')\n",
            "x = 1\nif not percent_chance(5) then\n return true\nend\n",
        ] {
            assert_eq!(split_leading_gate(b), (None, b));
        }
    }

    #[test]
    fn random_triggers_roll_only_where_legacy_does() {
        let (mut world, room) = base_world();
        // Zone 99 has a player; zone 98 does not.
        spawn_player(&mut world, room, "Watcher");
        let empty = spawn_room_in(&mut world, 98, 0);
        add_trigger(
            &mut world,
            1,
            vec![TriggerEvent::Random],
            "self:setvar('rolled', (self:getvar('rolled') or 0) + 1)",
        );
        add_trigger(
            &mut world,
            2,
            vec![TriggerEvent::Random, TriggerEvent::Global],
            "self:setvar('rolled', (self:getvar('rolled') or 0) + 1)",
        );
        let near = spawn_mob(&mut world, room, 1, vec![(99, 1)]);
        let far = spawn_mob(&mut world, empty, 2, vec![(99, 1)]);
        let far_global = spawn_mob(&mut world, empty, 3, vec![(99, 2)]);
        let item = spawn_item(&mut world, empty, 4, vec![(99, 1)]);
        let plain = spawn_mob(&mut world, room, 5, vec![]);
        // The insert hook indexed exactly the RANDOM-scripted entities.
        for e in [near, far, far_global, item] {
            assert!(world.get::<mud_world::RandomTriggers>(e).is_some());
        }
        assert!(world.get::<mud_world::RandomTriggers>(plain).is_none());

        run_random_pulse(&mut world);
        let rolled = |w: &World, id: i32, kind| ran(w, id, kind, "rolled");
        assert_eq!(
            rolled(&world, 1, EntityType::Mob),
            Some(1.into()),
            "player in zone"
        );
        assert!(
            rolled(&world, 2, EntityType::Mob).is_none(),
            "empty zone, not GLOBAL"
        );
        assert_eq!(
            rolled(&world, 3, EntityType::Mob),
            Some(1.into()),
            "GLOBAL ignores the gate"
        );
        assert_eq!(
            rolled(&world, 4, EntityType::Object),
            Some(1.into()),
            "objects always roll"
        );
    }

    #[test]
    fn random_percent_gate_is_rolled_once_and_only_one_trigger_runs() {
        let (mut world, room) = base_world();
        spawn_player(&mut world, room, "Watcher");
        let gate = |pct: u32, tag: &str| {
            format!(
                "-- {pct}% chance to trigger\nif not percent_chance({pct}) then\n    return true\nend\nself:setvar('{tag}', 1)\n"
            )
        };
        add_trigger(&mut world, 1, vec![TriggerEvent::Random], &gate(0, "never"));
        add_trigger(
            &mut world,
            2,
            vec![TriggerEvent::Random],
            &gate(100, "first"),
        );
        add_trigger(
            &mut world,
            3,
            vec![TriggerEvent::Random],
            &gate(100, "second"),
        );
        spawn_mob(&mut world, room, 1, vec![(99, 1), (99, 2), (99, 3)]);
        for _ in 0..5 {
            run_random_pulse(&mut world);
        }
        assert!(var(&world, 1, "never").is_none(), "0% never runs");
        assert_eq!(var(&world, 1, "first"), Some(1.into()));
        assert!(
            var(&world, 1, "second").is_none(),
            "one RANDOM trigger per pulse"
        );
    }

    #[test]
    fn random_pulse_runs_on_the_13_second_boundary_only() {
        let (mut world, room) = base_world();
        spawn_player(&mut world, room, "Watcher");
        add_trigger(
            &mut world,
            1,
            vec![TriggerEvent::Random],
            "self:setvar('n', (self:getvar('n') or 0) + 1)",
        );
        spawn_mob(&mut world, room, 1, vec![(99, 1)]);
        for tick in [1u64, 129, 131] {
            world.insert_resource(crate::TickCount(tick));
            random_trigger_tick(&mut world);
        }
        assert!(var(&world, 1, "n").is_none());
        world.insert_resource(crate::TickCount(130));
        random_trigger_tick(&mut world);
        assert_eq!(var(&world, 1, "n"), Some(1.into()));
    }

    /// RANDOM dispatch visits only entities with a RANDOM trigger, so a
    /// prod-sized world costs what its scripted mobs cost. Prod scale:
    /// 10k rooms, 5,500 mobs (1,600 with RANDOM scripts: 600 of them in
    /// empty zones, 60 GLOBAL), 4,000 items (150 RANDOM), 60 RANDOM rooms,
    /// 50 players. The Lua cost is the passes: one in twenty here.
    /// Hard limit only enforced in release.
    #[test]
    fn random_pulse_prod_scale_is_fast() {
        const ZONES: i32 = 200;
        const ROOMS_PER_ZONE: i32 = 50;
        let (mut world, _) = base_world();
        let gated = "-- 5% chance to trigger\nif not percent_chance(5) then\n    return true\nend\nself:setvar('n', 1)\n";
        add_trigger(&mut world, 1, vec![TriggerEvent::Random], gated);
        add_trigger(
            &mut world,
            2,
            vec![TriggerEvent::Random, TriggerEvent::Global],
            gated,
        );
        let rooms: Vec<Entity> = (0..ZONES * ROOMS_PER_ZONE)
            .map(|i| spawn_room_in(&mut world, i / ROOMS_PER_ZONE, i % ROOMS_PER_ZONE))
            .collect();
        // Players only in the first 20 zones.
        for p in 0..50usize {
            let room = rooms[(p * 19) % (20 * ROOMS_PER_ZONE as usize)];
            spawn_player(&mut world, room, &format!("p{p}"));
        }
        for i in 0..5500usize {
            let room = rooms[(i * 7) % rooms.len()];
            let triggers = match i {
                _ if i % 11 == 0 && i % 7 == 0 => vec![(99, 2)],
                _ if i % 3 == 0 => vec![(99, 1)],
                _ => vec![],
            };
            spawn_mob(&mut world, room, i as i32, triggers);
        }
        for i in 0..4000usize {
            let holder = rooms[(i * 13) % rooms.len()];
            let triggers = if i % 27 == 0 { vec![(99, 1)] } else { vec![] };
            spawn_item(&mut world, holder, i as i32, triggers);
        }
        for i in 0..60usize {
            world
                .entity_mut(rooms[i * 150])
                .insert(AttachedTriggers(vec![(99, 1)]));
        }
        let scripted = world
            .query_filtered::<Entity, With<mud_world::RandomTriggers>>()
            .iter(&world)
            .count();
        assert!(scripted > 1600, "scripted entities: {scripted}");

        let start = std::time::Instant::now();
        run_random_pulse(&mut world);
        let elapsed = start.elapsed();
        eprintln!("random pulse, {scripted} RANDOM-scripted entities of ~20k: {elapsed:?}");
        if !cfg!(debug_assertions) {
            assert!(elapsed.as_millis() < 50, "random pulse took {elapsed:?}");
        }
    }
}
