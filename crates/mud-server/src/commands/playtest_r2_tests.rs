//! Playtest 2026-10-11, round 2 (commands.rs half): player-started fights
//! stay quiet, no debug effect count, blank exit descriptions, train cap.
//! Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Direction, ExitState};
use mud_world::{
    CoreStats, Exits, Fighting, Health, Keywords, Located, Mob, Named, Profile, RaceCatalog,
    RaceDef, RaceStatCaps, Room, SkillPoints, WorldKey,
};

use super::test_support::{Rx, drain, player_in};
use super::{dispatch, engage_combat, engage_combat_quiet, exit_description, invoke_ability_with};

fn two_player_room() -> (World, Entity, Entity, Rx, Entity, Rx) {
    let mut world = World::new();
    let room = world.spawn(Room).id();
    let (a, rx_a) = player_in(&mut world, room);
    let (b, rx_b) = player_in(&mut world, room);
    world.entity_mut(a).insert((
        Named {
            name: "Alice".into(),
        },
        Health { hp: 100, max: 100 },
    ));
    world.entity_mut(b).insert((
        Named { name: "Bob".into() },
        Keywords(vec!["bob".into()]),
        Health { hp: 100, max: 100 },
    ));
    (world, room, a, rx_a, b, rx_b)
}

// ---- #46, #48: a player-started fight is not announced as mob aggro ----

#[test]
fn quiet_engage_sets_fighting_without_the_aggro_line() {
    let (mut world, room, a, mut rx_a, b, mut rx_b) = two_player_room();
    engage_combat_quiet(&mut world, a, b, room);
    assert_eq!(world.get::<Fighting>(a).map(|f| f.0), Some(b));
    assert_eq!(world.get::<Fighting>(b).map(|f| f.0), Some(a));
    let (out_a, out_b) = (drain(&mut rx_a), drain(&mut rx_b));
    assert!(
        !out_a.contains("attacks!") && !out_b.contains("attacks!"),
        "{out_a}{out_b}"
    );
}

#[test]
fn loud_engage_still_announces_mob_aggro() {
    let (mut world, room, a, _rx_a, b, mut rx_b) = two_player_room();
    engage_combat(&mut world, a, b, room);
    assert!(drain(&mut rx_b).contains("sees you and attacks!"));
}

fn ray_world() -> (World, Entity, Rx, Entity, Rx) {
    let (mut world, room, a, rx_a, b, rx_b) = two_player_room();
    let mut catalog = mud_world::AbilityCatalog::default();
    super::violent_cast_tests::spell(&mut catalog, 1, "ray", true, 7);
    world.insert_resource(catalog);
    let mut effects = mud_world::EffectCatalog::default();
    effects
        .by_id
        .insert(7, super::violent_cast_tests::effect_def(7, "modify"));
    world.insert_resource(effects);
    world.insert_resource(mud_world::SpellSlotData::default());
    world.insert_resource(mud_world::ClassSkillsData::default());
    world.entity_mut(a).insert(mud_world::KnownAbilities {
        entries: vec![(1, 500, true)].into_iter().collect(),
    });
    let _ = room;
    (world, a, rx_a, b, rx_b)
}

#[test]
fn a_player_casting_a_violent_spell_does_not_print_the_aggro_line() {
    let (mut world, a, mut rx_a, _b, mut rx_b) = ray_world();
    let room = world.get::<Located>(a).unwrap().0;
    let goblin = world
        .spawn((
            Mob,
            Named {
                name: "a goblin".into(),
            },
            Keywords(vec!["goblin".into()]),
            Located(room),
            Health { hp: 50, max: 50 },
        ))
        .id();
    invoke_ability_with(
        &mut world,
        a,
        "'ray' goblin",
        AbilityKind::Spell,
        "cast",
        false,
        false,
        false,
        None,
    );
    assert_eq!(
        world.get::<Fighting>(goblin).map(|f| f.0),
        Some(a),
        "the spell still starts the fight"
    );
    let all = format!("{}{}", drain(&mut rx_a), drain(&mut rx_b));
    assert!(!all.contains("sees"), "{all}");
}

// ---- #51: no debug effect count on the victim's line ----

