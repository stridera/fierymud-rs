//! Staff access (issue #62): legacy gates for walking through closed doors
//! (`rooms.cpp`, `LVL_GOD`), unlocking without a key (`unlock_door`,
//! `LVL_IMMORT`), `kill` as an instant slay (`do_kill`, `LVL_GOD`) and
//! `transfer all` (`do_trans`, `LVL_GRGOD`). Test-only.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, UserRole, effective_rank};
use mud_world::{
    Account, CombatStats, ExitData, Exits, Health, Keywords, Located, Mob, Named, Online,
    PeacefulRoom, Profile, Room,
};

use super::dispatch;
use super::test_support::{Rx, drain, player_in};
use crate::commands::{CommandOrigin, with_command_origin};

fn level_account(level: i32) -> Account {
    Account {
        user_id: String::new(),
        character_id: String::new(),
        role: effective_rank(level, UserRole::Player),
        account_role: UserRole::Player,
        perms: vec![],
    }
}

fn exit(to: Entity, state: ExitState) -> ExitData {
    ExitData {
        to: Some(to),
        state,
        key: None,
        description: None,
        keywords: vec!["door".to_string()],
        is_hidden: false,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    }
}

/// Rooms A and B joined by a north door in `state`, and a player of
/// `level` in A.
fn door_world(level: i32, state: ExitState) -> (World, Entity, Entity, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    let a = world.spawn((Room, Exits::default())).id();
    let b = world.spawn((Room, Exits::default())).id();
    world
        .entity_mut(a)
        .insert(Exits(HashMap::from([(Direction::North, exit(b, state))])));
    world
        .entity_mut(b)
        .insert(Exits(HashMap::from([(Direction::South, exit(a, state))])));
    let (p, rx) = player_in(&mut world, a);
    world.entity_mut(p).insert((
        level_account(level),
        Profile {
            level,
            class_id: None,
            race: "Human".into(),
            experience: 0,
            gender: "neutral".into(),
        },
    ));
    (world, p, a, b, rx)
}

fn at(world: &World, e: Entity) -> Entity {
    world.get::<Located>(e).unwrap().0
}

fn state_of(world: &World, room: Entity) -> ExitState {
    world.get::<Exits>(room).unwrap().0[&Direction::North].state
}

#[test]
fn closed_and_locked_doors_stop_everyone_below_lvl_god() {
    // Legacy rooms.cpp: only GET_LEVEL >= LVL_GOD (101) ignores doors.
    // LVL_IMMORT (100) and mortals are stopped.
    for level in [10, 100] {
        for state in [ExitState::Closed, ExitState::Locked] {
            let (mut world, p, a, _b, mut rx) = door_world(level, state);
            dispatch(&mut world, p, "north");
            assert_eq!(
                at(&world, p),
                a,
                "level {level} {state:?}: {}",
                drain(&mut rx)
            );
        }
    }
}

#[test]
fn lvl_god_and_up_walk_through_closed_and_locked_doors() {
    for level in [101, 102, 105] {
        for state in [ExitState::Closed, ExitState::Locked] {
            let (mut world, p, _a, b, mut rx) = door_world(level, state);
            dispatch(&mut world, p, "north");
            assert_eq!(
                at(&world, p),
                b,
                "level {level} {state:?}: {}",
                drain(&mut rx)
            );
        }
    }
}

#[test]
fn a_forced_or_scripted_move_does_not_borrow_staff_rank_at_doors() {
    for origin in [CommandOrigin::Forced, CommandOrigin::Script] {
        let (mut world, p, a, _b, mut rx) = door_world(105, ExitState::Locked);
        with_command_origin(origin, || {
            crate::commands::cmd_move(&mut world, p, Direction::North);
        });
        assert_eq!(at(&world, p), a, "{origin:?}");
        assert!(drain(&mut rx).contains("is locked"));
    }
}

