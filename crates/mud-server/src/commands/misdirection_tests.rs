//! Misdirection (legacy `EFF_MISDIRECTION`): a second direction after a step
//! shows the room a false one while the caster really goes the first,
//! `stay <dir>` fakes a departure, `<dir> stay` leaves unannounced, staff of
//! the mover's level see through it, and a mob with the flag wanders the
//! same way. Test-only.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, UserRole};
use mud_world::{
    Account, ExitData, Exits, Located, Misdirecting, Misdirection, Mob, Named, Profile, Room,
};

use super::dispatch;
use super::test_support::{Rx, player_in};
use crate::TickCount;

/// What the player was told, as plain text: telnet GMCP frames and colour
/// codes removed.
fn drain(rx: &mut Rx) -> String {
    let mut bytes = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        bytes.extend(chunk);
    }
    let mut text = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0xFF && bytes.get(i + 1) == Some(&0xFA) {
            // IAC SB ... IAC SE
            while i + 1 < bytes.len() && !(bytes[i] == 0xFF && bytes[i + 1] == 0xF0) {
                i += 1;
            }
            i += 2;
        } else if bytes[i] == 0x1B && bytes.get(i + 1) == Some(&b'[') {
            while i < bytes.len() && bytes[i] != b'm' {
                i += 1;
            }
            i += 1;
        } else {
            text.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&text).into_owned()
}

fn account(role: UserRole) -> Account {
    Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role,
        account_role: role,
        perms: vec![],
    }
}

fn open_exit(to: Entity) -> ExitData {
    ExitData {
        to: Some(to),
        state: ExitState::Open,
        key: None,
        description: None,
        keywords: Vec::new(),
        is_hidden: false,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    }
}

struct Fx {
    world: World,
    a: Entity,
    b: Entity,
    mover: Entity,
    mrx: Rx,
    bob: Entity,
    brx: Rx,
    crx: Rx,
}

fn profile(level: i32) -> Profile {
    Profile {
        level,
        class_id: None,
        race: "Human".into(),
        experience: 0,
        gender: "male".into(),
    }
}

/// Mover "Tester" (level 30, with the Misdirection flag) in A, watcher
/// "Bob" in A, watcher "Cara" in B; A has an exit north to B.
fn fx() -> Fx {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    let a = world.spawn((Room, Exits::default())).id();
    let b = world.spawn((Room, Exits::default())).id();
    world.entity_mut(a).insert(Exits(HashMap::from([
        (Direction::North, open_exit(b)),
        (Direction::East, open_exit(b)),
    ])));
    let (mover, mrx) = player_in(&mut world, a);
    world
        .entity_mut(mover)
        .insert((account(UserRole::Player), profile(30), Misdirection));
    let (bob, brx) = player_in(&mut world, a);
    world.entity_mut(bob).insert((
        Named { name: "Bob".into() },
        account(UserRole::Player),
        profile(10),
    ));
    let (cara, crx) = player_in(&mut world, b);
    world.entity_mut(cara).insert((
        Named {
            name: "Cara".into(),
        },
        account(UserRole::Player),
        profile(10),
    ));
    Fx {
        world,
        a,
        b,
        mover,
        mrx,
        bob,
        brx,
        crx,
    }
}

impl Fx {
    fn at(&self, e: Entity) -> Entity {
        self.world.get::<Located>(e).unwrap().0
    }
}

#[test]
fn a_second_direction_shows_the_room_the_false_one() {
    let mut f = fx();
    dispatch(&mut f.world, f.mover, "north south");
    assert_eq!(f.at(f.mover), f.b, "the real step is north");
    let seen = drain(&mut f.brx);
    assert_eq!(seen, "Tester leaves south.\r\n", "{seen:?}");
    let mine = drain(&mut f.mrx);
    assert!(
        mine.starts_with("Your illusion leaves south.\r\n"),
        "{mine:?}"
    );
    // Arrival is not hidden: legacy only withholds the leave message.
    assert_eq!(
        drain(&mut f.crx),
        "Tester arrives from the south.\r\n",
        "arrival is told as usual"
    );
}

#[test]
fn short_forms_and_abbreviated_commands_work() {
    let mut f = fx();
    dispatch(&mut f.world, f.mover, "n w");
    assert_eq!(f.at(f.mover), f.b);
    assert_eq!(drain(&mut f.brx), "Tester leaves west.\r\n");
}

#[test]
fn without_the_flag_a_second_direction_is_ignored() {
    let mut f = fx();
    f.world.entity_mut(f.mover).remove::<Misdirection>();
    dispatch(&mut f.world, f.mover, "north south");
    assert_eq!(f.at(f.mover), f.b);
    assert_eq!(drain(&mut f.brx), "Tester leaves north.\r\n");
    assert!(!drain(&mut f.mrx).contains("illusion"));
}

#[test]
fn a_plain_step_with_the_flag_is_told_as_usual() {
    let mut f = fx();
    dispatch(&mut f.world, f.mover, "north");
    assert_eq!(drain(&mut f.brx), "Tester leaves north.\r\n");
}

