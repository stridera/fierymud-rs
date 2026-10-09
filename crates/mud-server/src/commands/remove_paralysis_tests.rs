//! Remove Paralysis (legacy `SPELL_REMOVE_PARALYSIS`, magic.cpp:4837
//! `mag_unaffect`): lifts Minor Paralysis, Major Paralysis and Entangle (and
//! nothing else holding a body still, such as Web), says "Your body begins to
//! move again." only when something was lifted, and the Stunned marker that
//! the paralysis backed goes with it so the patient can act again.
//! Test-only.

use bevy_ecs::prelude::*;
use mud_world::{AppliedTo, CombatStats, EffectInstance, Health, Stunned};

use super::blindness_tests::{CLEANSE, EffectRows, STATUS, cast, caster_with_spells};
use super::gmcp_tests::{Fx, player};
use super::test_support::{Rx, drain};
use super::{Prevent, effect_prevents, force_status_upgrade_roll, is_immobilized};

/// The seeded Remove Paralysis row: one cleanse over the three legacy spells.
fn remove_paralysis_row() -> EffectRows {
    vec![(
        CLEANSE,
        Some(serde_json::json!({
            "condition": ["minor_paralysis", "major_paralysis", "entangle"],
            "scope": "all",
            "message": "<b:yellow>Your body begins to move again.</>",
            "roomMessage": "<b:yellow>{target.name} begins to move again.</>",
            "noopMessage": "{target.name} can already move just fine.",
            "noopMessageSelf": "You can already move just fine.",
        })),
    )]
}

fn status(flag: &str, break_on_damage: bool) -> EffectRows {
    vec![(
        STATUS,
        Some(serde_json::json!({
            "flag": flag,
            "duration": 600,
            "breakOnDamage": break_on_damage,
        })),
    )]
}

/// The seeded Entangle row: webbed, with a chance of Major Paralysis.
fn entangle_row() -> EffectRows {
    vec![(
        STATUS,
        Some(serde_json::json!({
            "flag": "webbed",
            "duration": 600,
            "breakOnDamage": true,
            "upgrade": {
                "minSkill": 40,
                "chance": "2 + skill / 14",
                "flag": "paralyzed",
                "duration": 600,
            },
        })),
    )]
}

