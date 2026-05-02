//! Lua scripting host for fierymud-rs.
//!
//! Each call to `LuaHost::exec_for_actor` runs a snippet of Lua with `actor`
//! bound to the calling entity and `print` captured into a per-call buffer
//! that the caller (mud-server) routes back to the player. World access from
//! Lua callbacks goes through a raw pointer stashed in `Lua::app_data` —
//! unsafe in principle, sound under the invariant that we only call into Lua
//! while we hold `&mut World` and never re-enter.

use std::ptr::NonNull;

use bevy_ecs::prelude::*;
use mlua::{
    AnyUserData, Function, Lua, MetaMethod, MultiValue, Thread, ThreadStatus, UserData,
    UserDataMethods, Value, Variadic,
};
use mud_world::{
    AbilityCatalog, AppliedTo, AttachedTriggers, ClassCatalog, CombatStats, Description,
    EffectCatalog, EffectInstance, EquippedSlot, Fighting, Follower, Health, Item, Keywords,
    KnownAbilities, Located, LuaOutbox, Mob, MobPrototypes, Named, ObjectPrototypes, Player,
    Posture, PostureKind, Profile, TriggerCatalog, WorldKey, WorldKeyIndex,
};

/// One trigger body that ran into `wait(N)` and got parked. We hold the
/// `mlua::Thread` plus enough context (`acting` / `listener` / `object`
/// / `extras`) to re-bind the globals on resumption — Lua's globals are
/// shared across threads, so any other trigger that fires between
/// yield and resume will trample them.
pub struct YieldedThread {
    thread: Thread,
    listener: Entity,
    acting: Entity,
    object: Option<Entity>,
    extras: Vec<(String, String)>,
    /// Tick value at which this thread becomes due to resume. Computed
    /// at park time as `host.current_tick + wait_secs * TICK_HZ`.
    resume_at_tick: u64,
}

/// Bevy resource wrapping the Lua interpreter and the parked-coroutine
/// queue. mud-server stamps `current_tick` each frame; `tick_yielded`
/// resumes any threads whose `resume_at_tick` has elapsed.
#[derive(Resource)]
pub struct LuaHost {
    lua: Lua,
    /// Most recently observed tick from mud-server. Used as the basis
    /// for computing `resume_at_tick` when a new thread parks, and for
    /// deciding which parked threads are due in `tick_yielded`.
    current_tick: u64,
    yielded: Vec<YieldedThread>,
}

/// Real-tick rate, mirrored from mud-server's `TICK_HZ`. Used to convert
/// a Lua `wait(seconds)` into ticks. Kept here as a const so mud-script
/// doesn't need to depend on mud-server.
const TICK_HZ: u64 = 10;

impl Default for LuaHost {
    fn default() -> Self {
        Self::new()
    }
}

impl LuaHost {
    #[must_use]
    pub fn new() -> Self {
        // TODO: lock down os/io/debug modules. mlua 0.11 doesn't expose a
        // direct sandbox helper for stock Lua 5.4 builds; we'll do explicit
        // global removal once we land actual triggers (the admin-only
        // `lua` command isn't a meaningful threat surface).
        Self {
            lua: Lua::new(),
            current_tick: 0,
            yielded: Vec::new(),
        }
    }

    /// Stamp the current world tick. mud-server calls this once per
    /// tick before any Lua-firing system runs so newly-parked threads
    /// compute their resume time relative to a fresh value.
    pub fn set_current_tick(&mut self, tick: u64) {
        self.current_tick = tick;
    }

    /// Number of threads currently parked waiting for `wait(N)` to
    /// elapse. Surfaced for diagnostics (the `show` admin command).
    #[must_use]
    pub fn yielded_count(&self) -> usize {
        self.yielded.len()
    }

    /// Resume every parked thread whose `resume_at_tick <= current_tick`.
    /// Threads that yield again get re-parked with a fresh resume time;
    /// threads that finish or error fall off. Returns the number of
    /// threads resumed (whether they finished or yielded again).
    pub fn tick_yielded(&mut self, world: &mut World) -> usize {
        if self.yielded.is_empty() {
            return 0;
        }
        let due_tick = self.current_tick;
        let (due, parked): (Vec<_>, Vec<_>) = std::mem::take(&mut self.yielded)
            .into_iter()
            .partition(|y| y.resume_at_tick <= due_tick);
        self.yielded = parked;
        let resumed = due.len();
        for yielded in due {
            // Errors during resume are swallowed — the body's already
            // partway through, and mud-server's outbox-drain happens
            // around `tick_yielded`, so any pre-error output reaches
            // the player. Future work could route the error to
            // `ScriptErrorLog` like the initial-fire path.
            let _ = self.resume_thread(world, yielded);
        }
        resumed
    }