#[test]
fn mortals_need_the_key_but_immortals_unlock_without_one() {
    // Legacy unlock_door: only GET_LEVEL < LVL_IMMORT needs a key.
    let (mut world, mortal, a, _b, mut rx) = door_world(50, ExitState::Locked);
    dispatch(&mut world, mortal, "unlock north");
    assert_eq!(state_of(&world, a), ExitState::Locked);
    assert!(drain(&mut rx).contains("no keyhole"));

    // A keyed door the immortal holds no key for.
    let (mut world, imm, a, b, mut rx) = door_world(100, ExitState::Locked);
    world
        .get_mut::<Exits>(a)
        .unwrap()
        .0
        .get_mut(&Direction::North)
        .unwrap()
        .key = Some((30, 7));
    dispatch(&mut world, imm, "unlock north");
    assert_eq!(state_of(&world, a), ExitState::Closed, "{}", drain(&mut rx));
    assert_eq!(
        world.get::<Exits>(b).unwrap().0[&Direction::South].state,
        ExitState::Closed,
        "both sides unlock"
    );
    assert!(drain(&mut rx).contains("You unlock the way north."));

    // Keyless (no keyhole) door too.
    let (mut world, imm, a, _b, _rx) = door_world(100, ExitState::Locked);
    dispatch(&mut world, imm, "unlock north");
    assert_eq!(state_of(&world, a), ExitState::Closed);
}

#[test]
fn a_forced_unlock_still_needs_the_key() {
    let (mut world, imm, a, _b, mut rx) = door_world(105, ExitState::Locked);
    with_command_origin(CommandOrigin::Forced, || {
        dispatch(&mut world, imm, "unlock north");
    });
    assert_eq!(state_of(&world, a), ExitState::Locked);
    assert!(drain(&mut rx).contains("no keyhole"));
}

// ---------------------------------------------------------------- kill

fn kill_world(level: i32) -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    world.insert_resource(crate::TickCount(0));
    let room = world.spawn((Room, Exits::default())).id();
    let (p, rx) = player_in(&mut world, room);
    world.entity_mut(p).insert((
        level_account(level),
        Profile {
            level,
            class_id: None,
            race: "Human".into(),
            experience: 0,
            gender: "neutral".into(),
        },
        Health { hp: 100, max: 100 },
        CombatStats::default(),
    ));
    (world, room, p, rx)
}

fn goblin(world: &mut World, room: Entity) -> Entity {
    world
        .spawn((
            Mob,
            Named {
                name: "a goblin".to_string(),
            },
            Keywords(vec!["goblin".to_string()]),
            Located(room),
            Health {
                hp: 5000,
                max: 5000,
            },
            CombatStats::default(),
        ))
        .id()
}

#[test]
fn staff_kill_slays_instantly_with_legacy_messages() {
    for level in [101, 105] {
        let (mut world, room, god, mut rx) = kill_world(level);
        let (watcher, mut wrx) = player_in(&mut world, room);
        world.entity_mut(watcher).insert(Named {
            name: "Watcher".into(),
        });
        let g = goblin(&mut world, room);
        dispatch(&mut world, god, "kill goblin");
        assert!(world.get_entity(g).is_err(), "level {level}: goblin died");
        let out = drain(&mut rx);
        assert!(
            out.contains("You chop a goblin to pieces!   Ah!   The blood!"),
            "{out}"
        );
        let seen = drain(&mut wrx);
        assert!(seen.contains("Tester brutally slays a goblin!"), "{seen}");
    }
}

