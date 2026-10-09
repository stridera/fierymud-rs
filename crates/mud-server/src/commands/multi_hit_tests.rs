//! Multi-bolt damage spells (Magic Missile, Ice Darts, `multihit: true`):
//! legacy calls `mag_damage` once per missile, so every dart gets its own
//! caster, victim and room message and its own damage line. Test-only.

use bevy_ecs::prelude::*;
use mud_world::{AbilityCatalog, AbilityMessageSet, Health, Mob, Named};

use super::blindness_tests::{DAMAGE, cast, caster_with_spells};
use super::gmcp_tests::{Fx, player};
use super::test_support::{Rx, drain};

fn dart_caster(multihit: bool) -> (Fx, Entity, Rx, Entity) {
    dart_caster_with(multihit, "5")
}

fn dart_caster_with(multihit: bool, bolt_count: &str) -> (Fx, Entity, Rx, Entity) {
    let (mut fx, p, rx) = caster_with_spells(vec![(
        1,
        "Ice Darts",
        vec![(
            DAMAGE,
            Some(serde_json::json!({
                "type": "magic", "amount": "7", "multihit": multihit,
                "boltCount": bolt_count,
            })),
        )],
    )]);
    fx.world.resource_mut::<AbilityCatalog>().messages.insert(
        1,
        AbilityMessageSet {
            success_to_caster: Some("You sling an ice dart at {target.name}.".into()),
            success_to_victim: Some("{actor.name} slings an ice dart at you.".into()),
            success_to_room: Some("{actor.name} slings an ice dart at {target.name}.".into()),
            ..AbilityMessageSet::default()
        },
    );
    let room = fx.a;
    let rat = fx
        .world
        .spawn((
            Mob,
            Named {
                name: "a sewer rat".into(),
            },
            mud_world::Located(room),
            mud_world::CombatStats::default(),
            Health { hp: 500, max: 500 },
        ))
        .id();
    (fx, p, rx, rat)
}

#[test]
fn every_dart_gets_its_own_message_and_damage_line() {
    let (mut fx, p, mut rx, rat) = dart_caster(true);
    let room = fx.a;
    let (_watcher, mut wrx) = player(&mut fx.world, room, "Watcher");
    let _ = drain(&mut rx);
    let _ = drain(&mut wrx);
    cast(&mut fx, p, "cast 'ice darts' rat");
    let out = drain(&mut rx);
    // The row's `boltCount` formula is the constant 5.
    assert_eq!(
        out.matches("You sling an ice dart at a sewer rat.").count(),
        5,
        "{out}"
    );
    assert_eq!(
        out.matches("Your Ice Darts hits").count(),
        5,
        "one damage line per dart: {out}"
    );
    assert_eq!(out.matches("for 7 damage").count(), 5, "{out}");
    assert_eq!(fx.world.get::<Health>(rat).unwrap().hp, 500 - 5 * 7);
    let seen = drain(&mut wrx);
    assert_eq!(
        seen.matches("slings an ice dart at a sewer rat.").count(),
        5,
        "{seen}"
    );
}

#[test]
fn a_single_bolt_spell_keeps_one_message() {
    let (mut fx, p, mut rx, rat) = dart_caster(false);
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'ice darts' rat");
    let out = drain(&mut rx);
    assert_eq!(out.matches("You sling an ice dart").count(), 1, "{out}");
    assert_eq!(out.matches("for 7 damage").count(), 1, "{out}");
    assert_eq!(fx.world.get::<Health>(rat).unwrap().hp, 493);
}

#[test]
fn darts_stop_when_the_victim_dies_and_pelt_the_corpse() {
    let (mut fx, p, mut rx, rat) = dart_caster(true);
    fx.world.get_mut::<Health>(rat).unwrap().hp = 10;
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'ice darts' rat");
    let out = drain(&mut rx);
    assert_eq!(out.matches("for 7 damage").count(), 2, "{out}");
    assert_eq!(out.matches("dead already").count(), 1, "{out}");
}

#[test]
fn bolt_count_is_clamped_to_the_cap() {
    use super::{FormulaCtx, MAX_BOLT_COUNT, resolve_bolt_count};
    let ctx = FormulaCtx::default();
    let huge = serde_json::json!({"boltCount": "1000"});
    assert_eq!(
        resolve_bolt_count(Some(&huge), None, &ctx, "Test Darts"),
        MAX_BOLT_COUNT
    );
    // A second clamp for the same ability still clamps (the warning is
    // only logged once, the clamp is not).
    assert_eq!(
        resolve_bolt_count(Some(&huge), None, &ctx, "test darts"),
        MAX_BOLT_COUNT
    );
    let at_cap = serde_json::json!({"boltCount": "10"});
    assert_eq!(resolve_bolt_count(Some(&at_cap), None, &ctx, "x"), 10);
}

