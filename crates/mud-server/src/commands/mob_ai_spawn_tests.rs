//! Spawn wiring of the mob spellcasting / skill AI (WP-A): with the
//! `mob_ai.enabled` flag off a spawned mob is untouched; with it on, class
//! mobs get their class rules, `CUSTOM` mobs their own, `NONE` / `NoClassAi`
//! mobs nothing, and caster classes get `SpellSlots`.

use std::collections::BTreeSet;
use std::sync::Arc;

use bevy_ecs::prelude::*;
use mud_db::enums::{MobAiMode, MobBehavior, MobProfession};
use mud_world::{
    AiKind, AiRule, AiTarget, ConfigValue, Exits, MobAi, MobAiData, Named, Room, RuntimeConfig,
    SpellSlotData, SpellSlots, WorldKey,
};

use super::test_support::mob_proto;

const CASTER_CLASS: i32 = 7;
const WARRIOR_CLASS: i32 = 8;

fn rule(ability_id: i32, priority: i32) -> AiRule {
    AiRule {
        ability_id,
        kind: AiKind::Spell,
        priority,
        chance_pct: 100,
        cooldown_s: 0,
        when: Vec::new(),
        target: AiTarget::Auto,
        min_level: None,
    }
}

fn class_rules() -> Arc<[AiRule]> {
    Arc::from(vec![rule(11, 1), rule(12, 2)])
}

fn own_rules() -> Arc<[AiRule]> {
    Arc::from(vec![rule(99, 1)])
}

/// A world with a room, the rule tables, spell slot data and the flag set to
/// `enabled`.
fn world_with(enabled: bool) -> (World, Entity) {
    let mut world = World::new();
    let room = world
        .spawn((
            Room,
            Named {
                name: "A hall".into(),
            },
            Exits::default(),
            WorldKey { zone: 30, id: 100 },
        ))
        .id();
    let mut data = MobAiData::default();
    data.by_class.insert(CASTER_CLASS, class_rules());
    data.by_class.insert(WARRIOR_CLASS, class_rules());
    data.by_mob.insert((30, 3), own_rules());
    data.by_mob.insert((30, 4), own_rules());
    data.modes.insert((30, 3), MobAiMode::Custom);
    data.modes.insert((30, 4), MobAiMode::None);
    world.insert_resource(data);
    let mut slots = SpellSlotData::default();
    slots
        .class_circles
        .insert(CASTER_CLASS, vec![(1, 1), (2, 3)]);
    slots.progression.insert((5, 1), 2);
    slots.progression.insert((5, 2), 1);
    world.insert_resource(slots);
    let mut config = RuntimeConfig::default();
    config.by_key.insert(
        ("mob_ai".into(), "enabled".into()),
        ConfigValue::Bool(enabled),
    );
    world.insert_resource(config);
    (world, room)
}

fn proto(id: i32, class_id: Option<i32>) -> mud_world::MobProto {
    let mut p = mob_proto(30, id, MobProfession::Trainer);
    p.class_id = class_id;
    p
}

fn components(world: &World, e: Entity) -> BTreeSet<String> {
    world
        .inspect_entity(e)
        .expect("entity exists")
        .map(|c| c.name().to_string())
        .collect()
}

#[test]
fn flag_off_attaches_nothing() {
    let (mut world, room) = world_with(false);
    let mob =
        mud_world::spawn_mob_from_proto(&mut world, &proto(1, Some(CASTER_CLASS)), room, None);
    assert!(world.get::<MobAi>(mob).is_none());
    assert!(world.get::<SpellSlots>(mob).is_none());
    let custom = mud_world::spawn_mob_from_proto(&mut world, &proto(3, None), room, None);
    assert!(world.get::<MobAi>(custom).is_none());

    // Byte-identical: the very same component set as a world that has never
    // heard of mob AI (no rule tables, no config row).
    let mut bare = World::new();
    let bare_room = bare.spawn((Room, Exits::default())).id();
    let baseline =
        mud_world::spawn_mob_from_proto(&mut bare, &proto(1, Some(CASTER_CLASS)), bare_room, None);
    assert_eq!(components(&world, mob), components(&bare, baseline));
}

