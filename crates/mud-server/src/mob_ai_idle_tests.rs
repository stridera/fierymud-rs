//! The idle pulse of mob spell / skill AI (WP-D): `mob_ai::mob_idle_ai_tick`
//! (buffs and cures for mobs at peace in a room with a player, meditation for
//! casters with spent slots) and the slot recovery behind it.

use std::sync::Arc;

use bevy_ecs::prelude::*;
use mud_world::{
    AiCondition, AiKind, AiTarget, AppliedTo, CombatStats, ConfigValue, EffectInstance,
    EffectSource, Fighting, Health, Located, Meditating, MobAi, Posture, PostureKind,
    SpellCooldown, SpellSlots,
};

use crate::TickCount;
use crate::commands::test_support::{drain, player_in};
use crate::memorize::memorize_tick;
use crate::mob_ai::mob_idle_ai_tick;
use crate::mob_combat_ai_tests::{
    BLESS, FX_BUFF, FX_DAMAGE, HEAL, KICK, PLAYER_HP, fixture, last_used, round, rule, set_flag,
    spawn_mob,
};

fn buff() -> mud_world::AiRule {
    let mut r = rule(BLESS, AiKind::Spell, AiTarget::SelfTarget);
    r.when = vec![AiCondition::OutOfCombat];
    r
}

/// The fixture with the fight called off: the mob and the player share a room
/// and are at peace.
fn peaceful(rules: Vec<mud_world::AiRule>) -> crate::mob_combat_ai_tests::Fixture {
    let mut f = fixture(rules);
    f.world.entity_mut(f.mob).remove::<Fighting>();
    f.world.entity_mut(f.player).remove::<Fighting>();
    f
}

fn spend_slots(world: &mut World, mob: Entity, secs: i32, count: usize) {
    let mut slots = world.get_mut::<SpellSlots>(mob).unwrap();
    for _ in 0..count {
        slots.in_flight.push(SpellCooldown {
            circle: 1,
            secs_remaining: secs,
            total_secs: secs,
        });
    }
}

fn recover(world: &mut World, passes: u64) {
    for n in 1..=passes {
        world.insert_resource(TickCount(1000 + n * 10));
        memorize_tick(world);
    }
}

fn effect(ability: i32, name: &str, on: Entity) -> (EffectInstance, AppliedTo) {
    (
        EffectInstance {
            kind: FX_BUFF,
            name: name.into(),
            strength: 1,
            remaining_secs: 600,
            source: EffectSource::Spell,
            ability_id: Some(ability),
        },
        AppliedTo(on),
    )
}

// ---- who is considered ----

#[test]
fn a_mob_at_peace_with_a_player_present_buffs_itself() {
    let mut f = peaceful(vec![buff()]);
    mob_idle_ai_tick(&mut f.world);
    assert!(last_used(&f.world, f.mob, 0).is_some());
}

#[test]
fn nothing_happens_with_the_flag_off() {
    let mut f = peaceful(vec![buff()]);
    set_flag(&mut f.world, "enabled", ConfigValue::Bool(false));
    mob_idle_ai_tick(&mut f.world);
    assert!(
        f.world.get::<MobAi>(f.mob).is_none(),
        "the AI is taken back, not run"
    );
    assert!(
        !f.world
            .query::<&AppliedTo>()
            .iter(&f.world)
            .any(|a| a.0 == f.mob)
    );
}

#[test]
fn turning_the_flag_off_strips_the_ai_and_stands_meditating_mobs_up() {
    let mut f = peaceful(vec![buff()]);
    spend_slots(&mut f.world, f.mob, 30, 1);
    // A pulse with the flag on: the buff first, then (spent slots) the sit.
    mob_idle_ai_tick(&mut f.world);
    f.world.spawn(effect(BLESS, "bless", f.mob));
    // The cast lags the mob for a round (legacy `WAIT_STATE`).
    let now = f.world.get_resource::<TickCount>().map_or(0, |t| t.0);
    f.world
        .insert_resource(TickCount(now + crate::casting::POST_CAST_LAG_TICKS));
    mob_idle_ai_tick(&mut f.world);
    assert!(f.world.get::<Meditating>(f.mob).is_some());
    // A caster with no rules (slots, no MobAi), and a player's own slots.
    let plain = spawn_mob(&mut f.world, f.room, "a novice", "novice", None);
    f.world.entity_mut(plain).insert(SpellSlots::default());
    f.world.entity_mut(f.player).insert(SpellSlots::default());
    let mundane = spawn_mob(&mut f.world, f.room, "a rat", "rat", None);

    set_flag(&mut f.world, "enabled", ConfigValue::Bool(false));
    mob_idle_ai_tick(&mut f.world);

    for mob in [f.mob, plain] {
        assert!(f.world.get::<MobAi>(mob).is_none());
        assert!(f.world.get::<SpellSlots>(mob).is_none());
        assert!(f.world.get::<Meditating>(mob).is_none());
    }
    assert_eq!(
        f.world.get::<Posture>(f.mob).unwrap().0,
        PostureKind::Standing,
        "the meditating mob stood up"
    );
    assert!(
        f.world.get::<SpellSlots>(f.player).is_some(),
        "players keep their slots"
    );
    assert!(f.world.get::<Posture>(mundane).is_some());
    // Nothing more to do on later pulses with the flag off.
    mob_idle_ai_tick(&mut f.world);
}

