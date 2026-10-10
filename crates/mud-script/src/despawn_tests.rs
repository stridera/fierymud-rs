//! A script can hold a handle to an entity that has since despawned (a
//! mob killed during a `wait`, a player who quit, a purged room). Every
//! binding must treat such a handle as "nothing there": no panic, and no
//! Lua error either. The bindings are listed in a table; a second test
//! reads this crate's source and fails if a binding exists that the table
//! does not cover, so a new binding cannot slip in untested.

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::*;

const SRC: &str = include_str!("lib.rs");

struct Fx {
    world: World,
    me: Entity,
    dead: Entity,
    dead_item: Entity,
}

fn fixture() -> Fx {
    let mut world = World::new();
    world.init_resource::<MobPrototypes>();
    world.init_resource::<ObjectPrototypes>();
    world.init_resource::<ClassCatalog>();
    world.init_resource::<EffectCatalog>();
    world.init_resource::<AbilityCatalog>();
    world.init_resource::<EntityVariableCache>();
    world.init_resource::<mud_world::QuestVariableCache>();
    let room_a = world
        .spawn((
            mud_world::Room,
            Named {
                name: "Room A".to_string(),
            },
            WorldKey { zone: 99, id: 1 },
        ))
        .id();
    let room_b = world
        .spawn((
            mud_world::Room,
            Named {
                name: "Room B".to_string(),
            },
            WorldKey { zone: 99, id: 2 },
        ))
        .id();
    let dead_room = world
        .spawn((
            mud_world::Room,
            Named {
                name: "Gone".to_string(),
            },
            WorldKey { zone: 99, id: 3 },
        ))
        .id();
    let mut index = WorldKeyIndex::default();
    index.rooms.insert((99, 1), room_a);
    index.rooms.insert((99, 2), room_b);
    index.rooms.insert((99, 3), dead_room);
    world.insert_resource(index);
    let me = world
        .spawn((
            Mob,
            Named {
                name: "Self Mob".to_string(),
            },
            Health { hp: 10, max: 10 },
            WorldKey { zone: 99, id: 1 },
            Located(room_a),
        ))
        .id();
    let dead = world
        .spawn((
            Mob,
            Named {
                name: "Ghost Mob".to_string(),
            },
            Health { hp: 10, max: 10 },
            WorldKey { zone: 99, id: 1 },
            Located(room_a),
        ))
        .id();
    let dead_item = world
        .spawn((
            Item,
            Named {
                name: "a gone thing".to_string(),
            },
            Located(room_a),
        ))
        .id();
    world.despawn(dead);
    world.despawn(dead_item);
    world.despawn(dead_room);
    Fx {
        world,
        me,
        dead,
        dead_item,
    }
}

/// Run `code` with `self` = a live mob, `actor` = a despawned mob and
/// `object` = a despawned item. `d` / `o` / `DR` / `LR` / `X` / `Q` are
/// pre-bound: dead actor, dead item, dead room, live room, an exit and a
/// quest handle on the dead room / dead player.
fn run_case(code: &str) -> Result<String, String> {
    let mut fx = fixture();
    let mut host = LuaHost::new();
    let globals = host.lua.globals();
    let dead_room = fx
        .world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(99, 3))
        .copied()
        .unwrap();
    let live_room = fx
        .world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(99, 1))
        .copied()
        .unwrap();
    globals.set("d", LuaActor { entity: fx.dead }).unwrap();
    globals
        .set(
            "o",
            LuaActor {
                entity: fx.dead_item,
            },
        )
        .unwrap();
    globals.set("DR", LuaRoom { entity: dead_room }).unwrap();
    globals.set("LR", LuaRoom { entity: live_room }).unwrap();
    globals
        .set(
            "X",
            LuaExit {
                room: dead_room,
                dir: mud_db::enums::Direction::North,
            },
        )
        .unwrap();
    globals
        .set(
            "Q",
            LuaQuest {
                character_id: "gone".to_string(),
                quest_zone: 99,
                quest_id: 1,
            },
        )
        .unwrap();
    let me = fx.me;
    let dead = fx.dead;
    let dead_item = fx.dead_item;
    host.exec_for_event(&mut fx.world, me, dead, Some(dead_item), code, &[])
}

