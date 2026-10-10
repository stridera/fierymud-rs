//! Lua scripts that reach back into server code: a script-cast spell that
//! kills a mob, `combat.engage`, `actor:damage`, `room:purge` and
//! `world.destroy` must go through the same server paths as player
//! actions, and must never re-enter the `LuaHost` the script is running
//! on. Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::EntityType;
use mud_world::{
    AttachedTriggers, CombatStats, Corpse, EntityVariableCache, Fighting, Health, Item, Keywords,
    KnownAbilities, Located, Mob, Named, PeacefulRoom, Player, TriggerAttach, TriggerCatalog,
    TriggerDef, TriggerEvent, WorldKey,
};

use super::gmcp_tests::{Fx, fixture, player};
use super::test_support::{Rx, ability_def};

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

// ----- bindings that change the world go through the server's paths -----

/// Run `body` with `self` = `listener` and `actor` = `acting` on the real
/// host, then flush the outbox (and with it any deferred trigger).
fn run(fx: &mut Fx, listener: Entity, acting: Entity, body: &str) {
    install_executors(&mut fx.world);
    fx.world.insert_resource(super::lua_script_hooks());
    let result = fx
        .world
        .resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
            host.exec_for_listener_with_extras(world, listener, acting, body, &[])
        });
    result.unwrap_or_else(|e| panic!("script failed: {e}"));
    crate::commands::drain_lua_outbox(&mut fx.world);
}

fn fighting(world: &World, e: Entity) -> Option<Entity> {
    world.get::<Fighting>(e).map(|f| f.0)
}

fn item_on(world: &mut World, holder: Entity, name: &str) -> Entity {
    world
        .spawn((
            Item,
            Named { name: name.into() },
            Keywords(vec![name.to_ascii_lowercase()]),
            Located(holder),
        ))
        .id()
}

struct Arena {
    fx: Fx,
    me: Entity,
    bob: Entity,
    _rx: Rx,
}

fn arena() -> Arena {
    let mut fx = fixture();
    let a = fx.a;
    let me = mob(&mut fx.world, a, "Warden", (900, 1), 100);
    let (bob, rx) = player(&mut fx.world, a, "Bob");
    fx.world
        .entity_mut(bob)
        .insert((Health { hp: 100, max: 100 }, CombatStats::default()));
    Arena {
        fx,
        me,
        bob,
        _rx: rx,
    }
}

#[test]
fn engage_attacker_and_target_name_fight_each_other_not_themselves() {
    // The corpus form: `combat.engage(self, actor.name)`.
    let mut t = arena();
    run(&mut t.fx, t.me, t.bob, "combat.engage(self, actor.name)");
    assert_eq!(fighting(&t.fx.world, t.me), Some(t.bob));
    assert_eq!(fighting(&t.fx.world, t.bob), Some(t.me));
}

#[test]
fn engage_accepts_an_actor_and_the_single_argument_form() {
    let mut t = arena();
    run(&mut t.fx, t.me, t.bob, "combat.engage(self, actor)");
    assert_eq!(fighting(&t.fx.world, t.me), Some(t.bob));
    assert_eq!(fighting(&t.fx.world, t.bob), Some(t.me));

    let mut t = arena();
    run(&mut t.fx, t.me, t.bob, "combat.engage(actor)");
    assert_eq!(fighting(&t.fx.world, t.me), Some(t.bob));
    assert_eq!(fighting(&t.fx.world, t.bob), Some(t.me));
}

#[test]
fn engage_never_pits_the_mob_against_itself() {
    let mut t = arena();
    // Nothing named like the mob but the mob itself; and a self target.
    run(
        &mut t.fx,
        t.me,
        t.bob,
        "combat.engage(self, self.name)\ncombat.engage(self, self)\ncombat.engage(self, nil)\ncombat.engage(self, 'nobody')",
    );
    assert_eq!(fighting(&t.fx.world, t.me), None);
    assert_eq!(fighting(&t.fx.world, t.bob), None);
}

#[test]
fn engage_respects_peaceful_rooms_ghosts_and_distance() {
    let mut t = arena();
    let a = t.fx.a;
    t.fx.world.entity_mut(a).insert(PeacefulRoom);
    run(&mut t.fx, t.me, t.bob, "combat.engage(self, actor)");
    assert_eq!(fighting(&t.fx.world, t.me), None, "peaceful room");
    assert_eq!(fighting(&t.fx.world, t.bob), None);

    let mut t = arena();
    t.fx.world.entity_mut(t.bob).insert(mud_world::Ghost);
    run(&mut t.fx, t.me, t.bob, "combat.engage(self, actor)");
    assert_eq!(fighting(&t.fx.world, t.me), None, "ghost");

    let mut t = arena();
    let b = t.fx.b;
    t.fx.world.entity_mut(t.bob).insert(Located(b));
    run(&mut t.fx, t.me, t.bob, "combat.engage(self, actor)");
    assert_eq!(fighting(&t.fx.world, t.me), None, "other room");
}

