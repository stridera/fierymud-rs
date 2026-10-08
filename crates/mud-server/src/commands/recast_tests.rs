//! Recasting a buff replaces the old instance without leaving its numeric
//! bonus behind (resistance bumps used to stack without limit).

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::ElementType;
use mud_world::{AppliedTo, EffectInstance, Health, KnownAbilities, Resistances};

use super::dispatch;
use super::gmcp_tests::{Fx, fixture, player};
use super::test_support::{Rx, ability_def};

const STONE_SKIN: i32 = 1;
const EFFECT: i32 = 10;
const BUMP: i32 = 30;

/// A world where `stone skin` is a real spell applying an earth
/// `resistance` status effect.
fn world_with_stone_skin() -> (Fx, Entity, Rx) {
    let mut fx = fixture();
    let mut catalog = mud_world::AbilityCatalog::default();
    let mut spell = ability_def(STONE_SKIN, "Stone Skin", AbilityKind::Spell);
    spell.cast_time_rounds = 0;
    catalog.by_name.insert("stone skin".to_string(), spell);
    catalog.effects_for.insert(
        STONE_SKIN,
        vec![(
            EFFECT,
            Some(serde_json::json!({
                "flag": "resistance", "type": "earth", "amount": BUMP, "duration": 60
            })),
        )],
    );
    fx.world.insert_resource(catalog);
    let mut effects = mud_world::EffectCatalog::default();
    effects.by_id.insert(
        EFFECT,
        mud_world::EffectDef {
            id: EFFECT,
            name: "resistance".to_string(),
            description: None,
            effect_type: "status".to_string(),
            tags: Vec::new(),
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
    fx.world.insert_resource(effects);
    fx.world
        .insert_resource(mud_world::SpellSlotData::default());
    fx.world
        .insert_resource(mud_world::ClassSkillsData::default());
    let a = fx.a;
    let (p, rx) = player(&mut fx.world, a, "Caster");
    fx.world.entity_mut(p).insert((
        Health { hp: 50, max: 50 },
        KnownAbilities {
            entries: vec![(STONE_SKIN, 500, true)],
        },
    ));
    (fx, p, rx)
}

fn cast_once(fx: &mut Fx, p: Entity) {
    dispatch(&mut fx.world, p, "cast 'stone skin'");
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
}

fn earth(world: &World, p: Entity) -> Option<i32> {
    world
        .get::<Resistances>(p)
        .and_then(|r| r.0.get(&ElementType::Earth).copied())
}

fn instances(world: &mut World, p: Entity) -> usize {
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    q.iter(world).filter(|(_, a)| a.0 == p).count()
}

#[test]
fn recasting_stone_skin_keeps_exactly_one_resistance_bump() {
    let (mut fx, p, _rx) = world_with_stone_skin();
    cast_once(&mut fx, p);
    assert_eq!(earth(&fx.world, p), Some(BUMP), "first cast applies");
    cast_once(&mut fx, p);
    cast_once(&mut fx, p);
    assert_eq!(earth(&fx.world, p), Some(BUMP), "recasts do not stack");
    assert_eq!(instances(&mut fx.world, p), 1, "one live instance");
}