/// `(receiver type, binding, call)`; a call must run without a Lua error.
const METHOD_CASES: &[(&str, &str, &str)] = &[
    ("LuaActor", "room_name", "d:room_name()"),
    ("LuaActor", "whisper", "d:whisper('self', 'hi')"),
    ("LuaActor", "save", "d:save()"),
    ("LuaActor", "set_flag", "d:set_flag('nosummon', true)"),
    ("LuaActor", "has_skill", "d:has_skill('bash')"),
    ("LuaActor", "get_has_spell", "d:get_has_spell('fireball')"),
    ("LuaActor", "has_effect", "d:has_effect('bless')"),
    ("LuaActor", "has_item", "d:has_item(99, 1)"),
    ("LuaActor", "has_equipped", "d:has_equipped(99, 1)"),
    ("LuaActor", "get_worn", "d:get_worn('head')"),
    ("LuaActor", "get_quest_stage", "d:get_quest_stage('q')"),
    ("LuaActor", "get_quest_var", "d:get_quest_var('k')"),
    ("LuaActor", "get_has_completed", "d:get_has_completed('q')"),
    ("LuaActor", "get_has_failed", "d:get_has_failed('q')"),
    (
        "LuaActor",
        "set_quest_var",
        "d:set_quest_var('q', 'k', 'v')",
    ),
    ("LuaActor", "start_quest", "d:start_quest('q')"),
    ("LuaActor", "advance_quest", "d:advance_quest('q')"),
    ("LuaActor", "complete_quest", "d:complete_quest('q')"),
    ("LuaActor", "fail_quest", "d:fail_quest('q')"),
    ("LuaActor", "restart_quest", "d:restart_quest('q')"),
    ("LuaActor", "erase_quest", "d:erase_quest('q')"),
    ("LuaActor", "award_exp", "d:award_exp(10)"),
    ("LuaActor", "damage", "d:damage(5)"),
    ("LuaActor", "chant", "d:chant('peace')"),
    ("LuaActor", "perform", "d:perform('terror')"),
    ("LuaActor", "breath_attack", "d:breath_attack('fire', self)"),
    ("LuaActor", "attack_all", "d:attack_all()"),
    ("LuaActor", "shout", "d:shout('boo')"),
    ("LuaActor", "move", "d:move('north')"),
    ("LuaActor", "heal", "d:heal(5)"),
    ("LuaActor", "destroy_item", "d:destroy_item('sword')"),
    ("LuaActor", "spawn_object", "d:spawn_object(99, 1)"),
    ("LuaActor", "command", "d:command('look')"),
    ("LuaActor", "send", "d:send('hi')"),
    ("LuaActor", "follow", "d:follow(self)"),
    ("LuaActor", "say", "d:say('hi')"),
    ("LuaActor", "emote", "d:emote('grins')"),
    ("LuaActor", "teleport", "d:teleport(LR)"),
    ("LuaActor", "setvar", "d:setvar('k', 1)"),
    ("LuaActor", "getvar", "d:getvar('k')"),
    ("LuaActor", "clearvar", "d:clearvar('k')"),
    ("LuaActor", "active_quest", "d:active_quest(99, 1)"),
    ("LuaActor", "is_god", "d:is_god()"),
    ("LuaQuest", "getvar", "Q:getvar('k')"),
    ("LuaQuest", "setvar", "Q:setvar('k', 1)"),
    ("LuaQuest", "clearvar", "Q:clearvar('k')"),
    ("LuaRoom", "send", "DR:send('hi')"),
    ("LuaRoom", "setvar", "DR:setvar('k', 1)"),
    ("LuaRoom", "getvar", "DR:getvar('k')"),
    ("LuaRoom", "clearvar", "DR:clearvar('k')"),
    ("LuaRoom", "send_except", "DR:send_except(d, 'hi')"),
    ("LuaRoom", "spawn_mobile", "DR:spawn_mobile(99, 1)"),
    ("LuaRoom", "find_actor", "DR:find_actor('mob')"),
    ("LuaRoom", "spawn_object", "DR:spawn_object(99, 1)"),
    ("LuaRoom", "send_to_adjacent", "DR:send_to_adjacent('hi')"),
    ("LuaRoom", "purge", "DR:purge()"),
    ("LuaRoom", "teleport_all", "DR:teleport_all(LR)"),
    ("LuaRoom", "find_object", "DR:find_object('thing')"),
    ("LuaRoom", "weather", "DR:weather()"),
    ("LuaRoom", "temp", "DR:temp()"),
    ("LuaRoom", "sector", "DR:sector()"),
    ("LuaRoom", "is_outdoor", "DR:is_outdoor()"),
    ("LuaRoom", "at", "DR:at(function() end)"),
    ("LuaRoom", "exit", "DR:exit('north')"),
    ("LuaExit", "state", "X:state()"),
    ("LuaExit", "hidden", "X:hidden()"),
    ("LuaExit", "set_state", "X:set_state({ closed = true })"),
    ("LuaExit", "set_key", "X:set_key(99, 1)"),
    ("LuaExit", "set_destination", "X:set_destination(LR)"),
];

