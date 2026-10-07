//! `ExpandMobs` / `ExpandObjs` (issue #51): legacy `PRF_EXPAND_MOBS` /
//! `PRF_EXPAND_OBJS`. By default identical mobs / objects in room,
//! inventory and container listings stack as `(N) <name>`; with the flag
//! set each gets its own line. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{PlayerFlag, UserRole};
use mud_world::{
    Account, Corpse, Exits, Item, Keywords, Located, Mob, Named, PlayerFlags, Room, WorldKey,
};

use super::dispatch;
use super::test_support::{Rx, drain, player_in};

fn setup(flags: Vec<PlayerFlag>) -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    let room = world
        .spawn((
            Room,
            Named {
                name: "A quiet hall".into(),
            },
            Exits::default(),
        ))
        .id();
    let (player, rx) = player_in(&mut world, room);
    world.entity_mut(player).insert((
        Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        },
        PlayerFlags(flags),
    ));
    (world, room, player, rx)
}

fn spawn_rats(world: &mut World, room: Entity, n: usize) {
    for _ in 0..n {
        world.spawn((
            Mob,
            Named {
                name: "a grey rat".into(),
            },
            Located(room),
        ));
    }
}

fn spawn_coins(world: &mut World, holder: Entity, n: usize) {
    for _ in 0..n {
        world.spawn((
            Item,
            Named {
                name: "a copper coin".into(),
            },
            Keywords(vec!["coin".into()]),
            WorldKey { zone: 1, id: 1 },
            Located(holder),
        ));
    }
}

/// Occurrences in the visible text; GMCP frames (`Char.Items.List`
/// carries every item name) are ignored.
fn count(out: &str, needle: &str) -> usize {
    out.lines()
        .filter(|l| !l.contains("Char."))
        .map(|l| l.matches(needle).count())
        .sum()
}

#[test]
fn room_mobs_stack_by_default() {
    let (mut world, room, p, mut rx) = setup(vec![]);
    spawn_rats(&mut world, room, 3);
    dispatch(&mut world, p, "look");
    let out = drain(&mut rx);
    assert_eq!(count(&out, "a grey rat"), 1, "{out}");
    assert!(out.contains("(3)"), "{out}");
}

#[test]
fn room_mobs_list_one_per_line_with_expand_mobs() {
    let (mut world, room, p, mut rx) = setup(vec![PlayerFlag::ExpandMobs]);
    spawn_rats(&mut world, room, 3);
    dispatch(&mut world, p, "look");
    let out = drain(&mut rx);
    assert_eq!(count(&out, "a grey rat"), 3, "{out}");
    assert!(!out.contains("(3)"), "{out}");
}

#[test]
fn expand_objs_does_not_expand_mobs_and_vice_versa() {
    let (mut world, room, p, mut rx) = setup(vec![PlayerFlag::ExpandObjs]);
    spawn_rats(&mut world, room, 3);
    spawn_coins(&mut world, room, 2);
    dispatch(&mut world, p, "look");
    let out = drain(&mut rx);
    assert_eq!(count(&out, "a grey rat"), 1, "{out}");
    assert_eq!(count(&out, "a copper coin"), 2, "{out}");
}

#[test]
fn room_objects_stack_by_default_and_expand_with_flag() {
    let (mut world, room, p, mut rx) = setup(vec![]);
    spawn_coins(&mut world, room, 3);
    dispatch(&mut world, p, "look");
    let out = drain(&mut rx);
    assert_eq!(count(&out, "a copper coin"), 1, "{out}");
    assert!(out.contains("(3)"), "{out}");

    world
        .entity_mut(p)
        .insert(PlayerFlags(vec![PlayerFlag::ExpandObjs]));
    dispatch(&mut world, p, "look");
    let out = drain(&mut rx);
    assert_eq!(count(&out, "a copper coin"), 3, "{out}");
    assert!(!out.contains("(3)"), "{out}");
}

#[test]
fn inventory_stacks_by_default_and_expands_with_flag() {
    let (mut world, _room, p, mut rx) = setup(vec![]);
    spawn_coins(&mut world, p, 3);
    dispatch(&mut world, p, "inventory");
    let out = drain(&mut rx);
    assert_eq!(count(&out, "a copper coin"), 1, "{out}");
    assert!(out.contains("(3)"), "{out}");

    world
        .entity_mut(p)
        .insert(PlayerFlags(vec![PlayerFlag::ExpandObjs]));
    dispatch(&mut world, p, "inventory");
    let out = drain(&mut rx);
    assert_eq!(count(&out, "a copper coin"), 3, "{out}");
    assert!(!out.contains("(3)"), "{out}");
}

#[test]
fn container_contents_stack_by_default_and_expand_with_flag() {
    let (mut world, room, p, mut rx) = setup(vec![]);
    let corpse = world
        .spawn((
            Item,
            Corpse,
            Named {
                name: "the corpse of a rat".into(),
            },
            Keywords(vec!["corpse".into()]),
            Located(room),
        ))
        .id();
    spawn_coins(&mut world, corpse, 3);
    dispatch(&mut world, p, "look in corpse");
    let out = drain(&mut rx);
    assert_eq!(count(&out, "a copper coin"), 1, "{out}");
    assert!(out.contains("(3)"), "{out}");

    world
        .entity_mut(p)
        .insert(PlayerFlags(vec![PlayerFlag::ExpandObjs]));
    dispatch(&mut world, p, "look in corpse");
    let out = drain(&mut rx);
    assert_eq!(count(&out, "a copper coin"), 3, "{out}");
    assert!(!out.contains("(3)"), "{out}");
}

#[test]
fn toggle_lists_and_flips_both_expand_flags() {
    let (mut world, _room, p, mut rx) = setup(vec![PlayerFlag::ExpandMobs]);
    dispatch(&mut world, p, "toggle");
    let out = drain(&mut rx);
    let mobs = out
        .lines()
        .find(|l| l.contains("ExpandMobs"))
        .expect("ExpandMobs row");
    let objs = out
        .lines()
        .find(|l| l.contains("ExpandObjs"))
        .expect("ExpandObjs row");
    assert!(mobs.contains("ON"), "{mobs:?}");
    assert!(objs.contains("OFF"), "{objs:?}");

    dispatch(&mut world, p, "toggle expandobjs");
    let _ = drain(&mut rx);
    assert!(
        world
            .get::<PlayerFlags>(p)
            .is_some_and(|f| f.has(PlayerFlag::ExpandObjs))
    );
}

#[test]
fn stack_entries_folds_in_first_seen_order_unless_expanded() {
    let lines = || ["b", "a", "b", "b"].map(String::from);
    assert_eq!(
        super::stack_entries(lines(), false),
        vec![("b".to_string(), 3), ("a".to_string(), 1)]
    );
    assert_eq!(super::stack_entries(lines(), true).len(), 4);
}
