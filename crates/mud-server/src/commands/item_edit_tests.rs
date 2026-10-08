//! `nameitem` (issue #68). Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole};
use mud_world::{
    Account, Description, Exits, Item, ItemCustomization, Keywords, Located, Named,
    ObjectPrototypes, PendingSave, Room, WorldKey,
};

use super::dispatch;
use super::test_support::{Rx, drain, object_proto, player_in};
use crate::item_custom;

fn setup(role: UserRole) -> (World, Entity, Entity, Rx) {
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
    world.entity_mut(player).insert(Account {
        user_id: String::new(),
        character_id: "c".into(),
        role,
        account_role: role,
        perms: vec![],
    });
    (world, room, player, rx)
}

fn item(
    world: &mut World,
    holder: Entity,
    id: i32,
    kind: ObjectType,
    name: &str,
    keyword: &str,
) -> Entity {
    let mut proto = object_proto(1, id, kind);
    proto.name = name.into();
    proto.keywords = vec![keyword.into()];
    proto.examine_description = Some("A plain thing.".into());
    world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .insert((1, id), proto);
    world
        .spawn((
            Item,
            Named { name: name.into() },
            Keywords(vec![keyword.into()]),
            Description("A plain thing.".into()),
            WorldKey { zone: 1, id },
            Located(holder),
        ))
        .id()
}

fn name(world: &World, e: Entity) -> String {
    world.get::<Named>(e).unwrap().name.clone()
}

#[test]
fn nameitem_renames_a_container_and_its_words_target_it() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    let sack = item(
        &mut world,
        p,
        1,
        ObjectType::Container,
        "a cloth sack",
        "sack",
    );
    let other = item(
        &mut world,
        p,
        2,
        ObjectType::Container,
        "a cloth sack",
        "sack",
    );
    dispatch(&mut world, p, "nameitem 2.sack Daedela's cloth sack");
    let out = drain(&mut rx);
    assert!(
        out.contains("You name a cloth sack \"Daedela's cloth sack\"."),
        "{out}"
    );
    // `2.sack` is the older one in newest-first order, i.e. `sack`.
    assert_eq!(name(&world, sack), "Daedela's cloth sack");
    assert_eq!(name(&world, other), "a cloth sack");
    assert!(
        world.get::<PendingSave>(p).is_some(),
        "holder marked for save"
    );
    // The custom name's words join the keywords, the proto's stay.
    let kw = &world.get::<Keywords>(sack).unwrap().0;
    assert!(kw.contains(&"sack".to_string()) && kw.contains(&"daedela's".to_string()));
    // Inventory shows it and the name targets it alone.
    dispatch(&mut world, p, "inventory");
    assert!(drain(&mut rx).contains("Daedela's cloth sack"));
    dispatch(&mut world, p, "nameitem daed Gem bag");
    assert_eq!(name(&world, sack), "Gem bag");
    assert_eq!(name(&world, other), "a cloth sack");
}

#[test]
fn nameitem_only_names_containers() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    let sword = item(
        &mut world,
        p,
        1,
        ObjectType::Weapon,
        "a rusty sword",
        "sword",
    );
    dispatch(&mut world, p, "nameitem sword Excalibur");
    let out = drain(&mut rx);
    assert!(out.contains("You can only name containers"), "{out}");
    assert_eq!(name(&world, sword), "a rusty sword");
    assert!(world.get::<ItemCustomization>(sword).is_none());
}

#[test]
fn nameitem_strips_colour_and_control_codes_and_enforces_length() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    let sack = item(
        &mut world,
        p,
        1,
        ObjectType::Container,
        "a cloth sack",
        "sack",
    );
    dispatch(
        &mut world,
        p,
        "nameitem sack <red>Red</> \x1b[31mbag\x07 <b>",
    );
    let _ = drain(&mut rx);
    assert_eq!(name(&world, sack), "Red bag");
    dispatch(&mut world, p, "nameitem red ab");
    dispatch(&mut world, p, &format!("nameitem red {}", "x".repeat(41)));
    let out = drain(&mut rx);
    assert!(
        out.contains("too short") && out.contains("too long"),
        "{out}"
    );
    assert_eq!(name(&world, sack), "Red bag");
}

