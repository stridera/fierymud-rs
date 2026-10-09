//! Sane Mind (legacy `SPELL_SANE_MIND`, magic.cpp:4808 `mag_unaffect`): lifts
//! only insanity, confusion and crown of madness, so Sanctuary / Bless on the
//! same target survive, and says "Your mind comes back to reality." once.
//! Test-only.

use bevy_ecs::prelude::*;
use mud_world::{AppliedTo, CombatStats, EffectInstance, Health};

use super::blindness_tests::{CLEANSE, EffectRows, MODIFY, STATUS, cast, caster_with_spells};
use super::gmcp_tests::{Fx, player};
use super::test_support::{Rx, drain};

fn status(flag: &str) -> EffectRows {
    vec![(
        STATUS,
        Some(serde_json::json!({ "flag": flag, "duration": 600 })),
    )]
}

/// The seeded Sane Mind row: one cleanse over the three conditions.
fn sane_mind_row() -> EffectRows {
    vec![(
        CLEANSE,
        Some(serde_json::json!({
            "condition": ["insanity", "confusion", "crown_of_madness"],
            "scope": "all",
            "message": "Your mind comes back to reality.",
            "roomMessage": "{target.name} regains {target.his} senses.",
        })),
    )]
}

/// Caster, patient "Seer" and a bystander in room A, with the spells the
/// tests cast on the patient.
fn setup() -> (Fx, Entity, Rx, Entity, Rx, Rx) {
    let (mut fx, caster, crx) = caster_with_spells(vec![
        (1, "Sanctuary", status("sanctuary")),
        (2, "Bless", status("bless")),
        (3, "Confusion", status("confused")),
        (4, "Crown Of Madness", status("confused")),
        (
            5,
            "Insanity",
            vec![(
                MODIFY,
                Some(serde_json::json!({ "target": "wis", "amount": "-50", "duration": 600 })),
            )],
        ),
        (6, "Sane Mind", sane_mind_row()),
    ]);
    let a = fx.a;
    let (seer, srx) = player(&mut fx.world, a, "Seer");
    let (other, orx) = player(&mut fx.world, a, "Bystander");
    for p in [seer, other] {
        fx.world
            .entity_mut(p)
            .insert((Health { hp: 100, max: 100 }, CombatStats::default()));
    }
    (fx, caster, crx, seer, srx, orx)
}

fn effect_names(world: &mut World, target: Entity) -> Vec<String> {
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    let mut names: Vec<String> = q
        .iter(world)
        .filter(|(_, a)| a.0 == target)
        .map(|(e, _)| e.name.to_ascii_lowercase())
        .collect();
    names.sort();
    names
}

fn has(names: &[String], n: &str) -> bool {
    names.iter().any(|x| x == n)
}

#[test]
fn sane_mind_removes_confusion_and_keeps_sanctuary_and_bless() {
    let (mut fx, caster, mut crx, seer, mut srx, mut orx) = setup();
    for spell in ["sanctuary", "bless", "confusion"] {
        cast(&mut fx, caster, &format!("cast '{spell}' seer"));
    }
    let before = effect_names(&mut fx.world, seer);
    assert!(has(&before, "sanctuary"), "{before:?}");
    assert!(has(&before, "bless"), "{before:?}");
    assert!(has(&before, "confused"), "{before:?}");
    let _ = (drain(&mut crx), drain(&mut srx), drain(&mut orx));

    cast(&mut fx, caster, "cast 'sane mind' seer");

    let after = effect_names(&mut fx.world, seer);
    assert!(!has(&after, "confused"), "{after:?}");
    assert!(has(&after, "sanctuary"), "{after:?}");
    assert!(has(&after, "bless"), "{after:?}");
    let (cured, caster_saw, bystander) = (drain(&mut srx), drain(&mut crx), drain(&mut orx));
    assert!(
        cured.contains("Your mind comes back to reality."),
        "{cured}"
    );
    for room in [&caster_saw, &bystander] {
        assert!(room.contains("Seer regains"), "{room}");
        assert!(!room.contains("Your mind comes back"), "{room}");
    }
}

#[test]
fn sane_mind_removes_crown_of_madness_and_insanity_with_one_message() {
    let (mut fx, caster, mut crx, seer, mut srx, mut orx) = setup();
    for spell in ["bless", "crown of madness", "confusion", "insanity"] {
        cast(&mut fx, caster, &format!("cast '{spell}' seer"));
    }
    let before = effect_names(&mut fx.world, seer);
    assert!(
        has(&before, "wis"),
        "insanity's debuff is present: {before:?}"
    );
    let _ = (drain(&mut crx), drain(&mut srx), drain(&mut orx));

    cast(&mut fx, caster, "cast 'sane mind' seer");

    let after = effect_names(&mut fx.world, seer);
    assert_eq!(after, vec!["bless".to_string()], "{after:?}");
    let cured = drain(&mut srx);
    assert_eq!(
        cured.matches("Your mind comes back to reality.").count(),
        1,
        "{cured}"
    );
}

#[test]
fn sane_mind_on_a_sound_mind_removes_nothing_and_prints_nothing() {
    let (mut fx, caster, mut crx, seer, mut srx, mut orx) = setup();
    cast(&mut fx, caster, "cast 'sanctuary' seer");
    cast(&mut fx, caster, "cast 'bless' seer");
    let _ = (drain(&mut crx), drain(&mut srx), drain(&mut orx));
    cast(&mut fx, caster, "cast 'sane mind' seer");
    let after = effect_names(&mut fx.world, seer);
    assert!(
        has(&after, "sanctuary") && has(&after, "bless"),
        "{after:?}"
    );
    for out in [drain(&mut srx), drain(&mut crx), drain(&mut orx)] {
        assert!(!out.contains("Your mind comes back"), "{out}");
        assert!(!out.contains("regains"), "{out}");
    }
}
