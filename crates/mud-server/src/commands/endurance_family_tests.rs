//! The Endurance / Vitality family (fierymud-rs #149): the dev database's
//! formulas reproduce legacy `get_vitality_hp_gain` / `get_spell_duration`,
//! and a quaffed Lesser Endurance raises MAX HP for real. Test-only.

use mud_db::abilities::AbilityKind;
use mud_db::enums::ObjectType;
use mud_world::{EffectInstance, Health, Item, Keywords, Located, Named, WorldKey};

use super::skill_target_tests::arena;
use super::test_support::{ability_def, drain, object_proto};
use super::{dispatch, evaluate_simple_formula};

/// (amount formula, duration formula) exactly as in
/// `fierylib/data/sql/2026-10-13-endurance-family.sql`.
const ENDURANCE_DURATION: &str =
    "if(clamp(skill - 95, 0, 1), 5, if(skill / 20, skill / 20, 2 * clamp(skill, 0, 1)))";
const GREATER_DURATION: &str = "if(clamp(skill - 95, 0, 1), 7, skill / 15)";
const LESSER_DURATION: &str = "if(skill / 30, skill / 30, 2)";
const LESSER_AMOUNT: &str = "pow(skill, 2) / 80 + 25 - random(2, 10)";

/// Legacy `get_spell_duration` (magic.cpp), before the system's +1.
fn legacy_duration(kind: &str, skill: i32) -> i32 {
    match kind {
        "lesser" => {
            let d = skill / 30;
            if d == 0 { 2 } else { d }
        }
        "endurance" => {
            let mut d = skill / 20;
            if d == 0 && skill > 0 {
                d = 2;
            }
            if skill > 95 {
                d = 5;
            }
            d
        }
        "greater" => {
            if skill > 95 {
                7
            } else {
                skill / 15
            }
        }
        _ => unreachable!(),
    }
}

#[test]
fn duration_formulas_match_legacy_at_every_skill() {
    for skill in 0..=100 {
        for (kind, f) in [
            ("lesser", LESSER_DURATION),
            ("endurance", ENDURANCE_DURATION),
            ("greater", GREATER_DURATION),
        ] {
            assert_eq!(
                evaluate_simple_formula(f, 30, skill),
                Some(legacy_duration(kind, skill)),
                "{kind} at skill {skill}"
            );
        }
    }
}

#[test]
fn amount_formulas_match_legacy_hp_gain_minus_the_two_to_ten_wobble() {
    for (formula, divisor, base) in [
        ("pow(skill, 2) / 80 + 25 - random(2, 10)", 80, 25),
        ("pow(skill, 2) / 60 + 40 - random(2, 10)", 60, 40),
        ("pow(skill, 2) / 40 + 50 - random(2, 10)", 40, 50),
        ("pow(skill, 2) / 35 + 75 - random(2, 10)", 35, 75),
        ("pow(skill, 2) / 30 + 75 - random(2, 10)", 30, 75),
        ("pow(skill, 2) / 20 + 90 - random(2, 10)", 20, 90),
    ] {
        for skill in [0, 1, 17, 50, 83, 100] {
            let top = skill * skill / divisor + base;
            for _ in 0..20 {
                let got = evaluate_simple_formula(formula, 30, skill).unwrap();
                assert!(
                    (top - 10..=top - 2).contains(&got),
                    "{formula} @ {skill}: {got}"
                );
            }
        }
    }
}

#[test]
fn a_quaffed_lesser_endurance_raises_max_hp_and_lasts_hours_not_a_tick() {
    let (mut fx, p, mut rx) = arena();
    let mut def = ability_def(221, "Lesser Endurance", AbilityKind::Spell);
    def.cast_time_rounds = 0;
    let mut catalog = fx.world.resource_mut::<mud_world::AbilityCatalog>();
    catalog.by_name.insert("lesser endurance".into(), def);
    catalog.effects_for.insert(
        221,
        vec![(
            10,
            Some(serde_json::json!({
                "amount": LESSER_AMOUNT, "target": "max_hp",
                "duration": LESSER_DURATION, "durationUnit": "hours"
            })),
        )],
    );
    let mut modify = ability_effect_def(10);
    modify.effect_type = "modify".into();
    fx.world
        .resource_mut::<mud_world::EffectCatalog>()
        .by_id
        .insert(10, modify);
    let mut proto = object_proto(1, 41, ObjectType::Potion);
    proto.name = "a small vigor potion".into();
    proto.keywords = vec!["potion".into()];
    let mut protos = mud_world::ObjectPrototypes::default();
    protos.by_key.insert((1, 41), proto);
    fx.world.insert_resource(protos);
    let mut bindings = mud_world::ObjectAbilityCatalog::default();
    bindings.by_key.insert(
        (1, 41),
        vec![mud_world::resources::ObjectAbilityBinding {
            ability_id: 221,
            level: 10,
            charges: None,
        }],
    );
    fx.world.insert_resource(bindings);
    fx.world.spawn((
        Item,
        Named {
            name: "a small vigor potion".into(),
        },
        Keywords(vec!["potion".into()]),
        WorldKey { zone: 1, id: 41 },
        Located(p),
    ));
    dispatch(&mut fx.world, p, "quaff potion");
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
    let out = drain(&mut rx);
    assert!(!out.contains("unsupported target"), "{out}");
    let gained = fx.world.get::<Health>(p).unwrap().max - 100;
    assert!((15..=23).contains(&gained), "gained {gained}: {out}");
    let remaining = mud_world::effects_on(&mut fx.world, p)
        .into_iter()
        .find_map(|e| fx.world.get::<EffectInstance>(e).map(|i| i.remaining_secs))
        .expect("the buff is active");
    assert_eq!(remaining, 150, "two ticks, not one second: {out}");
}

fn ability_effect_def(id: i32) -> mud_world::EffectDef {
    mud_world::EffectDef {
        id,
        name: "modify".into(),
        description: None,
        effect_type: "modify".into(),
        tags: vec![],
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

#[test]
fn a_zero_roll_modify_is_not_reported_as_an_unsupported_target() {
    let (mut fx, p, mut rx) = arena();
    let mut def = ability_def(222, "Dud", AbilityKind::Spell);
    def.cast_time_rounds = 0;
    let mut catalog = fx.world.resource_mut::<mud_world::AbilityCatalog>();
    catalog.by_name.insert("dud".into(), def);
    catalog.effects_for.insert(
        222,
        vec![(
            10,
            Some(serde_json::json!({
                "amount": "skill / 4", "target": "max_hp",
                "duration": 5, "durationUnit": "hours"
            })),
        )],
    );
    fx.world
        .resource_mut::<mud_world::EffectCatalog>()
        .by_id
        .insert(10, ability_effect_def(10));
    dispatch(&mut fx.world, p, "cast dud");
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
    let out = drain(&mut rx);
    assert!(!out.contains("unsupported target"), "{out}");
}