#[test]
fn stay_as_the_second_word_leaves_unannounced() {
    let mut f = fx();
    dispatch(&mut f.world, f.mover, "north stay");
    assert_eq!(f.at(f.mover), f.b);
    assert_eq!(drain(&mut f.brx), "", "no departure at all");
    assert!(!drain(&mut f.mrx).contains("illusion"));
}

#[test]
fn stay_then_a_direction_fakes_a_departure_without_moving() {
    let mut f = fx();
    dispatch(&mut f.world, f.mover, "stay up");
    assert_eq!(f.at(f.mover), f.a, "nothing moves");
    assert_eq!(drain(&mut f.brx), "Tester leaves up.\r\n");
    assert!(
        drain(&mut f.mrx).contains("Your illusion leaves up.\r\n"),
        "the caster sees the illusion"
    );
    assert_eq!(drain(&mut f.crx), "", "nobody arrives anywhere");
}

#[test]
fn plain_stay_just_says_so() {
    let mut f = fx();
    dispatch(&mut f.world, f.mover, "stay");
    assert_eq!(drain(&mut f.mrx), "Ok, you stay put.\r\n");
    assert_eq!(drain(&mut f.brx), "");
    // Without the flag a direction after `stay` is ignored.
    f.world.entity_mut(f.mover).remove::<Misdirection>();
    dispatch(&mut f.world, f.mover, "stay up");
    assert_eq!(drain(&mut f.mrx), "Ok, you stay put.\r\n");
    assert_eq!(drain(&mut f.brx), "");
}

#[test]
fn a_bad_second_word_is_refused_and_nothing_happens() {
    let mut f = fx();
    dispatch(&mut f.world, f.mover, "north banana");
    assert_eq!(drain(&mut f.mrx), "That isn't a direction.\r\n");
    assert_eq!(f.at(f.mover), f.a);
    assert_eq!(drain(&mut f.brx), "");
}

#[test]
fn it_is_refused_while_riding_or_ridden() {
    let mut f = fx();
    let horse = f.world.spawn((Mob, Located(f.a))).id();
    f.world
        .entity_mut(f.mover)
        .insert(mud_world::Mounted(horse));
    dispatch(&mut f.world, f.mover, "north south");
    assert_eq!(
        drain(&mut f.mrx),
        "You can't misdirect people while riding.\r\n"
    );
    assert_eq!(f.at(f.mover), f.a);
    assert_eq!(drain(&mut f.brx), "");
}

#[test]
fn a_step_that_cannot_be_taken_shows_no_illusion() {
    let mut f = fx();
    let foe = f.world.spawn(Located(f.a)).id();
    f.world.entity_mut(f.mover).insert(mud_world::Fighting(foe));
    dispatch(&mut f.world, f.mover, "north south");
    assert_eq!(
        drain(&mut f.mrx),
        "No way!  You're fighting for your life!\r\n"
    );
    assert_eq!(drain(&mut f.brx), "");
}

#[test]
fn a_wall_ahead_still_shows_the_illusion_like_legacy() {
    let mut f = fx();
    dispatch(&mut f.world, f.mover, "south north");
    assert_eq!(f.at(f.mover), f.a);
    let mine = drain(&mut f.mrx);
    assert!(mine.contains("Your illusion leaves north."), "{mine:?}");
    assert!(mine.contains("You can't go that way."), "{mine:?}");
    assert_eq!(drain(&mut f.brx), "Tester leaves north.\r\n");
}

#[test]
fn the_misdirecting_marker_does_not_outlast_the_step() {
    let mut f = fx();
    dispatch(&mut f.world, f.mover, "north south");
    assert!(f.world.get::<Misdirecting>(f.mover).is_none());
    dispatch(&mut f.world, f.mover, "stay up");
    assert!(f.world.get::<Misdirecting>(f.mover).is_none());
}

#[test]
fn a_god_of_the_movers_level_sees_through_it() {
    let mut f = fx();
    f.world
        .entity_mut(f.bob)
        .insert((profile(105), account(UserRole::Implementor)));
    dispatch(&mut f.world, f.mover, "north south");
    let seen = drain(&mut f.brx);
    assert_eq!(
        seen, "Tester's illusion leaves south.\r\nTester leaves north.\r\n",
        "{seen:?}"
    );
}

#[test]
fn an_immortal_below_the_movers_level_is_fooled() {
    let mut f = fx();
    f.world.entity_mut(f.mover).insert(profile(105));
    f.world.entity_mut(f.bob).insert(profile(100));
    dispatch(&mut f.world, f.mover, "north south");
    assert_eq!(drain(&mut f.brx), "Tester leaves south.\r\n");
}

#[test]
fn a_mortal_cannot_see_through_whatever_the_level() {
    let mut f = fx();
    f.world.entity_mut(f.mover).insert(profile(10));
    f.world.entity_mut(f.bob).insert(profile(99));
    dispatch(&mut f.world, f.mover, "north south");
    assert_eq!(drain(&mut f.brx), "Tester leaves south.\r\n");
}