    /// Resume a single parked thread. Re-binds globals (actor / self /
    /// object / extras) since Lua globals are shared across threads;
    /// any other trigger that fired between yield and resume would
    /// have trampled them. On a second yield, re-parks; on completion
    /// or error, falls off.
    fn resume_thread(&mut self, world: &mut World, yielded: YieldedThread) -> Result<(), String> {
        let world_ptr = WorldPtr(NonNull::from(&mut *world));
        self.lua.set_app_data(world_ptr);
        self.lua.set_app_data(LuaCapture::default());
        self.lua.set_app_data(SelfEntity(yielded.listener));

        let extras_refs: Vec<(&str, &str)> = yielded
            .extras
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let result = (|| -> mlua::Result<Option<i64>> {
            self.bind_globals(
                world,
                yielded.listener,
                yielded.acting,
                yielded.object,
                &extras_refs,
            )?;
            let values: MultiValue = yielded.thread.resume(())?;
            if matches!(yielded.thread.status(), ThreadStatus::Resumable) {
                let wait_secs = values
                    .into_iter()
                    .next()
                    .and_then(|v| match v {
                        Value::Integer(i) => Some(i),
                        Value::Number(n) => {
                            #[allow(clippy::cast_possible_truncation)]
                            Some(n as i64)
                        }
                        _ => None,
                    })
                    .unwrap_or(1)
                    .max(1);
                Ok(Some(wait_secs))
            } else {
                Ok(None)
            }
        })();

        self.lua.remove_app_data::<LuaCapture>();
        self.lua.remove_app_data::<WorldPtr>();
        self.lua.remove_app_data::<SelfEntity>();
        self.unbind_globals(&extras_refs);

        match result {
            Ok(Some(wait_secs)) => {
                #[allow(clippy::cast_sign_loss)]
                let resume_at_tick = self
                    .current_tick
                    .saturating_add((wait_secs as u64).saturating_mul(TICK_HZ));
                self.yielded.push(YieldedThread {
                    thread: yielded.thread,
                    listener: yielded.listener,
                    acting: yielded.acting,
                    object: yielded.object,
                    extras: yielded.extras,
                    resume_at_tick,
                });
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(e) => Err(format!("lua resume error: {e}")),
        }
    }

    /// Run `code` with `actor` bound to the supplied entity. Captured `print`
    /// output is returned as a single string (one line per print call,
    /// terminated with \r\n).
    pub fn exec_for_actor(
        &mut self,
        world: &mut World,
        actor: Entity,
        code: &str,
    ) -> Result<String, String> {
        self.exec_for_actor_with_extras(world, actor, code, &[])
    }

    /// Like `exec_for_actor`, but also binds the supplied
    /// `(name, value)` pairs as Lua globals before the body runs.
    /// `actor` and `self` are bound to the same entity.
    pub fn exec_for_actor_with_extras(
        &mut self,
        world: &mut World,
        actor: Entity,
        code: &str,
        extras: &[(&str, &str)],
    ) -> Result<String, String> {
        self.exec_for_listener_with_extras(world, actor, actor, code, extras)
    }

    /// Run `code` with `self` bound to `listener` and `actor` bound
    /// to `acting_entity`. Used by event dispatchers (GREET, RECEIVE,
    /// FIGHT, etc.) where the trigger fires on one entity (the
    /// listener) and references another (the actor entering / giving /
    /// attacking). For LOAD/SPEECH/etc. callers where listener IS the
    /// actor, `exec_for_actor_with_extras` is the simpler entry.
    pub fn exec_for_listener_with_extras(
        &mut self,
        world: &mut World,
        listener: Entity,
        acting_entity: Entity,
        code: &str,
        extras: &[(&str, &str)],
    ) -> Result<String, String> {
        self.exec_for_event(world, listener, acting_entity, None, code, extras)
    }

    /// Full event dispatcher entry: `self` = listener, `actor` =
    /// `acting_entity`, `object` = the optional item entity (nil when
    /// `None`). Used by RECEIVE / GIVE / GET / DROP / WEAR / etc.
    /// Discards the body's return value; for COMMAND-style dispatch
    /// where the return value gates whether to continue, see
    /// `exec_for_event_with_value`.
    pub fn exec_for_event(
        &mut self,
        world: &mut World,
        listener: Entity,
        acting_entity: Entity,
        object_entity: Option<Entity>,
        code: &str,
        extras: &[(&str, &str)],
    ) -> Result<String, String> {
        self.exec_for_event_with_value(
            world,
            listener,
            acting_entity,
            object_entity,
            code,
            extras,
        )
        .map(|(out, _)| out)
    }

    /// Like `exec_for_event` but also captures the body's return
    /// value as an optional boolean (true when the body ended with
    /// `return true` or no return; false on `return false`; None
    /// for non-boolean returns). Used by COMMAND dispatch to gate
    /// whether the typed command continues to the default handler.
    ///
    /// Bodies that hit `wait(N)` yield via Lua coroutines. The thread
    /// is parked on `self.yielded` and `tick_yielded` resumes it once
    /// `current_tick` advances past `resume_at_tick`. Callers see the
    /// pre-yield captured output and `return_bool: None`; the body's
    /// final return value is dropped (current callers use it only for
    /// COMMAND-trigger gating, which doesn't yield in practice).
    #[allow(clippy::too_many_lines)]
    pub fn exec_for_event_with_value(
        &mut self,
        world: &mut World,
        listener: Entity,
        acting_entity: Entity,
        object_entity: Option<Entity>,
        code: &str,
        extras: &[(&str, &str)],
    ) -> Result<(String, Option<bool>), String> {
        let span = tracing::info_span!("lua_exec");
        let _g = span.enter();

        // Stash a raw pointer to the world for callbacks. Cleared in the
        // cleanup arm below regardless of code outcome.
        let world_ptr = WorldPtr(NonNull::from(&mut *world));
        self.lua.set_app_data(world_ptr);
        self.lua.set_app_data(LuaCapture::default());
        // Stash `self` as raw entity so callbacks like
        // `combat.engage(target)` can find the engager without a
        // userdata argument.
        self.lua.set_app_data(SelfEntity(listener));

        // Result is one of:
        //   - Ok(None): body finished normally (with optional bool return)
        //   - Ok(Some((thread, wait_secs))): body yielded; park it
        //   - Err: lua compile/exec error
        #[allow(clippy::type_complexity)]
        let result: Result<(Option<bool>, Option<(Thread, i64)>), mlua::Error> = (|| {
            let globals = self.lua.globals();
            globals.set("actor", LuaActor { entity: acting_entity })?;
            // `self` is the canonical name in DG-Script-converted bodies
            // ("set_level(self, ...)"). For SPEECH / LOAD / etc. it
            // points at the same entity as `actor`; for GREET / RECEIVE
            // / FIGHT it points at the listener while `actor` is the
            // entering / giving / attacking entity. Lua treats `self`
            // as an ordinary identifier outside of `:` method
            // definitions.
            globals.set("self", LuaActor { entity: listener })?;
            // `object` is the item-context binding for RECEIVE / GIVE
            // / GET / DROP / WEAR / etc. Nil when the event doesn't
            // carry an object (LOAD, SPEECH, GREET, DEATH).
            match object_entity {
                Some(e) => globals.set("object", LuaActor { entity: e })?,
                None => globals.set("object", Value::Nil)?,
            }

            // Override print so output flows back to the caller.
            globals.set(
                "print",
                self.lua
                    .create_function(|lua, args: Variadic<Value>| -> mlua::Result<()> {
                        let line = format_args(&args);
                        if let Some(mut cap) = lua.app_data_mut::<LuaCapture>() {
                            cap.lines.push(line);
                        }
                        Ok(())
                    })?,
            )?;

            // `globals` is a script-scoped scratchpad table. The
            // DG-Script-converted corpus uses the
            // `globals.x = globals.x or true` pattern as a "first-time
            // only" guard. v1: per-call empty table — writes/reads are
            // consistent within the body. Persistence (round-trip via
            // `Triggers.variables` jsonb) is a follow-up.
            globals.set("globals", self.lua.create_table()?)?;

            // `skills.set_level(actor, name, level)` upserts an entry
            // into the actor's `KnownAbilities`. Used by the
            // breathe-* family of LOAD triggers to grant abilities at
            // mob spawn time.
            let skills_tbl = self.lua.create_table()?;
            skills_tbl.set(
                "set_level",
                self.lua.create_function(
                    |lua, (a, name, level): (AnyUserData, String, i32)| -> mlua::Result<()> {
                        let entity = a.borrow::<LuaActor>()?.entity;
                        skills_set_level(lua, entity, &name, level)
                    },
                )?,
            )?;
            globals.set("skills", skills_tbl)?;

            // `world` namespace: read-only queries against the live
            // world. `count_mobiles` / `count_objects` return how many
            // entities of the given proto `(zone, id)` are currently
            // alive. `find_mobile` returns the first matching mob as
            // a LuaActor or nil. Mutating verbs (destroy / load) are
            // deliberately omitted from v1 — triggers calling them
            // error cleanly with a "nil value" message instead of
            // silently corrupting world state.
            let world_tbl = self.lua.create_table()?;
            world_tbl.set(
                "count_mobiles",
                self.lua.create_function(
                    |lua, (zone, id): (i32, i32)| -> mlua::Result<i64> {
                        world_count_kind(lua, zone, id, EntityKind::Mob)
                    },
                )?,
            )?;
            world_tbl.set(
                "count_objects",
                self.lua.create_function(
                    |lua, (zone, id): (i32, i32)| -> mlua::Result<i64> {
                        world_count_kind(lua, zone, id, EntityKind::Item)
                    },
                )?,
            )?;
            world_tbl.set(
                "find_mobile",
                self.lua.create_function(
                    |lua, (zone, id): (i32, i32)| -> mlua::Result<Value> {
                        world_find_kind(lua, zone, id, EntityKind::Mob)
                    },
                )?,
            )?;
            // `world.destroy(actor)` despawns the target entity.
            // Mobs destroyed mid-trigger are removed cleanly; any
            // subsequent field access against them returns defaults
            // (since the components are gone). 344 corpus refs.
            // `MobResetCatalog` accounting is unaffected — a
            // reset-spawned mob's reset row will still respawn it
            // on the next refill cycle.
            world_tbl.set(
                "destroy",
                self.lua.create_function(|lua, target: AnyUserData| -> mlua::Result<()> {
                    let entity = target.borrow::<LuaActor>()?.entity;
                    world_mut_from_lua(lua, |world| {
                        if let Ok(em) = world.get_entity_mut(entity) {
                            em.despawn();
                        }
                    })
                })?,
            )?;
            globals.set("world", world_tbl)?;

            // `combat` namespace — engage/rescue. Implemented via
            // direct Fighting component manipulation; the regular
            // combat tick picks up the new pairing on its next pass.
            let combat_tbl = self.lua.create_table()?;
            combat_tbl.set(
                "engage",
                self.lua.create_function(|lua, target: AnyUserData| -> mlua::Result<()> {
                    let target_entity = target.borrow::<LuaActor>()?.entity;
                    world_mut_from_lua(lua, |world| {
                        // `self` (in trigger context) is the engager;
                        // we don't have that entity here. The corpus
                        // calls are always `combat.engage(actor)`
                        // where `self` triggers the engagement, so
                        // bind via the Lua-globals `self` lookup.
                        if let Some(self_ud) =
                            lua.app_data_ref::<SelfEntity>().map(|s| s.0)
                        {
                            world.entity_mut(self_ud).insert(Fighting(target_entity));
                        }
                    })
                })?,
            )?;
            combat_tbl.set(
                "rescue",
                self.lua.create_function(|lua, victim: AnyUserData| -> mlua::Result<()> {
                    let victim_entity = victim.borrow::<LuaActor>()?.entity;
                    world_mut_from_lua(lua, |world| {
                        let Some(self_ent) =
                            lua.app_data_ref::<SelfEntity>().map(|s| s.0)
                        else {
                            return;
                        };
                        // Find any entity attacking the victim — if
                        // exists, swap them onto `self` (we draw aggro)
                        // and have us start fighting them.
                        let mut attackers: Vec<Entity> = Vec::new();
                        {
                            let mut q = world.query::<(Entity, &Fighting)>();
                            for (e, f) in q.iter(world) {
                                if f.0 == victim_entity {
                                    attackers.push(e);
                                }
                            }
                        }
                        if let Some(&attacker) = attackers.first() {
                            world.entity_mut(attacker).insert(Fighting(self_ent));
                            world.entity_mut(self_ent).insert(Fighting(attacker));
                        }
                    })
                })?,
            )?;
            globals.set("combat", combat_tbl)?;

            // `wait(seconds)` is the legacy DG coroutine sleep. The
            // body runs inside a coroutine thread (see below), so
            // `coroutine.yield(N)` parks it. The dispatcher
            // (`tick_yielded`) resumes due threads each tick. Pure
            // Lua so the yield works without mlua's async feature.
            globals.set(
                "wait",
                self.lua
                    .load("local y = coroutine.yield; return function(n) y(n or 1) end")
                    .eval::<Function>()?,
            )?;

            // `mobiles.template(zone, id)` and `objects.template(zone,
            // id)` return a read-only LuaProto userdata wrapping the
            // catalog entry. The corpus uses these as
            // `objects.template(555, 77).name` to get a proto's
            // display name without spawning. 363 + 353 corpus refs.
            let mobiles_tbl = self.lua.create_table()?;
            mobiles_tbl.set(
                "template",
                self.lua.create_function(
                    |lua, (zone, id): (i32, i32)| -> mlua::Result<Value> {
                        Ok(Value::UserData(lua.create_userdata(LuaProto {
                            zone,
                            id,
                            kind: ProtoKind::Mob,
                        })?))
                    },
                )?,
            )?;
            globals.set("mobiles", mobiles_tbl)?;
            let objects_tbl = self.lua.create_table()?;
            objects_tbl.set(
                "template",
                self.lua.create_function(
                    |lua, (zone, id): (i32, i32)| -> mlua::Result<Value> {
                        Ok(Value::UserData(lua.create_userdata(LuaProto {
                            zone,
                            id,
                            kind: ProtoKind::Item,
                        })?))
                    },
                )?,
            )?;
            globals.set("objects", objects_tbl)?;

            // `time` namespace — read-only clock fields. `time.stamp`
            // is Unix epoch seconds (used by FIGHT bodies to throttle
            // their "every 5s" actions); `.hour`/`.day`/`.month`/
            // `.year` come from `MudClock` — advanced one game hour
            // every 750 ticks (~75s real). Total ~32 corpus refs.
            let time_tbl = self.lua.create_table()?;
            let clock = world.get_resource::<mud_world::MudClock>().cloned().unwrap_or_default();
            time_tbl.set("stamp", clock.stamp)?;
            time_tbl.set("hour", i64::from(clock.hour))?;
            time_tbl.set("day", i64::from(clock.day))?;
            time_tbl.set("month", i64::from(clock.month))?;
            time_tbl.set("year", i64::from(clock.year))?;
            // String views of the calendar — let triggers branch on
            // "if time.season == 'Winter' then …" without rebuilding
            // the 16-month name table in every script.
            time_tbl.set("month_name", clock.month_name())?;
            time_tbl.set("season", clock.season().label())?;
            // Day/night convenience flag matches `commands::room_is_dark`'s
            // window (22..=05) so triggers don't have to redo the math.
            let is_night = matches!(clock.hour, 0..=4 | 22..=23);
            time_tbl.set("is_night", is_night)?;
            time_tbl.set("is_day", !is_night)?;
            globals.set("time", time_tbl)?;

            // `find_actor(keyword)` searches the entire world for the
            // first actor (mob or player) whose Named or Keywords
            // match. Returns a LuaActor or nil. 589 corpus refs —
            // typically used by scripted summons that need to find
            // a target by keyword.
            globals.set(
                "find_actor",
                self.lua.create_function(|lua, needle: String| -> mlua::Result<Value> {
                    find_actor(lua, &needle)
                })?,
            )?;

            // `Effect.<Name>` resolves to a lowercased name string,
            // so `actor:has_effect(Effect.Invisible)` matches the
            // EffectCatalog by case-insensitive name. The corpus
            // uses these as effectively-typed enum constants
            // (`Effect.Bless`, `Effect.Sanctuary`, ...). Implemented
            // via metatable __index.
            let effect_tbl = self.lua.create_table()?;
            let effect_meta = self.lua.create_table()?;
            effect_meta.set(
                "__index",
                self.lua.create_function(|_, (_t, key): (Value, String)| -> mlua::Result<String> {
                    Ok(key.to_ascii_lowercase())
                })?,
            )?;
            let _ = effect_tbl.set_metatable(Some(effect_meta));
            globals.set("Effect", effect_tbl)?;

            // `random(low, high)` returns a uniform integer in
            // `[low, high]`. Distinct from Lua's stdlib
            // `math.random` because the corpus uses bare `random(...)`
            // exclusively. 859 corpus refs.
            globals.set(
                "random",
                self.lua.create_function(
                    |_, (low, high): (i64, i64)| -> mlua::Result<i64> {
                        if low > high {
                            return Ok(low);
                        }
                        Ok(rand::random_range(low..=high))
                    },
                )?,
            )?;

            // `percent_chance(N)` returns true with N% probability.
            // 629 corpus refs — typically gates flavor emotes,
            // random combat moves, or ambient room behavior.
            globals.set(
                "percent_chance",
                self.lua.create_function(|_, n: i64| -> mlua::Result<bool> {
                    Ok(rand::random_range(1i64..=100) <= n.clamp(0, 100))
                })?,
            )?;

            // `get_room(zone, id)` returns a LuaRoom by lookup against
            // `WorldKeyIndex.rooms`, or nil if not found. 1019 corpus
            // refs — quest hints, scripted teleports, room reset
            // checks all use this.
            globals.set(
                "get_room",
                self.lua.create_function(
                    |lua, (zone, id): (i32, i32)| -> mlua::Result<Value> {
                        get_room(lua, zone, id)
                    },
                )?,
            )?;

            // Caller-supplied event-context globals (`speech` for
            // SPEECH triggers, etc.). Cleaned up alongside the
            // built-ins below.
            for (name, value) in extras {
                globals.set(*name, *value)?;
            }

            // Wrap the body in a coroutine thread instead of a
            // straight `eval`. `wait(N)` (= coroutine.yield) parks
            // the thread; on Resumable status we hand it back so the
            // outer arm can park it on `self.yielded`. The first
            // returned value (the yield argument or the body's
            // return) is the seconds-to-wait or the return-bool.
            let func: Function = self.lua.load(code).into_function()?;
            let thread = self.lua.create_thread(func)?;
            let values: MultiValue = thread.resume(())?;
            if matches!(thread.status(), ThreadStatus::Resumable) {
                let wait_secs = values
                    .into_iter()
                    .next()
                    .and_then(|v| match v {
                        Value::Integer(i) => Some(i),
                        Value::Number(n) => {
                            #[allow(clippy::cast_possible_truncation)]
                            Some(n as i64)
                        }
                        _ => None,
                    })
                    .unwrap_or(1)
                    .max(1);
                Ok::<(Option<bool>, Option<(Thread, i64)>), mlua::Error>((
                    None,
                    Some((thread, wait_secs)),
                ))
            } else {
                let return_bool = values.into_iter().next().and_then(|v| {
                    if let Value::Boolean(b) = v {
                        Some(b)
                    } else {
                        None
                    }
                });
                Ok((return_bool, None))
            }
        })();

        // Clean up app data so a later call gets a fresh capture.
        let captured = self
            .lua
            .remove_app_data::<LuaCapture>()
            .map(|c| c.lines)
            .unwrap_or_default();
        self.lua.remove_app_data::<WorldPtr>();
        self.lua.remove_app_data::<SelfEntity>();
        // Unbind globals to avoid leaking actor between calls.
        let _ = self.lua.globals().raw_remove("actor");
        let _ = self.lua.globals().raw_remove("self");
        let _ = self.lua.globals().raw_remove("object");
        let _ = self.lua.globals().raw_remove("globals");
        let _ = self.lua.globals().raw_remove("skills");
        let _ = self.lua.globals().raw_remove("world");
        let _ = self.lua.globals().raw_remove("combat");
        let _ = self.lua.globals().raw_remove("wait");
        let _ = self.lua.globals().raw_remove("get_room");
        let _ = self.lua.globals().raw_remove("random");
        let _ = self.lua.globals().raw_remove("percent_chance");
        let _ = self.lua.globals().raw_remove("Effect");
        let _ = self.lua.globals().raw_remove("find_actor");
        let _ = self.lua.globals().raw_remove("mobiles");
        let _ = self.lua.globals().raw_remove("objects");
        let _ = self.lua.globals().raw_remove("time");
        for (name, _) in extras {
            let _ = self.lua.globals().raw_remove(*name);
        }

        match result {
            Ok((return_bool, yield_info)) => {
                let mut out = String::new();
                for line in captured {
                    out.push_str(&line);
                    out.push_str("\r\n");
                }
                if let Some((thread, wait_secs)) = yield_info {
                    #[allow(clippy::cast_sign_loss)]
                    let resume_at_tick = self
                        .current_tick
                        .saturating_add((wait_secs as u64).saturating_mul(TICK_HZ));
                    self.yielded.push(YieldedThread {
                        thread,
                        listener,
                        acting: acting_entity,
                        object: object_entity,
                        extras: extras
                            .iter()
                            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                            .collect(),
                        resume_at_tick,
                    });
                }
                Ok((out, return_bool))
            }
            Err(e) => Err(format!("lua error: {e}")),
        }
    }

    /// Bind every host-supplied global the trigger body can see —
    /// `actor` / `self` / `object`, `print`, `wait`, `world`,
    /// `combat`, `skills`, `Effect`, `mobiles`, `objects`, `time`,
    /// plus any caller-provided extras. Used by both the initial fire
    /// path (above) and `resume_thread` since Lua globals are shared
    /// across threads — any other trigger that fired between yield
    /// and resume would have trampled them.
    #[allow(clippy::too_many_lines)]
    fn bind_globals(
        &self,
        world: &World,
        listener: Entity,
        acting: Entity,
        object: Option<Entity>,
        extras: &[(&str, &str)],
    ) -> mlua::Result<()> {
        let globals = self.lua.globals();
        globals.set("actor", LuaActor { entity: acting })?;
        globals.set("self", LuaActor { entity: listener })?;
        match object {
            Some(e) => globals.set("object", LuaActor { entity: e })?,
            None => globals.set("object", Value::Nil)?,
        }
        globals.set(
            "print",
            self.lua
                .create_function(|lua, args: Variadic<Value>| -> mlua::Result<()> {
                    let line = format_args(&args);
                    if let Some(mut cap) = lua.app_data_mut::<LuaCapture>() {
                        cap.lines.push(line);
                    }
                    Ok(())
                })?,
        )?;
        globals.set("globals", self.lua.create_table()?)?;

        let skills_tbl = self.lua.create_table()?;
        skills_tbl.set(
            "set_level",
            self.lua.create_function(
                |lua, (a, name, level): (AnyUserData, String, i32)| -> mlua::Result<()> {
                    let entity = a.borrow::<LuaActor>()?.entity;
                    skills_set_level(lua, entity, &name, level)
                },
            )?,
        )?;
        globals.set("skills", skills_tbl)?;

        let world_tbl = self.lua.create_table()?;
        world_tbl.set(
            "count_mobiles",
            self.lua
                .create_function(|lua, (zone, id): (i32, i32)| -> mlua::Result<i64> {
                    world_count_kind(lua, zone, id, EntityKind::Mob)
                })?,
        )?;
        world_tbl.set(
            "count_objects",
            self.lua
                .create_function(|lua, (zone, id): (i32, i32)| -> mlua::Result<i64> {
                    world_count_kind(lua, zone, id, EntityKind::Item)
                })?,
        )?;
        world_tbl.set(
            "find_mobile",
            self.lua.create_function(
                |lua, (zone, id): (i32, i32)| -> mlua::Result<Value> {
                    world_find_kind(lua, zone, id, EntityKind::Mob)
                },
            )?,
        )?;
        world_tbl.set(
            "destroy",
            self.lua
                .create_function(|lua, target: AnyUserData| -> mlua::Result<()> {
                    let entity = target.borrow::<LuaActor>()?.entity;
                    world_mut_from_lua(lua, |world| {
                        if let Ok(em) = world.get_entity_mut(entity) {
                            em.despawn();
                        }
                    })
                })?,
        )?;
        globals.set("world", world_tbl)?;

        let combat_tbl = self.lua.create_table()?;
        combat_tbl.set(
            "engage",
            self.lua
                .create_function(|lua, target: AnyUserData| -> mlua::Result<()> {
                    let target_entity = target.borrow::<LuaActor>()?.entity;
                    world_mut_from_lua(lua, |world| {
                        if let Some(self_ud) = lua.app_data_ref::<SelfEntity>().map(|s| s.0) {
                            world.entity_mut(self_ud).insert(Fighting(target_entity));
                        }
                    })
                })?,
        )?;
        combat_tbl.set(
            "rescue",
            self.lua
                .create_function(|lua, victim: AnyUserData| -> mlua::Result<()> {
                    let victim_entity = victim.borrow::<LuaActor>()?.entity;
                    world_mut_from_lua(lua, |world| {
                        let Some(self_ent) = lua.app_data_ref::<SelfEntity>().map(|s| s.0) else {
                            return;
                        };
                        let mut attackers: Vec<Entity> = Vec::new();
                        {
                            let mut q = world.query::<(Entity, &Fighting)>();
                            for (e, f) in q.iter(world) {
                                if f.0 == victim_entity {
                                    attackers.push(e);
                                }
                            }
                        }
                        if let Some(&attacker) = attackers.first() {
                            world.entity_mut(attacker).insert(Fighting(self_ent));
                            world.entity_mut(self_ent).insert(Fighting(attacker));
                        }
                    })
                })?,
        )?;
        globals.set("combat", combat_tbl)?;

        // wait(N) → coroutine.yield(N). Pure-Lua so the yield works
        // without mlua's async feature.
        globals.set(
            "wait",
            self.lua
                .load("local y = coroutine.yield; return function(n) y(n or 1) end")
                .eval::<Function>()?,
        )?;

        let mobiles_tbl = self.lua.create_table()?;
        mobiles_tbl.set(
            "template",
            self.lua
                .create_function(|lua, (zone, id): (i32, i32)| -> mlua::Result<Value> {
                    Ok(Value::UserData(lua.create_userdata(LuaProto {
                        zone,
                        id,
                        kind: ProtoKind::Mob,
                    })?))
                })?,
        )?;
        globals.set("mobiles", mobiles_tbl)?;
        let objects_tbl = self.lua.create_table()?;
        objects_tbl.set(
            "template",
            self.lua
                .create_function(|lua, (zone, id): (i32, i32)| -> mlua::Result<Value> {
                    Ok(Value::UserData(lua.create_userdata(LuaProto {
                        zone,
                        id,
                        kind: ProtoKind::Item,
                    })?))
                })?,
        )?;
        globals.set("objects", objects_tbl)?;

        let time_tbl = self.lua.create_table()?;
        let clock = world
            .get_resource::<mud_world::MudClock>()
            .cloned()
            .unwrap_or_default();
        time_tbl.set("stamp", clock.stamp)?;
        time_tbl.set("hour", i64::from(clock.hour))?;
        time_tbl.set("day", i64::from(clock.day))?;
        time_tbl.set("month", i64::from(clock.month))?;
        time_tbl.set("year", i64::from(clock.year))?;
        globals.set("time", time_tbl)?;

        globals.set(
            "find_actor",
            self.lua
                .create_function(|lua, needle: String| -> mlua::Result<Value> {
                    find_actor(lua, &needle)
                })?,
        )?;

        let effect_tbl = self.lua.create_table()?;
        let effect_meta = self.lua.create_table()?;
        effect_meta.set(
            "__index",
            self.lua
                .create_function(|_, (_t, key): (Value, String)| -> mlua::Result<String> {
                    Ok(key.to_ascii_lowercase())
                })?,
        )?;
        let _ = effect_tbl.set_metatable(Some(effect_meta));
        globals.set("Effect", effect_tbl)?;

        globals.set(
            "random",
            self.lua
                .create_function(|_, (low, high): (i64, i64)| -> mlua::Result<i64> {
                    if low > high {
                        return Ok(low);
                    }
                    Ok(rand::random_range(low..=high))
                })?,
        )?;

        globals.set(
            "percent_chance",
            self.lua
                .create_function(|_, n: i64| -> mlua::Result<bool> {
                    Ok(rand::random_range(1i64..=100) <= n.clamp(0, 100))
                })?,
        )?;

        globals.set(
            "get_room",
            self.lua
                .create_function(|lua, (zone, id): (i32, i32)| -> mlua::Result<Value> {
                    get_room(lua, zone, id)
                })?,
        )?;

        for (name, value) in extras {
            globals.set(*name, *value)?;
        }
        Ok(())
    }

    /// Inverse of `bind_globals` — clears every binding so the next
    /// fire's globals start fresh.
    fn unbind_globals(&self, extras: &[(&str, &str)]) {
        let g = self.lua.globals();
        let _ = g.raw_remove("actor");
        let _ = g.raw_remove("self");
        let _ = g.raw_remove("object");
        let _ = g.raw_remove("print");
        let _ = g.raw_remove("globals");
        let _ = g.raw_remove("skills");
        let _ = g.raw_remove("world");
        let _ = g.raw_remove("combat");
        let _ = g.raw_remove("wait");
        let _ = g.raw_remove("get_room");
        let _ = g.raw_remove("random");
        let _ = g.raw_remove("percent_chance");
        let _ = g.raw_remove("Effect");
        let _ = g.raw_remove("find_actor");
        let _ = g.raw_remove("mobiles");
        let _ = g.raw_remove("objects");
        let _ = g.raw_remove("time");
        for (name, _) in extras {
            let _ = g.raw_remove(*name);
        }
    }
}

#[derive(Default)]
struct LuaCapture {
    lines: Vec<String>,
}

/// Stash of the active trigger's `self` entity, accessible to
/// callbacks that need to act on the listener (e.g. `combat.engage`,
/// `combat.rescue`) without taking a userdata argument.
#[derive(Clone, Copy)]
struct SelfEntity(Entity);

/// Raw pointer wrapper so Lua callbacks can reach the world.
#[derive(Clone, Copy)]
struct WorldPtr(NonNull<World>);
// Only used on the thread that holds `&mut World` while a LuaHost call is
// in progress; we never share Lua state across threads.
unsafe impl Send for WorldPtr {}
unsafe impl Sync for WorldPtr {}

fn world_from_lua<R>(lua: &Lua, f: impl FnOnce(&World) -> R) -> mlua::Result<R> {
    let ptr = lua
        .app_data_ref::<WorldPtr>()
        .ok_or_else(|| mlua::Error::external("no world bound to Lua state"))?;
    // Safety: WorldPtr is set by exec_for_actor only while it holds
    // &mut World; the &World produced here lives only inside `f` and we
    // don't re-enter Lua before the function returns.
    let world = unsafe { ptr.0.as_ref() };
    Ok(f(world))
}

fn world_mut_from_lua<R>(lua: &Lua, f: impl FnOnce(&mut World) -> R) -> mlua::Result<R> {
    let ptr = lua
        .app_data_ref::<WorldPtr>()
        .ok_or_else(|| mlua::Error::external("no world bound to Lua state"))?;
    let mut p = ptr.0;
    drop(ptr);
    // Safety: same invariant as `world_from_lua` — exclusive access to
    // World is held by exec_for_actor for the duration of the Lua call,
    // and Lua callbacks never re-enter (no coroutine yielding into the
    // host yet).
    let world = unsafe { p.as_mut() };
    Ok(f(world))
}

/// Upsert a `KnownAbilities` row on `entity`. If the entity has no
/// `KnownAbilities` component yet (mobs typically don't), insert one.
/// Names are matched case-insensitively against `AbilityCatalog.by_name`
/// (the lowercased plain-name index). Unknown names are a no-op so
/// imported triggers don't crash on data drift.
fn skills_set_level(lua: &Lua, entity: Entity, name: &str, level: i32) -> mlua::Result<()> {
    world_mut_from_lua(lua, |world| {
        let key = name.trim().to_ascii_lowercase();
        let Some(ability_id) = world
            .resource::<AbilityCatalog>()
            .by_name
            .get(&key)
            .map(|d| d.id)
        else {
            return;
        };
        let known = if let Some(existing) = world.get_mut::<KnownAbilities>(entity) {
            existing
        } else {
            world.entity_mut(entity).insert(KnownAbilities::default());
            world
                .get_mut::<KnownAbilities>(entity)
                .expect("just inserted")
        };
        let mut known = known;
        if let Some(slot) = known.entries.iter_mut().find(|(id, _, _)| *id == ability_id) {
            slot.1 = level;
            slot.2 = true;
        } else {
            known.entries.push((ability_id, level, true));
            known.entries.sort_by_key(|(id, _, _)| *id);
        }
    })
}

/// Emit a per-recipient line: the speaker sees `self_line`, every
/// other player in the room sees `room_line`. Backed by `LuaOutbox`'s
/// `direct` queue (for the speaker) plus a `messages` push with
/// the speaker as the except-recipient (for the room broadcast).
/// Used by `actor:say` and `actor:emote` so the speaker reads
/// "You say, '...'" rather than the third-person form.
fn actor_emit_with_perspective(
    lua: &Lua,
    actor: Entity,
    self_fmt: impl FnOnce(&str) -> String,
    room_fmt: impl FnOnce(&str) -> String,
) -> mlua::Result<()> {
    world_mut_from_lua(lua, |world| {
        let Some(room) = world.get::<Located>(actor).map(|l| l.0) else {
            return;
        };
        let name = world
            .get::<Named>(actor)
            .map_or_else(|| "Someone".to_string(), |n| n.name.clone());
        let self_line = self_fmt(&name);
        let room_line = room_fmt(&name);
        if !world.contains_resource::<LuaOutbox>() {
            world.insert_resource(LuaOutbox::default());
        }
        let mut out = world.resource_mut::<LuaOutbox>();
        out.direct.push((actor, self_line));
        out.messages.push((room, room_line, Some(actor)));
    })
}

/// Walk every non-Item entity in the world and return the first
/// whose `Named` or `Keywords` match `needle` (case-insensitive
/// substring). Returns nil if none.
fn find_actor(lua: &Lua, needle: &str) -> mlua::Result<Value> {
    let needle = needle.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Ok(Value::Nil);
    }
    let entity = world_mut_from_lua(lua, |world| -> Option<Entity> {
        let mut q = world.query_filtered::<
            (Entity, &Named, Option<&Keywords>),
            Without<Item>,
        >();
        q.iter(world)
            .find(|(_, n, kw)| {
                n.name.to_ascii_lowercase().contains(&needle)
                    || kw.is_some_and(|k| {
                        k.0.iter().any(|w| w.to_ascii_lowercase().contains(&needle))
                    })
            })
            .map(|(e, _, _)| e)
    })?;
    match entity {
        Some(e) => Ok(Value::UserData(lua.create_userdata(LuaActor { entity: e })?)),
        None => Ok(Value::Nil),
    }
}

