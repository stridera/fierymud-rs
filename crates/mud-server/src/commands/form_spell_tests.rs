//! Farsee (longer `scan`, `look <dir>` down the exit) and Waterform /
//! Vaporform (a temporary body composition), fierymud-rs #35.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Composition, Direction};
use mud_world::{CompositionOverride, Farsee};

use super::god_zone_tests::Fx;
use super::info::{actor_composition, cmd_scan, scan_max_distance};
use super::test_support::drain;

fn strip_ansi(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A line of rooms 30:0 .. 30:`len`-1 running east.
fn corridor(fx: &mut Fx, len: i32) -> Vec<Entity> {
    let zone = fx.zone(30, false);
    let rooms: Vec<Entity> = (0..len).map(|i| fx.room(zone, 30, i)).collect();
    for pair in rooms.windows(2) {
        fx.link(pair[0], Direction::East, pair[1]);
    }
    rooms
}

#[test]
fn farsee_adds_a_room_and_two_divination_rolls() {
    let mut fx = Fx::new();
    let rooms = corridor(&mut fx, 1);
    let (mage, _rx) = fx.person("Mage", 30, rooms[0]);
    // No flag: one room, whatever the dice say.
    assert_eq!(scan_max_distance(&fx.world, mage, &mut |lo, _| lo), 1);
    fx.world.entity_mut(mage).insert(Farsee);
    // No Sphere of Divination: Farsee alone is one extra room.
    assert_eq!(scan_max_distance(&fx.world, mage, &mut |lo, _| lo), 2);
    assert_eq!(scan_max_distance(&fx.world, mage, &mut |_, hi| hi), 2);
}

#[test]
fn farsee_scan_reaches_a_room_a_plain_scan_does_not() {
    let mut fx = Fx::new();
    let rooms = corridor(&mut fx, 4);
    let (mage, mut rx) = fx.person("Mage", 30, rooms[0]);
    let (_far, _frx) = fx.person("Bob", 30, rooms[2]);
    cmd_scan(&mut fx.world, mage, "");
    let plain = drain(&mut rx);
    assert!(
        !plain.contains("Bob"),
        "two rooms out is beyond a plain scan: {plain}"
    );
    fx.world.entity_mut(mage).insert(Farsee);
    cmd_scan(&mut fx.world, mage, "");
    let out = drain(&mut rx);
    assert!(out.contains("Bob"), "Farsee reaches two rooms out: {out}");
    assert!(out.contains("close by east"), "{out}");
}

#[test]
fn farsee_look_keeps_going_down_the_exit() {
    let mut fx = Fx::new();
    let rooms = corridor(&mut fx, 4);
    let (mage, mut rx) = fx.person("Mage", 30, rooms[0]);
    crate::commands::look_direction(&mut fx.world, mage, Direction::East);
    let plain = drain(&mut rx);
    assert!(plain.contains("Room 30:1"), "{plain}");
    assert!(!plain.contains("Room 30:2"), "{plain}");
    fx.world.entity_mut(mage).insert(Farsee);
    crate::commands::look_direction(&mut fx.world, mage, Direction::East);
    let out = drain(&mut rx);
    assert!(out.contains("Room 30:1"), "{out}");
    // No Sphere of Divination: the first roll (1..=125) beats a skill of 0.
    assert!(out.contains("You can't see any farther."), "{out}");
}

const FORM_ABILITY: i32 = 301;
const FORM_EFFECT: i32 = 9;

fn form_fixture(fx: &mut Fx, name: &str, flag: &str) {
    let mut abilities = mud_world::AbilityCatalog::default();
    let mut def = super::test_support::ability_def(FORM_ABILITY, name, AbilityKind::Spell);
    def.cast_time_rounds = 0;
    abilities.by_name.insert(name.to_ascii_lowercase(), def);
    abilities.effects_for.insert(
        FORM_ABILITY,
        vec![(
            FORM_EFFECT,
            Some(serde_json::json!({"flag": flag, "duration": "60"})),
        )],
    );
    fx.world.insert_resource(abilities);
    let mut effects = mud_world::EffectCatalog::default();
    effects.by_id.insert(
        FORM_EFFECT,
        mud_world::EffectDef {
            id: FORM_EFFECT,
            name: "status".into(),
            description: None,
            effect_type: "status".into(),
            tags: vec![],
            presence_override: None,
            default_params: serde_json::json!({}),
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        },
    );
    fx.world.insert_resource(effects);
}

fn cast_self(fx: &mut Fx, caster: Entity, name: &str) {
    crate::commands::invoke_ability_with(
        &mut fx.world,
        caster,
        name,
        AbilityKind::Spell,
        "cast",
        false,
        true,
        true,
        None,
    );
}

#[test]
fn waterform_sets_water_composition_and_restores_flesh() {
    let mut fx = Fx::new();
    let rooms = corridor(&mut fx, 1);
    let (mage, mut rx) = fx.person("Mage", 30, rooms[0]);
    form_fixture(&mut fx, "Waterform", "waterform");
    assert_eq!(actor_composition(&fx.world, mage), Composition::Flesh);
    cast_self(&mut fx, mage, "waterform");
    let out = drain(&mut rx);
    assert_eq!(
        actor_composition(&fx.world, mage),
        Composition::Water,
        "{out}"
    );
    // Only a body of flesh can take the change: a second cast is refused.
    cast_self(&mut fx, mage, "waterform");
    assert!(
        drain(&mut rx).contains("Your body cannot sustain this change."),
        "second cast"
    );
    assert_eq!(actor_composition(&fx.world, mage), Composition::Water);
    // Expiry (or dispel) removes the last backing instance.
    let instances: Vec<Entity> = {
        let mut q = fx
            .world
            .query::<(Entity, &mud_world::EffectInstance, &mud_world::AppliedTo)>();
        q.iter(&fx.world)
            .filter(|(_, i, a)| a.0 == mage && i.name == "waterform")
            .map(|(e, _, _)| e)
            .collect()
    };
    assert_eq!(instances.len(), 1);
    for e in instances {
        fx.world.despawn(e);
    }
    crate::effects::teardown_markers_after_removal(&mut fx.world, mage, "waterform");
    assert!(fx.world.get::<CompositionOverride>(mage).is_none());
    assert_eq!(actor_composition(&fx.world, mage), Composition::Flesh);
}

#[test]
fn vaporform_is_mist_and_look_shows_the_new_composition() {
    let mut fx = Fx::new();
    let rooms = corridor(&mut fx, 1);
    let (mage, _rx) = fx.person("Mage", 30, rooms[0]);
    let (watcher, mut wrx) = fx.person("Watcher", 30, rooms[0]);
    fx.world
        .entity_mut(mage)
        .insert(mud_world::Keywords(vec!["mage".into()]));
    fx.world
        .resource_mut::<mud_world::RaceCatalog>()
        .by_race
        .insert(
            "Human".to_string(),
            mud_world::RaceDef {
                race: "Human".to_string(),
                default_size: "MEDIUM".to_string(),
                default_lifeforce: "LIFE".to_string(),
                default_composition: Composition::Flesh,
                ..mud_world::RaceDef::default()
            },
        );
    form_fixture(&mut fx, "Vaporform", "vaporform");
    super::info::cmd_look(&mut fx.world, watcher, "mage");
    assert!(
        !drain(&mut wrx).contains("composed of"),
        "flesh shows size only"
    );
    cast_self(&mut fx, mage, "vaporform");
    assert_eq!(actor_composition(&fx.world, mage), Composition::Mist);
    super::info::cmd_look(&mut fx.world, watcher, "mage");
    let out = strip_ansi(&drain(&mut wrx));
    assert!(out.contains("composed of mist"), "{out}");
}
