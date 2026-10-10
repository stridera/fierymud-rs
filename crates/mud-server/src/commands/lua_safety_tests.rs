//! Lua scripts that reach back into server code: a script-cast spell that
//! kills a mob, `combat.engage`, `actor:damage`, `room:purge` and
//! `world.destroy` must go through the same server paths as player
//! actions, and must never re-enter the `LuaHost` the script is running
//! on. Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::EntityType;
use mud_world::{
    AttachedTriggers, CombatStats, EntityVariableCache, Health, Keywords, KnownAbilities, Located,
    Mob, Named, TriggerAttach, TriggerCatalog, TriggerDef, TriggerEvent, WorldKey,
};

use super::gmcp_tests::{Fx, fixture};
use super::test_support::ability_def;

const SMITE: i32 = 77;
const ROOM_KEY: (i32, i32) = (550, 18);

fn trigger(zone: i32, id: i32, event: TriggerEvent, body: &str) -> TriggerDef {
    TriggerDef {
        zone_id: zone,
        id,
        name: format!("t{zone}_{id}"),
        attach_type: TriggerAttach::Mob,
        commands: body.to_string(),
        flags: vec![event],
        arg_list: vec![],
        num_args: 0,
    }
}

fn install_executors(world: &mut World) {
    world.insert_resource(crate::TickCount(0));
    world.insert_resource(mud_script::SkillExecutor(Some(super::lua_invoke_skill)));
    world.insert_resource(mud_script::SpellExecutor(Some(super::lua_invoke_spell)));
}

/// A skill `smite` that deals far more damage than any mob has hp.
fn install_smite(world: &mut World) {
    let mut abilities = mud_world::AbilityCatalog::default();
    let mut def = ability_def(SMITE, "Smite", AbilityKind::Skill);
    def.violent = true;
    def.cast_time_rounds = 0;
    abilities.by_name.insert("smite".to_string(), def);
    abilities.effects_for.insert(
        SMITE,
        vec![(
            9,
            Some(serde_json::json!({ "type": "pierce", "amount": "5000" })),
        )],
    );
    world.insert_resource(abilities);
    let mut effects = mud_world::EffectCatalog::default();
    effects.by_id.insert(
        9,
        mud_world::EffectDef {
            id: 9,
            name: "damage".into(),
            description: None,
            effect_type: "damage".into(),
            tags: vec![],
            presence_override: None,
            default_params: serde_json::json!({}),
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        },
    );
    world.insert_resource(effects);
}

fn mob(world: &mut World, room: Entity, name: &str, key: (i32, i32), hp: i32) -> Entity {
    world
        .spawn((
            Mob,
            Named { name: name.into() },
            Keywords(vec![name.to_ascii_lowercase()]),
            Located(room),
            CombatStats::default(),
            Health { hp, max: hp },
            WorldKey {
                zone: key.0,
                id: key.1,
            },
        ))
        .id()
}

fn room_log(world: &World) -> Option<String> {
    world
        .get_resource::<EntityVariableCache>()?
        .get(EntityType::Room, ROOM_KEY.0, ROOM_KEY.1, "log")
        .and_then(|v| v.as_str().map(str::to_string))
}

const APPEND: &str = "self.room:setvar('log', tostring(self.room:getvar('log') or '') .. '";

/// Lua -> `skills.execute` -> `invoke_ability` -> `handle_death` ->
/// DEATH trigger. The nested fire used to panic on the `LuaHost` the
/// outer script holds; it must instead run once the outer script is done,
/// and still see its (not yet despawned) `self`.
#[test]
fn script_cast_kill_defers_the_death_trigger_until_the_script_returns() {
    let mut fx: Fx = fixture();
    install_executors(&mut fx.world);
    install_smite(&mut fx.world);
    let a = fx.a;
    let caster = mob(&mut fx.world, a, "Smiter", (900, 1), 100);
    fx.world.entity_mut(caster).insert((
        KnownAbilities {
            entries: vec![(SMITE, 1000, true)],
        },
        AttachedTriggers(vec![(900, 1)]),
    ));
    let rat = mob(&mut fx.world, a, "rat", (900, 5), 5);
    fx.world
        .entity_mut(rat)
        .insert(AttachedTriggers(vec![(900, 2)]));
    let mut catalog = TriggerCatalog::default();
    catalog.by_key.insert(
        (900, 1),
        trigger(
            900,
            1,
            TriggerEvent::Load,
            &format!("skills.execute(self, 'smite', 'rat')\n{APPEND}outer;')"),
        ),
    );
    catalog.by_key.insert(
        (900, 2),
        trigger(900, 2, TriggerEvent::Death, &format!("{APPEND}death;')")),
    );
    fx.world.insert_resource(catalog);

    crate::triggers::fire_event(&mut fx.world, caster, TriggerEvent::Load);

    assert_eq!(
        room_log(&fx.world).as_deref(),
        Some("outer;death;"),
        "death trigger must run after the outer script, once"
    );
    assert!(
        fx.world.get_entity(rat).is_err(),
        "the slain mob is despawned once its death trigger has run"
    );
}

/// Any dispatcher asked to fire while a script runs queues instead of
/// panicking; the fire happens at the next drain, and only once.
#[test]
fn trigger_fire_requested_during_a_script_is_queued_then_run_once() {
    let mut fx: Fx = fixture();
    let a = fx.a;
    let mob_e = mob(&mut fx.world, a, "rat", (900, 5), 5);
    fx.world
        .entity_mut(mob_e)
        .insert(AttachedTriggers(vec![(900, 2)]));
    let mut catalog = TriggerCatalog::default();
    catalog.by_key.insert(
        (900, 2),
        trigger(900, 2, TriggerEvent::Greet, &format!("{APPEND}greet;')")),
    );
    fx.world.insert_resource(catalog);
    let player = fx
        .world
        .spawn((Named { name: "Bob".into() }, Located(a)))
        .id();

    // A script is mid-run: the host is out of the world.
    let host = fx.world.remove_resource::<mud_script::LuaHost>().unwrap();
    fx.world.insert_resource(mud_script::ScriptRunning);
    crate::triggers::fire_greet_in_room(&mut fx.world, player, a);
    crate::triggers::fire_event_with_actor(&mut fx.world, mob_e, player, TriggerEvent::Fight);
    assert!(
        !crate::triggers::fire_command_in_room(&mut fx.world, player, a, "look", ""),
        "a typed command is not intercepted while a script runs"
    );
    crate::deferred_triggers::drain(&mut fx.world);
    assert_eq!(
        room_log(&fx.world),
        None,
        "nothing runs while the script does"
    );

    fx.world.remove_resource::<mud_script::ScriptRunning>();
    fx.world.insert_resource(host);
    crate::commands::drain_lua_outbox(&mut fx.world);
    assert_eq!(room_log(&fx.world).as_deref(), Some("greet;"));
    crate::commands::drain_lua_outbox(&mut fx.world);
    assert_eq!(room_log(&fx.world).as_deref(), Some("greet;"), "fires once");
}