#[test]
fn a_room_without_a_player_is_not_considered() {
    let mut f = peaceful(vec![buff()]);
    let elsewhere = f.world.spawn_empty().id();
    f.world.entity_mut(f.player).insert(Located(elsewhere));
    mob_idle_ai_tick(&mut f.world);
    assert!(last_used(&f.world, f.mob, 0).is_none());
}

#[test]
fn a_fighting_mob_is_left_to_the_combat_tick() {
    let mut f = peaceful(vec![buff()]);
    f.world.entity_mut(f.mob).insert(Fighting(f.player));
    mob_idle_ai_tick(&mut f.world);
    assert!(last_used(&f.world, f.mob, 0).is_none());
}

#[test]
fn a_mob_a_player_is_fighting_never_acts() {
    // The mob has not (yet) swung back, but a player is on it.
    let mut f = peaceful(vec![buff()]);
    f.world.entity_mut(f.player).insert(Fighting(f.mob));
    mob_idle_ai_tick(&mut f.world);
    assert!(last_used(&f.world, f.mob, 0).is_none());
}

#[test]
fn a_stunned_mob_does_nothing() {
    let mut f = peaceful(vec![buff()]);
    f.world.entity_mut(f.mob).insert(mud_world::Stunned);
    mob_idle_ai_tick(&mut f.world);
    assert!(last_used(&f.world, f.mob, 0).is_none());
}

// ---- which rules ----

#[test]
fn combat_rules_are_not_used_at_peace_and_peace_rules_not_in_a_fight() {
    let mut fight_rule = rule(BLESS, AiKind::Spell, AiTarget::SelfTarget);
    fight_rule.when = vec![AiCondition::InCombat];
    let mut f = peaceful(vec![fight_rule]);
    mob_idle_ai_tick(&mut f.world);
    assert!(
        last_used(&f.world, f.mob, 0).is_none(),
        "an in-combat rule waits for a fight"
    );

    let mut g = fixture(vec![buff()]);
    assert!(
        round(&mut g.world, 1).is_empty(),
        "a buff rule is not used in a fight"
    );
}

#[test]
fn skills_and_hostile_spells_are_left_to_their_own_pulses() {
    let mut kick = rule(KICK, AiKind::Skill, AiTarget::Auto);
    kick.when = vec![AiCondition::OutOfCombat];
    let mut bolt = rule(
        crate::mob_combat_ai_tests::FIREBALL,
        AiKind::Spell,
        AiTarget::Auto,
    );
    bolt.when = vec![AiCondition::OutOfCombat];
    let mut f = peaceful(vec![kick, bolt]);
    mob_idle_ai_tick(&mut f.world);
    assert!(last_used(&f.world, f.mob, 0).is_none());
    assert!(last_used(&f.world, f.mob, 1).is_none(), "nobody to aim at");
    assert_eq!(f.world.get::<Health>(f.player).unwrap().hp, PLAYER_HP);
}

#[test]
fn a_buff_already_worn_is_not_recast() {
    let mut f = peaceful(vec![buff()]);
    f.world.spawn(effect(BLESS, "bless", f.mob));
    // The cast lags the mob for a round (legacy `WAIT_STATE`).
    let now = f.world.get_resource::<TickCount>().map_or(0, |t| t.0);
    f.world
        .insert_resource(TickCount(now + crate::casting::POST_CAST_LAG_TICKS));
    mob_idle_ai_tick(&mut f.world);
    assert!(last_used(&f.world, f.mob, 0).is_none());
}

#[test]
fn a_cure_fires_only_for_the_debuff_it_answers() {
    let mut cure = rule(HEAL, AiKind::Spell, AiTarget::SelfTarget);
    cure.when = vec![
        AiCondition::OutOfCombat,
        AiCondition::SelfHasDebuffNamed(vec!["poison".into()]),
    ];
    let mut f = peaceful(vec![cure]);
    f.world.get_mut::<Health>(f.mob).unwrap().hp = 50_000;
    mob_idle_ai_tick(&mut f.world);
    assert!(
        last_used(&f.world, f.mob, 0).is_none(),
        "healthy and unpoisoned"
    );
    f.world.spawn(effect(FX_DAMAGE, "poison", f.mob));
    // Different ability id on purpose: the effect name alone matches.
    f.world.get_mut::<MobAi>(f.mob).unwrap().last_use.clear();
    mob_idle_ai_tick(&mut f.world);
    assert!(last_used(&f.world, f.mob, 0).is_some());
}