#[test]
fn an_oversized_bolt_count_fires_at_most_the_cap() {
    let (mut fx, p, mut rx, rat) = dart_caster_with(true, "1000");
    {
        let mut hp = fx.world.get_mut::<Health>(rat).unwrap();
        hp.hp = 5000;
        hp.max = 5000;
    }
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'ice darts' rat");
    let out = drain(&mut rx);
    assert_eq!(out.matches("for 7 damage").count(), 10, "{out}");
    assert_eq!(fx.world.get::<Health>(rat).unwrap().hp, 5000 - 10 * 7);
}

/// The seeded Ice Darts / Magic Missile `boltCount` (legacy `spell_ice_darts`,
/// `spell_magic_missile`): 1 + one chance roll per skill tier, each gated on
/// proficiency. `clamp(x, 0, 1)` turns `skill >= N` / `roll > P` into 0 or 1.
const LEGACY_BOLTS: &str = "1 + clamp(skill - 4, 0, 1) * clamp(random(1, 100) - 80, 0, 1) \
    + clamp(skill - 13, 0, 1) * clamp(random(1, 100) - 75, 0, 1) \
    + clamp(skill - 23, 0, 1) * clamp(random(1, 100) - 60, 0, 1) \
    + clamp(skill - 33, 0, 1) * clamp(random(1, 100) - 55, 0, 1) \
    + clamp(skill - 43, 0, 1) * clamp(random(1, 100) - 50, 0, 1) \
    + clamp(skill - 73, 0, 1) * clamp(random(1, 100) - 25, 0, 1)";

/// Evaluate the legacy formula with every `random(1, 100)` pinned to `roll`.
fn bolts(skill: i32, roll: i32) -> i32 {
    let ctx = super::FormulaCtx {
        skill,
        ..super::FormulaCtx::default()
    };
    super::evaluate_formula(LEGACY_BOLTS, &ctx, &mut |_, _, _| roll).expect("formula parses")
}

#[test]
fn legacy_bolt_count_is_gated_by_skill_tier() {
    // A winning roll (100 beats every threshold) adds one dart per tier the
    // caster has reached: skill >= 5, 14, 24, 34, 44, 74.
    for (skill, expected) in [
        (0, 1),
        (4, 1),
        (5, 2),
        (13, 2),
        (14, 3),
        (23, 3),
        (24, 4),
        (33, 4),
        (34, 5),
        (43, 5),
        (44, 6),
        (73, 6),
        (74, 7),
        (100, 7),
    ] {
        assert_eq!(bolts(skill, 100), expected, "skill {skill}");
    }
}

#[test]
fn legacy_bolt_count_needs_the_roll_to_beat_each_threshold() {
    // `random_number(1, 100) > P` with P = 80, 75, 60, 55, 50, 25.
    for (roll, expected) in [
        (1, 1),
        (25, 1),
        (26, 2),
        (51, 3),
        (56, 4),
        (61, 5),
        (76, 6),
        (81, 7),
    ] {
        assert_eq!(bolts(100, roll), expected, "roll {roll}");
    }
}

#[test]
fn bolt_count_comes_from_the_row_and_never_drops_below_one() {
    use super::{FormulaCtx, resolve_bolt_count};
    let ctx = FormulaCtx::default();
    let row = serde_json::json!({"multihit": true, "boltCount": "3"});
    let default = serde_json::json!({"boltCount": "6"});
    assert_eq!(resolve_bolt_count(Some(&row), Some(&default), &ctx, "t"), 3);
    assert_eq!(resolve_bolt_count(None, Some(&default), &ctx, "t"), 6);
    assert_eq!(resolve_bolt_count(None, None, &ctx, "t"), 1);
    let negative = serde_json::json!({"boltCount": "0 - 4"});
    assert_eq!(resolve_bolt_count(Some(&negative), None, &ctx, "t"), 1);
    // The seeded formula, live RNG: always within the legacy 1..=7 range.
    let live = serde_json::json!({ "boltCount": LEGACY_BOLTS });
    for _ in 0..200 {
        let ctx = FormulaCtx {
            skill: 100,
            ..FormulaCtx::default()
        };
        assert!((1..=7).contains(&resolve_bolt_count(Some(&live), None, &ctx, "t")));
        let low = FormulaCtx {
            skill: 4,
            ..FormulaCtx::default()
        };
        assert_eq!(resolve_bolt_count(Some(&live), None, &low, "t"), 1);
    }
}
