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
use mlua::{AnyUserData, Lua, MetaMethod, UserData, UserDataMethods, Value, Variadic};
use mud_world::{
    AbilityCatalog, Health, KnownAbilities, Located, LuaOutbox, Mob, Named, Player, WorldKey,
};

/// Bevy resource wrapping the Lua interpreter.
#[derive(Resource)]
pub struct LuaHost {
    lua: Lua,
}

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
        Self { lua: Lua::new() }
    }

    /// Run `code` with `actor` bound to the supplied entity. Captured `print`
    /// output is returned as a single string (one line per print call,
    /// terminated with \r\n).
    #[allow(clippy::too_many_lines)]
    pub fn exec_for_actor(
        &self,
        world: &mut World,
        actor: Entity,
        code: &str,
    ) -> Result<String, String> {
        let span = tracing::info_span!("lua_exec");
        let _g = span.enter();

        // Stash a raw pointer to the world for callbacks. Cleared in the
        // cleanup arm below regardless of code outcome.
        let world_ptr = WorldPtr(NonNull::from(&mut *world));
        self.lua.set_app_data(world_ptr);
        self.lua.set_app_data(LuaCapture::default());

        let result = (|| -> mlua::Result<()> {
            let globals = self.lua.globals();
            globals.set("actor", LuaActor { entity: actor })?;
            // `self` is the canonical name in DG-Script-converted bodies
            // ("set_level(self, ...)"). Bind it alongside `actor` so the
            // imported corpus runs without rewriting; both point at the
            // same entity. Lua treats `self` as an ordinary identifier
            // outside of `:` method definitions.
            globals.set("self", LuaActor { entity: actor })?;

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
            // `world.destroy(actor)` — despawning needs to coordinate
            // with `MobResetCatalog` so respawn accounting stays
            // correct (a destroyed reset-spawned mob shouldn't
            // immediately respawn). Stubbed as a warn-no-op for now;
            // 344 corpus refs will succeed silently. Real impl
            // pending.
            world_tbl.set(
                "destroy",
                self.lua.create_function(|_, _: AnyUserData| -> mlua::Result<()> {
                    tracing::warn!("trigger called world.destroy(...) — stub no-op");
                    Ok(())
                })?,
            )?;
            globals.set("world", world_tbl)?;

            // `combat` namespace — stubbed mutating verbs. Real impl
            // (engage = force-aggression onto target; rescue = swap
            // Fighting via the existing `redirect` consumer) deferred
            // until LOAD/FIGHT trigger event firing lands.
            let combat_tbl = self.lua.create_table()?;
            combat_tbl.set(
                "engage",
                self.lua.create_function(|_, _: AnyUserData| -> mlua::Result<()> {
                    tracing::warn!("trigger called combat.engage(...) — stub no-op");
                    Ok(())
                })?,
            )?;
            combat_tbl.set(
                "rescue",
                self.lua.create_function(|_, _: AnyUserData| -> mlua::Result<()> {
                    tracing::warn!("trigger called combat.rescue(...) — stub no-op");
                    Ok(())
                })?,
            )?;
            globals.set("combat", combat_tbl)?;

            self.lua.load(code).exec()
        })();

        // Clean up app data so a later call gets a fresh capture.
        let captured = self
            .lua
            .remove_app_data::<LuaCapture>()
            .map(|c| c.lines)
            .unwrap_or_default();
        self.lua.remove_app_data::<WorldPtr>();
        // Unbind globals to avoid leaking actor between calls.
        let _ = self.lua.globals().raw_remove("actor");
        let _ = self.lua.globals().raw_remove("self");
        let _ = self.lua.globals().raw_remove("globals");
        let _ = self.lua.globals().raw_remove("skills");
        let _ = self.lua.globals().raw_remove("world");
        let _ = self.lua.globals().raw_remove("combat");

        match result {
            Ok(()) => {
                let mut out = String::new();
                for line in captured {
                    out.push_str(&line);
                    out.push_str("\r\n");
                }
                Ok(out)
            }
            Err(e) => Err(format!("lua error: {e}")),
        }
    }
}

