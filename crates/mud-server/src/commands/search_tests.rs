//! `search` against hidden exits: reveal, `exits` listing, and walking through.

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, UserRole, effective_rank};
use mud_world::{
    Account, CoreStats, ExitData, Exits, Located, Named, Online, Player, Profile, Room, WorldKey,
    WorldKeyIndex, Zone,
};

use super::test_support::{Rx, drain};
use super::{Connection, dispatch};

fn world_with_rooms() -> (World, Entity, Entity) {
    let mut world = World::new();
    world.insert_resource(mud_script::LuaHost::default());
    world.insert_resource(WorldKeyIndex::default());
    world.insert_resource(mud_world::WeatherCatalog::default());
    world.insert_resource(mud_world::AbilityCatalog::default());
    world.insert_resource(mud_world::EffectCatalog::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    world.insert_resource(mud_world::RuntimeConfig::default());
    world.insert_resource(crate::TickCount(0));
    let zone = world
        .spawn((
            Zone,
            WorldKey { zone: 550, id: 0 },
            Named {
                name: "Tech".into(),
            },
        ))
        .id();
    world
        .resource_mut::<WorldKeyIndex>()
        .zones
        .insert(550, zone);
    let room = |world: &mut World, id: i32| {
        let r = world
            .spawn((
                Room,
                WorldKey { zone: 550, id },
                Named {
                    name: format!("Room {id}"),
                },
                Located(zone),
                Exits::default(),
            ))
            .id();
        world
            .resource_mut::<WorldKeyIndex>()
            .rooms
            .insert((550, id), r);
        r
    };
    let a = room(&mut world, 18);
    let b = room(&mut world, 22);
    (world, a, b)
}

fn hidden_door(to: Entity, state: ExitState) -> ExitData {
    ExitData {
        to: Some(to),
        state,
        key: None,
        description: None,
        keywords: vec!["monolith".into()],
        is_hidden: true,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    }
}

fn player(world: &mut World, room: Entity) -> (Entity, Rx) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let e = world
        .spawn((
            Player,
            Online,
            Named {
                name: "Seeker".into(),
            },
            Located(room),
            Connection(tx),
            Account {
                user_id: String::new(),
                character_id: "c-seeker".into(),
                role: effective_rank(20, UserRole::Player),
                account_role: UserRole::Player,
                perms: vec![],
            },
            Profile {
                level: 20,
                class_id: None,
                race: "Human".into(),
                experience: 0,
                gender: "neutral".into(),
            },
        ))
        .id();
    (e, rx)
}

