//! `cancel` (issue #136): drop one of your own timed, beneficial spells.
//! Out of combat only; permanent and harmful effects refuse; the effect
//! ends through the expiry path so stat mods reverse exactly.

use super::dispatch;
use super::test_support::{Rx, ability_def, drain, player_in};
use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::UserRole;
use mud_world::{
    AbilityCatalog, AbilityMessageSet, Account, AppliedTo, CoreStats, EffectCatalog, EffectDef,
    EffectInstance, EffectSource, Fighting, Health, Located, ModifyDelta, Named, Room,
};

const BLESS: i32 = 1;
const CURSE: i32 = 2;
const STONE_SKIN: i32 = 3;
const FX_MODIFY: i32 = 10;
const FX_DEBUFF: i32 = 11;

fn effect_def(id: i32, name: &str, tags: &[&str]) -> EffectDef {
    EffectDef {
        id,
        name: name.into(),
        description: None,
        effect_type: "modify".into(),
        tags: tags.iter().map(|t| (*t).to_string()).collect(),
        presence_override: None,
        default_params: serde_json::json!({}),
        prevents_speaking: false,
        prevents_casting: false,
        prevents_movement: false,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    }
}

fn setup() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    let mut abilities = AbilityCatalog::default();
    for (id, name, violent) in [
        (BLESS, "Bless", false),
        (CURSE, "Curse", true),
        (STONE_SKIN, "Stone Skin", false),
    ] {
        let mut def = ability_def(id, name, AbilityKind::Spell);
        def.violent = violent;
        abilities
            .by_name
            .insert(def.plain_name.to_ascii_lowercase(), def);
    }
    abilities.messages.insert(
        BLESS,
        AbilityMessageSet {
            wearoff_to_target: Some("You feel less blessed.".into()),
            wearoff_to_room: Some("{target.name} looks less blessed.".into()),
            ..Default::default()
        },
    );
    world.insert_resource(abilities);
    let mut effects = EffectCatalog::default();
    effects
        .by_id
        .insert(FX_MODIFY, effect_def(FX_MODIFY, "modify", &["buff"]));
    effects
        .by_id
        .insert(FX_DEBUFF, effect_def(FX_DEBUFF, "weaken", &["debuff"]));
    world.insert_resource(effects);
    let room = world.spawn(Room).id();
    let (player, rx) = player_in(&mut world, room);
    world.entity_mut(player).insert((
        Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        },
        CoreStats {
            strength: 50,
            ..Default::default()
        },
        Health { hp: 100, max: 100 },
    ));
    (world, room, player, rx)
}

fn spawn_effect(
    world: &mut World,
    on: Entity,
    kind: i32,
    name: &str,
    remaining: i32,
    ability_id: Option<i32>,
    source: EffectSource,
) -> Entity {
    world
        .spawn((
            EffectInstance {
                kind,
                name: name.into(),
                strength: 1,
                remaining_secs: remaining,
                source,
                ability_id,
            },
            AppliedTo(on),
        ))
        .id()
}

/// A stat buff applied the way a cast records it.
fn buff(world: &mut World, p: Entity, ability: i32, remaining: i32, amount: i32) -> Entity {
    assert!(super::apply_modify_delta(world, p, "strength", amount));
    let e = spawn_effect(
        world,
        p,
        FX_MODIFY,
        "strength",
        remaining,
        Some(ability),
        EffectSource::Spell,
    );
    world.entity_mut(e).insert(ModifyDelta {
        target: "strength".into(),
        amount,
    });
    e
}

fn strength(world: &World, p: Entity) -> i32 {
    world.get::<CoreStats>(p).unwrap().strength
}

fn alive(world: &World, e: Entity) -> bool {
    world.get::<EffectInstance>(e).is_some()
}

#[test]
fn cancel_removes_a_beneficial_timed_effect_and_reverses_its_stat_exactly() {
    let (mut world, room, p, mut rx) = setup();
    let watcher_room = room;
    let (watcher, mut wrx) = player_in(&mut world, watcher_room);
    world.entity_mut(watcher).insert(Named {
        name: "Watcher".into(),
    });
    let e = buff(&mut world, p, BLESS, 300, 7);
    assert_eq!(strength(&world, p), 57);
    dispatch(&mut world, p, "cancel bless");
    assert!(!alive(&world, e));
    assert_eq!(strength(&world, p), 50, "stat back to exactly the base");
    let out = drain(&mut rx);
    assert!(out.contains("You cancel Bless."), "{out}");
    assert!(
        out.contains("You feel less blessed."),
        "wear-off text: {out}"
    );
    let seen = drain(&mut wrx);
    assert!(seen.contains("looks less blessed"), "room line: {seen}");
}