#[test]
fn one_ability_per_pulse() {
    let mut second = rule(BLESS, AiKind::Spell, AiTarget::SelfTarget);
    second.when = vec![AiCondition::OutOfCombat];
    let mut f = peaceful(vec![buff(), second]);
    mob_idle_ai_tick(&mut f.world);
    let used = [0, 1]
        .iter()
        .filter(|i| last_used(&f.world, f.mob, **i).is_some())
        .count();
    assert_eq!(used, 1);
}

// ---- meditation ----

#[test]
fn a_caster_with_spent_slots_sits_down_to_meditate() {
    let mut f = peaceful(vec![]);
    spend_slots(&mut f.world, f.mob, 30, 2);
    let _ = drain(&mut f.rx);
    mob_idle_ai_tick(&mut f.world);
    assert_eq!(
        f.world.get::<Posture>(f.mob).unwrap().0,
        PostureKind::Sitting
    );
    assert!(f.world.get::<Meditating>(f.mob).is_some());
    let seen = drain(&mut f.rx);
    assert!(seen.contains("slipping into meditation"), "{seen:?}");
    // A second pulse leaves it be.
    let _ = drain(&mut f.rx);
    mob_idle_ai_tick(&mut f.world);
    assert!(f.world.get::<Meditating>(f.mob).is_some());
    assert!(drain(&mut f.rx).is_empty());
}

#[test]
fn a_caster_with_full_slots_stays_on_its_feet() {
    let mut f = peaceful(vec![]);
    mob_idle_ai_tick(&mut f.world);
    assert_eq!(
        f.world.get::<Posture>(f.mob).unwrap().0,
        PostureKind::Standing
    );
    assert!(f.world.get::<Meditating>(f.mob).is_none());
}

#[test]
fn a_mob_without_slots_never_meditates() {
    let mut f = peaceful(vec![]);
    f.world.entity_mut(f.mob).remove::<SpellSlots>();
    mob_idle_ai_tick(&mut f.world);
    assert!(f.world.get::<Meditating>(f.mob).is_none());
}

#[test]
fn an_insane_mob_cannot_meditate() {
    let mut f = peaceful(vec![]);
    spend_slots(&mut f.world, f.mob, 30, 1);
    f.world.spawn(effect(FX_DAMAGE, "insanity", f.mob));
    mob_idle_ai_tick(&mut f.world);
    assert!(f.world.get::<Meditating>(f.mob).is_none());
}

#[test]
fn buffs_come_before_meditation() {
    let mut f = peaceful(vec![buff()]);
    spend_slots(&mut f.world, f.mob, 30, 1);
    mob_idle_ai_tick(&mut f.world);
    assert!(last_used(&f.world, f.mob, 0).is_some(), "it buffs first");
    assert!(
        f.world.get::<Meditating>(f.mob).is_none(),
        "and sits on a later pulse"
    );
    f.world.spawn(effect(BLESS, "bless", f.mob));
    // The cast lags the mob for a round (legacy `WAIT_STATE`).
    let now = f.world.get_resource::<TickCount>().map_or(0, |t| t.0);
    f.world
        .insert_resource(TickCount(now + crate::casting::POST_CAST_LAG_TICKS));
    mob_idle_ai_tick(&mut f.world);
    assert!(f.world.get::<Meditating>(f.mob).is_some());
}

#[test]
fn a_meditating_mob_recovers_slots_faster_and_stands_when_full() {
    let mut f = peaceful(vec![]);
    let twin = spawn_mob(&mut f.world, f.room, "a twin mage", "twin", Some(vec![]));
    f.world.entity_mut(twin).insert(SpellSlots::default());
    spend_slots(&mut f.world, f.mob, 12, 1);
    spend_slots(&mut f.world, twin, 12, 1);
    mob_idle_ai_tick(&mut f.world);
    // The twin stays standing (it was not asked to meditate: undo for the control).
    f.world
        .entity_mut(twin)
        .remove::<Meditating>()
        .insert(Posture(PostureKind::Standing));
    assert!(f.world.get::<Meditating>(f.mob).is_some());

    recover(&mut f.world, 3);
    let left = |w: &World, e: Entity| w.get::<SpellSlots>(e).unwrap().in_flight.len();
    assert_eq!(
        left(&f.world, f.mob),
        0,
        "sitting x meditating: 12 s in 3 passes"
    );
    assert_eq!(
        left(&f.world, twin),
        1,
        "a standing caster is still waiting"
    );

    let _ = drain(&mut f.rx);
    mob_idle_ai_tick(&mut f.world);
    assert!(f.world.get::<Meditating>(f.mob).is_none());
    assert_eq!(
        f.world.get::<Posture>(f.mob).unwrap().0,
        PostureKind::Standing
    );
    assert!(drain(&mut f.rx).contains("ceases"));
}

