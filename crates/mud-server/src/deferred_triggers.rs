//! Trigger fires requested while a script is already running.
//!
//! A script runs with the `LuaHost` taken out of the world
//! (`resource_scope`) and `mud_script::ScriptRunning` in it, so anything it
//! calls that wants to fire another
//! trigger (a script-cast spell killing a mob that has a DEATH trigger,
//! an effect hook on a script-applied effect) cannot take the host again:
//! that would panic. Such a fire is queued here instead and run by
//! [`drain`] once the outer script has returned and the host is back.
//! `commands::drain_lua_outbox` calls [`drain`] after every script, so the
//! deferred fire follows its cause within the same server call.

use bevy_ecs::prelude::*;

type Fire = Box<dyn FnOnce(&mut World) + Send + Sync>;

/// Queue of fires waiting for the `LuaHost` to be free.
#[derive(Resource, Default)]
pub struct DeferredTriggers {
    queue: Vec<Fire>,
}

/// Upper bound on queue generations per drain: a deferred fire can itself
/// run a script that defers another (script A kills a mob whose death
/// script kills another ...). Past this the rest is dropped, not looped.
const MAX_ROUNDS: usize = 16;

/// Marks a mob whose DEATH trigger was deferred. The mob must outlive the
/// outer script, so the trigger body still sees `self`; `finish_mob_death`
/// defers the despawn behind the fire, and a repeated `handle_death` on
/// the marked mob is a no-op.
#[derive(Component, Debug, Clone, Copy)]
pub struct DeathFirePending;

/// True while a script is running: it holds the `LuaHost`, so a second
/// `resource_scope::<LuaHost>` would panic.
#[must_use]
pub fn lua_busy(world: &World) -> bool {
    world.contains_resource::<mud_script::ScriptRunning>()
}

/// Queue `fire` to run after the outer script returns.
pub fn defer(world: &mut World, fire: impl FnOnce(&mut World) + Send + Sync + 'static) {
    if !world.contains_resource::<DeferredTriggers>() {
        world.insert_resource(DeferredTriggers::default());
    }
    world
        .resource_mut::<DeferredTriggers>()
        .queue
        .push(Box::new(fire));
}

/// Run the queued fires. No-op while a script still holds the host (the
/// outermost script's own drain picks the queue up).
pub fn drain(world: &mut World) {
    if lua_busy(world) {
        return;
    }
    for _ in 0..MAX_ROUNDS {
        let batch = match world.get_resource_mut::<DeferredTriggers>() {
            Some(mut q) if !q.queue.is_empty() => std::mem::take(&mut q.queue),
            _ => return,
        };
        for fire in batch {
            fire(world);
        }
    }
    if let Some(mut q) = world.get_resource_mut::<DeferredTriggers>() {
        let dropped = q.queue.len();
        q.queue.clear();
        if dropped > 0 {
            tracing::warn!(dropped, "deferred trigger chain too deep; dropped the rest");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Resource, Default)]
    struct Log(Vec<&'static str>);

    #[test]
    fn drain_runs_in_order_and_follows_chains() {
        let mut world = World::new();
        world.insert_resource(Log::default());
        defer(&mut world, |w| {
            w.resource_mut::<Log>().0.push("a");
            defer(w, |w| w.resource_mut::<Log>().0.push("c"));
        });
        defer(&mut world, |w| w.resource_mut::<Log>().0.push("b"));
        drain(&mut world);
        assert_eq!(world.resource::<Log>().0, vec!["a", "b", "c"]);
        drain(&mut world);
        assert_eq!(world.resource::<Log>().0.len(), 3, "queue is empty now");
    }

    #[test]
    fn drain_waits_while_a_script_runs() {
        let mut world = World::new();
        world.insert_resource(Log::default());
        world.insert_resource(mud_script::ScriptRunning);
        defer(&mut world, |w| w.resource_mut::<Log>().0.push("x"));
        assert!(lua_busy(&world));
        drain(&mut world);
        assert!(world.resource::<Log>().0.is_empty(), "script running: held");
        world.remove_resource::<mud_script::ScriptRunning>();
        drain(&mut world);
        assert_eq!(world.resource::<Log>().0, vec!["x"]);
    }

    #[test]
    fn runaway_chain_is_cut_off() {
        fn again(w: &mut World) {
            w.resource_mut::<Log>().0.push("r");
            defer(w, again);
        }
        let mut world = World::new();
        world.insert_resource(Log::default());
        defer(&mut world, again);
        drain(&mut world);
        assert_eq!(world.resource::<Log>().0.len(), MAX_ROUNDS);
        assert!(world.resource::<DeferredTriggers>().queue.is_empty());
    }
}
