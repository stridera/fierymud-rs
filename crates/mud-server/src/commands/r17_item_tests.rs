//! Playtest r17: an item's minimum level gates wear / wield / hold (B10) and a
//! container's `Capacity` gates `put` (B7). Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole, WearFlag};
use mud_world::{
    Account, EquippedSlot, Exits, Item, Keywords, Located, Named, ObjectPrototypes, Profile, Room,
    WearableIn, WorldKey, wear_flags_primary_slot,
};

use super::dispatch;
use super::test_support::{Rx, drain, object_proto, player_in};

fn setup(level: i32, role: UserRole) -> (World, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(ObjectPrototypes::default());
    world.init_resource::<mud_world::ObjectAbilityCatalog>();
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
            role,
            account_role: role,
            perms: vec![],
        },
        Profile {
            level,
            class_id: None,
            race: "HUMAN".into(),
            experience: 0,
            gender: "male".into(),
        },
    ));
    (world, player, rx)
}

/// Carry a piece of gear of `level` wearable on `flags`.
fn gear(
    world: &mut World,
    holder: Entity,
    id: i32,
    name: &str,
    keyword: &str,
    flags: &[WearFlag],
    level: i32,
) -> Entity {
    let mut proto = object_proto(1, id, ObjectType::Armor);
    proto.name = name.into();
    proto.keywords = vec![keyword.into()];
    proto.wear_flags = flags.to_vec();
    proto.level = level;
    world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .insert((1, id), proto);
    let mut item = world.spawn((
        Item,
        Named { name: name.into() },
        Keywords(vec![keyword.into()]),
        WorldKey { zone: 1, id },
        Located(holder),
    ));
    if let Some(slot) = wear_flags_primary_slot(flags) {
        item.insert(WearableIn(slot));
    }
    item.id()
}

fn is_worn(world: &World, item: Entity) -> bool {
    world.get::<EquippedSlot>(item).is_some()
}

#[test]
fn wear_refuses_an_item_above_the_wearers_level() {
    let (mut world, p, mut rx) = setup(25, UserRole::Player);
    let cloak = gear(
        &mut world,
        p,
        1,
        "a thin sheet of cloud",
        "cloud",
        &[WearFlag::About],
        70,
    );
    dispatch(&mut world, p, "wear cloud");
    let out = drain(&mut rx);
    assert!(
        out.contains("You're not a high enough level to use a thin sheet of cloud."),
        "{out}"
    );
    assert!(!is_worn(&world, cloak));
}

#[test]
fn wield_and_hold_refuse_an_item_above_the_wearers_level() {
    let (mut world, p, mut rx) = setup(25, UserRole::Player);
    let sword = gear(
        &mut world,
        p,
        1,
        "a long sword",
        "sword",
        &[WearFlag::Mainhand],
        40,
    );
    let torch = gear(
        &mut world,
        p,
        2,
        "a torch",
        "torch",
        &[WearFlag::Offhand],
        40,
    );
    dispatch(&mut world, p, "wield sword");
    dispatch(&mut world, p, "hold torch");
    let out = drain(&mut rx);
    assert_eq!(
        out.matches("You're not a high enough level to use").count(),
        2,
        "{out}"
    );
    assert!(!is_worn(&world, sword) && !is_worn(&world, torch));
}

#[test]
fn an_item_at_or_below_the_wearers_level_is_worn() {
    let (mut world, p, mut rx) = setup(25, UserRole::Player);
    let hat = gear(&mut world, p, 1, "a felt hat", "hat", &[WearFlag::Head], 25);
    dispatch(&mut world, p, "wear hat");
    let out = drain(&mut rx);
    assert!(!out.contains("high enough level"), "{out}");
    assert!(is_worn(&world, hat));
}

#[test]
fn wear_all_skips_an_over_level_item_quietly() {
    let (mut world, p, mut rx) = setup(25, UserRole::Player);
    let hat = gear(&mut world, p, 1, "a felt hat", "hat", &[WearFlag::Head], 5);
    let cloak = gear(
        &mut world,
        p,
        2,
        "a thin sheet of cloud",
        "cloud",
        &[WearFlag::About],
        70,
    );
    dispatch(&mut world, p, "wear all");
    let out = drain(&mut rx);
    assert!(!out.contains("high enough level"), "{out}");
    assert!(is_worn(&world, hat));
    assert!(!is_worn(&world, cloak));
}