/// Despawn items in `actor`'s inventory matching `needle`. The
/// needle can be a bare keyword (despawns the first match) or
/// `all.<keyword>` (despawns every match). Lookup is case-insensitive
/// against each candidate's `Keywords` and `Named.name`.
fn destroy_item(lua: &Lua, actor: Entity, needle: &str) -> mlua::Result<()> {
    let trimmed = needle.trim();
    let (all, keyword) = if let Some(rest) = trimmed.strip_prefix("all.") {
        (true, rest.to_ascii_lowercase())
    } else {
        (false, trimmed.to_ascii_lowercase())
    };
    if keyword.is_empty() {
        return Ok(());
    }
    world_mut_from_lua(lua, |world| {
        let mut to_remove: Vec<Entity> = Vec::new();
        {
            let mut q = world.query_filtered::<
                (Entity, &Located, Option<&Keywords>, &Named),
                With<Item>,
            >();
            for (e, l, kw, n) in q.iter(world) {
                if l.0 != actor {
                    continue;
                }
                let matches = n.name.to_ascii_lowercase().contains(&keyword)
                    || kw.is_some_and(|k| {
                        k.0.iter().any(|w| w.to_ascii_lowercase().contains(&keyword))
                    });
                if matches {
                    to_remove.push(e);
                    if !all {
                        break;
                    }
                }
            }
        }
        for e in to_remove {
            if let Ok(em) = world.get_entity_mut(e) {
                em.despawn();
            }
        }
    })
}