#[test]
fn getting_into_a_fight_ends_the_trance() {
    let mut f = peaceful(vec![]);
    spend_slots(&mut f.world, f.mob, 30, 1);
    mob_idle_ai_tick(&mut f.world);
    assert!(f.world.get::<Meditating>(f.mob).is_some());
    f.world.entity_mut(f.mob).insert(Fighting(f.player));
    assert!(crate::mob_ai::stand_up_for_fight(&mut f.world, f.mob));
    assert!(f.world.get::<Meditating>(f.mob).is_none());
    assert_eq!(
        f.world.get::<Posture>(f.mob).unwrap().0,
        PostureKind::Standing
    );
}

// ---- slot recovery for mobs ----

#[test]
fn mob_slots_recover_only_for_mobs_running_spell_ai() {
    let mut f = peaceful(vec![]);
    let plain = spawn_mob(&mut f.world, f.room, "a plain mob", "plain", None);
    f.world.entity_mut(plain).insert(SpellSlots::default());
    spend_slots(&mut f.world, f.mob, 5, 1);
    spend_slots(&mut f.world, plain, 5, 1);
    recover(&mut f.world, 6);
    let left = |w: &World, e: Entity| w.get::<SpellSlots>(e).unwrap().in_flight.len();
    assert_eq!(left(&f.world, f.mob), 0, "the AI mob got its slot back");
    assert_eq!(
        left(&f.world, plain),
        1,
        "a mob without MobAi is untouched (flag-off behaviour)"
    );
}

#[test]
fn restored_slot_lines_go_to_players_only() {
    let mut f = peaceful(vec![]);
    spend_slots(&mut f.world, f.mob, 1, 1);
    let _ = drain(&mut f.rx);
    recover(&mut f.world, 2);
    assert!(
        drain(&mut f.rx).is_empty(),
        "the player in the room hears nothing about a mob's slots"
    );
}

// ---- cost ----

/// 5500 mobs in 1100 rooms, 50 of them with a player in: the pulse may only
/// look at the 250 mobs there. Hard limit only enforced in release.
#[test]
fn idle_pulse_prod_scale_is_fast() {
    const ROOMS: usize = 1100;
    const PLAYER_ROOMS: usize = 50;
    let mut f = peaceful(vec![]);
    let kit: Arc<[mud_world::AiRule]> = Arc::from(vec![buff()]);
    let mut rooms = Vec::with_capacity(ROOMS);
    for _ in 0..ROOMS {
        rooms.push(f.world.spawn_empty().id());
    }
    for (i, &room) in rooms.iter().enumerate() {
        for m in 0..5 {
            let mob = spawn_mob(&mut f.world, room, "a guard", "guard", None);
            f.world
                .entity_mut(mob)
                .insert((MobAi::new(Arc::clone(&kit)), SpellSlots::default()));
            if m == 0 {
                // One in five is a caster that has spent a slot.
                spend_slots(&mut f.world, mob, 30, 1);
            }
        }
        if i < PLAYER_ROOMS {
            let (p, rx) = player_in(&mut f.world, room);
            std::mem::forget(rx);
            f.world.entity_mut(p).insert((
                Health {
                    hp: PLAYER_HP,
                    max: PLAYER_HP,
                },
                CombatStats::default(),
            ));
        }
    }
    let start = std::time::Instant::now();
    mob_idle_ai_tick(&mut f.world);
    let first = start.elapsed();
    let start = std::time::Instant::now();
    mob_idle_ai_tick(&mut f.world);
    let second = start.elapsed();
    let acted = {
        let mut q = f.world.query::<&MobAi>();
        q.iter(&f.world).filter(|a| a.last_use.len() == 1).count()
    };
    eprintln!(
        "mob_idle_ai_tick 5500 mobs / {PLAYER_ROOMS} player rooms ({acted} buffed): \
         first={first:?} second={second:?}"
    );
    assert!(acted > 0, "the pulse really ran");
    if !cfg!(debug_assertions) {
        assert!(first.as_millis() < 10, "first pulse took {first:?}");
        assert!(second.as_millis() < 5, "second pulse took {second:?}");
    }
}
