//! Eye Gouge (legacy "make gouge more like an attack roll", stridera/fierymud#403):
//! a skill whose effect rows carry `"attackRoll": true` rolls to hit with the
//! melee accuracy/evasion contest, plus the target's dodge/parry, instead of
//! always landing. Test-only.

use bevy_ecs::prelude::*;
use mud_world::{Blinded, CombatStats, Fighting, Posture, PostureKind};

use super::blindness_tests::{STATUS, cast, caster_with_spells};
use super::test_support::drain;
use crate::combat::{SkillAttack, force_hit_roll, skill_attack_roll};

fn gouge_rows(attack_roll: bool) -> Vec<(i32, Option<serde_json::Value>)> {
    let mut params = serde_json::json!({"flag": "blind", "duration": 1, "durationUnit": "hours"});
    if attack_roll {
        params["attackRoll"] = serde_json::json!(true);
    }
    vec![(STATUS, Some(params))]
}

/// A caster (skill 50) with a violent Gouge, and a rat with accuracy and
/// evasion 50 in the room.
fn gouge_fixture(
    attack_roll: bool,
) -> (
    super::gmcp_tests::Fx,
    Entity,
    super::test_support::Rx,
    Entity,
) {
    let (mut fx, p, rx) = caster_with_spells(vec![(1, "Gouge", gouge_rows(attack_roll))]);
    {
        let mut catalog = fx.world.resource_mut::<mud_world::AbilityCatalog>();
        catalog.by_name.get_mut("gouge").unwrap().violent = true;
        catalog.messages.insert(
            1,
            mud_world::AbilityMessageSet {
                fail_to_caster: Some("Your lunge goes wide.".into()),
                fail_to_victim: Some("{actor.name} grabs for your eyes and misses.".into()),
                ..Default::default()
            },
        );
    }
    let a = fx.a;
    let rat = fx
        .world
        .spawn((
            mud_world::Mob,
            mud_world::Named {
                name: "a sewer rat".into(),
            },
            mud_world::Located(a),
            CombatStats {
                accuracy: 50,
                evasion: 50,
                ..CombatStats::default()
            },
            mud_world::Health { hp: 100, max: 100 },
        ))
        .id();
    (fx, p, rx, rat)
}

#[test]
fn a_gouge_that_rolls_too_low_misses_and_starts_the_fight() {
    let (mut fx, p, mut rx, rat) = gouge_fixture(true);
    // Accuracy 50 + 50 / 2 against evasion 50: 62% to hit; a 100 misses.
    force_hit_roll(Some(100));
    cast(&mut fx, p, "cast 'gouge' rat");
    force_hit_roll(None);
    let said = drain(&mut rx);
    assert!(said.contains("Your lunge goes wide."), "{said}");
    assert!(
        fx.world.get::<Blinded>(rat).is_none(),
        "no blinding: {said}"
    );
    assert!(
        fx.world.get::<Fighting>(rat).is_some(),
        "a missed gouge is still an attack"
    );
}

#[test]
fn a_gouge_that_rolls_well_lands() {
    let (mut fx, p, mut rx, rat) = gouge_fixture(true);
    force_hit_roll(Some(1));
    cast(&mut fx, p, "cast 'gouge' rat");
    force_hit_roll(None);
    let said = drain(&mut rx);
    assert!(fx.world.get::<Blinded>(rat).is_some(), "{said}");
    assert!(!said.contains("goes wide"), "{said}");
}

#[test]
fn without_the_attack_roll_flag_the_ability_always_lands() {
    let (mut fx, p, _rx, rat) = gouge_fixture(false);
    force_hit_roll(Some(100));
    cast(&mut fx, p, "cast 'gouge' rat");
    force_hit_roll(None);
    assert!(fx.world.get::<Blinded>(rat).is_some());
}

#[test]
fn a_sleeping_target_cannot_be_missed() {
    let (mut fx, p, _rx, rat) = gouge_fixture(true);
    fx.world
        .entity_mut(rat)
        .insert(Posture(PostureKind::Sleeping));
    force_hit_roll(Some(100));
    cast(&mut fx, p, "cast 'gouge' rat");
    force_hit_roll(None);
    assert!(fx.world.get::<Blinded>(rat).is_some());
}

#[test]
fn skill_adds_half_its_percent_to_accuracy() {
    let mut world = World::new();
    let attacker = world
        .spawn(CombatStats {
            accuracy: 50,
            evasion: 50,
            ..CombatStats::default()
        })
        .id();
    let target = world
        .spawn(CombatStats {
            accuracy: 50,
            evasion: 50,
            ..CombatStats::default()
        })
        .id();
    // Skill 0: 50% to hit, so a roll of 60 misses; skill 100 adds 50 / 2 = 25
    // accuracy, 62% to hit, and the same roll lands.
    force_hit_roll(Some(60));
    assert_eq!(
        skill_attack_roll(&world, attacker, target, 0),
        SkillAttack::Miss
    );
    assert_eq!(
        skill_attack_roll(&world, attacker, target, 100),
        SkillAttack::Hit
    );
    // A defender who is easier to hit (resting) shifts the same roll.
    world
        .entity_mut(target)
        .insert(Posture(PostureKind::Resting));
    assert_eq!(
        skill_attack_roll(&world, attacker, target, 0),
        SkillAttack::Hit
    );
    force_hit_roll(None);
}