#[test]
fn spell_line_to_the_target_has_no_effect_count() {
    let (mut world, a, mut rx_a, _b, mut rx_b) = ray_world();
    let mut catalog = mud_world::AbilityCatalog::default();
    super::violent_cast_tests::spell(&mut catalog, 2, "buff", false, 7);
    world.insert_resource(catalog);
    world
        .get_mut::<mud_world::KnownAbilities>(a)
        .unwrap()
        .entries
        .push((2, 500, true));
    invoke_ability_with(
        &mut world,
        a,
        "'buff' bob",
        AbilityKind::Spell,
        "cast",
        false,
        false,
        false,
        None,
    );
    let all = format!("{}{}", drain(&mut rx_a), drain(&mut rx_b));
    assert!(all.contains("Alice"), "the target is told: {all}");
    assert!(!all.contains("effect(s)"), "{all}");
}

// ---- #69: blank exit descriptions are no description ----

#[test]
fn blank_exit_descriptions_read_as_none() {
    let mut ed = super::gmcp_tests::exit(Entity::PLACEHOLDER, ExitState::Closed, false, &[]);
    assert_eq!(exit_description(&ed), None);
    for blank in ["", "   ", "\r\n \t"] {
        ed.description = Some(blank.to_string());
        assert_eq!(exit_description(&ed), None, "{blank:?}");
    }
    ed.description = Some("A curtain of beads.".into());
    assert_eq!(exit_description(&ed), Some("A curtain of beads."));
}

#[test]
fn look_at_a_closed_exit_with_a_blank_description_prints_no_blank_lines() {
    let mut fx = super::gmcp_tests::fixture();
    let (a, b) = (fx.a, fx.b);
    let mut ed = super::gmcp_tests::exit(b, ExitState::Closed, false, &["door"]);
    ed.description = Some(String::new());
    fx.world
        .get_mut::<Exits>(a)
        .unwrap()
        .0
        .insert(Direction::Down, ed);
    let (p, mut rx) = super::gmcp_tests::player(&mut fx.world, a, "Alice");
    dispatch(&mut fx.world, p, "look down");
    let out = drain(&mut rx);
    assert!(out.contains("closed"), "{out}");
    assert!(!out.contains("\r\n\r\n"), "{out:?}");
    assert!(!out.starts_with("\r\n"), "{out:?}");
}

// ---- #59: train works on the 0-100 stat scale ----

fn train_world(strength: i32, cap: i32) -> (World, Entity, Rx) {
    let mut world = World::new();
    let room = world.spawn(Room).id();
    let mut races = RaceCatalog::default();
    races.by_race.insert(
        "ELF".into(),
        RaceDef {
            race: "ELF".into(),
            stat_caps: RaceStatCaps {
                strength: cap,
                dexterity: cap,
                constitution: cap,
                intelligence: cap,
                wisdom: cap,
                charisma: cap,
            },
            ..RaceDef::default()
        },
    );
    world.insert_resource(races);
    let mut protos = mud_world::MobPrototypes::default();
    protos.by_key.insert(
        (7, 1),
        super::test_support::mob_proto(7, 1, mud_db::enums::MobProfession::Trainer),
    );
    world.insert_resource(protos);
    world.spawn((Mob, WorldKey { zone: 7, id: 1 }, Located(room)));
    let (p, rx) = player_in(&mut world, room);
    world.entity_mut(p).insert((
        mud_world::Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: mud_db::enums::UserRole::Player,
            account_role: mud_db::enums::UserRole::Player,
            perms: vec![],
        },
        Profile {
            level: 10,
            class_id: None,
            race: "ELF".into(),
            experience: 0,
            gender: "female".into(),
        },
        CoreStats {
            strength,
            ..CoreStats::default()
        },
        SkillPoints(3),
    ));
    (world, p, rx)
}

#[test]
fn train_raises_a_stat_well_above_eighteen_up_to_the_race_cap() {
    let (mut world, p, mut rx) = train_world(50, 64);
    dispatch(&mut world, p, "train str");
    let out = drain(&mut rx);
    assert!(out.contains("now 51"), "{out}");
    assert_eq!(world.get::<CoreStats>(p).unwrap().strength, 51);
    assert_eq!(world.get::<SkillPoints>(p).unwrap().0, 2);
    dispatch(&mut world, p, "train");
    assert!(
        drain(&mut rx).contains("/64"),
        "the listing shows the race cap"
    );
}

#[test]
fn train_refuses_at_the_race_cap() {
    let (mut world, p, mut rx) = train_world(64, 64);
    dispatch(&mut world, p, "train str");
    assert!(drain(&mut rx).contains("cap of 64"));
    assert_eq!(world.get::<CoreStats>(p).unwrap().strength, 64);
    assert_eq!(world.get::<SkillPoints>(p).unwrap().0, 3);
}
