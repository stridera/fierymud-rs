//! Global name lookups (summon / relocate / dimension door / track) must
//! not see through invisibility, and `track` must not match items.

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState};
use mud_world::{ExitData, Exits, Invisible, Item, Located, Mob, Named, Online, Player, Room};

use super::find_online_player_anywhere;
use super::info::cmd_track;
use super::test_support::{drain, player_in};

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

fn two_rooms(world: &mut World) -> (Entity, Entity) {
    let a = world.spawn((Room, Exits::default())).id();
    let b = world.spawn((Room, Exits::default())).id();
    world
        .get_mut::<Exits>(a)
        .unwrap()
        .0
        .insert(Direction::East, open_exit(b));
    world
        .get_mut::<Exits>(b)
        .unwrap()
        .0
        .insert(Direction::West, open_exit(a));
    (a, b)
}

fn online_player(world: &mut World, name: &str, room: Entity) -> Entity {
    world
        .spawn((Player, Online, Named { name: name.into() }, Located(room)))
        .id()
}

#[test]
fn global_player_lookup_skips_players_the_seeker_cannot_see() {
    let mut world = World::new();
    let (a, b) = two_rooms(&mut world);
    let me = online_player(&mut world, "Me", a);
    let ghost = online_player(&mut world, "Ghost", b);
    assert_eq!(
        find_online_player_anywhere(&mut world, "ghost", me),
        Some(ghost)
    );
    world.entity_mut(ghost).insert(Invisible);
    assert_eq!(find_online_player_anywhere(&mut world, "ghost", me), None);
    // The invisible one can't make a visible prefix match ambiguous either.
    let gus = online_player(&mut world, "Gus", b);
    assert_eq!(find_online_player_anywhere(&mut world, "g", me), Some(gus));
}

#[test]
fn track_ignores_items_and_invisible_players() {
    let mut world = World::new();
    let (a, b) = two_rooms(&mut world);
    let (me, mut rx) = player_in(&mut world, a);
    world.spawn((
        Item,
        Named {
            name: "sword".into(),
        },
        Located(b),
    ));
    cmd_track(&mut world, me, "sword");
    let out = drain(&mut rx);
    assert!(out.contains("no trace"), "item tracked: {out}");

    let ghost = online_player(&mut world, "Ghost", b);
    world.entity_mut(ghost).insert(Invisible);
    cmd_track(&mut world, me, "ghost");
    let out = drain(&mut rx);
    assert!(out.contains("no trace"), "invisible tracked: {out}");

    world.entity_mut(ghost).remove::<Invisible>();
    cmd_track(&mut world, me, "ghost");
    let out = drain(&mut rx);
    assert!(out.contains("trail leading east"), "{out}");

    world.spawn((
        Mob,
        Named {
            name: "wolf".into(),
        },
        Located(b),
    ));
    cmd_track(&mut world, me, "wolf");
    assert!(drain(&mut rx).contains("trail leading east"));
}