#[test]
fn flag_default_is_off_without_a_config_row() {
    let (mut world, room) = world_with(true);
    world.resource_mut::<RuntimeConfig>().by_key.clear();
    let mob =
        mud_world::spawn_mob_from_proto(&mut world, &proto(1, Some(CASTER_CLASS)), room, None);
    assert!(world.get::<MobAi>(mob).is_none());
    assert!(world.get::<SpellSlots>(mob).is_none());
}

#[test]
fn class_mob_gets_class_rules_and_caster_slots() {
    let (mut world, room) = world_with(true);
    let mob =
        mud_world::spawn_mob_from_proto(&mut world, &proto(1, Some(CASTER_CLASS)), room, None);
    let ai = world.get::<MobAi>(mob).expect("class rules attached");
    assert!(Arc::ptr_eq(
        &ai.rules,
        &world.resource::<MobAiData>().by_class[&CASTER_CLASS]
    ));
    assert!(ai.last_use.is_empty());
    assert!(
        world.get::<SpellSlots>(mob).is_some(),
        "caster class has slots"
    );
}

#[test]
fn non_caster_class_mob_gets_rules_but_no_slots() {
    let (mut world, room) = world_with(true);
    let mob =
        mud_world::spawn_mob_from_proto(&mut world, &proto(1, Some(WARRIOR_CLASS)), room, None);
    assert!(world.get::<MobAi>(mob).is_some());
    assert!(world.get::<SpellSlots>(mob).is_none());
}

#[test]
fn classless_or_ruleless_class_mob_gets_nothing() {
    let (mut world, room) = world_with(true);
    let none = mud_world::spawn_mob_from_proto(&mut world, &proto(1, None), room, None);
    assert!(world.get::<MobAi>(none).is_none());
    assert!(world.get::<SpellSlots>(none).is_none());
    let unknown = mud_world::spawn_mob_from_proto(&mut world, &proto(1, Some(42)), room, None);
    assert!(world.get::<MobAi>(unknown).is_none());
}

#[test]
fn custom_mob_gets_its_own_rules() {
    let (mut world, room) = world_with(true);
    // Even with a class set, CUSTOM uses the per-mob list, not the class one.
    let mob =
        mud_world::spawn_mob_from_proto(&mut world, &proto(3, Some(WARRIOR_CLASS)), room, None);
    let ai = world.get::<MobAi>(mob).expect("custom rules attached");
    assert!(Arc::ptr_eq(
        &ai.rules,
        &world.resource::<MobAiData>().by_mob[&(30, 3)]
    ));
    assert_eq!(ai.rules[0].ability_id, 99);
}

#[test]
fn none_mode_gets_nothing_even_for_a_caster_class() {
    let (mut world, room) = world_with(true);
    let mob =
        mud_world::spawn_mob_from_proto(&mut world, &proto(4, Some(CASTER_CLASS)), room, None);
    assert!(world.get::<MobAi>(mob).is_none());
    assert!(world.get::<SpellSlots>(mob).is_none());
}

#[test]
fn no_class_ai_behavior_gets_nothing() {
    let (mut world, room) = world_with(true);
    let mut p = proto(1, Some(CASTER_CLASS));
    p.behaviors = vec![MobBehavior::NoClassAi];
    let mob = mud_world::spawn_mob_from_proto(&mut world, &p, room, None);
    assert!(world.get::<MobAi>(mob).is_none());
    assert!(world.get::<SpellSlots>(mob).is_none());
}

// ---- race rules (WP-D) ----

fn breath_rules() -> Arc<[AiRule]> {
    Arc::from(vec![rule(40, 10), rule(41, 20)])
}

fn proto_of_race(id: i32, class_id: Option<i32>, race: &str) -> mud_world::MobProto {
    let mut p = proto(id, class_id);
    p.race = race.to_string();
    p
}

fn world_with_race_rules(enabled: bool) -> (World, Entity) {
    let (mut world, room) = world_with(enabled);
    world
        .resource_mut::<MobAiData>()
        .by_race
        .insert("dragon_fire".into(), breath_rules());
    (world, room)
}