#[test]
fn staff_kill_legacy_refusals() {
    let (mut world, room, god, mut rx) = kill_world(101);
    let g = goblin(&mut world, room);

    dispatch(&mut world, god, "kill");
    assert!(drain(&mut rx).contains("Kill who?"));
    dispatch(&mut world, god, "kill nobody");
    assert!(drain(&mut rx).contains("They aren't here."));
    dispatch(&mut world, god, "kill self");
    assert!(drain(&mut rx).contains("Your mother would be so sad.. :("));

    // Level-105 targets are off limits; players are never slain.
    let (boss, _b) = player_in(&mut world, room);
    world.entity_mut(boss).insert((
        Named {
            name: "Boss".into(),
        },
        Online,
        level_account(105),
    ));
    dispatch(&mut world, god, "kill boss");
    assert!(drain(&mut rx).contains("You dare NOT do that!"));
    let (pc, _p) = player_in(&mut world, room);
    world.entity_mut(pc).insert((
        Named {
            name: "Newbie".into(),
        },
        level_account(5),
    ));
    dispatch(&mut world, god, "kill newbie");
    assert!(drain(&mut rx).contains("Slaying players is not allowed"));

    // Peaceful and magically dark rooms stop it even for gods.
    world.entity_mut(room).insert(PeacefulRoom);
    dispatch(&mut world, god, "kill goblin");
    assert!(drain(&mut rx).contains("You feel ashamed trying to disturb the peace"));
    world.entity_mut(room).remove::<PeacefulRoom>();
    world
        .entity_mut(room)
        .insert(mud_world::RoomMagicalDarkness);
    dispatch(&mut world, god, "kill goblin");
    assert!(drain(&mut rx).contains("It is just too damn dark!"));
    assert!(world.get_entity(g).is_ok(), "goblin untouched by refusals");
}

#[test]
fn kill_is_slay_only_from_lvl_god_up() {
    use crate::room_access::is_god_level;
    let (mut world, _room, p, _rx) = kill_world(100);
    assert!(!is_god_level(&world, p), "LVL_IMMORT still fights");
    world.entity_mut(p).insert(level_account(101));
    assert!(is_god_level(&world, p));
    world.entity_mut(p).insert(level_account(40));
    assert!(!is_god_level(&world, p));
}

// ------------------------------------------------------- transfer all

fn named_player(world: &mut World, name: &str, level: i32, room: Entity) -> (Entity, Rx) {
    let (e, rx) = player_in(world, room);
    world
        .entity_mut(e)
        .insert((Named { name: name.into() }, Online, level_account(level)));
    world.entity_mut(e).insert(Profile {
        level,
        class_id: None,
        race: "Human".into(),
        experience: 0,
        gender: "neutral".into(),
    });
    (e, rx)
}

fn transfer_world() -> (World, Entity, Entity) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    let here = world.spawn((Room, Exits::default())).id();
    let far = world.spawn((Room, Exits::default())).id();
    (world, here, far)
}

#[test]
fn transfer_all_brings_every_lower_level_player() {
    let (mut world, here, far) = transfer_world();
    let (staff, mut rx) = named_player(&mut world, "Laoris", 102, here);
    let (bob, mut brx) = named_player(&mut world, "Bob", 20, far);
    let (cara, _c) = named_player(&mut world, "Cara", 99, far);
    let (peer, _p) = named_player(&mut world, "Peer", 102, far);
    let (boss, _b) = named_player(&mut world, "Boss", 105, far);
    let rat = world
        .spawn((
            Mob,
            Named {
                name: "a rat".into(),
            },
            Located(far),
        ))
        .id();

    dispatch(&mut world, staff, "transfer all");
    assert_eq!(at(&world, bob), here);
    assert_eq!(at(&world, cara), here);
    assert_eq!(at(&world, peer), far, "equal level stays");
    assert_eq!(at(&world, boss), far, "higher level stays");
    assert_eq!(at(&world, rat), far, "mobs are not players");
    let out = drain(&mut rx);
    assert!(out.contains("Ok."), "{out}");
    assert!(drain(&mut brx).contains("Laoris summons you."));
}

#[test]
fn transfer_all_needs_lvl_grgod() {
    let (mut world, here, far) = transfer_world();
    let (staff, mut rx) = named_player(&mut world, "Laoris", 101, here);
    let (bob, _b) = named_player(&mut world, "Bob", 20, far);
    dispatch(&mut world, staff, "transfer all");
    assert_eq!(at(&world, bob), far);
    assert!(drain(&mut rx).contains("I think not."));
}