/// Look up a room by `(zone, id)` via `WorldKeyIndex.rooms` and
/// return a `LuaRoom` userdata, or nil if not found.
fn get_room(lua: &Lua, zone: i32, id: i32) -> mlua::Result<Value> {
    let entity = world_from_lua(lua, |world| {
        world.resource::<WorldKeyIndex>().rooms.get(&(zone, id)).copied()
    })?;
    match entity {
        Some(e) => Ok(Value::UserData(lua.create_userdata(LuaRoom { entity: e })?)),
        None => Ok(Value::Nil),
    }
}

/// Filter for `world_count_kind` / `world_find_kind`. Distinguishes
/// "count me a mob" from "count me an item" without needing two
/// near-identical helpers.
#[derive(Clone, Copy)]
enum EntityKind {
    Mob,
    Item,
}

/// Count entities of `kind` whose `WorldKey == (zone, id)`. Used by
/// `world.count_mobiles` / `world.count_objects`.
fn world_count_kind(lua: &Lua, zone: i32, id: i32, kind: EntityKind) -> mlua::Result<i64> {
    use mud_world::Item;
    world_mut_from_lua(lua, |world| {
        let mut count: i64 = 0;
        match kind {
            EntityKind::Mob => {
                let mut q = world.query_filtered::<&WorldKey, With<Mob>>();
                for wk in q.iter(world) {
                    if wk.zone == zone && wk.id == id {
                        count += 1;
                    }
                }
            }
            EntityKind::Item => {
                let mut q = world.query_filtered::<&WorldKey, With<Item>>();
                for wk in q.iter(world) {
                    if wk.zone == zone && wk.id == id {
                        count += 1;
                    }
                }
            }
        }
        count
    })
}