/// Calls that pass a despawned entity as an argument to a live receiver,
/// plus the global functions.
const ARG_CASES: &[&str] = &[
    "self:follow(d)",
    "self:teleport(DR)",
    "self:breath_attack('fire', d)",
    "self:whisper('Ghost', 'hi')",
    "LR:send_except(d, 'hi')",
    "LR:teleport_all(DR)",
    "LR:find_actor('ghost')",
    "skills.set_level(d, 'bash', 5)",
    "skills.execute(d, 'kick', self)",
    "skills.execute(self, 'kick', d)",
    "spells.cast(d, 'fireball')",
    "spells.cast(self, 'fireball', d)",
    "world.destroy(d)",
    "world.destroy(o)",
    "combat.engage(d)",
    "combat.engage(self, d)",
    "combat.engage(d, self)",
    "combat.rescue(d)",
    "combat.rescue(self, d)",
    "run_room_trigger(99, 3)",
    "print(tostring(d), tostring(DR), tostring(o))",
    "print(find_actor('ghost'), get_room(99, 3))",
    "print(object.name, object.id, actor.hp, actor.room)",
];

#[test]
fn every_binding_tolerates_despawned_entities() {
    let mut failures = Vec::new();
    let cases = METHOD_CASES
        .iter()
        .map(|(_, _, code)| *code)
        .chain(ARG_CASES.iter().copied());
    for code in cases {
        match catch_unwind(AssertUnwindSafe(|| run_case(code))) {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => failures.push(format!("{code}: lua error: {e}")),
            Err(_) => failures.push(format!("{code}: PANIC")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Field-style reads (`d.name`, `DR.id`, ...) go through the `Index`
/// metamethod; its keys are read straight from the source so a new key is
/// exercised without touching this file.
#[test]
fn every_index_key_tolerates_despawned_entities() {
    let mut failures = Vec::new();
    for (ty, var) in [("LuaActor", "d"), ("LuaRoom", "DR")] {
        for key in index_keys(ty) {
            let code = format!("local _ = {var}[{key:?}]");
            match catch_unwind(AssertUnwindSafe(|| run_case(&code))) {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => failures.push(format!("{ty}.{key}: lua error: {e}")),
                Err(_) => failures.push(format!("{ty}.{key}: PANIC")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The crate source up to its test modules.
fn production_source() -> &'static str {
    &SRC[..SRC.find("#[cfg(test)]").expect("test module marker")]
}

/// The body of `impl UserData for <ty>`.
fn impl_body(ty: &str) -> &'static str {
    let src = production_source();
    let start = src
        .find(&format!("impl UserData for {ty} "))
        .unwrap_or_else(|| panic!("no UserData impl for {ty}"));
    let rest = &src[start + 1..];
    let end = rest
        .find("\nimpl UserData for ")
        .map_or(rest.len(), |e| e + 1);
    &src[start..start + 1 + end]
}

/// String keys matched by the type's `MetaMethod::Index` handler: lines
/// shaped `"a" | "b" => ...`.
fn index_keys(ty: &str) -> Vec<String> {
    let body = impl_body(ty);
    let from = body.find("MetaMethod::Index").expect("Index handler");
    let mut keys = Vec::new();
    for line in body[from..].lines() {
        let line = line.trim();
        let Some((pat, _)) = line.split_once("=>") else {
            continue;
        };
        let alts: Vec<&str> = pat.split('|').map(str::trim).collect();
        if alts
            .iter()
            .all(|a| a.len() > 2 && a.starts_with('"') && a.ends_with('"'))
        {
            keys.extend(alts.iter().map(|a| a.trim_matches('"').to_string()));
        }
    }
    keys
}

/// Names registered with `add_method` / `add_method_mut` on a type.
fn method_names(ty: &str) -> Vec<String> {
    let body = impl_body(ty);
    let mut names = Vec::new();
    for marker in ["add_method(", "add_method_mut("] {
        let mut rest = body;
        while let Some(i) = rest.find(marker) {
            rest = &rest[i + marker.len()..];
            let arg = rest.trim_start();
            if let Some(stripped) = arg.strip_prefix('"')
                && let Some(end) = stripped.find('"')
            {
                names.push(stripped[..end].to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

#[test]
fn the_case_table_covers_every_registered_method() {
    let mut missing = Vec::new();
    for ty in ["LuaActor", "LuaQuest", "LuaRoom", "LuaExit"] {
        let names = method_names(ty);
        assert!(!names.is_empty(), "found no methods for {ty}");
        for name in names {
            if !METHOD_CASES.iter().any(|(t, n, _)| *t == ty && *n == name) {
                missing.push(format!("{ty}:{name}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "add a despawn case for: {}",
        missing.join(", ")
    );
}

/// Cases must name real methods, so the table cannot rot.
#[test]
fn the_case_table_has_no_stale_entries() {
    for (ty, name, _) in METHOD_CASES {
        assert!(
            method_names(ty).iter().any(|n| n == name),
            "{ty}:{name} is not a registered method"
        );
    }
}

/// The parked-thread path: a script waits, its mob is despawned, and the
/// resume must neither panic nor run.
#[test]
fn resume_after_listener_despawned_does_not_panic() {
    let mut fx = fixture();
    let mut host = LuaHost::new();
    host.set_current_tick(0);
    host.exec_for_actor(
        &mut fx.world,
        fx.me,
        "wait(1)\ncombat.engage(self, actor)\nself:setvar('ran', 1)",
    )
    .unwrap();
    assert_eq!(host.yielded_count(), 1);
    fx.world.despawn(fx.me);
    host.set_current_tick(10);
    // Dropped without being resumed.
    assert_eq!(host.tick_yielded(&mut fx.world), 0);
    assert_eq!(host.yielded_count(), 0);
    assert!(
        fx.world
            .get_resource::<EntityVariableCache>()
            .is_none_or(|c| c.get(EntityType::Mob, 99, 1, "ran").is_none()),
        "a despawned listener's script must not run"
    );
}

#[test]
fn resume_after_actor_despawned_drops_the_thread() {
    let mut fx = fixture();
    let actor = fx
        .world
        .spawn((
            Mob,
            Named {
                name: "Visitor".to_string(),
            },
        ))
        .id();
    let mut host = LuaHost::new();
    host.set_current_tick(0);
    host.exec_for_listener_with_extras(
        &mut fx.world,
        fx.me,
        actor,
        "wait(1)\nself:setvar('ran', 1)",
        &[],
    )
    .unwrap();
    assert_eq!(host.yielded_count(), 1);
    fx.world.despawn(actor);
    host.set_current_tick(10);
    assert_eq!(host.tick_yielded(&mut fx.world), 0);
    assert_eq!(host.yielded_count(), 0);
    assert!(
        fx.world
            .get_resource::<EntityVariableCache>()
            .is_none_or(|c| c.get(EntityType::Mob, 99, 1, "ran").is_none()),
    );
}

/// A parked thread whose listener is gone is dropped at once, not held
/// until its deadline.
#[test]
fn parked_thread_is_dropped_as_soon_as_its_listener_despawns() {
    let mut fx = fixture();
    let mut host = LuaHost::new();
    host.set_current_tick(0);
    host.exec_for_actor(&mut fx.world, fx.me, "wait(60)")
        .unwrap();
    assert_eq!(host.yielded_count(), 1);
    fx.world.despawn(fx.me);
    host.set_current_tick(1);
    host.tick_yielded(&mut fx.world);
    assert_eq!(host.yielded_count(), 0);
}

/// Control: the same script with everyone alive does run, so the two
/// tests above prove the guard rather than a broken fixture.
#[test]
fn resume_with_live_entities_still_runs() {
    let mut fx = fixture();
    let mut host = LuaHost::new();
    host.set_current_tick(0);
    host.exec_for_actor(&mut fx.world, fx.me, "wait(1)\nself:setvar('ran', 1)")
        .unwrap();
    host.set_current_tick(10);
    assert_eq!(host.tick_yielded(&mut fx.world), 1);
    assert!(
        fx.world
            .resource::<EntityVariableCache>()
            .get(EntityType::Mob, 99, 1, "ran")
            .is_some()
    );
}