#[test]
fn sanitize_player_name_rules() {
    assert_eq!(
        item_custom::sanitize_player_name("  <red>Mira's</>   gem\x1b bag  ").unwrap(),
        "Mira's gem bag"
    );
    assert!(item_custom::sanitize_player_name("ab").is_err());
    assert!(item_custom::sanitize_player_name("<red></>").is_err());
    assert!(item_custom::sanitize_player_name(&"x".repeat(41)).is_err());
    assert!(item_custom::sanitize_player_name(&"x".repeat(40)).is_ok());
    // No markup can survive.
    let n = item_custom::sanitize_player_name("a<b>c&d@e%f{g}|h").unwrap();
    assert!(!n.contains(['<', '>', '&', '@', '%', '{', '}', '|']), "{n}");
}

#[test]
fn nameitem_clear_restores_the_prototype_name() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    let sack = item(
        &mut world,
        p,
        1,
        ObjectType::Container,
        "a cloth sack",
        "sack",
    );
    dispatch(&mut world, p, "nameitem sack Mira's bag");
    dispatch(&mut world, p, "nameitem mira clear");
    let _ = drain(&mut rx);
    assert_eq!(name(&world, sack), "a cloth sack");
    assert_eq!(
        world.get::<Keywords>(sack).unwrap().0,
        vec!["sack".to_string()]
    );
    let c = world.get::<ItemCustomization>(sack).unwrap();
    assert!(
        c.name.is_none() && c.dirty,
        "cleared override is dirty so the save clears the row"
    );
}

#[test]
fn customization_round_trips_through_the_row_loader() {
    use mud_db::character_items::CharacterItemRow;
    let (mut world, _room, p, _rx) = setup(UserRole::Player);
    world.insert_resource(mud_world::TriggerCatalog::default());
    let sack = item(
        &mut world,
        p,
        1,
        ObjectType::Container,
        "a cloth sack",
        "sack",
    );
    item_custom::edit(&mut world, sack, |c| {
        c.name = Some("Daedela's cloth sack".into());
        c.examine = Some("Stitched with care.".into());
        c.keywords = Some(vec!["sack".into(), "gems".into()]);
    });
    let c = world.get::<ItemCustomization>(sack).unwrap().clone();

    let (mut world2, _room2, p2, _rx2) = setup(UserRole::Player);
    world2.insert_resource(mud_world::TriggerCatalog::default());
    *world2.resource_mut::<ObjectPrototypes>() = ObjectPrototypes {
        by_key: world.resource::<ObjectPrototypes>().by_key.clone(),
    };
    let rows = vec![CharacterItemRow {
        id: 5,
        character_id: "c".into(),
        object_zone_id: 1,
        object_id: 1,
        container_id: None,
        equipped_location: None,
        charges: -1,
        liquid_remaining: 0,
        liquid_type: None,
        lit: false,
        custom_name: c.name.clone(),
        custom_examine_description: c.examine.clone(),
        custom_keywords: c.keywords.clone(),
    }];
    assert_eq!(crate::login::spawn_inventory(&mut world2, p2, &rows), 1);
    let loaded = world2
        .query_filtered::<Entity, With<Item>>()
        .iter(&world2)
        .next()
        .unwrap();
    assert_eq!(name(&world2, loaded), "Daedela's cloth sack");
    assert_eq!(
        world2.get::<Description>(loaded).unwrap().0,
        "Stitched with care."
    );
    let kw = &world2.get::<Keywords>(loaded).unwrap().0;
    assert!(kw.contains(&"gems".to_string()) && kw.contains(&"daedela's".to_string()));
    let loaded_c = world2.get::<ItemCustomization>(loaded).unwrap();
    assert!(
        !loaded_c.dirty,
        "freshly loaded customization must not overwrite the row"
    );
}