#[test]
fn flag_off_gives_a_dragon_nothing() {
    let (mut world, room) = world_with_race_rules(false);
    let dragon = mud_world::spawn_mob_from_proto(
        &mut world,
        &proto_of_race(1, Some(WARRIOR_CLASS), "DRAGON_FIRE"),
        room,
        None,
    );
    assert!(world.get::<MobAi>(dragon).is_none());
}

#[test]
fn a_classless_dragon_gets_just_its_race_rules() {
    let (mut world, room) = world_with_race_rules(true);
    let dragon = mud_world::spawn_mob_from_proto(
        &mut world,
        &proto_of_race(1, None, "DRAGON_FIRE"),
        room,
        None,
    );
    let ai = world.get::<MobAi>(dragon).expect("race rules attached");
    assert!(Arc::ptr_eq(
        &ai.rules,
        &world.resource::<MobAiData>().by_race["dragon_fire"]
    ));
    assert!(world.get::<SpellSlots>(dragon).is_none());
}

#[test]
fn race_rules_run_ahead_of_the_class_rules() {
    let (mut world, room) = world_with_race_rules(true);
    let dragon = mud_world::spawn_mob_from_proto(
        &mut world,
        &proto_of_race(1, Some(CASTER_CLASS), "DRAGON_FIRE"),
        room,
        None,
    );
    let ids: Vec<i32> = world
        .get::<MobAi>(dragon)
        .expect("merged rules")
        .rules
        .iter()
        .map(|r| r.ability_id)
        .collect();
    assert_eq!(ids, vec![40, 41, 11, 12], "race first, then the class kit");
    assert!(
        world.get::<SpellSlots>(dragon).is_some(),
        "caster class keeps its slots"
    );
}

#[test]
fn the_merged_list_is_shared_between_mobs() {
    let (mut world, room) = world_with_race_rules(true);
    let p = proto_of_race(1, Some(WARRIOR_CLASS), "DRAGON_FIRE");
    let a = mud_world::spawn_mob_from_proto(&mut world, &p, room, None);
    let b = mud_world::spawn_mob_from_proto(&mut world, &p, room, None);
    assert!(Arc::ptr_eq(
        &world.get::<MobAi>(a).unwrap().rules,
        &world.get::<MobAi>(b).unwrap().rules
    ));
}

#[test]
fn a_race_without_rules_keeps_the_plain_class_list() {
    let (mut world, room) = world_with_race_rules(true);
    let orc = mud_world::spawn_mob_from_proto(
        &mut world,
        &proto_of_race(1, Some(WARRIOR_CLASS), "ORC"),
        room,
        None,
    );
    assert!(Arc::ptr_eq(
        &world.get::<MobAi>(orc).unwrap().rules,
        &world.resource::<MobAiData>().by_class[&WARRIOR_CLASS]
    ));
}

#[test]
fn no_class_ai_and_ai_modes_switch_race_rules_off_too() {
    let (mut world, room) = world_with_race_rules(true);
    let mut p = proto_of_race(1, Some(WARRIOR_CLASS), "DRAGON_FIRE");
    p.behaviors = vec![MobBehavior::NoClassAi];
    let quiet = mud_world::spawn_mob_from_proto(&mut world, &p, room, None);
    assert!(
        world.get::<MobAi>(quiet).is_none(),
        "NoClassAi silences the breath too"
    );
    // A CUSTOM dragon runs exactly its own list.
    let custom = mud_world::spawn_mob_from_proto(
        &mut world,
        &proto_of_race(3, None, "DRAGON_FIRE"),
        room,
        None,
    );
    let ids: Vec<i32> = world
        .get::<MobAi>(custom)
        .unwrap()
        .rules
        .iter()
        .map(|r| r.ability_id)
        .collect();
    assert_eq!(ids, vec![99]);
    let none = mud_world::spawn_mob_from_proto(
        &mut world,
        &proto_of_race(4, None, "DRAGON_FIRE"),
        room,
        None,
    );
    assert!(world.get::<MobAi>(none).is_none());
}
