//! Per-bearer effect lookups go through the `EffectsByBearer` index instead of
//! scanning every `EffectInstance` in the world (see `mud_world::effects_on`).
//! These tests pin that the converted call sites still answer per bearer, and
//! show the cost no longer grows with the number of effects elsewhere. Run
//! the timing tests with `--nocapture` to see the numbers.

use std::sync::Arc;
use std::time::Instant;

use bevy_ecs::prelude::*;
use mud_world::{
    AiCondition, AiKind, AiTarget, AppliedTo, EffectInstance, EffectSource, Fighting, MobAi,
    SpellCooldown, SpellSlots,
};

use crate::TickCount;
use crate::mob_ai::{has_named_debuff, mob_idle_ai_tick};
use crate::mob_combat_ai_tests::{BLESS, FX_BUFF, Fixture, fixture, round, rule, spawn_mob};

fn instance(name: &str, ability_id: Option<i32>, secs: i32) -> EffectInstance {
    EffectInstance {
        kind: FX_BUFF,
        name: name.into(),
        strength: 1,
        remaining_secs: secs,
        source: EffectSource::Spell,
        ability_id,
    }
}

fn put(world: &mut World, on: Entity, name: &str, ability_id: Option<i32>) -> Entity {
    world
        .spawn((instance(name, ability_id, 600), AppliedTo(on)))
        .id()
}

/// Reference: the world-wide scan the converted sites used to do.
fn scan_on(world: &mut World, bearer: Entity) -> Vec<Entity> {
    let mut q = world.query::<(Entity, &AppliedTo)>();
    q.iter(world)
        .filter(|(_, a)| a.0 == bearer)
        .map(|(e, _)| e)
        .collect()
}

/// Reference: the old `has_named_debuff` body, minus the catalog lookups
/// (these tests use plain effect names).
fn scan_has_named_debuff(world: &mut World, entity: Entity, name: &str) -> bool {
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    q.iter(world).any(|(eff, applied)| {
        applied.0 == entity
            && !mud_world::mob_effects::is_innate_effect(&eff.source)
            && eff.name.eq_ignore_ascii_case(name)
    })
}

#[test]
fn effects_on_ro_agrees_with_the_index_and_with_a_scan() {
    for with_index in [false, true] {
        let mut world = World::new();
        if with_index {
            world.insert_resource(mud_world::EffectsByBearer::default());
        }
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let a1 = put(&mut world, a, "bless", None);
        let b1 = put(&mut world, b, "bless", None);
        let a2 = put(&mut world, a, "sanctuary", None);
        assert_eq!(mud_world::effects_on_ro(&world, a), vec![a1, a2]);
        assert_eq!(mud_world::effects_on_ro(&world, b), vec![b1]);
        assert!(mud_world::effects_on_ro(&world, Entity::PLACEHOLDER).is_empty());
        world.despawn(a1);
        assert_eq!(mud_world::effects_on_ro(&world, a), vec![a2]);
        assert_eq!(mud_world::effects_on(&mut world, a), scan_on(&mut world, a));
    }
}

#[test]
fn any_effect_on_only_looks_at_its_own_bearer() {
    let mut world = World::new();
    let a = world.spawn_empty().id();
    let b = world.spawn_empty().id();
    put(&mut world, a, "Bleed", None);
    assert!(mud_world::any_effect_on(&mut world, a, |e| e
        .name
        .eq_ignore_ascii_case("bleed")));
    assert!(!mud_world::any_effect_on(&mut world, b, |_| true));
    assert!(!mud_world::any_effect_on(&mut world, a, |e| e.name == "other"));
}

#[test]
fn name_lookups_and_removals_touch_only_the_target() {
    for with_index in [false, true] {
        let mut world = World::new();
        if with_index {
            world.insert_resource(mud_world::EffectsByBearer::default());
        }
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        for who in [a, b] {
            put(&mut world, who, "bleed", None);
            put(&mut world, who, "poison", None);
        }
        assert!(crate::commands::has_effect_named(&mut world, a, "BLEED"));
        assert!(!crate::commands::has_effect_named(&mut world, a, "stun"));
        assert_eq!(
            crate::commands::remove_effect_named(&mut world, a, "bleed"),
            1
        );
        assert!(!crate::commands::has_effect_named(&mut world, a, "bleed"));
        assert!(crate::commands::has_effect_named(&mut world, b, "bleed"));
        assert_eq!(crate::commands::remove_all_effects_on(&mut world, a), 1);
        assert!(!crate::commands::has_effect_named(&mut world, a, "poison"));
        assert!(crate::commands::has_effect_named(&mut world, b, "poison"));
    }
}

#[test]
fn a_named_debuff_is_found_on_its_bearer_only() {
    let mut world = World::new();
    let a = world.spawn_empty().id();
    let b = world.spawn_empty().id();
    put(&mut world, a, "insanity", None);
    let names = ["insanity".to_string()];
    assert!(has_named_debuff(&mut world, a, &names));
    assert!(!has_named_debuff(&mut world, b, &names));
    // Innate (race / worn-gear) copies never count as a debuff.
    let c = world.spawn_empty().id();
    world.spawn((
        EffectInstance {
            source: EffectSource::Other(mud_world::mob_effects::RACE_EFFECT_SOURCE.to_string()),
            ..instance("insanity", None, -1)
        },
        AppliedTo(c),
    ));
    assert!(!has_named_debuff(&mut world, c, &names));
}