#[test]
fn cancel_reverses_stacked_buffs_independently() {
    let (mut world, _room, p, mut rx) = setup();
    let bless = buff(&mut world, p, BLESS, 300, 7);
    let skin = buff(&mut world, p, STONE_SKIN, 300, 3);
    assert_eq!(strength(&world, p), 60);
    dispatch(&mut world, p, "cancel bless");
    let _ = drain(&mut rx);
    assert!(!alive(&world, bless) && alive(&world, skin));
    assert_eq!(strength(&world, p), 53);
    dispatch(&mut world, p, "cancel stone");
    assert_eq!(strength(&world, p), 50);
}

#[test]
fn cancel_matches_natural_expiry_state() {
    // Cancelling leaves the same stat and effect list as letting it expire.
    let (mut a, _ra, pa, _rxa) = setup();
    let (mut b, _rb, pb, _rxb) = setup();
    buff(&mut a, pa, BLESS, 1, 9);
    buff(&mut b, pb, BLESS, 1, 9);
    dispatch(&mut a, pa, "cancel bless");
    b.insert_resource(crate::TickCount(10));
    crate::effects::effects_tick(&mut b);
    assert_eq!(strength(&a, pa), strength(&b, pb));
    let count = |w: &mut World| w.query::<&EffectInstance>().iter(w).count();
    assert_eq!(count(&mut a), count(&mut b));
}

#[test]
fn cancel_refuses_in_combat() {
    let (mut world, room, p, mut rx) = setup();
    let e = buff(&mut world, p, BLESS, 300, 7);
    let foe = world
        .spawn((Located(room), Health { hp: 10, max: 10 }))
        .id();
    world.entity_mut(p).insert(Fighting(foe));
    dispatch(&mut world, p, "cancel bless");
    assert!(alive(&world, e));
    assert_eq!(strength(&world, p), 57);
    assert!(drain(&mut rx).contains("fight"));
}

#[test]
fn cancel_refuses_when_someone_is_fighting_you() {
    let (mut world, room, p, mut rx) = setup();
    let e = buff(&mut world, p, BLESS, 300, 7);
    world.spawn((Located(room), Fighting(p)));
    dispatch(&mut world, p, "cancel bless");
    assert!(alive(&world, e));
    assert!(drain(&mut rx).contains("fight"));
}

#[test]
fn cancel_refuses_permanent_effects() {
    let (mut world, _room, p, mut rx) = setup();
    // A spell effect with no end, and a race innate.
    let endless = buff(&mut world, p, BLESS, -1, 4);
    let innate = spawn_effect(
        &mut world,
        p,
        FX_MODIFY,
        "infravision",
        -1,
        None,
        EffectSource::Other(mud_world::mob_effects::RACE_EFFECT_SOURCE.into()),
    );
    dispatch(&mut world, p, "cancel bless");
    let out = drain(&mut rx);
    assert!(out.contains("permanent"), "{out}");
    dispatch(&mut world, p, "cancel infravision");
    assert!(drain(&mut rx).contains("permanent"));
    assert!(alive(&world, endless) && alive(&world, innate));
    assert_eq!(strength(&world, p), 54);
}

#[test]
fn cancel_refuses_harmful_effects() {
    let (mut world, _room, p, mut rx) = setup();
    // A violent source ability, and a debuff-tagged effect row.
    let curse = buff(&mut world, p, CURSE, 300, -5);
    let weaken = spawn_effect(
        &mut world,
        p,
        FX_DEBUFF,
        "weaken",
        300,
        Some(BLESS),
        EffectSource::Spell,
    );
    dispatch(&mut world, p, "cancel curse");
    assert!(drain(&mut rx).contains("harmful"));
    dispatch(&mut world, p, "cancel weaken");
    assert!(drain(&mut rx).contains("harmful"));
    assert!(alive(&world, curse) && alive(&world, weaken));
    assert_eq!(strength(&world, p), 45);
}

