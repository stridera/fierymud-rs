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
