//! `search` against hidden exits: reveal, `exits` listing, and walking through.

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, UserRole, effective_rank};
use mud_world::{
    Account, ExitData, Exits, Located, Named, Online, Player, Profile, Room, WorldKey,
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

    dispatch(&mut world, p, "search");
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