#[test]
fn cancel_refuses_effects_with_no_source_spell() {
    let (mut world, _room, p, mut rx) = setup();
    let bleed = spawn_effect(
        &mut world,
        p,
        FX_MODIFY,
        "bleed",
        30,
        None,
        EffectSource::Spell,
    );
    dispatch(&mut world, p, "cancel bleed");
    assert!(alive(&world, bleed));
    assert!(drain(&mut rx).contains("can't cancel"));
}

#[test]
fn cancel_with_no_arg_lists_only_cancellable_effects() {
    let (mut world, _room, p, mut rx) = setup();
    dispatch(&mut world, p, "cancel");
    assert!(drain(&mut rx).contains("no effects you can cancel"));
    buff(&mut world, p, BLESS, 300, 7);
    buff(&mut world, p, STONE_SKIN, 300, 3);
    buff(&mut world, p, CURSE, 300, -5);
    buff(&mut world, p, BLESS, -1, 1);
    dispatch(&mut world, p, "cancel");
    let out = drain(&mut rx);
    assert!(out.contains("Stone Skin"), "{out}");
    assert!(!out.contains("Curse"), "harmful hidden: {out}");
    assert!(out.contains("Bless"), "{out}");
    assert!(out.contains("2 cancellable"), "{out}");
}

#[test]
fn cancel_takes_a_prefix_and_quoted_multi_word_names() {
    let (mut world, _room, p, mut rx) = setup();
    let skin = buff(&mut world, p, STONE_SKIN, 300, 3);
    let bless = buff(&mut world, p, BLESS, 300, 7);
    dispatch(&mut world, p, "cancel bl");
    let _ = drain(&mut rx);
    assert!(!alive(&world, bless) && alive(&world, skin));
    dispatch(&mut world, p, "cancel 'stone sk'");
    assert!(!alive(&world, skin));
    assert_eq!(strength(&world, p), 50);
    dispatch(&mut world, p, "cancel stone skin");
    assert!(drain(&mut rx).contains("No cancellable effect"));
}

#[test]
fn an_exact_name_beats_an_earlier_prefix_match() {
    let (mut world, _room, p, mut rx) = setup();
    let mut cat = world.resource_mut::<AbilityCatalog>();
    let long = ability_def(4, "Bless Weapon", AbilityKind::Spell);
    cat.by_name.insert("bless_weapon".into(), long);
    let long_fx = buff(&mut world, p, 4, 300, 2);
    let short_fx = buff(&mut world, p, BLESS, 300, 7);
    dispatch(&mut world, p, "cancel bless");
    let _ = drain(&mut rx);
    assert!(alive(&world, long_fx) && !alive(&world, short_fx));
}

#[test]
fn a_spell_with_several_effects_is_cancelled_whole() {
    let (mut world, _room, p, mut rx) = setup();
    let a = buff(&mut world, p, STONE_SKIN, 300, 3);
    let b = buff(&mut world, p, STONE_SKIN, 300, 2);
    dispatch(&mut world, p, "cancel");
    assert!(drain(&mut rx).contains("1 cancellable"));
    dispatch(&mut world, p, "cancel stone");
    assert!(!alive(&world, a) && !alive(&world, b));
    assert_eq!(strength(&world, p), 50);
}

#[test]
fn a_god_may_cancel_a_harmful_permanent_effect_in_combat_but_not_an_innate() {
    let (mut world, room, p, mut rx) = setup();
    world.entity_mut(p).insert(Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Immortal,
        account_role: UserRole::Immortal,
        perms: vec![],
    });
    let curse = buff(&mut world, p, CURSE, -1, -5);
    let innate = spawn_effect(
        &mut world,
        p,
        FX_MODIFY,
        "infravision",
        -1,
        None,
        EffectSource::Other(mud_world::mob_effects::RACE_EFFECT_SOURCE.into()),
    );
    let foe = world
        .spawn((Located(room), Health { hp: 10, max: 10 }))
        .id();
    world.entity_mut(p).insert(Fighting(foe));
    dispatch(&mut world, p, "cancel curse");
    let _ = drain(&mut rx);
    assert!(!alive(&world, curse));
    assert_eq!(strength(&world, p), 50);
    dispatch(&mut world, p, "cancel infravision");
    assert!(alive(&world, innate));
    assert!(drain(&mut rx).contains("permanent"));
}
