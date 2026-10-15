//! `require(name)`: DB-backed, sandboxed, cached libraries (see `require.rs`).

use super::*;
use mud_world::ScriptLibraries;

fn libs(pairs: &[(&str, &str)]) -> ScriptLibraries {
    ScriptLibraries::from_sources(
        pairs
            .iter()
            .map(|(n, c)| ((*n).to_string(), (*c).to_string())),
    )
}

/// A world with one mob (`self`), one player (`actor`) and the given libraries.
fn world_with(pairs: &[(&str, &str)]) -> (World, Entity, Entity) {
    let mut world = World::new();
    let room = world.spawn_empty().id();
    let me = world
        .spawn((
            Named {
                name: "Warden".into(),
            },
            Keywords(vec!["warden".into()]),
            Located(room),
            Health { hp: 20, max: 20 },
            Mob,
        ))
        .id();
    let bob = world
        .spawn((
            Named { name: "Bob".into() },
            Keywords(vec!["bob".into()]),
            Located(room),
            Health { hp: 20, max: 20 },
            Player,
        ))
        .id();
    world.insert_resource(libs(pairs));
    (world, me, bob)
}

/// Run `body` as a trigger on the shared host; returns the printed lines.
fn run(host: &mut LuaHost, world: &mut World, me: Entity, bob: Entity, body: &str) -> Vec<String> {
    host.exec_for_listener_with_extras(world, me, bob, body, &[])
        .unwrap_or_else(|e| panic!("script failed: {e}"))
        .lines()
        .map(str::to_owned)
        .collect()
}

fn run_err(host: &mut LuaHost, world: &mut World, me: Entity, bob: Entity, body: &str) -> String {
    host.exec_for_listener_with_extras(world, me, bob, body, &[])
        .expect_err("script should have failed")
}

#[test]
fn require_returns_the_libraries_table() {
    let (mut world, me, bob) = world_with(&[(
        "util",
        "local M = {}\nfunction M.double(n) return n * 2 end\nM.name = 'util'\nreturn M",
    )]);
    let mut host = LuaHost::new();
    let out = run(
        &mut host,
        &mut world,
        me,
        bob,
        "local u = require('util')\nprint(u.name, u.double(21))",
    );
    assert_eq!(out, ["util\t42"]);
}

#[test]
fn module_is_cached_and_shared_across_triggers() {
    let (mut world, me, bob) = world_with(&[(
        "counter",
        // Top-level side effect: runs once per Lua state, not once per require.
        "runs = (runs or 0) + 1\nreturn { runs_at_load = 1, hits = 0 }",
    )]);
    let mut host = LuaHost::new();
    let a = run(
        &mut host,
        &mut world,
        me,
        bob,
        "local c = require('counter')\nc.hits = c.hits + 1\nprint(tostring(c), c.hits, require('counter') == c)",
    );
    let b = run(
        &mut host,
        &mut world,
        me,
        bob,
        "local c = require('counter')\nc.hits = c.hits + 1\nprint(tostring(c), c.hits)",
    );
    let (id_a, rest_a) = a[0].split_once('\t').unwrap();
    let (id_b, rest_b) = b[0].split_once('\t').unwrap();
    assert_eq!(id_a, id_b, "the same table is handed to every trigger");
    assert_eq!(rest_a, "1\ttrue");
    assert_eq!(rest_b, "2", "state is shared like standard Lua modules");
}

#[test]
fn a_library_returning_nothing_is_recorded_as_true() {
    let (mut world, me, bob) = world_with(&[("quiet", "local x = 1")]);
    let mut host = LuaHost::new();
    let out = run(
        &mut host,
        &mut world,
        me,
        bob,
        "print(require('quiet'), require('quiet'))",
    );
    assert_eq!(out, ["true\ttrue"]);
}

#[test]
fn reload_invalidates_the_cache() {
    let (mut world, me, bob) = world_with(&[("v", "return { n = 1 }")]);
    let mut host = LuaHost::new();
    let body = "print(require('v').n)";
    assert_eq!(run(&mut host, &mut world, me, bob, body), ["1"]);
    // Unchanged generation: still served from the cache.
    assert_eq!(run(&mut host, &mut world, me, bob, body), ["1"]);
    world.insert_resource(libs(&[("v", "return { n = 2 }")]));
    assert_eq!(run(&mut host, &mut world, me, bob, body), ["2"]);
    // A reload that drops the library makes it unknown again.
    world.insert_resource(libs(&[]));
    let err = run_err(&mut host, &mut world, me, bob, body);
    assert!(err.contains("module 'v' not found"), "{err}");
}

