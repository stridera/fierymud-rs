//! `list` / `buy` against the shop catalog: producing items are infinite
//! stock, and pet/mount shops (issue #43) sell their `ShopMobs` offerings
//! through `buy` as well as `hire`.

use bevy_ecs::prelude::*;
use mud_db::enums::{MobProfession, MobTrait, ObjectType, Sector};
use mud_world::{
    Follower, Located, Mob, MobPrototypes, Mountable, Named, ObjectPrototypes, Room, RoomSector,
    ShopCatalog, ShopDef, ShopOffering, ShopPetOffering, Shopkeeper, Wealth,
};

use super::info::{cmd_buy, cmd_inspect, cmd_list, cmd_mount};
use super::test_support::{Rx, drain, mob_proto, object_proto, player_in};

const SHOP: (i32, i32) = (30, 91);

fn shop_def(items: Vec<ShopOffering>, pets: Vec<ShopPetOffering>) -> ShopDef {
    ShopDef {
        zone_id: SHOP.0,
        id: SHOP.1,
        keeper_zone_id: 30,
        keeper_id: 91,
        buy_profit: 1.0,
        sell_profit: 1.0,
        items,
        accepts: Vec::new(),
        pets,
    }
}

/// A room with a keeper bound to `SHOP`, a funded player and the catalog.
fn world_with_shop(def: ShopDef, wealth: i64) -> (World, Entity, Rx) {
    let mut world = World::new();
    let mut objects = ObjectPrototypes::default();
    let mut sword = object_proto(30, 7, ObjectType::Weapon);
    sword.name = "a steel sword".to_string();
    sword.keywords = vec!["sword".to_string()];
    sword.cost = 100;
    objects.by_key.insert((30, 7), sword);
    world.insert_resource(objects);
    let mut mobs = MobPrototypes::default();
    let mut mare = mob_proto(30, 81, MobProfession::Shopkeeper);
    mare.name = "a stout mare".to_string();
    mare.keywords = vec!["mare".to_string(), "horse".to_string()];
    mare.traits = vec![MobTrait::Mount];
    mare.level = 4;
    mobs.by_key.insert((30, 81), mare);
    let mut kitten = mob_proto(30, 90, MobProfession::Shopkeeper);
    kitten.name = "a kitten".to_string();
    kitten.keywords = vec!["kitten".to_string()];
    kitten.level = 1;
    mobs.by_key.insert((30, 90), kitten);
    world.insert_resource(mobs);
    let mut catalog = ShopCatalog::default();
    catalog.keeper_index.insert((30, 91), SHOP);
    catalog.by_key.insert(SHOP, def);
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
    world.entity_mut(player).insert(Wealth(wealth));
    (world, player, rx)
}

fn followers_of(world: &mut World, player: Entity) -> Vec<Entity> {
    let mut q = world.query_filtered::<(Entity, &Follower), With<Mob>>();
    q.iter(world)
        .filter(|(_, f)| f.0 == player)
        .map(|(e, _)| e)
        .collect()
}

#[test]
fn keeper_with_producing_items_lists_them_as_unlimited_and_buy_works() {
    let items = vec![ShopOffering {
        object_zone_id: 30,
        object_id: 7,
        amount: -1,
        price: 0,
    }];
    let (mut world, player, mut rx) = world_with_shop(shop_def(items, Vec::new()), 1_000);

    cmd_list(&mut world, player, "");
    let out = drain(&mut rx);
    assert!(out.contains("Jorhan offers"), "{out}");
    assert!(out.contains("a steel sword"), "{out}");
    assert!(out.contains("unlimited"), "{out}");

    cmd_buy(&mut world, player, "sword");
    let out = drain(&mut rx);
    assert!(out.contains("You buy a steel sword"), "{out}");
    assert_eq!(world.get::<Wealth>(player).unwrap().0, 900);
    // Infinite stock: a second purchase still succeeds.
    cmd_buy(&mut world, player, "1");
    assert!(drain(&mut rx).contains("You buy a steel sword"));
    assert_eq!(world.get::<Wealth>(player).unwrap().0, 800);
    let held = world
        .query::<&Located>()
        .iter(&world)
        .filter(|l| l.0 == player)
        .count();
    assert_eq!(held, 2);
}