#[test]
fn staff_ignore_item_levels() {
    let (mut world, p, mut rx) = setup(105, UserRole::Immortal);
    let cloak = gear(
        &mut world,
        p,
        1,
        "a thin sheet of cloud",
        "cloud",
        &[WearFlag::About],
        120,
    );
    dispatch(&mut world, p, "wear cloud");
    let out = drain(&mut rx);
    assert!(!out.contains("high enough level"), "{out}");
    assert!(is_worn(&world, cloak));
}

// -- container capacity (B7) -------------------------------------------------------------

/// A carried bag with `capacity` (own weight 1 lb) and a carried item of `weight`.
fn bag_and_item(world: &mut World, p: Entity, capacity: f64, weight: f64) -> (Entity, Entity) {
    let mut bag = object_proto(1, 10, ObjectType::Container);
    bag.name = "a bag".into();
    bag.keywords = vec!["bag".into()];
    bag.weight = 1.0;
    bag.capacity = Some(capacity);
    let mut mail = object_proto(1, 11, ObjectType::Armor);
    mail.name = "a banded mail".into();
    mail.keywords = vec!["mail".into()];
    mail.weight = weight;
    {
        let mut protos = world.resource_mut::<ObjectPrototypes>();
        protos.by_key.insert((1, 10), bag);
        protos.by_key.insert((1, 11), mail);
    }
    let spawn = |world: &mut World, name: &str, kw: &str, id: i32, at: Entity| {
        world
            .spawn((
                Item,
                Named { name: name.into() },
                Keywords(vec![kw.into()]),
                WorldKey { zone: 1, id },
                Located(at),
            ))
            .id()
    };
    let bag = spawn(world, "a bag", "bag", 10, p);
    let mail = spawn(world, "a banded mail", "mail", 11, p);
    (bag, mail)
}

#[test]
fn put_refuses_what_would_overfill_the_container() {
    let (mut world, p, mut rx) = setup(25, UserRole::Player);
    let (bag, mail) = bag_and_item(&mut world, p, 50.0, 65.0);
    dispatch(&mut world, p, "put mail bag");
    let out = drain(&mut rx);
    assert!(out.contains("A banded mail won't fit in a bag."), "{out}");
    assert_eq!(world.get::<Located>(mail).map(|l| l.0), Some(p));
    let _ = bag;
}

#[test]
fn put_counts_what_the_container_already_holds() {
    let (mut world, p, mut rx) = setup(25, UserRole::Player);
    // Capacity 50: the bag's own 1 lb + 30 lb fits, a second 30 lb does not.
    let (bag, mail) = bag_and_item(&mut world, p, 50.0, 30.0);
    let mail2 = world
        .spawn((
            Item,
            Named {
                name: "a banded mail".into(),
            },
            Keywords(vec!["mail".into()]),
            WorldKey { zone: 1, id: 11 },
            Located(p),
        ))
        .id();
    dispatch(&mut world, p, "put mail bag");
    let first = drain(&mut rx);
    assert!(first.contains("You put a banded mail in a bag."), "{first}");
    dispatch(&mut world, p, "put mail bag");
    let second = drain(&mut rx);
    assert!(second.contains("won't fit in a bag."), "{second}");
    let in_bag = [mail, mail2]
        .iter()
        .filter(|m| world.get::<Located>(**m).map(|l| l.0) == Some(bag))
        .count();
    assert_eq!(in_bag, 1);
}

#[test]
fn put_all_stores_what_fits_and_names_what_does_not() {
    let (mut world, p, mut rx) = setup(25, UserRole::Player);
    let (_bag, mail) = bag_and_item(&mut world, p, 50.0, 65.0);
    dispatch(&mut world, p, "put all bag");
    let out = drain(&mut rx);
    assert!(out.contains("A banded mail won't fit in a bag."), "{out}");
    assert_eq!(world.get::<Located>(mail).map(|l| l.0), Some(p));
}