#[test]
fn rescue_moves_the_attacker_onto_the_rescuer() {
    let mut t = arena();
    let a = t.fx.a;
    let orc = mob(&mut t.fx.world, a, "orc", (900, 7), 50);
    t.fx.world.entity_mut(orc).insert(Fighting(t.bob));
    t.fx.world.entity_mut(t.bob).insert(Fighting(orc));
    run(&mut t.fx, t.me, t.bob, "combat.rescue(self, actor)");
    assert_eq!(fighting(&t.fx.world, orc), Some(t.me));
    assert_eq!(fighting(&t.fx.world, t.me), Some(orc));
}

#[test]
fn damage_returns_what_it_dealt_and_lethal_damage_kills_through_handle_death() {
    let mut t = arena();
    let a = t.fx.a;
    let rat = mob(&mut t.fx.world, a, "rat", (900, 5), 30);
    // Non-lethal: reports the amount, leaves the rat standing.
    run(
        &mut t.fx,
        t.me,
        rat,
        "actor:setvar('dealt', actor:damage(12))",
    );
    assert_eq!(t.fx.world.get::<Health>(rat).unwrap().hp, 18);
    let dealt =
        t.fx.world
            .resource::<EntityVariableCache>()
            .get(EntityType::Mob, 900, 5, "dealt")
            .cloned();
    assert_eq!(dealt, Some(serde_json::json!(12)));

    // Lethal: the rat is dead for real (corpse, despawn), not left at 0 hp.
    run(&mut t.fx, t.me, rat, "actor:damage(500)");
    assert!(t.fx.world.get_entity(rat).is_err(), "rat despawned");
    let corpses =
        t.fx.world
            .query_filtered::<&Located, With<Corpse>>()
            .iter(&t.fx.world)
            .filter(|l| l.0 == a)
            .count();
    assert_eq!(corpses, 1, "death handling left a corpse");
}

#[test]
fn damage_can_kill_a_player_who_is_then_a_ghost() {
    let mut t = arena();
    t.fx.world
        .entity_mut(t.bob)
        .insert(Health { hp: 10, max: 100 });
    run(&mut t.fx, t.me, t.bob, "actor:damage(50)");
    assert!(t.fx.world.get::<mud_world::Ghost>(t.bob).is_some());
}

#[test]
fn negative_damage_heals_but_never_past_max() {
    let mut t = arena();
    t.fx.world
        .entity_mut(t.bob)
        .insert(Health { hp: 40, max: 100 });
    run(
        &mut t.fx,
        t.me,
        t.bob,
        "actor:setvar('r', actor:damage(-25))",
    );
    assert_eq!(t.fx.world.get::<Health>(t.bob).unwrap().hp, 65);
    run(
        &mut t.fx,
        t.me,
        t.bob,
        "actor:setvar('r', actor:damage(-1000))",
    );
    assert_eq!(t.fx.world.get::<Health>(t.bob).unwrap().hp, 100);
}

#[test]
fn purge_extracts_mobs_with_what_they_carry_and_spares_players_and_floor() {
    let mut t = arena();
    let a = t.fx.a;
    let rat = mob(&mut t.fx.world, a, "rat", (900, 5), 5);
    let cat = mob(&mut t.fx.world, a, "cat", (900, 6), 5);
    let loot = item_on(&mut t.fx.world, rat, "fang");
    let bag = item_on(&mut t.fx.world, cat, "bag");
    let coin = item_on(&mut t.fx.world, bag, "coin");
    let floor = item_on(&mut t.fx.world, a, "rock");
    // `self` is the player Bob so the script survives its own purge.
    run(&mut t.fx, t.bob, t.bob, "self.room:purge()");
    for gone in [rat, cat, t.me, loot, bag, coin] {
        assert!(
            t.fx.world.get_entity(gone).is_err(),
            "{gone:?} should be gone"
        );
    }
    assert!(
        t.fx.world.get_entity(t.bob).is_ok(),
        "players are not purged"
    );
    assert!(t.fx.world.get_entity(floor).is_ok(), "floor items stay");
}

#[test]
fn destroy_refuses_players_and_cleans_up_mobs_and_items() {
    let mut t = arena();
    let a = t.fx.a;
    run(&mut t.fx, t.me, t.bob, "world.destroy(actor)");
    assert!(
        t.fx.world.get_entity(t.bob).is_ok(),
        "a player survives world.destroy"
    );
    assert!(t.fx.world.get::<Player>(t.bob).is_some());

    let rat = mob(&mut t.fx.world, a, "rat", (900, 5), 5);
    let fang = item_on(&mut t.fx.world, rat, "fang");
    run(&mut t.fx, t.me, rat, "world.destroy(actor)");
    assert!(t.fx.world.get_entity(rat).is_err());
    assert!(
        t.fx.world.get_entity(fang).is_err(),
        "carried item must not leak"
    );

    let sack = item_on(&mut t.fx.world, a, "sack");
    let gem = item_on(&mut t.fx.world, sack, "gem");
    run(&mut t.fx, t.me, sack, "world.destroy(actor)");
    assert!(t.fx.world.get_entity(sack).is_err());
    assert!(
        t.fx.world.get_entity(gem).is_err(),
        "container contents go too"
    );
}
