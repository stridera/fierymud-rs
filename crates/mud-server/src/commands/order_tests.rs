//! Listing order (issue #56): legacy `obj_to_room` / `obj_to_char` /
//! `obj_to_obj` / `char_to_room` push onto the HEAD of the linked list, so
//! the most recent arrival is listed first in room contents, inventory and
//! container contents. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole};
use mud_world::{
    Account, Corpse, Exits, Item, Keywords, Located, Mob, Named, ObjectPrototypes, Room, WorldKey,
};

use super::dispatch;
use super::test_support::{Rx, drain, object_proto, player_in};

fn setup() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(ObjectPrototypes::default());
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
    (world, room, player, rx)
}

fn spawn_item(world: &mut World, holder: Entity, name: &str, keyword: &str, id: i32) -> Entity {
    world
        .spawn((
            Item,
            Named { name: name.into() },
            Keywords(vec![keyword.into()]),
            WorldKey { zone: 1, id },
            Located(holder),
        ))
        .id()
}

fn spawn_mob(world: &mut World, room: Entity, name: &str) -> Entity {
    world
        .spawn((Mob, Named { name: name.into() }, Located(room)))
        .id()
}

/// Offsets of `names` in the visible text (GMCP frames skipped; room
/// items share one comma-joined line, so offsets, not lines); panics when
/// one is missing so a bad listing fails loudly.
fn positions(out: &str, names: &[&str]) -> Vec<usize> {
    let text: String = out
        .lines()
        .filter(|l| !l.contains("Char.") && !l.contains("Room."))
        .collect::<Vec<_>>()
        .join("\n");
    names
        .iter()
        .map(|n| {
            text.find(n)
                .unwrap_or_else(|| panic!("{n} missing from:\n{out}"))
        })
        .collect()
}

fn assert_in_order(out: &str, names: &[&str]) {
    let pos = positions(out, names);
    assert!(
        pos.windows(2).all(|w| w[0] < w[1]),
        "expected order {names:?}, got offsets {pos:?} in:\n{out}"
    );
}

#[test]
fn dropped_items_list_newest_first_and_get_reverses_into_inventory() {
    let (mut world, _room, p, mut rx) = setup();
    spawn_item(&mut world, p, "a rusty sword", "sword", 1);
    spawn_item(&mut world, p, "a wooden shield", "shield", 2);

    dispatch(&mut world, p, "drop sword");
    dispatch(&mut world, p, "drop shield");
    let _ = drain(&mut rx);
    dispatch(&mut world, p, "look");
    let out = drain(&mut rx);
    assert_in_order(&out, &["wooden shield", "rusty sword"]);

    // `get sword` then `get shield`: the shield is the newest arrival.
    dispatch(&mut world, p, "get sword");
    dispatch(&mut world, p, "get shield");
    let _ = drain(&mut rx);
    dispatch(&mut world, p, "inventory");
    let out = drain(&mut rx);
    assert_in_order(&out, &["wooden shield", "rusty sword"]);
}

#[test]
fn drop_then_get_the_same_item_puts_it_first_in_inventory() {
    let (mut world, _room, p, mut rx) = setup();
    spawn_item(&mut world, p, "a rusty sword", "sword", 1);
    spawn_item(&mut world, p, "a wooden shield", "shield", 2);
    // Inventory spawn order sword, shield -> listing shield, sword.
    dispatch(&mut world, p, "inventory");
    assert_in_order(&drain(&mut rx), &["wooden shield", "rusty sword"]);

    dispatch(&mut world, p, "drop sword");
    dispatch(&mut world, p, "get sword");
    let _ = drain(&mut rx);
    dispatch(&mut world, p, "inventory");
    assert_in_order(&drain(&mut rx), &["rusty sword", "wooden shield"]);
}

#[test]
fn room_mobs_list_newest_arrival_first() {
    let (mut world, room, p, mut rx) = setup();
    spawn_mob(&mut world, room, "a first rat");
    spawn_mob(&mut world, room, "a second rat");
    spawn_mob(&mut world, room, "a third rat");
    dispatch(&mut world, p, "look");
    assert_in_order(
        &drain(&mut rx),
        &["a third rat", "a second rat", "a first rat"],
    );
}