/// Caster, patient "Seer" and a bystander in room A.
fn setup() -> (Fx, Entity, Rx, Entity, Rx, Rx) {
    let (mut fx, caster, crx) = caster_with_spells(vec![
        (1, "Entangle", entangle_row()),
        (2, "Minor Paralysis", status("paralyzed", true)),
        (3, "Major Paralysis", status("paralyzed", false)),
        (4, "Web", status("webbed", false)),
        (5, "Bless", status("bless", false)),
        (6, "Remove Paralysis", remove_paralysis_row()),
    ]);
    let a = fx.a;
    let (seer, srx) = player(&mut fx.world, a, "Seer");
    let (_other, orx) = player(&mut fx.world, a, "Bystander");
    fx.world
        .entity_mut(seer)
        .insert((Health { hp: 100, max: 100 }, CombatStats::default()));
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

fn can_act(fx: &mut Fx, who: Entity) -> bool {
    fx.world.get::<Stunned>(who).is_none()
        && !is_immobilized(&mut fx.world, who)
        && !effect_prevents(&mut fx.world, who, Prevent::Movement)
        && !effect_prevents(&mut fx.world, who, Prevent::Casting)
}

const MOVES_AGAIN: &str = "Your body begins to move again.";

#[test]
fn remove_paralysis_frees_an_entangled_player_and_clears_the_stunned_marker() {
    let (mut fx, caster, mut crx, seer, mut srx, mut orx) = setup();
    // Roll 0 always upgrades: Entangle lands Major Paralysis (name "paralyzed").
    force_status_upgrade_roll(Some(0));
    cast(&mut fx, caster, "cast 'entangle' seer");
    force_status_upgrade_roll(None);
    assert_eq!(effect_names(&mut fx.world, seer), vec!["paralyzed"]);
    assert!(fx.world.get::<Stunned>(seer).is_some(), "held");
    assert!(!can_act(&mut fx, seer));
    let _ = (drain(&mut crx), drain(&mut srx), drain(&mut orx));

    cast(&mut fx, caster, "cast 'remove paralysis' seer");

    assert!(effect_names(&mut fx.world, seer).is_empty());
    assert!(fx.world.get::<Stunned>(seer).is_none(), "marker cleared");
    assert!(can_act(&mut fx, seer));
    let (cured, caster_saw, bystander) = (drain(&mut srx), drain(&mut crx), drain(&mut orx));
    assert_eq!(cured.matches(MOVES_AGAIN).count(), 1, "{cured}");
    for room in [&caster_saw, &bystander] {
        assert!(room.contains("Seer begins to move again."), "{room}");
        assert!(!room.contains(MOVES_AGAIN), "{room}");
    }
}

#[test]
fn remove_paralysis_frees_a_plain_webbed_entangle() {
    let (mut fx, caster, _crx, seer, _srx, _orx) = setup();
    // Roll 100 never upgrades: Entangle stays "webbed" (immobilizes, no Stunned).
    force_status_upgrade_roll(Some(100));
    cast(&mut fx, caster, "cast 'entangle' seer");
    force_status_upgrade_roll(None);
    assert_eq!(effect_names(&mut fx.world, seer), vec!["webbed"]);
    assert!(!can_act(&mut fx, seer));

    cast(&mut fx, caster, "cast 'remove paralysis' seer");

    assert!(effect_names(&mut fx.world, seer).is_empty());
    assert!(can_act(&mut fx, seer));
}

#[test]
fn remove_paralysis_lifts_minor_and_major_together_with_one_message() {
    let (mut fx, caster, mut crx, seer, mut srx, mut orx) = setup();
    for spell in ["bless", "minor paralysis", "major paralysis"] {
        cast(&mut fx, caster, &format!("cast '{spell}' seer"));
    }
    assert!(fx.world.get::<Stunned>(seer).is_some());
    let _ = (drain(&mut crx), drain(&mut srx), drain(&mut orx));

    cast(&mut fx, caster, "cast 'remove paralysis' seer");

    assert_eq!(effect_names(&mut fx.world, seer), vec!["bless"]);
    assert!(fx.world.get::<Stunned>(seer).is_none());
    assert!(can_act(&mut fx, seer));
    let cured = drain(&mut srx);
    assert_eq!(cured.matches(MOVES_AGAIN).count(), 1, "{cured}");
}

#[test]
fn remove_paralysis_leaves_web_alone_like_legacy() {
    let (mut fx, caster, _crx, seer, _srx, _orx) = setup();
    cast(&mut fx, caster, "cast 'web' seer");
    assert_eq!(effect_names(&mut fx.world, seer), vec!["webbed"]);

    cast(&mut fx, caster, "cast 'remove paralysis' seer");

    assert_eq!(effect_names(&mut fx.world, seer), vec!["webbed"]);
    assert!(!can_act(&mut fx, seer));
}

#[test]
fn remove_paralysis_on_a_free_target_tells_only_the_caster() {
    let (mut fx, caster, mut crx, seer, mut srx, mut orx) = setup();
    cast(&mut fx, caster, "cast 'bless' seer");
    let _ = (drain(&mut crx), drain(&mut srx), drain(&mut orx));

    cast(&mut fx, caster, "cast 'remove paralysis' seer");

    assert_eq!(effect_names(&mut fx.world, seer), vec!["bless"]);
    // Legacy `spell_remove_paralysis`: "$N can already move just fine." to the caster only.
    let said = drain(&mut crx);
    assert!(said.contains("Seer can already move just fine."), "{said}");
    let (patient, bystander) = (drain(&mut srx), drain(&mut orx));
    for out in [&said, &patient, &bystander] {
        assert!(!out.contains("begins to move again"), "{out}");
    }
    for out in [&patient, &bystander] {
        assert!(!out.contains("can already move"), "{out}");
    }
}

#[test]
fn remove_paralysis_on_yourself_with_nothing_to_lift_says_you() {
    let (mut fx, caster, mut crx, _seer, _srx, mut orx) = setup();
    let _ = (drain(&mut crx), drain(&mut orx));

    cast(&mut fx, caster, "cast 'remove paralysis' self");

    let said = drain(&mut crx);
    assert!(said.contains("You can already move just fine."), "{said}");
    assert!(!drain(&mut orx).contains("can already move"));
}