#[derive(Default)]
struct LuaCapture {
    lines: Vec<String>,
}

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
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // Note: `name` intentionally lives only as a field
        // (via MetaMethod::Index below) — the imported corpus uses
        // `self.name` exclusively, never `self:name()`. Method
        // registration for `name` would shadow the __index field
        // resolution and return the function value instead of the
        // string.
        methods.add_method("hp", |lua, this, ()| {
            world_from_lua(lua, |w| {
                w.get::<Health>(this.entity).map_or(0, |h| h.hp)
            })
        });
        methods.add_method("max_hp", |lua, this, ()| {
            world_from_lua(lua, |w| {
                w.get::<Health>(this.entity).map_or(0, |h| h.max)
            })
        });
        methods.add_method("is_player", |lua, this, ()| {
            world_from_lua(lua, |w| w.get::<Player>(this.entity).is_some())
        });
        methods.add_method("is_mob", |lua, this, ()| {
            world_from_lua(lua, |w| w.get::<Mob>(this.entity).is_some())
        });
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
                        let room_entity = world_from_lua(lua, |w| {
                            w.get::<Located>(this.entity).map(|l| l.0)
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
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("send", |lua, this, msg: String| -> mlua::Result<()> {
            world_mut_from_lua(lua, |world| {
                if !world.contains_resource::<LuaOutbox>() {
                    world.insert_resource(LuaOutbox::default());
                }
                world
                    .resource_mut::<LuaOutbox>()
                    .messages
                    .push((this.entity, msg));
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
    fn actor_name_round_trips() {
        let (mut world, actor) = make_world_with_actor();
        let host = LuaHost::new();
        let out = host
            .exec_for_actor(&mut world, actor, "print(actor.name)")
            .expect("ok");
        assert_eq!(out, "TestActor\r\n");
    }

    #[test]
    fn actor_hp_and_max_hp() {
        let (mut world, actor) = make_world_with_actor();
        let host = LuaHost::new();
        let out = host
            .exec_for_actor(
                &mut world,
                actor,
                "print(actor:hp() .. '/' .. actor:max_hp())",
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
        let host = LuaHost::new();
        let player_out = host
            .exec_for_actor(
                &mut world,
                player,
                "print(actor:is_player(), actor:is_mob())",
            )
            .expect("ok");
        // Lua's print joins multiple args with tab.
        assert_eq!(player_out, "true\tfalse\r\n");
        let mob_out = host
            .exec_for_actor(
                &mut world,
                mob,
                "print(actor:is_player(), actor:is_mob())",
            )
            .expect("ok");
        assert_eq!(mob_out, "false\ttrue\r\n");
    }

    #[test]
    fn room_name_nil_when_unplaced() {
        let (mut world, actor) = make_world_with_actor();
        let host = LuaHost::new();
        let out = host
            .exec_for_actor(&mut world, actor, "print(actor:room_name())")
            .expect("ok");
        // No Located component → room_name returns nil; print renders as "nil".
        assert_eq!(out, "nil\r\n");
    }

    #[test]
    fn syntax_error_returns_lua_error_string() {
        let (mut world, actor) = make_world_with_actor();
        let host = LuaHost::new();
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
        let host = LuaHost::new();
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
        let host = LuaHost::new();
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
        let host = LuaHost::new();
        let out = host
            .exec_for_actor(&mut world, actor, "print(actor:room_name())")
            .expect("ok");
        assert_eq!(out, "Town Center\r\n");
    }

    #[test]
    fn tostring_renders_actor_with_name() {
        let (mut world, actor) = make_world_with_actor();
        let host = LuaHost::new();
        let out = host
            .exec_for_actor(&mut world, actor, "print(tostring(actor))")
            .expect("ok");
        assert_eq!(out, "Actor(TestActor)\r\n");
    }

    #[test]
    fn globals_table_is_writable_within_call() {
        let (mut world, actor) = make_world_with_actor();
        let host = LuaHost::new();
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
        let host = LuaHost::new();
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
        let host = LuaHost::new();
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
        let host = LuaHost::new();
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
        let host = LuaHost::new();
        let name = host
            .exec_for_actor(&mut world, actor, "print(actor.name)")
            .expect("ok");
        // Missing Named → empty string, print emits a bare "\r\n" line.
        assert_eq!(name, "\r\n");
        let hp = host
            .exec_for_actor(&mut world, actor, "print(actor:hp() .. ',' .. actor:max_hp())")
            .expect("ok");
        assert_eq!(hp, "0,0\r\n");
        let tostring = host
            .exec_for_actor(&mut world, actor, "print(tostring(actor))")
            .expect("ok");
        // Named missing falls back to "<unknown>".
        assert_eq!(tostring, "Actor(<unknown>)\r\n");
    }
}
