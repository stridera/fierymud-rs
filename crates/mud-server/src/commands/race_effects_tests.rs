//! `RaceEffects` (issue #84 follow-up): race-innate permanent effects
//! load from the DB rows, apply to players at login and to mobs at
//! spawn, show as "(permanent)", and never stack on relog.

use bevy_ecs::prelude::*;
use mud_db::enums::MobProfession;
use mud_world::{
    AppliedTo, Bless, EffectCatalog, EffectDef, EffectInstance, EffectSource, Flying, Profile,
    RaceEffect, RaceEffectCatalog, Room,
};

use super::info::cmd_effects;
use super::test_support::{Rx, drain, mob_proto, player_in};
use crate::login::{PersistedEffects, restore_persisted_effects};

/// The real `status` Effect row: its `default_params.flag` is `bless`,
/// which a flag-less `RaceEffects` row would wrongly inherit.
fn world_with_races(rows: &[(&str, &[&str])]) -> World {
    let mut world = World::new();
    world.init_resource::<mud_world::AbilityCatalog>();
    world
        .get_resource_or_insert_with(EffectCatalog::default)
        .by_id
        .insert(
            4,
            EffectDef {
                id: 4,
                name: "status".into(),
                description: None,
                effect_type: "status".into(),
                tags: vec![],
                presence_override: None,
                default_params: serde_json::json!({"flag": "bless", "duration": "level * 2"}),
                prevents_speaking: false,
                prevents_casting: false,
                prevents_movement: false,
                on_apply: None,
                on_tick: None,
                on_remove: None,
            },
        );
    let mut cat = RaceEffectCatalog::default();
    for (race, flags) in rows {
        cat.insert(
            race,
            RaceEffect {
                effect_id: 4,
                strength: 1,
                modifier_data: serde_json::json!({ "flags": flags }),
            },
        );
    }
    world.insert_resource(cat);
    world
}

fn player_of(world: &mut World, race: &str) -> (Entity, Rx) {
    let room = world.spawn(Room).id();
    let (player, rx) = player_in(world, room);
    world.entity_mut(player).insert(Profile {
        level: 1,
        class_id: None,
        race: race.to_string(),
        experience: 0,
        gender: "male".into(),
    });
    (player, rx)
}

fn instances(world: &mut World, target: Entity) -> Vec<(String, i32, EffectSource)> {
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    let mut v: Vec<_> = q
        .iter(world)
        .filter(|(_, a)| a.0 == target)
        .map(|(i, _)| (i.name.clone(), i.remaining_secs, i.source.clone()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

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

#[test]
fn elf_player_gets_permanent_infravision_and_no_bless() {
    let mut world = world_with_races(&[("ELF", &["infravision"])]);
    let (player, _rx) = player_of(&mut world, "ELF");
    crate::login::apply_player_race_effects(&mut world, player);
    let inst = instances(&mut world, player);
    assert_eq!(inst.len(), 1, "{inst:?}");
    assert_eq!(inst[0].0, "infravision");
    assert_eq!(inst[0].1, -1, "permanent");
    assert!(
        world.get::<Bless>(player).is_none(),
        "the status Effect default flag (bless) must not leak in"
    );
    assert!(!inst.iter().any(|(n, ..)| n == "bless"));
}

#[test]
fn race_without_rows_gets_nothing() {
    let mut world = world_with_races(&[("ELF", &["infravision"])]);
    let (player, _rx) = player_of(&mut world, "HUMAN");
    crate::login::apply_player_race_effects(&mut world, player);
    assert!(instances(&mut world, player).is_empty());
}

#[test]
fn relog_does_not_duplicate_race_effects() {
    let mut world = world_with_races(&[("DROW", &["infravision", "ultravision"])]);
    let (player, _rx) = player_of(&mut world, "DROW");
    crate::login::apply_player_race_effects(&mut world, player);
    // The save path drops race-sourced instances; even if an older save
    // carried one, restore skips it.
    let stale: PersistedEffects = serde_json::from_value(serde_json::json!({
        "saved_at_unix": i64::MAX / 2,
        "effects": [{
            "kind": 4,
            "name": "infravision",
            "strength": 1,
            "remaining_secs": -1,
            "source": { "Other": mud_world::mob_effects::RACE_EFFECT_SOURCE },
            "ability_id": null,
            "modify_delta": null,
        }],
    }))
    .expect("persisted effects shape");
    restore_persisted_effects(&mut world, player, stale);
    crate::login::apply_player_race_effects(&mut world, player);
    crate::login::apply_player_race_effects(&mut world, player);
    let names: Vec<String> = instances(&mut world, player)
        .into_iter()
        .map(|(n, ..)| n)
        .collect();
    assert_eq!(names, vec!["infravision", "ultravision"]);
}

#[test]
fn race_effects_show_permanent_in_effects_list() {
    let mut world = world_with_races(&[("ELF", &["infravision"])]);
    let (player, mut rx) = player_of(&mut world, "ELF");
    crate::login::apply_player_race_effects(&mut world, player);
    cmd_effects(&mut world, player, "");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("(permanent)"), "{out}");
    assert!(out.to_lowercase().contains("infravision"), "{out}");
}

#[test]
fn spawned_mob_gets_its_race_effects_once() {
    let mut world = world_with_races(&[("DRAGON_FIRE", &["fly"]), ("ELF", &["infravision"])]);
    let room = world.spawn(Room).id();
    let mut proto = mob_proto(30, 1, MobProfession::Trainer);
    proto.race = "dragon_fire".into();
    let dragon = mud_world::spawn_mob_from_proto(&mut world, &proto, room, None);
    assert!(
        world.get::<Flying>(dragon).is_some(),
        "race fly installs the marker"
    );
    let inst = instances(&mut world, dragon);
    assert_eq!(inst.len(), 1, "{inst:?}");
    assert_eq!((inst[0].0.as_str(), inst[0].1), ("fly", -1));

    // The mob already carries fly from its own default effects: the
    // race row must not double it.
    super::test_support::grant_default_flags(&mut world, (30, 2), &["fly"]);
    let mut twin = mob_proto(30, 2, MobProfession::Trainer);
    twin.race = "dragon_fire".into();
    let mob = mud_world::spawn_mob_from_proto(&mut world, &twin, room, None);
    let inst = instances(&mut world, mob);
    assert_eq!(inst.len(), 1, "{inst:?}");
    assert_eq!(inst[0].2, EffectSource::Other("mob_default".into()));
}
