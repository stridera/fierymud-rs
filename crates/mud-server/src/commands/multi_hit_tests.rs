//! Multi-bolt damage spells (Magic Missile, Ice Darts, `multihit: true`):
//! legacy calls `mag_damage` once per missile, so every dart gets its own
//! caster, victim and room message and its own damage line. Test-only.

use bevy_ecs::prelude::*;
use mud_world::{AbilityCatalog, AbilityMessageSet, Health, Mob, Named};

use super::blindness_tests::{DAMAGE, cast, caster_with_spells};
use super::gmcp_tests::{Fx, player};
use super::test_support::{Rx, drain};

fn dart_caster(multihit: bool) -> (Fx, Entity, Rx, Entity) {
    let (mut fx, p, rx) = caster_with_spells(vec![(
        1,
        "Ice Darts",
        vec![(
            DAMAGE,
            Some(serde_json::json!({
                "type": "magic", "amount": "7", "multihit": multihit,
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
    // The fixture caster is level 20: the curve caps at five darts.
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
    assert!(out.contains("dead already"), "{out}");
}
