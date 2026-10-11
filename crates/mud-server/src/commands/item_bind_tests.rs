//! `donate`, `put` and `house place` honour `SOULBOUND` / `NO_DROP`, and `put`
//! only targets real containers. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectFlag, ObjectRestriction};
use mud_world::{Located, ObjectFlags, ObjectRestrictions, Room, WorldKey};

use super::info::{cmd_donate, cmd_house_place, cmd_put};
use super::item_guard_tests::{bag_at, item_at, protos, shop_world, sword_at};
use super::test_support::{drain, player_in};

fn bind(world: &mut World, item: Entity, soulbound: bool, no_drop: bool) {
    if soulbound {
        world
            .entity_mut(item)
            .insert(ObjectFlags(vec![ObjectFlag::Soulbound]));
    }
    if no_drop {
        world
            .entity_mut(item)
            .insert(ObjectRestrictions(vec![ObjectRestriction::NoDrop]));
    }
}

#[test]
fn donate_refuses_soulbound_and_no_drop_items() {
    for (soulbound, no_drop) in [(true, false), (false, true)] {
        let (mut world, player, mut rx) = shop_world();
        let room = world.get::<Located>(player).unwrap().0;
        let sword = sword_at(&mut world, player);
        bind(&mut world, sword, soulbound, no_drop);
        cmd_donate(&mut world, player, "sword");
        let out = drain(&mut rx);
        assert!(out.contains("soulbound") || out.contains("let go"), "{out}");
        assert_eq!(world.get::<Located>(sword).unwrap().0, player);
        assert_ne!(world.get::<Located>(sword).unwrap().0, room);
    }
    // An ordinary item is still donated.
    let (mut world, player, mut rx) = shop_world();
    let room = world.get::<Located>(player).unwrap().0;
    let sword = sword_at(&mut world, player);
    cmd_donate(&mut world, player, "sword");
    assert!(drain(&mut rx).contains("You leave"));
    assert_eq!(world.get::<Located>(sword).unwrap().0, room);
}

#[test]
fn put_refuses_soulbound_and_no_drop_items() {
    for (soulbound, no_drop) in [(true, false), (false, true)] {
        let (mut world, player, mut rx) = shop_world();
        let bag = bag_at(&mut world, player);
        let sword = sword_at(&mut world, player);
        bind(&mut world, sword, soulbound, no_drop);
        cmd_put(&mut world, player, "sword bag");
        let out = drain(&mut rx);
        assert!(out.contains("soulbound") || out.contains("let go"), "{out}");
        assert_eq!(world.get::<Located>(sword).unwrap().0, player);

        // `put all` skips the bound item and still stores the rest.
        let other = item_at(&mut world, player, 7, "a spare sword", "spare");
        cmd_put(&mut world, player, "all bag");
        assert_eq!(world.get::<Located>(sword).unwrap().0, player);
        assert_eq!(world.get::<Located>(other).unwrap().0, bag);
        drain(&mut rx);
    }
}

#[test]
fn put_refuses_a_target_that_is_not_a_container() {
    let (mut world, player, mut rx) = shop_world();
    let pile = item_at(&mut world, player, 9, "a pile of coins", "coins");
    let sword = sword_at(&mut world, player);
    cmd_put(&mut world, player, "sword coins");
    let out = drain(&mut rx);
    assert!(out.contains("isn't a container"), "{out}");
    assert_eq!(world.get::<Located>(sword).unwrap().0, player);
    cmd_put(&mut world, player, "all coins");
    assert!(drain(&mut rx).contains("isn't a container"));
    assert_eq!(world.get::<Located>(sword).unwrap().0, player);
    assert_eq!(world.get::<Located>(pile).unwrap().0, player);
}

#[test]
fn house_place_refuses_soulbound_and_no_drop_items() {
    for (soulbound, no_drop) in [(true, false), (false, true)] {
        let mut world = World::new();
        protos(&mut world);
        let room = world
            .spawn((
                Room,
                mud_world::HouseRoom {
                    house_id: 1,
                    local_index: 0,
                },
            ))
            .id();
        let (player, mut rx) = player_in(&mut world, room);
        let sword = sword_at(&mut world, player);
        bind(&mut world, sword, soulbound, no_drop);
        let house = mud_world::HouseSummary {
            house_id: 1,
            entrance_room: WorldKey { zone: 30, id: 1 },
            return_room: None,
            rooms: vec![mud_world::HouseRoomEntry {
                id: 100,
                local_index: 0,
                name: "Foyer".into(),
                description: String::new(),
                is_peaceful: true,
                capacity: 10,
            }],
            exits: Vec::new(),
            items: Vec::new(),
            guests: Vec::new(),
        };
        cmd_house_place(&mut world, player, &house, "sword");
        let out = drain(&mut rx);
        assert!(out.contains("soulbound") || out.contains("let go"), "{out}");
        assert_eq!(world.get::<Located>(sword).unwrap().0, player);
        assert!(world.get::<mud_world::HousePlacement>(sword).is_none());
    }
}

/// Playtest r3 #31: `put flour bag` with a bag of flour AND a bag. The
/// container is the last word and must be a real container that is not the
/// item itself, even though the bag of flour also answers to `bag`.
#[test]
fn put_flour_bag_picks_the_real_container_over_the_bag_of_flour() {
    let (mut world, player, mut rx) = shop_world();
    let bag = bag_at(&mut world, player);
    // Newer than the bag, so it wins a plain `bag` lookup.
    let flour = item_at(&mut world, player, 9, "a bag of flour", "flour");
    world
        .entity_mut(flour)
        .insert(mud_world::Keywords(vec!["flour".into(), "bag".into()]));
    cmd_put(&mut world, player, "flour bag");
    let out = drain(&mut rx);
    assert!(!out.contains("isn't a container"), "{out}");
    assert_eq!(world.get::<Located>(flour).unwrap().0, bag, "{out}");
    assert_eq!(world.get::<Located>(bag).unwrap().0, player);
}

/// Without a preposition the item is every word before the last one.
#[test]
fn put_treats_the_last_word_as_the_container() {
    let (mut world, player, mut rx) = shop_world();
    let bag = bag_at(&mut world, player);
    let sword = item_at(&mut world, player, 7, "a steel sword", "steel");
    world
        .entity_mut(sword)
        .insert(mud_world::Keywords(vec!["steel".into(), "sword".into()]));
    cmd_put(&mut world, player, "steel sword bag");
    let out = drain(&mut rx);
    assert_eq!(world.get::<Located>(sword).unwrap().0, bag, "{out}");
}