/// Return the first entity of `kind` matching `(zone, id)` as a
/// `LuaActor` userdata. Returns nil if none. Used by
/// `world.find_mobile`.
fn world_find_kind(lua: &Lua, zone: i32, id: i32, kind: EntityKind) -> mlua::Result<Value> {
    use mud_world::Item;
    let entity = world_mut_from_lua(lua, |world| -> Option<Entity> {
        match kind {
            EntityKind::Mob => {
                let mut q = world.query_filtered::<(Entity, &WorldKey), With<Mob>>();
                q.iter(world)
                    .find(|(_, wk)| wk.zone == zone && wk.id == id)
                    .map(|(e, _)| e)
            }
            EntityKind::Item => {
                let mut q = world.query_filtered::<(Entity, &WorldKey), With<Item>>();
                q.iter(world)
                    .find(|(_, wk)| wk.zone == zone && wk.id == id)
                    .map(|(e, _)| e)
            }
        }
    })?;
    match entity {
        Some(e) => Ok(Value::UserData(lua.create_userdata(LuaActor { entity: e })?)),
        None => Ok(Value::Nil),
    }
}

fn format_args(args: &Variadic<Value>) -> String {
    args.iter()
        .map(|v| match v {
            Value::String(s) => s
                .to_str().map_or_else(|_| "<bad-utf8>".to_string(), |cow| cow.to_string()),
            Value::Integer(n) => n.to_string(),
            Value::Number(n) => format!("{n}"),
            Value::Boolean(b) => b.to_string(),
            Value::Nil => "nil".to_string(),
            other => format!("<{}>", other.type_name()),
        })
        .collect::<Vec<_>>()
        .join("\t")
}

// ---------------------------------------------------------------------------
// LuaActor userdata
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct LuaActor {
    pub entity: Entity,
}

impl UserData for LuaActor {
    #[allow(clippy::too_many_lines)]
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // `name`, `hp`, `max_hp`, `level`, `class`, `shortdesc`,
        // `room`, `id`, `zone_id` are all exposed as fields via the
        // MetaMethod::Index handler below — the imported corpus
        // uses `self.X` exclusively. Adding them as methods would
        // shadow the __index resolution and return the function
        // value (`<function>`) instead of the bound value.
        // `is_player` lives only as a field on __index — the corpus
        // uses `self.is_player` (211 refs), not the method form, and
        // method registration would shadow it. `is_mob` is similar
        // (no field-form refs, but method-form is rare). Both are
        // exposed via __index below.
        // Returns the room name, or nil if the actor isn't in a room.
        methods.add_method(
            "room_name",
            |lua, this, ()| -> mlua::Result<Option<String>> {
                world_from_lua(lua, |w| {
                    w.get::<Located>(this.entity)
                        .and_then(|l| w.get::<Named>(l.0).map(|n| n.name.clone()))
                })
            },
        );
        methods.add_meta_method(MetaMethod::ToString, |lua, this, ()| {
            world_from_lua(lua, |w| {
                let name = w
                    .get::<Named>(this.entity).map_or_else(|| "<unknown>".to_string(), |n| n.name.clone());
                format!("Actor({name})")
            })
        });

        // `actor:has_skill(name)` — true if `KnownAbilities` has the
        // ability identified by lowercased plain name. 77 corpus
        // refs (gating combat moves on character class proficiency).
        methods.add_method("has_skill", |lua, this, name: String| -> mlua::Result<bool> {
            world_from_lua(lua, |w| {
                let key = name.trim().to_ascii_lowercase();
                let Some(id) = w.resource::<AbilityCatalog>().by_name.get(&key).map(|d| d.id)
                else {
                    return false;
                };
                w.get::<KnownAbilities>(this.entity)
                    .is_some_and(|ka| ka.has_any(id))
            })
        });

        // `actor:has_effect(name)` — true if any `EffectInstance`
        // applied to this entity resolves through `EffectCatalog`
        // to a definition whose name matches case-insensitively.
        // 65 corpus refs.
        methods.add_method("has_effect", |lua, this, name: String| -> mlua::Result<bool> {
            world_mut_from_lua(lua, |w| {
                let needle = name.trim().to_ascii_lowercase();
                let mut effect_ids: Vec<i32> = Vec::new();
                {
                    let mut q = w.query::<(&EffectInstance, &AppliedTo)>();
                    for (inst, applied) in q.iter(w) {
                        if applied.0 == this.entity {
                            effect_ids.push(inst.kind);
                        }
                    }
                }
                let catalog = w.resource::<EffectCatalog>();
                effect_ids.iter().any(|id| {
                    catalog
                        .by_id
                        .get(id)
                        .is_some_and(|d| d.name.eq_ignore_ascii_case(&needle))
                })
            })
        });

        // `actor:has_item(zone, id)` — true if the actor has any
        // entity in their inventory (Item Located on actor) whose
        // `WorldKey == (zone, id)`. 65 corpus refs.
        methods.add_method(
            "has_item",
            |lua, this, (zone, id): (i32, i32)| -> mlua::Result<bool> {
                world_mut_from_lua(lua, |w| {
                    let mut q = w.query_filtered::<(&Located, &WorldKey), With<Item>>();
                    q.iter(w).any(|(l, wk)| {
                        l.0 == this.entity && wk.zone == zone && wk.id == id
                    })
                })
            },
        );

        // `actor:has_equipped(zone, id)` — like has_item but the
        // matching item also has an `EquippedSlot` component (worn,
        // not just carried). 141 corpus refs.
        methods.add_method(
            "has_equipped",
            |lua, this, (zone, id): (i32, i32)| -> mlua::Result<bool> {
                world_mut_from_lua(lua, |w| {
                    let mut q = w.query_filtered::<
                        (&Located, &WorldKey),
                        (With<Item>, With<EquippedSlot>),
                    >();
                    q.iter(w).any(|(l, wk)| {
                        l.0 == this.entity && wk.zone == zone && wk.id == id
                    })
                })
            },
        );

        // Quest API stubs. The corpus references these heavily
        // (get_quest_stage 2271, get_quest_var 2007, set_quest_var
        // 727, get_has_completed 344, advance_quest 283, start_quest
        // 92, award_exp 95, complete_quest 76). Real implementation
        // requires loading per-character `CharacterQuest` rows into
        // an ECS component and round-tripping them on save —
        // substantial work deferred to a follow-up. v1 stubs keep
        // trigger bodies from crashing on a nil index.
        methods.add_method(
            "get_quest_stage",
            |_, _this, _: Variadic<Value>| -> mlua::Result<i64> { Ok(0) },
        );
        methods.add_method(
            "get_quest_var",
            |_, _this, _: Variadic<Value>| -> mlua::Result<String> { Ok(String::new()) },
        );
        methods.add_method(
            "get_has_completed",
            |_, _this, _: Variadic<Value>| -> mlua::Result<bool> { Ok(false) },
        );
        methods.add_method(
            "set_quest_var",
            |_, _this, _: Variadic<Value>| -> mlua::Result<()> {
                tracing::warn!("trigger called set_quest_var(...) — stub no-op");
                Ok(())
            },
        );
        methods.add_method(
            "advance_quest",
            |_, _this, _: Variadic<Value>| -> mlua::Result<()> {
                tracing::warn!("trigger called advance_quest(...) — stub no-op");
                Ok(())
            },
        );
        methods.add_method(
            "complete_quest",
            |_, _this, _: Variadic<Value>| -> mlua::Result<()> {
                tracing::warn!("trigger called complete_quest(...) — stub no-op");
                Ok(())
            },
        );
        methods.add_method(
            "start_quest",
            |_, _this, _: Variadic<Value>| -> mlua::Result<()> {
                tracing::warn!("trigger called start_quest(...) — stub no-op");
                Ok(())
            },
        );
        methods.add_method(
            "award_exp",
            |_, _this, _: Variadic<Value>| -> mlua::Result<()> {
                tracing::warn!("trigger called award_exp(...) — stub no-op");
                Ok(())
            },
        );

        // `actor:damage(amount)` subtracts `amount` from this entity's
        // `Health.hp`, capped at 0. 157 corpus refs — typically used
        // by ATTACK / FIGHT triggers to apply scripted damage on top
        // of the regular combat round. Does not trigger death
        // handling here; the next combat tick or cmd_lethal_check
        // picks up `hp <= 0`.
        methods.add_method("damage", |lua, this, amount: i32| -> mlua::Result<()> {
            world_mut_from_lua(lua, |world| {
                if let Some(mut h) = world.get_mut::<Health>(this.entity) {
                    h.hp = (h.hp - amount).max(0);
                }
            })
        });

        // `actor:destroy_item(needle)` removes items from the actor's
        // inventory whose keywords match. `all.X` form despawns
        // every matching item; bare `X` despawns just the first.
        // 172 corpus refs — typically used by DEATH bodies to clean
        // up mob-specific items (e.g. drunk drops their whisky
        // bottle when killed).
        methods.add_method(
            "destroy_item",
            |lua, this, needle: String| -> mlua::Result<()> {
                destroy_item(lua, this.entity, &needle)
            },
        );

        // `actor:spawn_object(zone, id)` materializes an object proto
        // directly into this actor's inventory. Used by mob LOAD
        // bodies to give NPCs starter equipment via
        // `find_actor("name"):spawn_object(zone, id)`. Returns a
        // LuaActor on the new item entity, or nil.
        methods.add_method(
            "spawn_object",
            |lua, this, (zone, id): (i32, i32)| -> mlua::Result<Value> {
                spawn_obj_proto(lua, this.entity, zone, id)
            },
        );

