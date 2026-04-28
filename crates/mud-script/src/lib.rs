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
use mlua::{Lua, MetaMethod, UserData, UserDataMethods, Value, Variadic};
use mud_world::{Health, Located, Mob, Named, Player};

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

fn format_args(args: &Variadic<Value>) -> String {
    args.iter()
        .map(|v| match v {
            Value::String(s) => s
                .to_str()
                .map(|cow| cow.to_string())
                .unwrap_or_else(|_| "<bad-utf8>".to_string()),
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
        methods.add_method("name", |lua, this, ()| {
            world_from_lua(lua, |w| {
                w.get::<Named>(this.entity)
                    .map(|n| n.name.clone())
                    .unwrap_or_default()
            })
        });
        methods.add_method("hp", |lua, this, ()| {
            world_from_lua(lua, |w| {
                w.get::<Health>(this.entity).map(|h| h.hp).unwrap_or(0)
            })
        });
        methods.add_method("max_hp", |lua, this, ()| {
            world_from_lua(lua, |w| {
                w.get::<Health>(this.entity).map(|h| h.max).unwrap_or(0)
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
                    .get::<Named>(this.entity)
                    .map(|n| n.name.clone())
                    .unwrap_or_else(|| "<unknown>".to_string());
                format!("Actor({name})")
            })
        });
    }
}