#[test]
fn unknown_module_is_a_clear_error() {
    let (mut world, me, bob) = world_with(&[]);
    let mut host = LuaHost::new();
    let err = run_err(&mut host, &mut world, me, bob, "require('nope')");
    assert!(err.contains("module 'nope' not found"), "{err}");
}

#[test]
fn no_libraries_resource_means_unknown_module() {
    let (mut world, me, bob) = world_with(&[]);
    world.remove_resource::<ScriptLibraries>();
    let mut host = LuaHost::new();
    let err = run_err(&mut host, &mut world, me, bob, "require('x')");
    assert!(err.contains("module 'x' not found"), "{err}");
}

#[test]
fn circular_require_is_detected() {
    let (mut world, me, bob) = world_with(&[
        ("a", "local b = require('b')\nreturn { b = b }"),
        ("b", "local a = require('a')\nreturn { a = a }"),
        ("selfish", "return require('selfish')"),
    ]);
    let mut host = LuaHost::new();
    let err = run_err(&mut host, &mut world, me, bob, "require('a')");
    assert!(err.contains("circular require of 'a'"), "{err}");
    assert!(
        err.contains("library 'b'"),
        "names the library in the chain: {err}"
    );
    let err = run_err(&mut host, &mut world, me, bob, "require('selfish')");
    assert!(err.contains("circular require of 'selfish'"), "{err}");
    // The loading stack was unwound: a normal library still loads afterwards.
    world.insert_resource(libs(&[("ok", "return 1")]));
    assert_eq!(
        run(&mut host, &mut world, me, bob, "print(require('ok'))"),
        ["1"]
    );
}

#[test]
fn a_failing_library_is_not_cached_and_names_itself() {
    // Fails until an actor named "latecomer" exists, then succeeds.
    let (mut world, me, bob) = world_with(&[(
        "late",
        "if not find_actor('latecomer') then error('not ready yet') end\nreturn { ready = true }",
    )]);
    let mut host = LuaHost::new();
    let err = run_err(&mut host, &mut world, me, bob, "require('late')");
    assert!(err.contains("library 'late'"), "{err}");
    assert!(err.contains("lib:late"), "chunk name is lib:<name>: {err}");
    assert!(err.contains("not ready yet"), "{err}");

    // Same generation, so a cached failure would stick; it must retry instead.
    let room = world.get::<Located>(me).unwrap().0;
    world.spawn((
        Named {
            name: "Latecomer".into(),
        },
        Keywords(vec!["latecomer".into()]),
        Located(room),
    ));
    assert_eq!(
        run(
            &mut host,
            &mut world,
            me,
            bob,
            "print(require('late').ready)"
        ),
        ["true"]
    );
}

#[test]
fn a_syntax_error_is_reported_with_the_library_name() {
    let (mut world, me, bob) = world_with(&[("broken", "local x = = 1")]);
    let mut host = LuaHost::new();
    let err = run_err(&mut host, &mut world, me, bob, "require('broken')");
    assert!(err.contains("library 'broken'"), "{err}");
    assert!(err.contains("lib:broken:1"), "{err}");
}