        // `actor:command(line)` queues `line` to be dispatched as if
        // the actor typed it. 2106 corpus refs — used by mob bodies
        // to drive themselves through standard commands ("emote",
        // "wear", "follow", etc.). Queued rather than invoked
        // synchronously: the dispatcher runs after the Lua body
        // returns, avoiding re-entry into the LuaHost (which holds
        // exclusive access to World during exec).
        methods.add_method("command", |lua, this, line: String| -> mlua::Result<()> {
            world_mut_from_lua(lua, |world| {
                if !world.contains_resource::<LuaOutbox>() {
                    world.insert_resource(LuaOutbox::default());
                }
                world
                    .resource_mut::<LuaOutbox>()
                    .commands
                    .push((this.entity, line));
            })
        });

        // `actor:send(msg)` sends a single private line to this
        // entity. Paired with `room:send_except(actor, ...)` for the
        // per-recipient framing pattern (10901 corpus refs). Pushed
        // into `LuaOutbox.direct` and drained directly to the
        // entity's Connection by mud-server.
        methods.add_method("send", |lua, this, msg: String| -> mlua::Result<()> {
            world_mut_from_lua(lua, |world| {
                if !world.contains_resource::<LuaOutbox>() {
                    world.insert_resource(LuaOutbox::default());
                }
                world
                    .resource_mut::<LuaOutbox>()
                    .direct
                    .push((this.entity, msg));
            })
        });

        // `actor:follow(leader)` makes the actor follow `leader`. Used
        // by mob-pack LOAD bodies (e.g. wolf pack) where one alpha
        // spawns followers that move with it. Self-follow and follow-
        // already-set are no-ops; the upstream movement system reads
        // `Follower(leader)` to chain movements.
        methods.add_method(
            "follow",
            |lua, this, leader: AnyUserData| -> mlua::Result<()> {
                let leader_entity = leader.borrow::<LuaActor>()?.entity;
                if leader_entity == this.entity {
                    return Ok(());
                }
                world_mut_from_lua(lua, |world| {
                    world
                        .entity_mut(this.entity)
                        .insert(Follower(leader_entity));
                })
            },
        );

        // `actor:say(msg)` broadcasts "<name> says, '<msg>'" to every
        // player in the actor's current room. 2390 corpus refs —
        // the dominant scripted-speech verb.
        methods.add_method("say", |lua, this, msg: String| -> mlua::Result<()> {
            let m = msg.clone();
            actor_emit_with_perspective(
                lua,
                this.entity,
                move |_name| format!("You say, '{m}'"),
                move |name| format!("{name} says, '{msg}'"),
            )
        });

        // `actor:emote(msg)` broadcasts "<name> <msg>" to the actor's
        // room — third-person free-form action text. Speaker sees
        // "You <msg>" instead of the third-person form. 724 corpus
        // refs.
        methods.add_method("emote", |lua, this, msg: String| -> mlua::Result<()> {
            let m = msg.clone();
            actor_emit_with_perspective(
                lua,
                this.entity,
                move |_name| format!("You {m}"),
                move |name| format!("{name} {msg}"),
            )
        });

        // `actor:teleport(room)` updates the actor's `Located` to
        // point at the room entity. Used by mob LOAD bodies to
        // shuffle spawned mobs into specific rooms without going
        // through movement triggers. Silently no-ops if either side
        // is missing the expected components — corrupted data
        // shouldn't crash the body.
        methods.add_method("teleport", |lua, this, target: AnyUserData| -> mlua::Result<()> {
            let room_entity = target.borrow::<LuaRoom>()?.entity;
            world_mut_from_lua(lua, |world| {
                if let Some(mut loc) = world.get_mut::<Located>(this.entity) {
                    loc.0 = room_entity;
                }
            })
        });

        // Field access (`self.room`, `self.id`, `self.zone_id`,
        // `self.name`, `self.hp`, `self.max_hp`). The DG-Script-converted
        // corpus uses `obj.field` syntax, not `obj:field()`. Returning
        // nil for unknown fields is intentional — the trigger's intent
        // is unfulfilled but the body doesn't crash.
        methods.add_meta_method(
            MetaMethod::Index,
            |lua, this, key: String| -> mlua::Result<Value> {
                match key.as_str() {
                    "room" => {
                        // For room-attached triggers (PREENTRY /
                        // POSTENTRY / RESET), `self` IS the room and
                        // the body uses `self.room:send(...)` to emit.
                        // Returning Located(this).0 would give the
                        // zone — wrong. So when `this` already has a
                        // Room component, return self as a LuaRoom.
                        // Otherwise resolve via Located as usual.
                        use mud_world::Room;
                        let room_entity = world_from_lua(lua, |w| {
                            if w.get::<Room>(this.entity).is_some() {
                                Some(this.entity)
                            } else {
                                w.get::<Located>(this.entity).map(|l| l.0)
                            }
                        })?;
                        match room_entity {
                            Some(e) => Ok(Value::UserData(
                                lua.create_userdata(LuaRoom { entity: e })?,
                            )),
                            None => Ok(Value::Nil),
                        }
                    }
                    "id" => world_from_lua(lua, |w| {
                        Value::Integer(
                            w.get::<WorldKey>(this.entity).map_or(0, |wk| wk.id).into(),
                        )
                    }),
                    "zone_id" => world_from_lua(lua, |w| {
                        Value::Integer(
                            w.get::<WorldKey>(this.entity).map_or(0, |wk| wk.zone).into(),
                        )
                    }),
                    "name" => {
                        let s = world_from_lua(lua, |w| {
                            w.get::<Named>(this.entity)
                                .map(|n| n.name.clone())
                                .unwrap_or_default()
                        })?;
                        Ok(Value::String(lua.create_string(&s)?))
                    }
                    "hp" => world_from_lua(lua, |w| {
                        Value::Integer(w.get::<Health>(this.entity).map_or(0, |h| h.hp).into())
                    }),
                    "max_hp" => world_from_lua(lua, |w| {
                        Value::Integer(w.get::<Health>(this.entity).map_or(0, |h| h.max).into())
                    }),
                    // `actor.gender` reads `Profile.gender` for
                    // players ("male" / "female" / "neutral"). Mobs
                    // currently return empty since their schema
                    // gender column isn't plumbed into MobProto yet
                    // (most legacy mob bodies don't read it from
                    // the mob anyway — they read it from the player).
                    "gender" => {
                        let s = world_from_lua(lua, |w| {
                            w.get::<Profile>(this.entity)
                                .map(|p| p.gender.clone())
                                .unwrap_or_default()
                        })?;
                        Ok(Value::String(lua.create_string(&s)?))
                    }
                    "level" => world_from_lua(lua, |w| {
                        let level = if let Some(p) = w.get::<Profile>(this.entity) {
                            p.level
                        } else if let Some(wk) = w.get::<WorldKey>(this.entity) {
                            // Mobs: source level from the proto catalog
                            // since they don't carry Profile.
                            w.resource::<MobPrototypes>()
                                .by_key
                                .get(&(wk.zone, wk.id))
                                .map_or(0, |p| p.level)
                        } else {
                            0
                        };
                        Value::Integer(level.into())
                    }),
                    // `actor.class` returns the plain_name of the
                    // actor's class (e.g. "warrior"). Players source
                    // via Profile.class_id; mobs source via
                    // MobPrototypes.by_key[WorldKey].class_id.
                    // Empty string when no class is assigned. The
                    // corpus uses string compares
                    // ("if actor.class == 'Paladin'") and
                    // string.find for substring matches.
                    "class" => {
                        let s = world_from_lua(lua, |w| {
                            let class_id = if let Some(p) = w.get::<Profile>(this.entity) {
                                p.class_id
                            } else if let Some(wk) = w.get::<WorldKey>(this.entity) {
                                w.resource::<MobPrototypes>()
                                    .by_key
                                    .get(&(wk.zone, wk.id))
                                    .and_then(|p| p.class_id)
                            } else {
                                None
                            };
                            class_id
                                .and_then(|id| {
                                    w.resource::<ClassCatalog>()
                                        .by_id
                                        .get(&id)
                                        .map(|c| c.plain_name.to_ascii_lowercase())
                                })
                                .unwrap_or_default()
                        })?;
                        Ok(Value::String(lua.create_string(&s)?))
                    }
                    // `actor.shortdesc` returns the third-person room
                    // description used in look output. Falls back to
                    // the actor's name when missing. 207 corpus refs.
                    "shortdesc" => {
                        let s = world_from_lua(lua, |w| {
                            w.get::<Description>(this.entity)
                                .map(|d| d.0.clone())
                                .or_else(|| w.get::<Named>(this.entity).map(|n| n.name.clone()))
                                .unwrap_or_default()
                        })?;
                        Ok(Value::String(lua.create_string(&s)?))
                    }
                    // 167 corpus refs: gating against race name in
                    // string compare patterns (`actor.race == "elf"`).
                    "race" => {
                        let s = world_from_lua(lua, |w| {
                            w.get::<Profile>(this.entity)
                                .map(|p| p.race.to_ascii_lowercase())
                                .unwrap_or_default()
                        })?;
                        Ok(Value::String(lua.create_string(&s)?))
                    }
                    // 211 corpus refs.
                    "is_player" => world_from_lua(lua, |w| {
                        Value::Boolean(w.get::<Player>(this.entity).is_some())
                    }),
                    "is_mob" => world_from_lua(lua, |w| {
                        Value::Boolean(w.get::<Mob>(this.entity).is_some())
                    }),
                    // 95 corpus refs — character alignment (good/evil
                    // axis as integer). Sourced from CombatStats so
                    // both players and mobs return their loaded value.
                    "alignment" => world_from_lua(lua, |w| {
                        Value::Integer(
                            w.get::<CombatStats>(this.entity)
                                .map_or(0, |c| c.alignment)
                                .into(),
                        )
                    }),
                    // 42 corpus refs. Returns the posture label
                    // matching the legacy "Position" enum token
                    // ("standing" / "sitting" / "sleeping" / etc.).
                    "position" => {
                        let s = world_from_lua(lua, |w| {
                            w.get::<Posture>(this.entity).map_or_else(
                                || "standing".to_string(),
                                |p| {
                                    match p.0 {
                                        PostureKind::Standing => "standing",
                                        PostureKind::Sitting => "sitting",
                                        PostureKind::Resting => "resting",
                                        PostureKind::Sleeping => "sleeping",
                                        PostureKind::Kneeling => "kneeling",
                                    }
                                    .to_string()
                                },
                            )
                        })?;
                        Ok(Value::String(lua.create_string(&s)?))
                    }
                    // 34 corpus refs.
                    "is_fighting" => world_from_lua(lua, |w| {
                        Value::Boolean(w.get::<mud_world::Fighting>(this.entity).is_some())
                    }),
                    // 62 corpus refs — gender-keyed pronoun ("his" /
                    // "her" / "its"). Sourced from Profile.gender
                    // (players); mobs return "its" by default.
                    "possessive" => {
                        let s = world_from_lua(lua, |w| {
                            w.get::<Profile>(this.entity)
                                .map_or("its", |p| match p.gender.as_str() {
                                    "male" => "his",
                                    "female" => "her",
                                    _ => "its",
                                })
                                .to_string()
                        })?;
                        Ok(Value::String(lua.create_string(&s)?))
                    }
                    _ => Ok(Value::Nil),
                }
            },
        );
    }
}