#[test]
fn a_sleeper_sees_no_illusion() {
    let mut f = fx();
    f.world
        .entity_mut(f.bob)
        .insert(mud_world::Posture(mud_world::PostureKind::Sleeping));
    dispatch(&mut f.world, f.mover, "stay east");
    assert_eq!(drain(&mut f.brx), "");
}

#[test]
fn followers_walk_the_real_way_and_are_told_normally() {
    let mut f = fx();
    f.world
        .entity_mut(f.bob)
        .insert(mud_world::Follower(f.mover));
    dispatch(&mut f.world, f.mover, "north south");
    assert_eq!(f.at(f.bob), f.b, "followers follow the real step");
    assert_eq!(f.at(f.mover), f.b);
}

/// A mob carrying the flag shows false directions as it wanders: players
/// see a leave line for a direction it did not take, and never the real one
/// unless they are staff who see through it.
#[test]
fn a_misdirecting_mob_wanders_with_false_directions() {
    let mut f = fx();
    let goblin = f
        .world
        .spawn((
            Mob,
            Named {
                name: "goblin".into(),
            },
            Located(f.a),
            Misdirection,
        ))
        .id();
    f.world
        .entity_mut(f.bob)
        .insert((profile(105), account(UserRole::Implementor)));
    // The mover (a mortal) and Bob (a god) both watch from room A.
    let mut moved = false;
    for _ in 0..400 {
        for t in 0..300 {
            f.world.insert_resource(TickCount(t));
            crate::wander::wander_tick(&mut f.world);
        }
        if f.at(goblin) != f.a {
            moved = true;
            break;
        }
    }
    assert!(moved, "the goblin eventually leaves");
    let mortal = drain(&mut f.mrx);
    let god = drain(&mut f.brx);
    assert!(!mortal.contains("illusion"), "{mortal:?}");
    assert!(mortal.contains("Goblin leaves "), "{mortal:?}");
    assert!(god.contains("Goblin's illusion leaves "), "{god:?}");
    // The god gets the real departure on top of what the mortal hears.
    assert_eq!(
        god.matches(" leaves ").count(),
        mortal.matches(" leaves ").count() + 1,
        "mortal {mortal:?} god {god:?}"
    );
    assert!(
        god.contains("Goblin leaves north.") || god.contains("Goblin leaves east."),
        "{god:?}"
    );
}

#[test]
fn a_mob_without_the_flag_leaves_normally() {
    let mut f = fx();
    let goblin = f
        .world
        .spawn((
            Mob,
            Named {
                name: "goblin".into(),
            },
            Located(f.a),
        ))
        .id();
    for _ in 0..400 {
        for t in 0..300 {
            f.world.insert_resource(TickCount(t));
            crate::wander::wander_tick(&mut f.world);
        }
        if f.at(goblin) != f.a {
            break;
        }
    }
    assert_ne!(f.at(goblin), f.a);
    let seen = drain(&mut f.brx);
    assert!(
        seen == "Goblin leaves north.\r\n" || seen == "Goblin leaves east.\r\n",
        "{seen:?}"
    );
}

/// The spell as the data patch leaves it: a `status` row for the
/// `misdirection` flag (2 + skill / 4 hours) installs the marker, and
/// the effect instance carries the flag's name.
#[test]
fn casting_the_spell_installs_the_flag() {
    use mud_db::abilities::AbilityKind;
    use mud_world::{
        AbilityCatalog, EffectCatalog, EffectDef, EffectInstance, Health, KnownAbilities,
    };
    const SPELL: i32 = 241;
    const STATUS: i32 = 10;
    let mut f = fx();
    f.world.entity_mut(f.mover).remove::<Misdirection>();
    f.world.insert_resource(mud_world::SpellSlotData::default());
    f.world
        .insert_resource(mud_world::ClassSkillsData::default());
    f.world
        .insert_resource(mud_world::WeatherCatalog::default());
    let mut def = super::test_support::ability_def(SPELL, "Misdirection", AbilityKind::Spell);
    def.cast_time_rounds = 0;
    let mut catalog = AbilityCatalog::default();
    catalog.by_name.insert("misdirection".into(), def);
    catalog.effects_for.insert(
        SPELL,
        vec![(
            STATUS,
            Some(serde_json::json!({
                "flag": "misdirection", "duration": "2 + (skill / 4)", "durationUnit": "hours"
            })),
        )],
    );
    f.world.insert_resource(catalog);
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(
        STATUS,
        EffectDef {
            id: STATUS,
            name: "status".into(),
            description: None,
            effect_type: "status".into(),
            tags: Vec::new(),
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
    f.world.insert_resource(effects);
    f.world.entity_mut(f.mover).insert((
        KnownAbilities {
            entries: vec![(SPELL, 1000, true)],
        },
        Health { hp: 50, max: 50 },
    ));
    dispatch(&mut f.world, f.mover, "cast 'misdirection'");
    assert!(f.world.get::<Misdirection>(f.mover).is_some());
    assert!(f.world.get::<mud_world::Stealth>(f.mover).is_none());
    let mut q = f.world.query::<&EffectInstance>();
    let names: Vec<_> = q.iter(&f.world).map(|e| e.name.clone()).collect();
    assert_eq!(names, vec!["misdirection".to_string()]);
}
