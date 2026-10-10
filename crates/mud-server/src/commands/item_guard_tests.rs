//! Guards that keep items from being lost: a full container survives
//! `sell` / `junk` instead of having its contents orphaned. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, Sector};
use mud_world::{
    Item, Keywords, Located, Mob, Named, ObjectPrototypes, Room, RoomSector, ShopAcceptRule,
    ShopCatalog, ShopDef, Shopkeeper, Wealth, WorldKey,
};

use super::info::{cmd_junk, cmd_sell};
use super::test_support::{Rx, drain, object_proto, player_in};

const SHOP: (i32, i32) = (30, 91);

pub(super) fn protos(world: &mut World) {
    let mut objects = ObjectPrototypes::default();
    let mut sword = object_proto(30, 7, ObjectType::Weapon);
    sword.name = "a steel sword".to_string();
    sword.keywords = vec!["sword".to_string()];
    sword.cost = 100;
    objects.by_key.insert((30, 7), sword);
    let mut bag = object_proto(30, 8, ObjectType::Container);
    bag.name = "a leather bag".to_string();
    bag.keywords = vec!["bag".to_string()];
    bag.cost = 50;
    objects.by_key.insert((30, 8), bag);
    let mut coins = object_proto(30, 9, ObjectType::Money);
    coins.name = "a pile of coins".to_string();
    coins.keywords = vec!["coins".to_string()];
    objects.by_key.insert((30, 9), coins);
    world.insert_resource(objects);
}

/// A shop room that buys weapons and containers, with a funded player.
pub(super) fn shop_world() -> (World, Entity, Rx) {
    let mut world = World::new();
    protos(&mut world);
    let mut catalog = ShopCatalog::default();
    catalog.keeper_index.insert((30, 91), SHOP);
    catalog.by_key.insert(
        SHOP,
        ShopDef {
            zone_id: SHOP.0,
            id: SHOP.1,
            keeper_zone_id: 30,
            keeper_id: 91,
            buy_profit: 1.0,
            sell_profit: 0.5,
            items: Vec::new(),
            accepts: ["WEAPON", "CONTAINER"]
                .into_iter()
                .map(|t| ShopAcceptRule {
                    object_type: t.to_string(),
                    keywords: Vec::new(),
                })
                .collect(),
            pets: Vec::new(),
        },
    );
    world.insert_resource(catalog);
    let room = world.spawn((Room, RoomSector(Sector::Field))).id();
    world.spawn((
        Mob,
        Named {
            name: "Jorhan".to_string(),
        },
        Located(room),
        Shopkeeper {
            shop_zone_id: SHOP.0,
            shop_id: SHOP.1,
        },
    ));
    let (player, rx) = player_in(&mut world, room);
    world.entity_mut(player).insert(Wealth(0));
    (world, player, rx)
}

pub(super) fn item_at(world: &mut World, holder: Entity, id: i32, name: &str, kw: &str) -> Entity {
    world
        .spawn((
            Item,
            Named {
                name: name.to_string(),
            },
            Keywords(vec![kw.to_string()]),
            WorldKey { zone: 30, id },
            Located(holder),
        ))
        .id()
}

pub(super) fn bag_at(world: &mut World, holder: Entity) -> Entity {
    item_at(world, holder, 8, "a leather bag", "bag")
}

pub(super) fn sword_at(world: &mut World, holder: Entity) -> Entity {
    item_at(world, holder, 7, "a steel sword", "sword")
}

#[test]
fn selling_a_full_bag_is_refused_and_nothing_is_lost() {
    let (mut world, player, mut rx) = shop_world();
    let bag = bag_at(&mut world, player);
    let sword = sword_at(&mut world, bag);
    cmd_sell(&mut world, player, "bag");
    let out = drain(&mut rx);
    assert!(out.contains("still has things in it"), "{out}");
    assert_eq!(world.get::<Wealth>(player).unwrap().0, 0);
    assert_eq!(world.get::<Located>(bag).unwrap().0, player);
    assert_eq!(world.get::<Located>(sword).unwrap().0, bag);

    // Emptied, the bag sells; the sword it held is still intact.
    world.entity_mut(sword).insert(Located(player));
    cmd_sell(&mut world, player, "bag");
    assert!(drain(&mut rx).contains("You sell"));
    assert_eq!(world.get::<Wealth>(player).unwrap().0, 25);
    assert!(world.get_entity(bag).is_err());
    assert_eq!(world.get::<Located>(sword).unwrap().0, player);
}

#[test]
fn junking_a_full_bag_is_refused_and_nothing_is_lost() {
    let (mut world, player, mut rx) = shop_world();
    let bag = bag_at(&mut world, player);
    let sword = sword_at(&mut world, bag);
    cmd_junk(&mut world, player, "bag");
    let out = drain(&mut rx);
    assert!(out.contains("still has things in it"), "{out}");
    assert!(world.get_entity(bag).is_ok());
    assert_eq!(world.get::<Located>(sword).unwrap().0, bag);

    world.entity_mut(sword).insert(Located(player));
    cmd_junk(&mut world, player, "bag");
    assert!(drain(&mut rx).contains("You destroy"));
    assert!(world.get_entity(bag).is_err());
    assert!(world.get_entity(sword).is_ok());
}