// ---------------------------------------------------------------------------
// LuaRoom userdata
// ---------------------------------------------------------------------------

/// A reference to a Room entity, returned by `actor.room`. Today its
/// only method is `:send(msg)` — broadcast a line to every player in
/// the room. Bodies enqueue into `LuaOutbox`; mud-server drains and
/// emits after the Lua call returns.
#[derive(Clone, Copy)]
pub struct LuaRoom {
    pub entity: Entity,
}

impl UserData for LuaRoom {
    #[allow(clippy::too_many_lines)]
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("send", |lua, this, msg: String| -> mlua::Result<()> {
            world_mut_from_lua(lua, |world| {
                if !world.contains_resource::<LuaOutbox>() {
                    world.insert_resource(LuaOutbox::default());
                }
                world
                    .resource_mut::<LuaOutbox>()
                    .messages
                    .push((this.entity, msg, None));
            })
        });

        // `room:send_except(target, msg)` broadcasts to every player
        // in the room except `target`. 791 corpus refs — typically
        // used to give the player a different view than everyone
        // else of the same scene (e.g. "X looks at you" vs "X looks
        // at Bob"). Both messages get sent in pairs, one to the
        // target via `actor:send` and the rest of the room via
        // `room:send_except`.
        methods.add_method(
            "send_except",
            |lua, this, (target, msg): (AnyUserData, String)| -> mlua::Result<()> {
                let except = target.borrow::<LuaActor>()?.entity;
                world_mut_from_lua(lua, |world| {
                    if !world.contains_resource::<LuaOutbox>() {
                        world.insert_resource(LuaOutbox::default());
                    }
                    world
                        .resource_mut::<LuaOutbox>()
                        .messages
                        .push((this.entity, msg, Some(except)));
                })
            },
        );

        // `room:spawn_mobile(zone, id)` materializes a fresh Mob from
        // the prototype catalog into this room. Returns a LuaActor on
        // the new entity, or nil if the proto doesn't exist. v1
        // intentionally does NOT fire LOAD on the spawned mob — the
        // dispatcher is currently single-threaded and re-entrant
        // firing would risk infinite recursion (LOAD → spawn → LOAD).
        methods.add_method(
            "spawn_mobile",
            |lua, this, (zone, id): (i32, i32)| -> mlua::Result<Value> {
                spawn_mob_proto(lua, this.entity, zone, id)
            },
        );

        // `room:find_actor(keyword)` searches this room only for an
        // actor matching `keyword`. Returns a LuaActor or nil.
        methods.add_method(
            "find_actor",
            |lua, this, needle: String| -> mlua::Result<Value> {
                let needle = needle.trim().to_ascii_lowercase();
                if needle.is_empty() {
                    return Ok(Value::Nil);
                }
                let entity = world_mut_from_lua(lua, |world| -> Option<Entity> {
                    let mut q = world.query_filtered::<
                        (Entity, &Located, &Named, Option<&Keywords>),
                        Without<Item>,
                    >();
                    q.iter(world)
                        .find(|(_, l, n, kw)| {
                            l.0 == this.entity
                                && (n.name.to_ascii_lowercase().contains(&needle)
                                    || kw.is_some_and(|k| {
                                        k.0.iter().any(|w| {
                                            w.to_ascii_lowercase().contains(&needle)
                                        })
                                    }))
                        })
                        .map(|(e, _, _, _)| e)
                })?;
                match entity {
                    Some(e) => Ok(Value::UserData(lua.create_userdata(LuaActor { entity: e })?)),
                    None => Ok(Value::Nil),
                }
            },
        );

        // `room:spawn_object(zone, id)` materializes a fresh Item from
        // the prototype catalog into this room. Returns a LuaActor on
        // the new item entity, or nil if the proto doesn't exist.
        methods.add_method(
            "spawn_object",
            |lua, this, (zone, id): (i32, i32)| -> mlua::Result<Value> {
                spawn_obj_proto(lua, this.entity, zone, id)
            },
        );

        // `room:weather()` returns the current precip label
        // ("clear", "rain", "blizzard", …) for the room's zone.
        // Returns "clear" when the zone has no entry in the
        // catalog (e.g. planar rooms). Lets triggers branch on
        // weather state without re-implementing the lookup.
        methods.add_method("weather", |lua, this, ()| -> mlua::Result<String> {
            world_from_lua(lua, |w| {
                let zone = w.get::<WorldKey>(this.entity).map(|k| k.zone);
                zone.and_then(|z| {
                    w.get_resource::<mud_world::WeatherCatalog>()
                        .and_then(|c| c.by_zone.get(&z).copied())
                })
                .map_or_else(|| "clear".to_string(), |s| s.precip.label().to_string())
            })
        });

        // `room:temp()` returns the current temperature band label
        // ("frigid", "mild", "sweltering", …) for the room's zone.
        // Same fallback shape as `weather()` — defaults to "mild"
        // for unmapped zones.
        methods.add_method("temp", |lua, this, ()| -> mlua::Result<String> {
            world_from_lua(lua, |w| {
                let zone = w.get::<WorldKey>(this.entity).map(|k| k.zone);
                zone.and_then(|z| {
                    w.get_resource::<mud_world::WeatherCatalog>()
                        .and_then(|c| c.by_zone.get(&z).copied())
                })
                .map_or_else(|| "mild".to_string(), |s| s.temp.label().to_string())
            })
        });

        // `room:sector()` returns the schema sector name in
        // SCREAMING_SNAKE form ("FOREST", "CITY", "UNDERWATER",
        // "AIR", …). Lets triggers branch on terrain without
        // re-implementing the dark-room / outdoor checks.
        methods.add_method("sector", |lua, this, ()| -> mlua::Result<String> {
            world_from_lua(lua, |w| {
                w.get::<mud_world::RoomSector>(this.entity)
                    .map_or_else(|| "STRUCTURE".to_string(), |s| format!("{:?}", s.0).to_uppercase())
            })
        });

        // `room:is_outdoor()` returns true for sectors the weather /
        // dark-room / sky-look systems treat as outdoor. Cave,
        // underdark, underwater, planar, and structure rooms read
        // false. Mirrors the runtime helper, single source of truth.
        methods.add_method("is_outdoor", |lua, this, ()| -> mlua::Result<bool> {
            world_from_lua(lua, |w| {
                w.get::<mud_world::RoomSector>(this.entity)
                    .is_some_and(|s| {
                        use mud_db::enums::Sector;
                        matches!(
                            s.0,
                            Sector::City
                                | Sector::Field
                                | Sector::Forest
                                | Sector::Hills
                                | Sector::Mountain
                                | Sector::Shallows
                                | Sector::Water
                                | Sector::Air
                                | Sector::Road
                                | Sector::Grasslands
                                | Sector::Beach
                                | Sector::Swamp
                                | Sector::Ruins
                        )
                    })
            })
        });

        methods.add_meta_method(MetaMethod::ToString, |lua, this, ()| {
            world_from_lua(lua, |w| {
                let name = w
                    .get::<Named>(this.entity)
                    .map_or_else(|| "<unknown>".to_string(), |n| n.name.clone());
                format!("Room({name})")
            })
        });
    }
}

// ---------------------------------------------------------------------------
// LuaProto userdata (read-only catalog entry)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum ProtoKind {
    Mob,
    Item,
}

/// Read-only handle to a prototype row. Returned by `mobiles.template`
/// and `objects.template` so trigger bodies can inspect proto fields
/// (`.name`, `.id`, `.zone_id`) without spawning.
#[derive(Clone, Copy)]
pub struct LuaProto {
    pub zone: i32,
    pub id: i32,
    kind: ProtoKind,
}

impl UserData for LuaProto {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(
            MetaMethod::Index,
            |lua, this, key: String| -> mlua::Result<Value> {
                match key.as_str() {
                    "id" => Ok(Value::Integer(this.id.into())),
                    "zone_id" => Ok(Value::Integer(this.zone.into())),
                    "name" => {
                        let s = world_from_lua(lua, |w| match this.kind {
                            ProtoKind::Mob => w
                                .resource::<MobPrototypes>()
                                .by_key
                                .get(&(this.zone, this.id))
                                .map(|p| p.name.clone())
                                .unwrap_or_default(),
                            ProtoKind::Item => w
                                .resource::<ObjectPrototypes>()
                                .by_key
                                .get(&(this.zone, this.id))
                                .map(|p| p.name.clone())
                                .unwrap_or_default(),
                        })?;
                        Ok(Value::String(lua.create_string(&s)?))
                    }
                    "shortdesc" => {
                        let s = world_from_lua(lua, |w| match this.kind {
                            ProtoKind::Mob => w
                                .resource::<MobPrototypes>()
                                .by_key
                                .get(&(this.zone, this.id))
                                .map(|p| p.room_description.clone())
                                .unwrap_or_default(),
                            ProtoKind::Item => w
                                .resource::<ObjectPrototypes>()
                                .by_key
                                .get(&(this.zone, this.id))
                                .and_then(|p| p.examine_description.clone())
                                .unwrap_or_default(),
                        })?;
                        Ok(Value::String(lua.create_string(&s)?))
                    }
                    _ => Ok(Value::Nil),
                }
            },
        );
    }
}

