//! `effects` listing (issue #38): stat effects spelled out instead of
//! three-letter stubs, durations in MUD hours (75 real seconds each),
//! permanent effects flagged.

use bevy_ecs::prelude::*;
use mud_world::{AppliedTo, EffectInstance, EffectSource, ModifyDelta, Room};

use super::info::cmd_effects;
use super::test_support::{Rx, drain, player_in};

fn setup() -> (World, Entity, Rx) {
    let mut world = World::new();
    world.init_resource::<mud_world::AbilityCatalog>();
    let room = world.spawn(Room).id();
    let (player, rx) = player_in(&mut world, room);
    (world, player, rx)
}

fn add_effect(
    world: &mut World,
    player: Entity,
    name: &str,
    remaining_secs: i32,
    delta: Option<i32>,
) {
    let mut e = world.spawn((
        EffectInstance {
            kind: 1,
            name: name.to_string(),
            strength: 1,
            remaining_secs,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(player),
    ));
    if let Some(amount) = delta {
        e.insert(ModifyDelta {
            target: name.to_string(),
            amount,
        });
    }
}

/// Output with ANSI escapes removed.
fn plain(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn effects_output(world: &mut World, player: Entity, rx: &mut Rx) -> String {
    cmd_effects(world, player, "");
    plain(&drain(rx))
}

#[test]
fn stat_effects_are_spelled_out_not_abbreviated() {
    let (mut world, player, mut rx) = setup();
    add_effect(&mut world, player, "cha", 1050, Some(14));
    add_effect(&mut world, player, "eva", 1650, None);
    add_effect(&mut world, player, "max_hp", 750, Some(20));
    let out = effects_output(&mut world, player, &mut rx);
    assert!(out.contains("Charisma (+14)"), "{out}");
    assert!(out.contains("Evasion"), "{out}");
    assert!(out.contains("Maximum Hit Points (+20)"), "{out}");
    assert!(!out.contains("Cha "), "{out}");
    assert!(!out.contains("Eva "), "{out}");
}

#[test]
fn long_effect_name_is_not_truncated() {
    let (mut world, player, mut rx) = setup();
    add_effect(&mut world, player, "save_spell", 750, None);
    add_effect(
        &mut world,
        player,
        "a_rather_extremely_long_effect_name_indeed",
        750,
        None,
    );
    let out = effects_output(&mut world, player, &mut rx);
    assert!(
        out.contains("Spell Saving Throw (10 hours remaining)"),
        "{out}"
    );
    assert!(
        out.contains("A Rather Extremely Long Effect Name Indeed (10 hours remaining)"),
        "{out}"
    );
}

#[test]
fn durations_are_shown_in_mud_hours() {
    let (mut world, player, mut rx) = setup();
    add_effect(&mut world, player, "cha", 75 * 14, Some(14));
    add_effect(&mut world, player, "wis", 75 * 3 + 1, None);
    let out = effects_output(&mut world, player, &mut rx);
    assert!(out.contains("(14 hours remaining)"), "{out}");
    // Partial hours round up, like legacy `duration + 1`.
    assert!(out.contains("(4 hours remaining)"), "{out}");
    assert!(!out.contains("1050"), "{out}");
}

#[test]
fn sub_hour_durations_read_one_hour() {
    let (mut world, player, mut rx) = setup();
    add_effect(&mut world, player, "str", 74, None);
    add_effect(&mut world, player, "dex", 75, None);
    add_effect(&mut world, player, "con", 76, None);
    add_effect(&mut world, player, "int", 5, None);
    let out = effects_output(&mut world, player, &mut rx);
    assert_eq!(out.matches("(1 hour remaining)").count(), 3, "{out}");
    assert!(out.contains("Constitution (2 hours remaining)"), "{out}");
    assert!(!out.contains("1 hours"), "{out}");
}

#[test]
fn permanent_effect_is_flagged_not_timed() {
    let (mut world, player, mut rx) = setup();
    add_effect(&mut world, player, "infravision", -1, None);
    let out = effects_output(&mut world, player, &mut rx);
    assert!(out.contains("Infravision (permanent)"), "{out}");
    assert!(!out.contains("remaining"), "{out}");
}
