//! `require(name)`: sandboxed, database-backed Lua libraries.
//!
//! The standard `require` is stripped by the sandbox (it reads the filesystem). This one
//! resolves `name` against [`ScriptLibraries`] (the `"ScriptLibrary"` table, loaded at boot and
//! by the trigger reload) and follows standard Lua semantics:
//!
//! * the first `require(name)` compiles the source with chunk name `lib:<name>`, runs it once in
//!   a **clean environment** and caches its first return value (`true` when it returns nothing);
//! * later calls, from any trigger in this Lua state, return the same cached value (so the same
//!   table identity: libraries should return data and functions, not per-call mutable state);
//! * a library that fails to compile or run is not cached, so the next `require` retries it, and
//!   the error reaches the trigger's normal error path prefixed with the library name;
//! * `require` of a library that is still loading is a cycle and errors;
//! * a reload ([`ScriptLibraries`] generation change) drops the whole cache.
//!
//! The library environment is built exactly like a trigger environment (same sandboxed globals
//! and read-only standard-library proxies, same stateless API: `world`, `mobiles`, `random`,
//! ...) but holds none of the firing context: no `actor`, `self`, `object`, `speech`, `event`
//! or `globals`, and it is shared by every trigger that requires the library. `time` reads the
//! clock live because the environment outlives the fire that created it.
//!
//! Libraries cannot yield while they load (`require` is a native call and a coroutine cannot
//! yield across it): `wait`, `wait_until` and `wait_hours` raise a clear error then. Functions a
//! library returns run inside the calling trigger's coroutine, so they may `wait()` normally.

use mlua::{Lua, Table, Value};
use mud_world::ScriptLibraries;

use super::{EnvKit, LuaHost, make_time_table, world_from_lua};

/// Named registry slot holding the `name -> module value` table.
const MODULES_KEY: &str = "fierymud_script_modules";

/// Bookkeeping that is plain data (the module values live in the registry table).
#[derive(Default)]
struct ModuleState {
    /// `ScriptLibraries::generation` the cached modules were built from.
    generation: u64,
    /// Libraries currently executing their top level, outermost first.
    loading: Vec<String>,
}

/// Install `require` as a sandbox global (visible, through the environment fall-through, to
/// trigger bodies and to library bodies alike).
pub(crate) fn install(lua: &Lua, env_kit: &EnvKit) -> mlua::Result<()> {
    lua.set_app_data(env_kit.clone());
    lua.set_app_data(ModuleState::default());
    lua.set_named_registry_value(MODULES_KEY, lua.create_table()?)?;
    let require = lua.create_function(|lua, name: String| require(lua, &name))?;
    lua.globals().set("require", require)
}

fn require(lua: &Lua, name: &str) -> mlua::Result<Value> {
    let (generation, source) = world_from_lua(lua, |world| {
        world
            .get_resource::<ScriptLibraries>()
            .map_or((0, None), |libs| {
                (libs.generation(), libs.get(name).map(str::to_owned))
            })
    })?;

    let modules: Table = lua.named_registry_value(MODULES_KEY)?;
    // A reload replaced the library set: forget every module built from the old one.
    let stale = {
        let mut modules_state = state(lua)?;
        let changed = modules_state.generation != generation;
        modules_state.generation = generation;
        changed
    };
    if stale {
        modules.clear()?;
    }

    let cached: Value = modules.raw_get(name)?;
    if !cached.is_nil() {
        return Ok(cached);
    }

    if state(lua)?.loading.iter().any(|n| n == name) {
        return Err(mlua::Error::runtime(format!(
            "circular require of '{name}'"
        )));
    }
    let Some(source) = source else {
        return Err(mlua::Error::runtime(format!("module '{name}' not found")));
    };

    state(lua)?.loading.push(name.to_owned());
    let loaded = load(lua, name, &source);
    state(lua)?.loading.pop();

    let value =
        loaded.map_err(|e| mlua::Error::runtime(format!("error loading library '{name}': {e}")))?;
    // Standard Lua: a module that returns nothing is recorded as `true`.
    let value = if value.is_nil() {
        Value::Boolean(true)
    } else {
        value
    };
    modules.raw_set(name, value.clone())?;
    Ok(value)
}

fn state(lua: &Lua) -> mlua::Result<mlua::AppDataRefMut<'_, ModuleState>> {
    lua.app_data_mut::<ModuleState>()
        .ok_or_else(|| mlua::Error::runtime("require state missing"))
}

/// Compile and run one library body in a clean environment; returns its first result.
fn load(lua: &Lua, name: &str, source: &str) -> mlua::Result<Value> {
    let env = library_env(lua)?;
    let func = lua
        .load(source)
        .set_name(format!("=lib:{name}"))
        .set_environment(env)
        .into_function()?;
    func.call::<Value>(())
}

/// A fresh environment for one library: sandboxed like a trigger's, minus everything that
/// belongs to a particular fire.
fn library_env(lua: &Lua) -> mlua::Result<Table> {
    let kit = lua
        .app_data_ref::<EnvKit>()
        .ok_or_else(|| mlua::Error::runtime("require environment missing"))?
        .clone();
    let env = kit.new_env(lua)?;
    // The trigger fingerprint is per fire; a library has none (`trigger_log` names it "?").
    env.raw_set("__trigger_fp", 0i64)?;
    world_from_lua(lua, |world| LuaHost::bind_api(lua, &env, world))??;

    // The environment outlives the fire that built it, so `time` must not be a snapshot.
    let live = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.set(
        "__index",
        lua.create_function(|lua, (_t, key): (Value, Value)| {
            let snapshot = world_from_lua(lua, |world| make_time_table(lua, world))??;
            snapshot.get::<Value>(key)
        })?,
    )?;
    meta.set(
        "__newindex",
        lua.create_function(|_, _: mlua::MultiValue| -> mlua::Result<()> {
            Err(mlua::Error::runtime("time is read-only"))
        })?,
    )?;
    meta.set("__metatable", false)?;
    live.set_metatable(Some(meta))?;
    env.raw_set("time", live)?;

    guard_yields(lua, &env)?;
    Ok(env)
}

/// `wait` and friends are coroutine yields. While any library's top level is running they
/// would yield across the native `require` call, which Lua cannot do, so they raise a clear
/// error instead. Outside a load (a trigger calling a function the library returned) they are
/// the real thing and park the trigger's coroutine as usual.
fn guard_yields(lua: &Lua, env: &Table) -> mlua::Result<()> {
    let loading = lua.create_function(|lua, ()| {
        Ok(lua
            .app_data_ref::<ModuleState>()
            .and_then(|s| s.loading.last().cloned()))
    })?;
    lua.load(
        r#"
        local env, loading = ...
        for _, fname in ipairs({ "wait", "wait_until", "wait_hours" }) do
            local real = env[fname]
            env[fname] = function(...)
                local lib = loading()
                if lib then
                    error(fname .. "() cannot be called while library '" .. lib
                        .. "' is loading: a library's top level must not wait; "
                        .. "call wait() from a function the trigger calls instead", 2)
                end
                return real(...)
            end
        end
        "#,
    )
    .call::<()>((env.clone(), loading))
}