#[test]
fn mount_shop_lists_its_stable_and_buy_hires_a_mountable_steed() {
    let pets = vec![
        ShopPetOffering {
            mob_zone_id: 30,
            mob_id: 81,
            amount: -1,
            price: 0,
        },
        ShopPetOffering {
            mob_zone_id: 30,
            mob_id: 90,
            amount: -1,
            price: 0,
        },
    ];
    let (mut world, player, mut rx) = world_with_shop(shop_def(Vec::new(), pets), 1_000);

    cmd_list(&mut world, player, "");
    let out = drain(&mut rx);
    assert!(out.contains("a stout mare"), "{out}");
    assert!(out.contains("a kitten"), "{out}");
    assert!(!out.contains("nothing to sell"), "{out}");

    // Plain `buy <name>` works in a stable, and the mare costs level * 100.
    cmd_buy(&mut world, player, "mare");
    let out = drain(&mut rx);
    assert!(out.contains("You hire a stout mare"), "{out}");
    assert_eq!(world.get::<Wealth>(player).unwrap().0, 600);
    let followers = followers_of(&mut world, player);
    assert_eq!(followers.len(), 1);
    assert!(
        world.get::<Mountable>(followers[0]).is_some(),
        "a bought mount must be rideable"
    );

    // Numeric index also works when the shop has no item stock.
    cmd_buy(&mut world, player, "2");
    assert!(drain(&mut rx).contains("You hire a kitten"));
    assert_eq!(followers_of(&mut world, player).len(), 2);
}

#[test]
fn buying_something_the_stable_does_not_have_is_refused_without_charge() {
    let pets = vec![ShopPetOffering {
        mob_zone_id: 30,
        mob_id: 81,
        amount: -1,
        price: 0,
    }];
    let (mut world, player, mut rx) = world_with_shop(shop_def(Vec::new(), pets), 1_000);
    cmd_buy(&mut world, player, "dragon");
    let out = drain(&mut rx);
    assert!(out.contains("doesn't have 'dragon' for hire"), "{out}");
    assert_eq!(world.get::<Wealth>(player).unwrap().0, 1_000);
    assert!(followers_of(&mut world, player).is_empty());
}

fn mixed_shop() -> ShopDef {
    // Items: sword (1). Pets: mare (2), kitten (3).
    shop_def(
        vec![ShopOffering {
            object_zone_id: 30,
            object_id: 7,
            amount: -1,
            price: 0,
        }],
        vec![
            ShopPetOffering {
                mob_zone_id: 30,
                mob_id: 81,
                amount: -1,
                price: 0,
            },
            ShopPetOffering {
                mob_zone_id: 30,
                mob_id: 90,
                amount: -1,
                price: 0,
            },
        ],
    )
}

#[test]
fn list_numbers_pets_after_the_items() {
    let (mut world, player, mut rx) = world_with_shop(mixed_shop(), 1_000);
    cmd_list(&mut world, player, "");
    let out = drain(&mut rx);
    let row = |needle: &str| {
        out.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} missing in {out}"))
            .split_whitespace()
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(row("a steel sword"), "1");
    assert_eq!(row("a stout mare"), "2");
    assert_eq!(row("a kitten"), "3");
}

#[test]
fn buy_number_uses_the_unified_numbering() {
    let (mut world, player, mut rx) = world_with_shop(mixed_shop(), 1_000);
    cmd_buy(&mut world, player, "3");
    assert!(drain(&mut rx).contains("You hire a kitten"));
    cmd_buy(&mut world, player, "1");
    assert!(drain(&mut rx).contains("You buy a steel sword"));
    cmd_buy(&mut world, player, "2");
    assert!(drain(&mut rx).contains("You hire a stout mare"));
    cmd_buy(&mut world, player, "4");
    let out = drain(&mut rx);
    assert!(out.contains("doesn't have '4' for hire"), "{out}");
    assert_eq!(followers_of(&mut world, player).len(), 2);
}

#[test]
fn keyword_matching_both_prefers_the_item_and_pet_only_keyword_hires() {
    let (mut world, player, mut rx) = world_with_shop(mixed_shop(), 1_000);
    // Give the mare the keyword "sword" too: the item still wins.
    world
        .resource_mut::<MobPrototypes>()
        .by_key
        .get_mut(&(30, 81))
        .unwrap()
        .keywords
        .push("sword".to_string());
    cmd_buy(&mut world, player, "sword");
    assert!(drain(&mut rx).contains("You buy a steel sword"));
    assert!(followers_of(&mut world, player).is_empty());
    cmd_buy(&mut world, player, "kitten");
    assert!(drain(&mut rx).contains("You hire a kitten"));
}

