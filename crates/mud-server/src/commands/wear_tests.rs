//! `wear` parity (issues #71, #72): `wear all`, paired anatomy filling the
//! second side, and `wear <item> <where>` body keywords. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole, WearFlag};
use mud_world::{
    Account, EquippedSlot, Exits, Item, Keywords, Located, Named, ObjectPrototypes, Room, Slot,
    WearableIn, WorldKey, wear_flags_primary_slot,
};

use super::dispatch;
use super::test_support::{Rx, drain, object_proto, player_in};

fn setup() -> (World, Entity, Rx) {
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
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    });
    (world, player, rx)
}

/// Carry a piece of gear whose prototype has `flags`.
fn gear(
    world: &mut World,
    holder: Entity,
    id: i32,
    name: &str,
    keyword: &str,
    flags: &[WearFlag],
) -> Entity {
    let mut proto = object_proto(1, id, ObjectType::Armor);
    proto.name = name.into();
    proto.keywords = vec![keyword.into()];
    proto.wear_flags = flags.to_vec();
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

fn worn_in(world: &World, item: Entity) -> Option<Slot> {
    world.get::<EquippedSlot>(item).map(|e| e.0)
}

#[test]
fn wear_all_equips_every_wearable_item_with_legacy_messages() {
    let (mut world, p, mut rx) = setup();
    // Display names carry an article; `wear all` must not look items up
    // by that name.
    let hat = gear(&mut world, p, 1, "a felt hat", "hat", &[WearFlag::Head]);
    let boots = gear(
        &mut world,
        p,
        2,
        "a pair of boots",
        "boots",
        &[WearFlag::Feet],
    );
    let rock = gear(&mut world, p, 3, "a grey rock", "rock", &[]);
    dispatch(&mut world, p, "wear all");
    let out = drain(&mut rx);
    assert_eq!(worn_in(&world, hat), Some(Slot::Head), "{out}");
    assert_eq!(worn_in(&world, boots), Some(Slot::Feet), "{out}");
    assert_eq!(worn_in(&world, rock), None);
    assert!(out.contains("You wear a felt hat on your head."), "{out}");
    assert!(
        out.contains("You wear a pair of boots on your feet."),
        "{out}"
    );
    assert!(!out.contains("aren't carrying"), "{out}");
}

#[test]
fn wear_all_skips_blocked_items_silently_and_reports_nothing_wearable() {
    let (mut world, p, mut rx) = setup();
    let hat = gear(&mut world, p, 1, "a felt hat", "hat", &[WearFlag::Head]);
    world.entity_mut(hat).insert(EquippedSlot(Slot::Head));
    let cap = gear(&mut world, p, 2, "a wool cap", "cap", &[WearFlag::Head]);
    let _rock = gear(&mut world, p, 3, "a grey rock", "rock", &[]);
    dispatch(&mut world, p, "wear all");
    let out = drain(&mut rx);
    assert_eq!(worn_in(&world, cap), None);
    assert!(
        out.contains("You don't have anything you can wear."),
        "{out}"
    );
    assert!(!out.contains("already occupied"), "{out}");
}

#[test]
fn wear_all_falls_through_to_the_second_side_of_a_pair() {
    let (mut world, p, mut rx) = setup();
    let a = gear(
        &mut world,
        p,
        1,
        "a jade earring",
        "earring",
        &[WearFlag::Ear],
    );
    let b = gear(
        &mut world,
        p,
        2,
        "a ruby earring",
        "earring",
        &[WearFlag::Ear],
    );
    dispatch(&mut world, p, "wear all");
    let _ = drain(&mut rx);
    let mut slots = [worn_in(&world, a), worn_in(&world, b)];
    slots.sort_by_key(|s| s.map(Slot::db_label));
    assert_eq!(slots, [Some(Slot::LeftEar), Some(Slot::RightEar)]);
}

#[test]
fn second_wrist_and_ear_item_fill_the_other_side() {
    let (mut world, p, mut rx) = setup();
    let w1 = gear(
        &mut world,
        p,
        1,
        "a silver bracelet",
        "silver",
        &[WearFlag::Wrist],
    );
    let w2 = gear(
        &mut world,
        p,
        2,
        "a gold bracelet",
        "gold",
        &[WearFlag::Wrist],
    );
    let e1 = gear(&mut world, p, 3, "a jade earring", "jade", &[WearFlag::Ear]);
    let e2 = gear(&mut world, p, 4, "a ruby earring", "ruby", &[WearFlag::Ear]);
    for kw in ["silver", "gold", "jade", "ruby"] {
        dispatch(&mut world, p, &format!("wear {kw}"));
    }
    let out = drain(&mut rx);
    assert_eq!(worn_in(&world, w1), Some(Slot::RightWrist), "{out}");
    assert_eq!(worn_in(&world, w2), Some(Slot::LeftWrist), "{out}");
    assert_eq!(worn_in(&world, e1), Some(Slot::LeftEar), "{out}");
    assert_eq!(worn_in(&world, e2), Some(Slot::RightEar), "{out}");
    assert!(out.contains("on your right wrist"), "{out}");
    assert!(out.contains("in your right ear"), "{out}");
}

#[test]
fn a_third_wrist_item_reports_both_wrists_taken() {
    let (mut world, p, mut rx) = setup();
    for (i, kw) in ["a", "b", "c"].iter().enumerate() {
        gear(
            &mut world,
            p,
            i32::try_from(i).unwrap() + 1,
            &format!("bracelet {kw}"),
            kw,
            &[WearFlag::Wrist],
        );
    }
    for kw in ["a", "b", "c"] {
        dispatch(&mut world, p, &format!("wear {kw}"));
    }
    let out = drain(&mut rx);
    assert!(
        out.contains("Both of your wrists are already occupied"),
        "{out}"
    );
}

#[test]
fn explicit_body_keyword_picks_the_position() {
    let (mut world, p, mut rx) = setup();
    // Cloak pin: wearable about the body or as a badge; bare `wear`
    // takes the higher-priority position (badge), `about` is explicit.
    let pin = gear(
        &mut world,
        p,
        1,
        "a nexus cloak pin",
        "nexus",
        &[WearFlag::About, WearFlag::Badge],
    );
    dispatch(&mut world, p, "wear nexus about");
    let out = drain(&mut rx);
    assert_eq!(worn_in(&world, pin), Some(Slot::About), "{out}");
    world.entity_mut(pin).remove::<EquippedSlot>();
    dispatch(&mut world, p, "wear nexus badge");
    let out = drain(&mut rx);
    assert_eq!(worn_in(&world, pin), Some(Slot::Badge), "{out}");
    assert!(
        out.contains("You wear a nexus cloak pin as a badge."),
        "{out}"
    );
}

#[test]
fn explicit_keyword_the_item_cannot_use_is_refused() {
    let (mut world, p, mut rx) = setup();
    let hat = gear(&mut world, p, 1, "a felt hat", "hat", &[WearFlag::Head]);
    dispatch(&mut world, p, "wear hat feet");
    let out = drain(&mut rx);
    assert_eq!(worn_in(&world, hat), None);
    assert!(out.contains("You can't wear a felt hat there."), "{out}");
}

#[test]
fn multi_word_item_names_still_work_without_a_keyword() {
    let (mut world, p, mut rx) = setup();
    let ring = gear(
        &mut world,
        p,
        1,
        "a gold ring",
        "gold ring",
        &[WearFlag::Finger],
    );
    dispatch(&mut world, p, "wear gold ring");
    let out = drain(&mut rx);
    assert_eq!(worn_in(&world, ring), Some(Slot::RightFinger), "{out}");
    assert!(out.contains("onto your right ring finger"), "{out}");
}

#[test]
fn bare_wear_uses_legacy_flag_priority() {
    assert_eq!(
        wear_flags_primary_slot(&[WearFlag::Neck, WearFlag::Badge]),
        Some(Slot::Badge)
    );
    assert_eq!(
        wear_flags_primary_slot(&[WearFlag::Finger, WearFlag::Body]),
        Some(Slot::Body)
    );
}