#[test]
fn stacked_group_sits_where_its_newest_member_is() {
    let (mut world, room, p, mut rx) = setup();
    spawn_item(&mut world, room, "a copper coin", "coin", 1);
    spawn_item(&mut world, room, "a rusty sword", "sword", 2);
    spawn_item(&mut world, room, "a copper coin", "coin", 1);
    // Room order newest-first: coin, sword, coin -> (2) coin, sword.
    dispatch(&mut world, p, "look");
    let out = drain(&mut rx);
    assert_in_order(&out, &["a copper coin", "a rusty sword"]);
    assert!(out.contains("(2)"), "{out}");

    // A newer sword makes the sword group lead instead.
    spawn_item(&mut world, room, "a rusty sword", "sword", 2);
    dispatch(&mut world, p, "look");
    let out = drain(&mut rx);
    assert_in_order(&out, &["a rusty sword", "a copper coin"]);
}

#[test]
fn container_contents_list_newest_first() {
    let (mut world, room, p, mut rx) = setup();
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
    spawn_item(&mut world, corpse, "a rat tail", "tail", 1);
    spawn_item(&mut world, corpse, "a rat fang", "fang", 2);
    dispatch(&mut world, p, "look in corpse");
    assert_in_order(&drain(&mut rx), &["a rat fang", "a rat tail"]);
}

#[test]
fn indexed_target_matches_the_displayed_order() {
    let (mut world, room, _p, _rx) = setup();
    let old = spawn_item(&mut world, room, "a rusty sword", "sword", 1);
    let new = spawn_item(&mut world, room, "a rusty sword", "sword", 1);
    assert_eq!(super::find_in_room(&mut world, "sword", room), Some(new));
    assert_eq!(super::find_in_room(&mut world, "2.sword", room), Some(old));
}

#[test]
fn get_all_and_drop_all_sweep_newest_first_like_legacy() {
    let (mut world, room, p, mut rx) = setup();
    spawn_item(&mut world, room, "a rusty sword", "sword", 1);
    spawn_item(&mut world, room, "a wooden shield", "shield", 2);
    // Floor lists shield, sword; `get all` takes the shield first, so the
    // sword ends up the newest arrival in the inventory.
    dispatch(&mut world, p, "get all");
    let _ = drain(&mut rx);
    dispatch(&mut world, p, "inventory");
    assert_in_order(&drain(&mut rx), &["rusty sword", "wooden shield"]);
    // ...and `drop all` reverses it again.
    dispatch(&mut world, p, "drop all");
    let _ = drain(&mut rx);
    dispatch(&mut world, p, "look");
    assert_in_order(&drain(&mut rx), &["wooden shield", "rusty sword"]);
}

/// The save path walks `Contents` oldest-first and stamps rows with an
/// increasing `updated_at`; `list_for` returns them in that order. This
/// replays that (with deliberately scrambled row ids, as after a
/// drop/get cycle) through the real loader and checks the reloaded
/// inventory lists exactly like the live one did.
#[test]
fn inventory_order_survives_a_save_load_round_trip() {
    use mud_db::character_items::CharacterItemRow;

    let (mut world, _room, p, mut rx) = setup();
    world.insert_resource(mud_world::TriggerCatalog::default());
    for (id, name) in [
        (1, "a rusty sword"),
        (2, "a wooden shield"),
        (3, "a red potion"),
    ] {
        let mut proto = object_proto(1, id, ObjectType::Other);
        proto.name = name.to_string();
        proto.keywords = vec![name.rsplit(' ').next().unwrap().to_string()];
        world
            .resource_mut::<ObjectPrototypes>()
            .by_key
            .insert((1, id), proto);
    }
    // Arrival order: sword, shield, potion; then the sword is dropped and
    // picked up again, so it is the newest.
    spawn_item(&mut world, p, "a rusty sword", "sword", 1);
    spawn_item(&mut world, p, "a wooden shield", "shield", 2);
    spawn_item(&mut world, p, "a red potion", "potion", 3);
    dispatch(&mut world, p, "drop sword");
    dispatch(&mut world, p, "get sword");
    let _ = drain(&mut rx);
    dispatch(&mut world, p, "inventory");
    let before = drain(&mut rx);
    assert_in_order(&before, &["rusty sword", "red potion", "wooden shield"]);

    // "Save": Contents order (oldest first), ids intentionally descending
    // so an `ORDER BY id` load would reverse everything.
    let saved: Vec<WorldKey> = world
        .get::<mud_world::Contents>(p)
        .unwrap()
        .iter()
        .filter_map(|e| world.get::<WorldKey>(e).copied())
        .collect();
    let rows: Vec<CharacterItemRow> = saved
        .iter()
        .enumerate()
        .map(|(i, wk)| CharacterItemRow {
            id: 1000 - i32::try_from(i).unwrap(),
            character_id: "c".into(),
            object_zone_id: wk.zone,
            object_id: wk.id,
            container_id: None,
            equipped_location: None,
            charges: -1,
            liquid_remaining: 0,
            liquid_type: None,
            lit: false,
            custom_name: None,
            custom_examine_description: None,
            custom_keywords: None,
        })
        .collect();

    // "Load" into a fresh player.
    let (mut world2, _room2, p2, mut rx2) = setup();
    world2.insert_resource(mud_world::TriggerCatalog::default());
    world2.insert_resource(mud_world::ObjectAbilityCatalog::default());
    *world2.resource_mut::<ObjectPrototypes>() = ObjectPrototypes {
        by_key: world.resource::<ObjectPrototypes>().by_key.clone(),
    };
    assert_eq!(crate::login::spawn_inventory(&mut world2, p2, &rows), 3);
    dispatch(&mut world2, p2, "inventory");
    let after = drain(&mut rx2);
    assert_in_order(&after, &["rusty sword", "red potion", "wooden shield"]);
}

