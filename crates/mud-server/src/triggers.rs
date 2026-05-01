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

use bevy_ecs::prelude::*;
use mud_world::{AttachedTriggers, Located, Mob, TriggerCatalog, TriggerEvent};
use tracing::warn;

use crate::commands::drain_lua_outbox;

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
        let result = world.resource_scope::<mud_script::LuaHost, _>(|world, host| {
            host.exec_for_actor(world, entity, &body)
        });
        drain_lua_outbox(world);
        if let Err(e) = result {
            warn!(zone, id, name = %name, error = %e, "trigger fire failed");
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
            let result = world.resource_scope::<mud_script::LuaHost, _>(|world, host| {
                host.exec_for_actor_with_extras(
                    world,
                    listener,
                    &body,
                    &[("speech", &lowered)],
                )
            });
            drain_lua_outbox(world);
            if let Err(e) = result {
                warn!(zone, id, name = %name, error = %e, "SPEECH trigger fire failed");
            }
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
            let result = world.resource_scope::<mud_script::LuaHost, _>(|world, host| {
                host.exec_for_listener_with_extras(world, listener, entering, &body, &[])
            });
            drain_lua_outbox(world);
            if let Err(e) = result {
                warn!(zone, id, name = %name, error = %e, "GREET trigger fire failed");
            }
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
        let result = world.resource_scope::<mud_script::LuaHost, _>(|world, host| {
            host.exec_for_event(world, recipient, giver, Some(item), &body, &[])
        });
        drain_lua_outbox(world);
        if let Err(e) = result {
            warn!(zone, id, name = %name, error = %e, "RECEIVE trigger fire failed");
        }
    }
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