#[test]
fn only_trait_tagged_mobs_are_mountable() {
    let mut tagged = mob_proto(1, 1, MobProfession::Shopkeeper);
    tagged.traits = vec![MobTrait::Mount];
    assert!(tagged.is_mountable());
    let mut mountain = mob_proto(1, 2, MobProfession::Shopkeeper);
    mountain.keywords = vec!["mountain".to_string(), "horse".to_string()];
    assert!(!mountain.is_mountable());
}

#[test]
fn persisted_mount_is_rideable_after_relog() {
    let (mut world, player, mut rx) = world_with_shop(shop_def(Vec::new(), Vec::new()), 0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let persisted: crate::login::PersistedPets = serde_json::from_value(serde_json::json!({
        "saved_at_unix": now,
        "pets": [{
            "proto_zone_id": 30, "proto_id": 81,
            "name": "Tester's a stout mare", "hp": 10, "max_hp": 10
        }]
    }))
    .unwrap();
    crate::login::restore_persisted_pets(&mut world, player, persisted);
    let pets = followers_of(&mut world, player);
    assert_eq!(pets.len(), 1);
    assert!(world.get::<Mountable>(pets[0]).is_some());
    cmd_mount(&mut world, player, "mare");
    let out = drain(&mut rx);
    assert!(out.contains("You mount"), "{out}");
}

fn sword_shop() -> ShopDef {
    shop_def(
        vec![ShopOffering {
            object_zone_id: 30,
            object_id: 7,
            amount: -1,
            price: 0,
        }],
        Vec::new(),
    )
}

#[test]
fn inspect_without_argument_lists_wares_with_a_tenth_fee() {
    let (mut world, player, mut rx) = world_with_shop(sword_shop(), 1_000);
    cmd_inspect(&mut world, player, "");
    let out = drain(&mut rx);
    assert!(out.contains("Jorhan will inspect"), "{out}");
    assert!(out.contains("a steel sword"), "{out}");
    // Sword costs 100 at profit 1.0 -> fee 10 copper.
    assert!(out.contains("1 silver"), "{out}");
    assert_eq!(world.get::<Wealth>(player).unwrap().0, 1_000);
}

#[test]
fn inspect_item_charges_the_fee_shows_stats_and_does_not_buy() {
    let (mut world, player, mut rx) = world_with_shop(sword_shop(), 1_000);
    // Resources the stat block reads unconditionally.
    world.init_resource::<mud_world::ObjectAbilityCatalog>();
    world.init_resource::<mud_world::AbilityCatalog>();
    world.init_resource::<mud_world::ClassCatalog>();
    world.init_resource::<mud_world::LiquidCatalog>();
    cmd_inspect(&mut world, player, "sword");
    let out = drain(&mut rx);
    assert!(out.contains("Properties"), "{out}");
    assert!(out.contains("Weapon"), "{out}");
    assert_eq!(world.get::<Wealth>(player).unwrap().0, 990);
    // Nothing was handed over and the throwaway copy is gone.
    let held = world
        .query::<&Located>()
        .iter(&world)
        .filter(|l| l.0 == player)
        .count();
    assert_eq!(held, 0);
    let items = world.query::<&mud_world::Item>().iter(&world).count();
    assert_eq!(items, 0);
}

#[test]
fn inspect_refuses_when_the_player_cannot_cover_the_fee() {
    let (mut world, player, mut rx) = world_with_shop(sword_shop(), 3);
    cmd_inspect(&mut world, player, "1");
    let out = drain(&mut rx);
    assert!(out.contains("to have that inspected"), "{out}");
    assert!(!out.contains("Properties"), "{out}");
    assert_eq!(world.get::<Wealth>(player).unwrap().0, 3);
}

#[test]
fn inspect_pet_shows_stats_with_unified_numbering() {
    let (mut world, player, mut rx) = world_with_shop(mixed_shop(), 1_000);
    cmd_inspect(&mut world, player, "kitten");
    let out = drain(&mut rx);
    assert!(out.contains("Name: a kitten"), "{out}");
    assert!(out.contains("Level: 1"), "{out}");
}