#[test]
fn library_environment_is_clean_and_sandboxed() {
    let (mut world, me, bob) = world_with(&[(
        "probe",
        r"polluted = true
return {
  actor = actor ~= nil, self = self ~= nil, object = object ~= nil,
  speech = speech ~= nil, event = event ~= nil, globals = globals ~= nil,
  secret = secret, leak = leak,
  os = os ~= nil, io = io ~= nil, debug = debug ~= nil, package = package ~= nil,
  dofile = dofile ~= nil, loadfile = loadfile ~= nil, load = load ~= nil,
  require = type(require),
  api = type(mobiles) == 'table' and type(world) == 'table' and type(random) == 'function',
  stdlib = type(string.format) == 'function',
}",
    )]);
    let mut host = LuaHost::new();
    let out = run(
        &mut host,
        &mut world,
        me,
        bob,
        r"local secret = 42
leak = 'trigger global'
local p = require('probe')
local keys = {}
for k, v in pairs(p) do keys[#keys + 1] = k .. '=' .. tostring(v) end
table.sort(keys)
print(table.concat(keys, ' '))
print('polluted in trigger:', polluted)",
    );
    // `table.sort` orders the keys bytewise ("self" < "speech" < "stdlib").
    assert_eq!(
        out[0],
        "actor=false api=true debug=false dofile=false event=false globals=false io=false \
         load=false loadfile=false object=false os=false package=false require=function \
         self=false speech=false stdlib=true"
    );
    assert_eq!(
        out[1], "polluted in trigger:\tnil",
        "library globals stay in the library"
    );
}

#[test]
fn libraries_cannot_mutate_the_shared_standard_library() {
    let (mut world, me, bob) = world_with(&[("evil", "string.find = nil")]);
    let mut host = LuaHost::new();
    let err = run_err(&mut host, &mut world, me, bob, "require('evil')");
    assert!(err.contains("read-only"), "{err}");
    assert_eq!(
        run(&mut host, &mut world, me, bob, "print(type(string.find))"),
        ["function"]
    );
}

#[test]
fn wait_at_a_librarys_top_level_is_a_clear_error() {
    let (mut world, me, bob) = world_with(&[("sleepy", "wait(1)\nreturn {}")]);
    let mut host = LuaHost::new();
    let err = run_err(&mut host, &mut world, me, bob, "require('sleepy')");
    assert!(err.contains("wait()"), "{err}");
    assert!(err.contains("library 'sleepy' is loading"), "{err}");
    assert_eq!(host.yielded_count(), 0);
    // Not cached: a corrected library loads on the next generation.
    world.insert_resource(libs(&[("sleepy", "return {}")]));
    assert_eq!(
        run(
            &mut host,
            &mut world,
            me,
            bob,
            "print(type(require('sleepy')))"
        ),
        ["table"]
    );
}

#[test]
fn wait_inside_a_returned_function_parks_the_calling_trigger() {
    let (mut world, me, bob) = world_with(&[(
        "slow",
        r"return {
  engage_later = function(attacker, target)
    wait(2)
    combat.engage(attacker, target)
  end,
}",
    )]);
    let mut host = LuaHost::new();
    host.set_current_tick(100);
    let out = run(
        &mut host,
        &mut world,
        me,
        bob,
        "require('slow').engage_later(self, actor)\nprint('not reached before the wait')",
    );
    assert!(
        out.is_empty(),
        "the trigger parked inside the library function: {out:?}"
    );
    assert_eq!(host.yielded_count(), 1);
    assert!(world.get::<Fighting>(me).is_none());

    host.set_current_tick(100 + 2 * TICK_HZ);
    assert_eq!(host.tick_yielded(&mut world), 1);
    assert_eq!(world.get::<Fighting>(me).map(|f| f.0), Some(bob));
    assert_eq!(host.yielded_count(), 0);
}

#[test]
fn library_functions_use_the_calling_triggers_context() {
    // The library has no `self`/`actor`, but combat.engage(target) reads the firing entity from
    // the host, so a returned function acts for whichever trigger calls it.
    let (mut world, me, bob) =
        world_with(&[("act", "return { go = function(t) combat.engage(t) end }")]);
    let mut host = LuaHost::new();
    run(&mut host, &mut world, me, bob, "require('act').go(actor)");
    assert_eq!(world.get::<Fighting>(me).map(|f| f.0), Some(bob));
}

#[test]
fn libraries_can_require_libraries() {
    let (mut world, me, bob) = world_with(&[
        ("base", "return { greeting = 'hello' }"),
        (
            "top",
            "local base = require('base')\nreturn { say = function() return base.greeting .. '!' end }",
        ),
    ]);
    let mut host = LuaHost::new();
    assert_eq!(
        run(
            &mut host,
            &mut world,
            me,
            bob,
            "print(require('top').say())"
        ),
        ["hello!"]
    );
}

#[test]
fn time_is_read_live_inside_library_functions() {
    let (mut world, me, bob) =
        world_with(&[("clock", "return { hour = function() return time.hour end }")]);
    world.insert_resource(mud_world::MudClock {
        hour: 3,
        ..Default::default()
    });
    let mut host = LuaHost::new();
    let body = "print(require('clock').hour())";
    assert_eq!(run(&mut host, &mut world, me, bob, body), ["3"]);
    world.resource_mut::<mud_world::MudClock>().hour = 9;
    assert_eq!(run(&mut host, &mut world, me, bob, body), ["9"]);
}

#[test]
fn condition_scripts_cannot_require() {
    let (mut world, me, bob) = world_with(&[("x", "return 1")]);
    let mut host = LuaHost::new();
    let result = host.eval_condition(&mut world, bob, None, "return require('x') == 1");
    assert!(result.is_err(), "{result:?}");
    // And the shadow is per environment: triggers still get it.
    assert_eq!(
        run(&mut host, &mut world, me, bob, "print(require('x'))"),
        ["1"]
    );
}