#[test]
fn two_saved_ears_rows_load_into_left_and_right_ear() {
    use mud_db::character_items::CharacterItemRow;
    let (mut world, _room, p, _rx) = setup();
    world.insert_resource(mud_world::TriggerCatalog::default());
    world.insert_resource(mud_world::ObjectAbilityCatalog::default());
    let mut protos = ObjectPrototypes::default();
    for id in [1, 2, 3] {
        protos
            .by_key
            .insert((1, id), object_proto(1, id, ObjectType::Other));
    }
    *world.resource_mut::<ObjectPrototypes>() = protos;
    let row = |id: i32, obj: i32, loc: &str| CharacterItemRow {
        id,
        character_id: "c".into(),
        object_zone_id: 1,
        object_id: obj,
        container_id: None,
        equipped_location: Some(loc.into()),
        charges: -1,
        liquid_remaining: 0,
        liquid_type: None,
        lit: false,
        custom_name: None,
        custom_examine_description: None,
        custom_keywords: None,
    };
    let rows = vec![row(1, 1, "EARS"), row(2, 2, "EARS"), row(3, 3, "EARS")];
    assert_eq!(crate::login::spawn_inventory(&mut world, p, &rows), 3);
    let mut q = world.query_filtered::<(&WorldKey, &Located, Option<&mud_world::EquippedSlot>), With<mud_world::Item>>();
    let mut got: Vec<(i32, Option<mud_world::Slot>)> = q
        .iter(&world)
        .filter(|(_, l, _)| l.0 == p)
        .map(|(k, _, s)| (k.id, s.map(|s| s.0)))
        .collect();
    got.sort_by_key(|(id, _)| *id);
    // Third row: both ears taken, keeps the label's slot (still on the
    // character, not dropped).
    assert_eq!(
        got,
        vec![
            (1, Some(mud_world::Slot::LeftEar)),
            (2, Some(mud_world::Slot::RightEar)),
            (3, Some(mud_world::Slot::LeftEar)),
        ]
    );
}

#[test]
fn indexed_actor_target_matches_look_mobs_before_players() {
    let (mut world, room, p, mut rx) = setup();
    let other = world
        .spawn((
            mud_world::Player,
            Named {
                name: "Bobby".into(),
            },
            Located(room),
        ))
        .id();
    // The mob arrives after the player, so a single newest-first ranking
    // would agree by accident; spawn a second, newer player so it would
    // not: `look` renders the mob lines, then "Also here:" players.
    let mob = spawn_mob(&mut world, room, "a bobby rat");
    let newer = world
        .spawn((
            mud_world::Player,
            Named {
                name: "Bobbo".into(),
            },
            Located(room),
        ))
        .id();
    dispatch(&mut world, p, "look");
    assert_in_order(&drain(&mut rx), &["a bobby rat", "Bobbo, Bobby"]);
    let find = |w: &mut World, n: &str| super::find_actor_in_room(w, n, room, p);
    assert_eq!(find(&mut world, "bob"), Some(mob));
    assert_eq!(find(&mut world, "2.bob"), Some(newer));
    assert_eq!(find(&mut world, "3.bob"), Some(other));
}

