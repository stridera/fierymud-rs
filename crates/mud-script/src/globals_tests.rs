//! `globals`: the DG `global <var>` store. One table per script-owning
//! entity instance, shared by every trigger that fires on it, kept between
//! fires, gone with the entity. Legacy parity: `ScriptData::global_vars`
//! lives on the mob / object / room and is freed by `extract_script`.

use super::*;

fn spawn_mob(world: &mut World, name: &str) -> Entity {
    let room = world.spawn_empty().id();
    world
        .spawn((
            Mob,
            Named { name: name.into() },
            Health { hp: 10, max: 10 },
            Located(room),
        ))
        .id()
}

fn fire(host: &mut LuaHost, world: &mut World, owner: Entity, body: &str) -> String {
    host.exec_for_listener_with_extras(world, owner, owner, body, &[])
        .unwrap()
        .trim()
        .to_string()
}

fn stored_tables(host: &LuaHost) -> usize {
    match host
        .lua
        .named_registry_value::<Value>(ENTITY_GLOBALS_KEY)
        .unwrap()
    {
        Value::Table(t) => t.pairs::<Value, Value>().count(),
        _ => 0,
    }
}

#[test]
fn starts_empty_and_is_a_plain_table() {
    let mut world = World::new();
    let me = spawn_mob(&mut world, "Druid");
    let mut host = LuaHost::new();
    let out = fire(
        &mut host,
        &mut world,
        me,
        "print(type(globals), next(globals) == nil, getmetatable(globals) == nil)",
    );
    assert_eq!(out, "table\ttrue\ttrue");
}

#[test]
fn two_triggers_on_one_mob_share_state_across_fires() {
    let mut world = World::new();
    let me = spawn_mob(&mut world, "Druid");
    let mut host = LuaHost::new();
    // "Trigger A" (LOAD) declares the variables, "trigger B" (GREET) reads
    // them in a later fire: the private _ENV of A is long gone.
    fire(
        &mut host,
        &mut world,
        me,
        "local wandstep = 4\nglobals.wandstep = wandstep\nglobals.type = 'fire'\nglobals.pylon = {1, 2}",
    );
    let out = fire(
        &mut host,
        &mut world,
        me,
        "print(globals.wandstep, globals.type, #globals.pylon, wandstep)",
    );
    assert_eq!(out, "4\tfire\t2\tnil");
    // Writes by a later fire are visible to the next (DG `set` + `global` again).
    fire(
        &mut host,
        &mut world,
        me,
        "globals.wandstep = globals.wandstep + 1",
    );
    assert_eq!(
        fire(&mut host, &mut world, me, "print(globals.wandstep)"),
        "5"
    );
    // DG `unset`: clearing removes it.
    fire(&mut host, &mut world, me, "globals.wandstep = nil");
    assert_eq!(
        fire(&mut host, &mut world, me, "print(globals.wandstep)"),
        "nil"
    );
}

#[test]
fn different_instances_of_the_same_prototype_do_not_share() {
    let mut world = World::new();
    let a = spawn_mob(&mut world, "Druid");
    let b = spawn_mob(&mut world, "Druid");
    let mut host = LuaHost::new();
    fire(&mut host, &mut world, a, "globals.count = 7");
    assert_eq!(
        fire(&mut host, &mut world, b, "print(globals.count)"),
        "nil"
    );
    fire(&mut host, &mut world, b, "globals.count = 9");
    assert_eq!(fire(&mut host, &mut world, a, "print(globals.count)"), "7");
    assert_eq!(fire(&mut host, &mut world, b, "print(globals.count)"), "9");
}

#[test]
fn actor_is_not_the_owner() {
    // A GREET trigger on the mob run for a player: the table is the mob's.
    let mut world = World::new();
    let mob = spawn_mob(&mut world, "Druid");
    let room = world.spawn_empty().id();
    let player = world
        .spawn((
            Player,
            Named { name: "Bob".into() },
            Health { hp: 10, max: 10 },
            Located(room),
        ))
        .id();
    let mut host = LuaHost::new();
    host.exec_for_listener_with_extras(&mut world, mob, player, "globals.seen = actor.name", &[])
        .unwrap();
    assert_eq!(
        fire(&mut host, &mut world, mob, "print(globals.seen)"),
        "Bob"
    );
    assert_eq!(
        fire(&mut host, &mut world, player, "print(globals.seen)"),
        "nil"
    );
}

#[test]
fn a_respawn_starts_clean_even_when_the_slot_is_recycled() {
    let mut world = World::new();
    let first = spawn_mob(&mut world, "Druid");
    let mut host = LuaHost::new();
    fire(
        &mut host,
        &mut world,
        first,
        "globals.paralysis_victim_1 = 'Bob'",
    );
    world.despawn(first);
    let second = spawn_mob(&mut world, "Druid");
    assert_ne!(first, second);
    assert_eq!(
        fire(
            &mut host,
            &mut world,
            second,
            "print(globals.paralysis_victim_1)"
        ),
        "nil"
    );
}

#[test]
fn despawned_owners_are_swept() {
    let mut world = World::new();
    let mut host = LuaHost::new();
    let mut doomed = Vec::new();
    for _ in 0..(ENTITY_GLOBALS_SWEEP - 1) {
        let e = spawn_mob(&mut world, "Rat");
        fire(&mut host, &mut world, e, "globals.x = 1");
        doomed.push(e);
    }
    assert_eq!(stored_tables(&host), ENTITY_GLOBALS_SWEEP - 1);
    for e in doomed {
        world.despawn(e);
    }
    let keeper = spawn_mob(&mut world, "Keeper");
    fire(&mut host, &mut world, keeper, "globals.x = 2");
    // The sweep ran on this insert: only the live owner is left.
    assert_eq!(stored_tables(&host), 1);
    assert_eq!(fire(&mut host, &mut world, keeper, "print(globals.x)"), "2");
}

#[test]
fn survives_a_wait_within_the_run_and_later_fires_see_the_result() {
    let mut world = World::new();
    let me = spawn_mob(&mut world, "Druid");
    let mut host = LuaHost::new();
    host.set_current_tick(1);
    fire(
        &mut host,
        &mut world,
        me,
        "globals.stage = 1\nwait(1)\nglobals.stage = globals.stage + 1",
    );
    host.set_current_tick(1000);
    host.tick_yielded(&mut world);
    assert_eq!(fire(&mut host, &mut world, me, "print(globals.stage)"), "2");
}

#[test]
fn the_sandbox_is_intact() {
    let mut world = World::new();
    let me = spawn_mob(&mut world, "Druid");
    let mut host = LuaHost::new();
    // Assigning a bare global stays private to the fire.
    fire(&mut host, &mut world, me, "leaked = 1");
    assert_eq!(fire(&mut host, &mut world, me, "print(leaked)"), "nil");
    // The shared table does not reach the environment or the registry.
    let out = fire(
        &mut host,
        &mut world,
        me,
        "print(globals._G, globals.os, globals.io, globals.self, rawget(globals, 'actor'))",
    );
    assert_eq!(out, "nil\tnil\tnil\tnil\tnil");
    // Stdlib stays read-only.
    assert!(
        host.exec_for_listener_with_extras(&mut world, me, me, "string.find = nil", &[])
            .is_err()
    );
}