// ---- cost ----

const BYSTANDER_MOBS: usize = 5200;
const EFFECTS_PER_MOB: usize = 3;

/// ~5500 mobs carrying a few permanent effects each (race / mob-default
/// grants, as in the live world), spread over many rooms.
fn crowd(world: &mut World) {
    let rooms: Vec<Entity> = (0..500).map(|_| world.spawn_empty().id()).collect();
    for i in 0..BYSTANDER_MOBS {
        let mob = spawn_mob(
            world,
            rooms[i % rooms.len()],
            "a villager",
            "villager",
            None,
        );
        for n in 0..EFFECTS_PER_MOB {
            world.spawn((
                EffectInstance {
                    source: EffectSource::Other("race".into()),
                    ..instance(["fly", "infravision", "detect_invis"][n % 3], None, -1)
                },
                AppliedTo(mob),
            ));
        }
    }
}

/// The fixture's room with `casters` idle casters in it: each already wears
/// Bless (so its buff rule has nothing to do) and has spent a spell slot (so
/// the idle pulse asks whether it is insane before sitting to meditate).
fn idle_casters(f: &mut Fixture, casters: usize) -> Vec<Entity> {
    f.world.entity_mut(f.mob).remove::<Fighting>();
    f.world.entity_mut(f.player).remove::<Fighting>();
    let mut buff = rule(BLESS, AiKind::Spell, AiTarget::SelfTarget);
    buff.when = vec![AiCondition::OutOfCombat];
    let rules: Arc<[mud_world::AiRule]> = Arc::from(vec![buff]);
    let mobs: Vec<Entity> = (0..casters)
        .map(|_| {
            let m = spawn_mob(&mut f.world, f.room, "a mage", "mage", None);
            let mut slots = SpellSlots::default();
            slots.in_flight.push(SpellCooldown {
                circle: 1,
                secs_remaining: 600,
                total_secs: 600,
            });
            f.world
                .entity_mut(m)
                .insert((MobAi::new(Arc::clone(&rules)), slots));
            put(&mut f.world, m, "bless", Some(BLESS));
            m
        })
        .collect();
    mobs
}

/// Mob AI per idle pulse: with many effects elsewhere in the world, the
/// per-mob "insane?" check (and every other effect question the pulse asks)
/// must cost the mob's own effects, not the world's.
#[test]
fn idle_pulse_cost_ignores_other_bearers_effects() {
    let mut f = fixture(Vec::new());
    let casters = idle_casters(&mut f, 300);
    crowd(&mut f.world);
    let effects = f.world.query::<&EffectInstance>().iter(&f.world).count();
    let names = ["insanity".to_string()];
    let _ = mud_world::effects_on(&mut f.world, casters[0]); // build the index

    // The hot per-mob call: indexed vs the old world scan.
    let t = Instant::now();
    for &m in &casters {
        assert!(!std::hint::black_box(has_named_debuff(
            &mut f.world,
            m,
            &names
        )));
    }
    let indexed = t.elapsed();
    let t = Instant::now();
    for &m in &casters {
        assert!(!std::hint::black_box(scan_has_named_debuff(
            &mut f.world,
            m,
            "insanity"
        )));
    }
    let scan = t.elapsed();
    eprintln!(
        "300 casters x insanity check, {effects} effects in world: indexed {indexed:?}; world scan {scan:?}"
    );
    assert!(indexed < scan, "indexed {indexed:?} vs scan {scan:?}");

    // And the whole pulse still works: every caster sits down to meditate.
    f.world.insert_resource(TickCount(10_000));
    let t = Instant::now();
    mob_idle_ai_tick(&mut f.world);
    eprintln!(
        "idle pulse over 300 casters, {effects} effects: {:?}",
        t.elapsed()
    );
    for &m in &casters {
        assert!(
            f.world.get::<mud_world::Meditating>(m).is_some(),
            "caster sits to meditate"
        );
    }
}

/// A combat round of mob AI that asks about effects must not sweep the world.
#[test]
fn combat_round_cost_ignores_other_bearers_effects() {
    let mut rules = vec![rule(BLESS, AiKind::Spell, AiTarget::SelfTarget)];
    rules[0].when = vec![AiCondition::SelfHasDebuff, AiCondition::InCombat];
    let mut f = fixture(rules);
    crowd(&mut f.world);
    let effects = f.world.query::<&EffectInstance>().iter(&f.world).count();
    let _ = mud_world::effects_on(&mut f.world, f.mob); // build the index
    let t = Instant::now();
    for n in 1..=50 {
        round(&mut f.world, n);
    }
    let indexed = t.elapsed();
    let t = Instant::now();
    for _ in 0..50 {
        let mut q = f.world.query::<(&EffectInstance, &AppliedTo)>();
        std::hint::black_box(q.iter(&f.world).filter(|(_, a)| a.0 == f.mob).count());
    }
    let scan = t.elapsed();
    eprintln!("50 combat AI rounds, {effects} effects: {indexed:?}; 50 bare world scans {scan:?}");
    assert!(indexed < scan, "indexed {indexed:?} vs scan {scan:?}");
}