#[test]
fn indexed_worn_target_follows_equipment_slot_order() {
    use mud_world::{EquippedSlot, Slot};
    let (mut world, _room, p, mut rx) = setup();
    let left = spawn_item(&mut world, p, "a gold ring", "ring", 1);
    let right = spawn_item(&mut world, p, "a silver ring", "ring", 2);
    world
        .entity_mut(left)
        .insert(EquippedSlot(Slot::LeftFinger));
    world
        .entity_mut(right)
        .insert(EquippedSlot(Slot::RightFinger));
    // `equipment` lists slot order (left finger, then right finger) even
    // though the right ring arrived later.
    dispatch(&mut world, p, "equipment");
    assert_in_order(&drain(&mut rx), &["gold ring", "silver ring"]);
    let eq = super::EquipFilter::Equipped;
    assert_eq!(
        super::find_carried_by(&mut world, "ring", p, eq),
        Some(left)
    );
    assert_eq!(
        super::find_carried_by(&mut world, "2.ring", p, eq),
        Some(right)
    );

    dispatch(&mut world, p, "remove 2.ring");
    let _ = drain(&mut rx);
    assert!(world.get::<EquippedSlot>(left).is_some());
    assert!(world.get::<EquippedSlot>(right).is_none());
}

#[test]
fn anywhere_search_checks_equipment_before_inventory_like_generic_find() {
    use mud_world::{EquippedSlot, Slot};
    let (mut world, _room, p, _rx) = setup();
    let packed = spawn_item(&mut world, p, "a brass ring", "ring", 1);
    let worn = spawn_item(&mut world, p, "a gold ring", "ring", 2);
    world
        .entity_mut(worn)
        .insert(EquippedSlot(Slot::LeftFinger));
    let any = super::EquipFilter::Anywhere;
    assert_eq!(
        super::find_carried_by(&mut world, "ring", p, any),
        Some(worn)
    );
    // The counter restarts per list (legacy copies the find context), so
    // a second ring is looked for in the pack, not past the worn one.
    assert_eq!(super::find_carried_by(&mut world, "2.ring", p, any), None);
    assert_eq!(
        super::find_carried_by(&mut world, "1.ring", p, super::EquipFilter::Inventory),
        Some(packed)
    );
}

#[test]
fn examine_indexed_actor_matches_look_mobs_before_players() {
    let (mut world, room, p, mut rx) = setup();
    let spawn_player = |w: &mut World, name: &str| {
        w.spawn((
            mud_world::Player,
            Named { name: name.into() },
            Located(room),
        ))
        .id()
    };
    spawn_player(&mut world, "Bobby");
    spawn_mob(&mut world, room, "a bobby rat");
    spawn_player(&mut world, "Bobbo");
    let _ = drain(&mut rx);
    // Mobs first, then players newest first: bobby rat, Bobbo, Bobby.
    dispatch(&mut world, p, "examine bob");
    let first = drain(&mut rx);
    assert!(first.contains("bobby rat"), "{first}");
    dispatch(&mut world, p, "examine 2.bob");
    let second = drain(&mut rx);
    assert!(second.contains("Bobbo"), "{second}");
    assert!(!second.contains("bobby rat"), "{second}");
    dispatch(&mut world, p, "examine 3.bob");
    let third = drain(&mut rx);
    assert!(third.contains("Bobby"), "{third}");
    assert!(!third.contains("Bobbo"), "{third}");
}

#[test]
fn remove_all_strips_in_equipment_slot_order() {
    use mud_world::{EquippedSlot, Slot};
    let (mut world, _room, p, mut rx) = setup();
    // Spawn in reverse slot order so query / arrival order differs.
    let right = spawn_item(&mut world, p, "a silver ring", "ring", 2);
    let left = spawn_item(&mut world, p, "a gold ring", "ring", 1);
    world
        .entity_mut(right)
        .insert(EquippedSlot(Slot::RightFinger));
    world
        .entity_mut(left)
        .insert(EquippedSlot(Slot::LeftFinger));
    dispatch(&mut world, p, "remove all");
    assert_in_order(
        &drain(&mut rx),
        &["You remove a gold ring", "You remove a silver ring"],
    );
    assert!(world.get::<EquippedSlot>(left).is_none());
    assert!(world.get::<EquippedSlot>(right).is_none());
}
