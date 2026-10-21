//! Killing blows from damage spells (issue #134): a spell with fatality
//! lines (legacy `die_msg`) prints them in place of its hit lines, to the
//! caster, the victim and the bystanders. Test-only.

use bevy_ecs::prelude::*;
use mud_world::{AbilityCatalog, AbilityMessageSet, Health, Mob, Named};

use super::blindness_tests::{DAMAGE, cast, caster_with_spells};
use super::gmcp_tests::{Fx, player};
use super::test_support::{Rx, drain};

fn freeze_caster(multihit: bool, kill: bool, mob_hp: i32) -> (Fx, Entity, Rx, Entity) {
    let (mut fx, p, rx) = caster_with_spells(vec![(
        1,
        "Freeze",
        vec![(
            DAMAGE,
            Some(serde_json::json!({
                "type": "magic", "amount": "7", "multihit": multihit, "boltCount": "3",
            })),
        )],
    )]);
    let mut set = AbilityMessageSet {
        success_to_caster: Some("A huge block of ice forms around {target.name}.".into()),
        success_to_victim: Some("A huge block of ice forms around you.".into()),
        success_to_room: Some("A huge block of ice forms around {target.name}.".into()),
        ..AbilityMessageSet::default()
    };
    if kill {
        set.kill_to_caster = Some("{target.name} is encased in ice, permanently.".into());
        set.kill_to_victim = Some("You are encased in ice, permanently.".into());
        set.kill_to_room = Some("{target.name} is encased in ice, permanently!".into());
    }
    fx.world
        .resource_mut::<AbilityCatalog>()
        .messages
        .insert(1, set);
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
            Health {
                hp: mob_hp,
                max: 500,
            },
        ))
        .id();
    (fx, p, rx, rat)
}

#[test]
fn a_killing_blow_prints_the_fatality_line_not_the_hit_line() {
    let (mut fx, p, mut rx, rat) = freeze_caster(false, true, 5);
    let room = fx.a;
    let (_watcher, mut wrx) = player(&mut fx.world, room, "Watcher");
    let _ = drain(&mut rx);
    let _ = drain(&mut wrx);
    cast(&mut fx, p, "cast 'freeze' rat");
    let out = drain(&mut rx);
    assert!(
        out.contains("A sewer rat is encased in ice, permanently."),
        "{out}"
    );
    assert!(out.contains("(7 damage)"), "{out}");
    assert!(!out.contains("huge block of ice forms"), "{out}");
    let seen = drain(&mut wrx);
    assert!(
        seen.contains("A sewer rat is encased in ice, permanently!"),
        "{seen}"
    );
    assert!(!seen.contains("huge block of ice forms"), "{seen}");
    assert!(fx.world.get::<Health>(rat).is_none_or(|h| h.hp <= 0));
}

#[test]
fn a_blow_that_does_not_kill_keeps_the_hit_line() {
    let (mut fx, p, mut rx, _rat) = freeze_caster(false, true, 500);
    let room = fx.a;
    let (_watcher, mut wrx) = player(&mut fx.world, room, "Watcher");
    let _ = drain(&mut rx);
    let _ = drain(&mut wrx);
    cast(&mut fx, p, "cast 'freeze' rat");
    let out = drain(&mut rx);
    assert!(out.contains("huge block of ice forms"), "{out}");
    assert!(!out.contains("encased in ice"), "{out}");
    let seen = drain(&mut wrx);
    assert!(seen.contains("huge block of ice forms"), "{seen}");
    assert!(!seen.contains("encased in ice"), "{seen}");
}

#[test]
fn a_spell_without_fatality_lines_keeps_the_hit_line_on_a_kill() {
    let (mut fx, p, mut rx, _rat) = freeze_caster(false, false, 5);
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'freeze' rat");
    let out = drain(&mut rx);
    assert!(out.contains("huge block of ice forms"), "{out}");
}

#[test]
fn a_killing_bolt_prints_the_fatality_line_once() {
    let (mut fx, p, mut rx, _rat) = freeze_caster(true, true, 10);
    let room = fx.a;
    let (_watcher, mut wrx) = player(&mut fx.world, room, "Watcher");
    let _ = drain(&mut rx);
    let _ = drain(&mut wrx);
    cast(&mut fx, p, "cast 'freeze' rat");
    let out = drain(&mut rx);
    // 7 damage per bolt: the first bolt hits, the second kills.
    assert_eq!(out.matches("huge block of ice forms").count(), 1, "{out}");
    assert_eq!(out.matches("encased in ice").count(), 1, "{out}");
    let seen = drain(&mut wrx);
    assert_eq!(seen.matches("huge block of ice forms").count(), 1, "{seen}");
    assert_eq!(seen.matches("encased in ice").count(), 1, "{seen}");
}