/// The GMCP `Room.Info` payload the prompt path would send now.
fn room_info(world: &mut World, p: Entity, rx: &mut Rx) -> String {
    drain(rx);
    super::send_prompt(world, p);
    let out = drain(rx);
    out.split("Room.Info")
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn search_reveals_hidden_exit_in_exits_and_allows_walking() {
    let (mut world, tunnel, ice) = world_with_rooms();
    world
        .get_mut::<Exits>(tunnel)
        .unwrap()
        .0
        .insert(Direction::East, hidden_door(ice, ExitState::Open));
    let (p, mut rx) = player(&mut world, tunnel);

    dispatch(&mut world, p, "exits");
    assert!(!drain(&mut rx).contains("east"), "hidden before search");
    assert!(
        !room_info(&mut world, p, &mut rx).contains("\"east\""),
        "hidden in Room.Info"
    );
    dispatch(&mut world, p, "east");
    assert!(drain(&mut rx).contains("can't go that way"));

    world.entity_mut(p).insert(CoreStats {
        intelligence: 100,
        ..CoreStats::default()
    });
    super::info::search_with_roll(&mut world, p, "", &mut |_| 50);
    let out = drain(&mut rx);
    assert!(
        out.contains("You have found a hidden monolith to the east."),
        "{out}"
    );

    dispatch(&mut world, p, "exits");
    assert!(drain(&mut rx).contains("east"), "revealed in exits");
    assert!(
        room_info(&mut world, p, &mut rx).contains("\"east\""),
        "revealed in Room.Info"
    );
    dispatch(&mut world, p, "east");
    drain(&mut rx);
    assert_eq!(world.get::<Located>(p).unwrap().0, ice);
}

#[test]
fn search_by_keyword_reveals_closed_hidden_door_then_open_works() {
    let (mut world, tunnel, ice) = world_with_rooms();
    world
        .get_mut::<Exits>(tunnel)
        .unwrap()
        .0
        .insert(Direction::East, hidden_door(ice, ExitState::Closed));
    let (p, mut rx) = player(&mut world, tunnel);

    dispatch(&mut world, p, "open monolith");
    drain(&mut rx);
    assert_eq!(
        world.get::<Exits>(tunnel).unwrap().0[&Direction::East].state,
        ExitState::Closed,
        "undiscovered door must not open"
    );

    dispatch(&mut world, p, "search monolith");
    let out = drain(&mut rx);
    assert!(out.contains("hidden"), "{out}");

    dispatch(&mut world, p, "open monolith");
    drain(&mut rx);
    assert_eq!(
        world.get::<Exits>(tunnel).unwrap().0[&Direction::East].state,
        ExitState::Open
    );
    dispatch(&mut world, p, "east");
    drain(&mut rx);
    assert_eq!(world.get::<Located>(p).unwrap().0, ice);
}

fn advance(world: &mut World, ticks: u64) {
    world.resource_mut::<crate::TickCount>().0 += ticks;
}

fn searcher_with_int(world: &mut World, room: Entity, int: i32) -> (Entity, Rx) {
    let (p, rx) = player(world, room);
    world.entity_mut(p).insert(CoreStats {
        intelligence: int,
        ..CoreStats::default()
    });
    (p, rx)
}

fn revealed(world: &World, p: Entity, room: Entity, dir: Direction) -> bool {
    world
        .get::<mud_world::RevealedExits>(p)
        .is_some_and(|r| r.set.contains(&(room, dir)))
}

#[test]
fn search_roll_is_int_against_random_0_to_200() {
    let (mut world, tunnel, ice) = world_with_rooms();
    world
        .get_mut::<Exits>(tunnel)
        .unwrap()
        .0
        .insert(Direction::East, hidden_door(ice, ExitState::Open));
    let (p, mut rx) = searcher_with_int(&mut world, tunnel, 50);

    // INT must strictly beat the roll: 50 > 50 fails, 50 > 49 passes.
    let mut seen = Vec::new();
    super::info::search_with_roll(&mut world, p, "", &mut |hi| {
        seen.push(hi);
        50
    });
    assert!(drain(&mut rx).contains("You find nothing of interest."));
    assert!(!revealed(&world, p, tunnel, Direction::East));
    assert_eq!(seen, vec![200]);

    advance(&mut world, 20);
    super::info::search_with_roll(&mut world, p, "", &mut |_| 49);
    assert!(drain(&mut rx).contains("You have found a hidden monolith"));
    assert!(revealed(&world, p, tunnel, Direction::East));
}

#[test]
fn search_keyword_match_always_reveals_without_rolling() {
    let (mut world, tunnel, ice) = world_with_rooms();
    world
        .get_mut::<Exits>(tunnel)
        .unwrap()
        .0
        .insert(Direction::East, hidden_door(ice, ExitState::Closed));
    let (p, mut rx) = searcher_with_int(&mut world, tunnel, 0);

    // A wrong keyword falls back to the (failing) roll.
    super::info::search_with_roll(&mut world, p, "portal", &mut |_| 200);
    assert!(!revealed(&world, p, tunnel, Direction::East));
    advance(&mut world, 20);
    // A keyword prefix reveals even with INT 0 and a hopeless roll.
    super::info::search_with_roll(&mut world, p, "mono", &mut |_| panic!("no roll on a match"));
    assert!(drain(&mut rx).contains("You have found a hidden monolith to the east."));
    assert!(revealed(&world, p, tunnel, Direction::East));
}

#[test]
fn search_rolls_per_hidden_exit_and_reveals_only_the_first_found() {
    let (mut world, tunnel, ice) = world_with_rooms();
    {
        let mut exits = world.get_mut::<Exits>(tunnel).unwrap();
        exits
            .0
            .insert(Direction::North, hidden_door(ice, ExitState::Open));
        exits
            .0
            .insert(Direction::East, hidden_door(ice, ExitState::Open));
    }
    let (p, mut rx) = searcher_with_int(&mut world, tunnel, 100);

    // North fails (150), east passes (10): only east is found.
    let mut rolls = [150, 10].into_iter();
    super::info::search_with_roll(&mut world, p, "", &mut |_| rolls.next().unwrap());
    drain(&mut rx);
    assert!(!revealed(&world, p, tunnel, Direction::North));
    assert!(revealed(&world, p, tunnel, Direction::East));

    // Both would pass: legacy stops at the first, so north (visited
    // before east) is found and the search ends.
    let (q, _rx) = searcher_with_int(&mut world, tunnel, 100);
    super::info::search_with_roll(&mut world, q, "", &mut |_| 0);
    assert!(revealed(&world, q, tunnel, Direction::North));
    assert!(!revealed(&world, q, tunnel, Direction::East));
}

#[test]
fn search_lags_the_searcher_for_half_a_combat_round() {
    let (mut world, tunnel, ice) = world_with_rooms();
    world
        .get_mut::<Exits>(tunnel)
        .unwrap()
        .0
        .insert(Direction::East, hidden_door(ice, ExitState::Open));
    let (p, mut rx) = searcher_with_int(&mut world, tunnel, 50);

    // A failed search still costs the lag (legacy WAIT_STATE).
    super::info::search_with_roll(&mut world, p, "", &mut |_| 200);
    drain(&mut rx);
    advance(&mut world, 19);
    super::info::search_with_roll(&mut world, p, "", &mut |_| 0);
    assert!(drain(&mut rx).contains("still recovering"));
    assert!(!revealed(&world, p, tunnel, Direction::East));
    advance(&mut world, 1);
    super::info::search_with_roll(&mut world, p, "", &mut |_| 0);
    assert!(drain(&mut rx).contains("You have found a hidden monolith"));
}

#[test]
fn staff_always_find_hidden_exits_and_are_not_lagged() {
    let (mut world, tunnel, ice) = world_with_rooms();
    {
        let mut exits = world.get_mut::<Exits>(tunnel).unwrap();
        exits
            .0
            .insert(Direction::East, hidden_door(ice, ExitState::Open));
        exits
            .0
            .insert(Direction::West, hidden_door(ice, ExitState::Open));
    }
    let (p, mut rx) = searcher_with_int(&mut world, tunnel, 0);
    world.get_mut::<Account>(p).unwrap().role = effective_rank(100, UserRole::Immortal);

    // INT 0 and a hopeless roll: a player would find nothing.
    super::info::search_with_roll(&mut world, p, "", &mut |_| panic!("staff never roll"));
    assert!(drain(&mut rx).contains("You have found a hidden monolith to the east."));
    assert!(revealed(&world, p, tunnel, Direction::East));
    // No lag: the next search (same tick) finds the west exit.
    super::info::search_with_roll(&mut world, p, "", &mut |_| 200);
    assert!(revealed(&world, p, tunnel, Direction::West));
}