/// Materialize a Mob entity from `MobPrototypes` into `room` and
/// return a `LuaActor` wrapping it. Returns nil if the proto isn't
/// in the catalog. Mirrors the loader's Pass 5 mob spawn but skips
/// reset / shop / mountable bookkeeping (script-spawned mobs don't
/// participate in the reset cycle).
fn spawn_mob_proto(lua: &Lua, room: Entity, zone: i32, id: i32) -> mlua::Result<Value> {
    let entity = world_mut_from_lua(lua, |world| -> Option<Entity> {
        let proto = world
            .resource::<MobPrototypes>()
            .by_key
            .get(&(zone, id))
            .cloned()?;
        let trigger_keys = world
            .resource::<TriggerCatalog>()
            .mob_attachments
            .get(&(zone, id))
            .cloned();
        let hp = proto.rolled_hp();
        let dmg = proto.avg_damage();
        let mut em = world.spawn((
            Mob,
            Named { name: proto.name.clone() },
            Keywords(proto.keywords.clone()),
            Description(proto.room_description.clone()),
            WorldKey { zone, id },
            Located(room),
            Health { hp, max: hp },
            CombatStats {
                hit_roll: proto.hit_roll,
                dmg_roll: dmg,
                ac: proto.armor_class,
                alignment: proto.alignment,
            },
            Posture(PostureKind::Standing),
        ));
        if let Some(keys) = trigger_keys {
            em.insert(AttachedTriggers(keys));
        }
        Some(em.id())
    })?;
    match entity {
        Some(e) => Ok(Value::UserData(lua.create_userdata(LuaActor { entity: e })?)),
        None => Ok(Value::Nil),
    }
}

/// Materialize an Item entity from `ObjectPrototypes` into `room`
/// and return a `LuaActor` wrapping it (so triggers can call further
/// methods on it as a generic entity). Returns nil if the proto is
/// missing.
fn spawn_obj_proto(lua: &Lua, room: Entity, zone: i32, id: i32) -> mlua::Result<Value> {
    let entity = world_mut_from_lua(lua, |world| -> Option<Entity> {
        let proto = world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(zone, id))
            .cloned()?;
        let trigger_keys = world
            .resource::<TriggerCatalog>()
            .object_attachments
            .get(&(zone, id))
            .cloned();
        let mut em = world.spawn((
            Item,
            Named { name: proto.name.clone() },
            Keywords(proto.keywords.clone()),
            WorldKey { zone, id },
            Located(room),
        ));
        if let Some(desc) = proto.examine_description.clone() {
            em.insert(Description(desc));
        }
        if let Some(keys) = trigger_keys {
            em.insert(AttachedTriggers(keys));
        }
        Some(em.id())
    })?;
    match entity {
        Some(e) => Ok(Value::UserData(lua.create_userdata(LuaActor { entity: e })?)),
        None => Ok(Value::Nil),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_world_with_actor() -> (World, Entity) {
        let mut world = World::new();
        let actor = world
            .spawn((
                Player,
                Named { name: "TestActor".to_string() },
                Health { hp: 42, max: 100 },
            ))
            .id();
        (world, actor)
    }

    #[test]
    fn wait_yields_and_resumes_on_tick_advance() {
        // Body prints once, waits 2s, prints again. Initial fire
        // captures the pre-yield output and parks; tick_yielded
        // resumes it once current_tick passes the deadline.
        let (mut world, actor) = make_world_with_actor();
        let mut host = LuaHost::new();
        host.set_current_tick(0);
        let body = "print('before')\nwait(2)\nprint('after')";
        let pre = host.exec_for_actor(&mut world, actor, body).expect("ok");
        assert!(pre.contains("before"));
        assert!(!pre.contains("after"), "post-wait line should not have run");
        assert_eq!(host.yielded_count(), 1, "thread should be parked");

        // 2 seconds at 10Hz = 20 ticks. Advance just before the
        // deadline; thread stays parked.
        host.set_current_tick(19);
        let resumed_early = host.tick_yielded(&mut world);
        assert_eq!(resumed_early, 0, "still under wait threshold");
        assert_eq!(host.yielded_count(), 1, "still parked");

        // Cross the threshold; thread resumes and finishes.
        host.set_current_tick(20);
        let resumed = host.tick_yielded(&mut world);
        assert_eq!(resumed, 1);
        assert_eq!(host.yielded_count(), 0, "finished, no longer parked");
    }

    #[test]
    fn actor_name_round_trips() {
        let (mut world, actor) = make_world_with_actor();
        let mut host = LuaHost::new();
        let out = host
            .exec_for_actor(&mut world, actor, "print(actor.name)")
            .expect("ok");
        assert_eq!(out, "TestActor\r\n");
    }

    #[test]
    fn actor_hp_and_max_hp() {
        let (mut world, actor) = make_world_with_actor();
        let mut host = LuaHost::new();
        let out = host
            .exec_for_actor(
                &mut world,
                actor,
                "print(actor.hp .. '/' .. actor.max_hp)",
            )
            .expect("ok");
        assert_eq!(out, "42/100\r\n");
    }

    #[test]
    fn is_player_vs_is_mob() {
        let (mut world, player) = make_world_with_actor();
        let mob = world
            .spawn((Mob, Named { name: "Goblin".to_string() }))
            .id();
        let mut host = LuaHost::new();
        let player_out = host
            .exec_for_actor(
                &mut world,
                player,
                "print(actor.is_player, actor.is_mob)",
            )
            .expect("ok");
        // Lua's print joins multiple args with tab.
        assert_eq!(player_out, "true\tfalse\r\n");
        let mob_out = host
            .exec_for_actor(
                &mut world,
                mob,
                "print(actor.is_player, actor.is_mob)",
            )
            .expect("ok");
        assert_eq!(mob_out, "false\ttrue\r\n");
    }

    #[test]
    fn room_name_nil_when_unplaced() {
        let (mut world, actor) = make_world_with_actor();
        let mut host = LuaHost::new();
        let out = host
            .exec_for_actor(&mut world, actor, "print(actor:room_name())")
            .expect("ok");
        // No Located component → room_name returns nil; print renders as "nil".
        assert_eq!(out, "nil\r\n");
    }

    #[test]
    fn syntax_error_returns_lua_error_string() {
        let (mut world, actor) = make_world_with_actor();
        let mut host = LuaHost::new();
        let err = host
            .exec_for_actor(&mut world, actor, "this is not valid lua")
            .expect_err("syntax error expected");
        assert!(
            err.starts_with("lua error:"),
            "error string starts with prefix: got {err}"
        );
    }

    #[test]
    fn multi_print_concatenates_lines() {
        let (mut world, actor) = make_world_with_actor();
        let mut host = LuaHost::new();
        let out = host
            .exec_for_actor(
                &mut world,
                actor,
                "print('one'); print('two'); print('three')",
            )
            .expect("ok");
        assert_eq!(out, "one\r\ntwo\r\nthree\r\n");
    }

    #[test]
    fn each_call_clears_actor_global() {
        let (mut world, actor) = make_world_with_actor();
        let mut host = LuaHost::new();
        // First call binds actor; second should rebind, but if the first
        // somehow leaked, the second call could still see the first's actor.
        // Both calls should print the SAME actor's name (TestActor).
        let _ = host.exec_for_actor(&mut world, actor, "print(actor.name)").unwrap();
        let out2 = host
            .exec_for_actor(&mut world, actor, "print(actor.name)")
            .expect("ok");
        assert_eq!(out2, "TestActor\r\n");
    }

    #[test]
    fn room_name_returns_named_room_when_located() {
        let mut world = World::new();
        let room = world
            .spawn(Named { name: "Town Center".to_string() })
            .id();
        let actor = world
            .spawn((
                Named { name: "TestActor".to_string() },
                Health { hp: 1, max: 1 },
                Located(room),
            ))
            .id();
        let mut host = LuaHost::new();
        let out = host
            .exec_for_actor(&mut world, actor, "print(actor:room_name())")
            .expect("ok");
        assert_eq!(out, "Town Center\r\n");
    }

    #[test]
    fn tostring_renders_actor_with_name() {
        let (mut world, actor) = make_world_with_actor();
        let mut host = LuaHost::new();
        let out = host
            .exec_for_actor(&mut world, actor, "print(tostring(actor))")
            .expect("ok");
        assert_eq!(out, "Actor(TestActor)\r\n");
    }

    #[test]
    fn globals_table_is_writable_within_call() {
        let (mut world, actor) = make_world_with_actor();
        let mut host = LuaHost::new();
        // The trigger corpus uses `globals.x = globals.x or true` patterns.
        // v1 is a per-call empty table — within the body, reads after writes
        // should round-trip.
        let out = host
            .exec_for_actor(
                &mut world,
                actor,
                "globals.flag = 'hello'; print(globals.flag)",
            )
            .expect("ok");
        assert_eq!(out, "hello\r\n");
    }

    #[test]
    fn globals_does_not_persist_across_calls() {
        let (mut world, actor) = make_world_with_actor();
        let mut host = LuaHost::new();
        let _ = host
            .exec_for_actor(&mut world, actor, "globals.flag = 'set-once'")
            .unwrap();
        let out = host
            .exec_for_actor(&mut world, actor, "print(tostring(globals.flag))")
            .expect("ok");
        // Per-call table → fresh empty table → field is nil.
        assert_eq!(out, "nil\r\n");
    }

    #[test]
    fn skills_set_level_unknown_ability_is_noop() {
        let (mut world, actor) = make_world_with_actor();
        // No AbilityCatalog inserted → all lookups miss → no error.
        world.insert_resource(AbilityCatalog::default());
        let mut host = LuaHost::new();
        let out = host
            .exec_for_actor(
                &mut world,
                actor,
                "skills.set_level(self, 'no-such-ability', 100); print('ok')",
            )
            .expect("ok");
        assert_eq!(out, "ok\r\n");
        // No KnownAbilities component should have been inserted for an
        // unknown ability — the no-op path doesn't create state.
        assert!(world.get::<KnownAbilities>(actor).is_none());
    }

    #[test]
    fn self_binding_aliases_actor() {
        let (mut world, actor) = make_world_with_actor();
        let mut host = LuaHost::new();
        let out = host
            .exec_for_actor(&mut world, actor, "print(self.name)")
            .expect("ok");
        assert_eq!(out, "TestActor\r\n");
    }

    #[test]
    fn missing_components_have_safe_defaults() {
        // Spawn an actor with NO Named, NO Health — the bindings should
        // return empty/zero rather than panic.
        let mut world = World::new();
        let actor = world.spawn_empty().id();
        let mut host = LuaHost::new();
        let name = host
            .exec_for_actor(&mut world, actor, "print(actor.name)")
            .expect("ok");
        // Missing Named → empty string, print emits a bare "\r\n" line.
        assert_eq!(name, "\r\n");
        let hp = host
            .exec_for_actor(&mut world, actor, "print(actor.hp .. ',' .. actor.max_hp)")
            .expect("ok");
        assert_eq!(hp, "0,0\r\n");
        let tostring = host
            .exec_for_actor(&mut world, actor, "print(tostring(actor))")
            .expect("ok");
        // Named missing falls back to "<unknown>".
        assert_eq!(tostring, "Actor(<unknown>)\r\n");
    }
}
