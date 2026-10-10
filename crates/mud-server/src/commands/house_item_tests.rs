//! Items stored in a player house keep their per-instance state (label,
//! enchantment, curse) across a save/reload. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{Alignment, ObjectFlag, ObjectRestriction, ObjectType};
use mud_db::housing::HouseItemCustom;
use mud_world::components::{ItemApplies, ItemBarredAlignments};
use mud_world::{
    Item, ItemCustomization, Keywords, Located, Named, ObjectFlags, ObjectPrototypes,
    ObjectRestrictions, WorldKey,
};

use super::test_support::object_proto;
use super::{house_item_custom, spawn_house_item};

fn world_with_sword() -> World {
    let mut world = World::new();
    let mut protos = ObjectPrototypes::default();
    let mut proto = object_proto(30, 7, ObjectType::Weapon);
    proto.name = "a plain sword".into();
    proto.keywords = vec!["sword".into()];
    proto.restrictions = vec![ObjectRestriction::NoSell];
    protos.by_key.insert((30, 7), proto);
    world.insert_resource(protos);
    world
}

/// A sword that has been enchanted, cursed and labeled in play.
fn enchanted_sword(world: &mut World) -> Entity {
    let item = world
        .spawn((
            Item,
            Named {
                name: "a plain sword".into(),
            },
            Keywords(vec!["sword".into()]),
            WorldKey { zone: 30, id: 7 },
            ObjectRestrictions(vec![ObjectRestriction::NoSell, ObjectRestriction::NoDrop]),
            ItemApplies(vec![("accuracy".into(), 2), ("attack_power".into(), 5)]),
            ItemBarredAlignments(vec![Alignment::Evil]),
        ))
        .id();
    crate::item_alter::add_flag(world, item, ObjectFlag::Magic);
    crate::item_alter::mark_dirty(world, item);
    crate::item_custom::install(
        world,
        item,
        ItemCustomization {
            name: Some("a plain sword (Grim)".into()),
            examine: Some("It hums.".into()),
            keywords: Some(vec!["sword".into(), "grim".into()]),
            dirty: true,
        },
    );
    item
}

#[test]
fn stored_columns_round_trip_the_instance_state() {
    let mut world = world_with_sword();
    let item = enchanted_sword(&mut world);
    let custom = house_item_custom(&world, item);
    assert_eq!(custom.name.as_deref(), Some("a plain sword (Grim)"));
    let alter = custom.alter.clone().expect("alter captured");
    assert_eq!(alter.applies.len(), 2);

    // What the DB row holds: two text columns and the JSON.
    let back = HouseItemCustom::from_columns(
        custom.name.clone(),
        custom.examine.clone(),
        &custom.values(),
    );
    assert_eq!(back, custom);
}

#[test]
fn enchanted_item_reloaded_from_the_house_keeps_its_enchant_once() {
    let mut world = world_with_sword();
    let item = enchanted_sword(&mut world);
    let custom = house_item_custom(&world, item);
    let before = crate::item_alter::snapshot(&world, item);

    // A fresh world is the "reload": only the prototype plus the stored row.
    let mut fresh = world_with_sword();
    let room = fresh.spawn_empty().id();
    spawn_house_item(&mut fresh, 11, 30, 7, room, &custom);
    let reloaded = {
        let mut q = fresh.query_filtered::<Entity, With<mud_world::HouseItem>>();
        q.single(&fresh).unwrap()
    };

    assert_eq!(fresh.get::<Located>(reloaded).unwrap().0, room);
    assert_eq!(
        fresh.get::<Named>(reloaded).unwrap().name,
        "a plain sword (Grim)"
    );
    let applies = &fresh.get::<ItemApplies>(reloaded).unwrap().0;
    assert_eq!(
        applies,
        &vec![("accuracy".to_string(), 2), ("attack_power".to_string(), 5)],
        "enchant applied once, not doubled"
    );
    let flags = &fresh.get::<ObjectFlags>(reloaded).unwrap().0;
    assert_eq!(
        flags.iter().filter(|f| **f == ObjectFlag::Magic).count(),
        1,
        "{flags:?}"
    );
    assert_eq!(
        fresh.get::<ItemBarredAlignments>(reloaded).unwrap().0,
        vec![Alignment::Evil]
    );
    let r = &fresh.get::<ObjectRestrictions>(reloaded).unwrap().0;
    assert!(r.contains(&ObjectRestriction::NoDrop) && r.contains(&ObjectRestriction::NoSell));
    assert_eq!(crate::item_alter::snapshot(&fresh, reloaded), before);

    // Stored again and reloaded again: still the same single enchant.
    let again = house_item_custom(&fresh, reloaded);
    let mut third = world_with_sword();
    let room3 = third.spawn_empty().id();
    spawn_house_item(&mut third, 12, 30, 7, room3, &again);
    let e3 = {
        let mut q = third.query_filtered::<Entity, With<mud_world::HouseItem>>();
        q.single(&third).unwrap()
    };
    assert_eq!(crate::item_alter::snapshot(&third, e3), before);
}

#[test]
fn a_plain_item_has_no_stored_state() {
    let mut world = world_with_sword();
    let item = world
        .spawn((
            Item,
            Named {
                name: "a plain sword".into(),
            },
            WorldKey { zone: 30, id: 7 },
            ObjectRestrictions(vec![ObjectRestriction::NoSell]),
        ))
        .id();
    let custom = house_item_custom(&world, item);
    assert_eq!(custom, HouseItemCustom::default());
    assert_eq!(custom.values(), serde_json::json!({}));
}

// ---------------------------------------------------------------------------
// Pickups release the house row (the duplication exploit)
// ---------------------------------------------------------------------------

use mud_world::{HouseItem, HousePlacement, HouseRoom, Room};

use super::info::{cmd_get, cmd_house_place, cmd_house_take};
use super::test_support::{drain, player_in};
use crate::house_items::Detached;

/// A house room, an owner standing in it, a world with the observer
/// registered. Returns `(world, room, owner)`.
fn house_world() -> (World, Entity, Entity, super::test_support::Rx) {
    let mut world = world_with_sword();
    crate::house_items::register_observers(&mut world);
    let room = world
        .spawn((
            Room,
            HouseRoom {
                house_id: 1,
                local_index: 0,
            },
        ))
        .id();
    let (owner, rx) = player_in(&mut world, room);
    (world, room, owner, rx)
}

fn placed_sword(world: &mut World, room: Entity, marker: impl Bundle) -> Entity {
    world
        .spawn((
            Item,
            Named {
                name: "a plain sword".into(),
            },
            Keywords(vec!["sword".into()]),
            WorldKey { zone: 30, id: 7 },
            Located(room),
            marker,
        ))
        .id()
}

#[test]
fn plain_get_of_a_loaded_house_item_releases_it() {
    let (mut world, room, owner, mut rx) = house_world();
    let sword = placed_sword(&mut world, room, HouseItem(11));
    cmd_get(&mut world, owner, "sword");
    let out = drain(&mut rx);
    assert!(out.contains("You pick up"), "{out}");
    assert_eq!(world.get::<Located>(sword).unwrap().0, owner);
    assert!(world.get::<HouseItem>(sword).is_none(), "row marker kept");
}

#[test]
fn get_all_releases_every_house_item_taken() {
    let (mut world, room, owner, _rx) = house_world();
    let a = placed_sword(&mut world, room, HouseItem(11));
    let placement = HousePlacement::pending();
    let b = placed_sword(&mut world, room, placement.clone());
    assert!(placement.settle(12));
    cmd_get(&mut world, owner, "all");
    for e in [a, b] {
        assert_eq!(world.get::<Located>(e).unwrap().0, owner);
        assert!(world.get::<HouseItem>(e).is_none());
        assert!(world.get::<HousePlacement>(e).is_none());
    }
    // The settled row was handed back for deletion exactly once.
    assert_eq!(placement.release(), None);
}

#[test]
fn a_guest_picking_up_releases_the_owners_row_too() {
    let (mut world, room, _owner, _rx) = house_world();
    let sword = placed_sword(&mut world, room, HouseItem(11));
    let (guest, mut grx) = player_in(&mut world, room);
    cmd_get(&mut world, guest, "sword");
    assert!(drain(&mut grx).contains("You pick up"));
    assert!(world.get::<HouseItem>(sword).is_none());
}

#[test]
fn an_item_still_in_the_house_keeps_its_marker() {
    let (mut world, room, _owner, _rx) = house_world();
    let sword = placed_sword(&mut world, room, HouseItem(11));
    // Re-homed within the house: not a pickup.
    let other = world
        .spawn((
            Room,
            HouseRoom {
                house_id: 1,
                local_index: 1,
            },
        ))
        .id();
    world.entity_mut(sword).insert(Located(other));
    assert!(world.get::<HouseItem>(sword).is_some());
}

#[test]
fn detach_hands_back_the_row_to_delete() {
    let (mut world, room, _owner, _rx) = house_world();
    let loaded = placed_sword(&mut world, room, HouseItem(5));
    assert_eq!(
        crate::house_items::detach_house_item(&mut world, loaded),
        Detached::Row(5)
    );
    assert_eq!(
        crate::house_items::detach_house_item(&mut world, loaded),
        Detached::NotHouseItem
    );

    // Insert still in flight: nothing to delete yet, the insert task cleans up.
    let placement = HousePlacement::pending();
    let fresh = placed_sword(&mut world, room, placement.clone());
    assert_eq!(
        crate::house_items::detach_house_item(&mut world, fresh),
        Detached::InFlight
    );
    assert!(!placement.settle(9), "insert must see the item was taken");

    // Insert already finished: the settled row is returned.
    let placement = HousePlacement::pending();
    let settled = placed_sword(&mut world, room, placement.clone());
    assert!(placement.settle(10));
    assert_eq!(
        crate::house_items::detach_house_item(&mut world, settled),
        Detached::Row(10)
    );
}

fn house_summary() -> mud_world::HouseSummary {
    mud_world::HouseSummary {
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
    }
}

#[test]
fn house_take_finds_an_item_placed_this_session() {
    let (mut world, room, owner, mut rx) = house_world();
    let sword = world
        .spawn((
            Item,
            Named {
                name: "a plain sword".into(),
            },
            Keywords(vec!["sword".into()]),
            WorldKey { zone: 30, id: 7 },
            Located(owner),
        ))
        .id();
    cmd_house_place(&mut world, owner, &house_summary(), "sword");
    assert_eq!(world.get::<Located>(sword).unwrap().0, room);
    assert!(world.get::<HousePlacement>(sword).is_some());
    drain(&mut rx);

    cmd_house_take(&mut world, owner, &house_summary(), "sword");
    let out = drain(&mut rx);
    assert!(out.contains("You take"), "{out}");
    assert_eq!(world.get::<Located>(sword).unwrap().0, owner);
    assert!(world.get::<HousePlacement>(sword).is_none());
}

#[test]
fn placing_then_getting_in_one_breath_leaves_nothing_tracked() {
    let (mut world, room, owner, mut rx) = house_world();
    let sword = world
        .spawn((
            Item,
            Named {
                name: "a plain sword".into(),
            },
            Keywords(vec!["sword".into()]),
            WorldKey { zone: 30, id: 7 },
            Located(owner),
            mud_world::PersistedItemId(41),
        ))
        .id();
    cmd_house_place(&mut world, owner, &house_summary(), "sword");
    assert_eq!(world.get::<Located>(sword).unwrap().0, room);
    // The pack-row stamp goes with the placement: the house row replaces it.
    assert!(world.get::<mud_world::PersistedItemId>(sword).is_none());
    let placement = world.get::<HousePlacement>(sword).unwrap().clone();
    cmd_get(&mut world, owner, "sword");
    drain(&mut rx);
    assert!(world.get::<HousePlacement>(sword).is_none());
    // The in-flight insert finds the slot released and must delete its row.
    assert!(!placement.settle(77));
}

// ---------------------------------------------------------------------------
// Live database: place, get, reload
// ---------------------------------------------------------------------------

struct HouseDb {
    _db_lock: tokio::sync::MutexGuard<'static, ()>,
    pool: mud_db::sqlx::PgPool,
    char_id: String,
    house_id: i32,
    foyer_id: i32,
}

async fn house_db() -> Option<HouseDb> {
    let db_lock = super::test_support::db_test_lock().await;
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
    let Ok(Ok(pool)) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        mud_db::connect_with(&url, super::test_support::db_test_pool_settings()),
    )
    .await
    else {
        eprintln!("skipping: dev database unavailable");
        return None;
    };
    let Ok(Some((rz, rid))) = mud_db::sqlx::query_as::<_, (i32, i32)>(
        "SELECT zone_id, id FROM \"Room\" ORDER BY zone_id, id LIMIT 1",
    )
    .fetch_optional(&pool)
    .await
    else {
        eprintln!("skipping: no Room rows");
        return None;
    };
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let char_id = format!("zz-house-c-{tag}");
    mud_db::sqlx::query("INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())")
        .bind(&char_id)
        .bind(format!("Zzhs{}", tag % 1_000_000_000_000))
        .execute(&pool)
        .await
        .unwrap();
    let (house_id, foyer_id) = mud_db::housing::create_house(&pool, &char_id, rz, rid)
        .await
        .unwrap();
    Some(HouseDb {
        _db_lock: db_lock,
        pool,
        char_id,
        house_id,
        foyer_id,
    })
}

impl HouseDb {
    async fn rows(&self) -> Vec<mud_db::housing::PlayerHouseItemRow> {
        mud_db::housing::items_for_house(&self.pool, self.house_id)
            .await
            .unwrap()
    }

    async fn pack_rows(&self) -> i64 {
        mud_db::sqlx::query_scalar(
            "SELECT COUNT(*) FROM \"CharacterItems\" WHERE character_id = $1",
        )
        .bind(&self.char_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn id_sequence(&self) -> i64 {
        mud_db::sqlx::query_scalar("SELECT last_value FROM player_house_items_id_seq")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// Poll until `done` or five seconds pass.
    async fn wait_for<F, Fut>(&self, mut done: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..100 {
            if done().await {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        false
    }

    fn summary(&self) -> mud_world::HouseSummary {
        let mut s = house_summary();
        s.house_id = self.house_id;
        s.rooms[0].id = self.foyer_id;
        s
    }

    /// The server's view of the house after a reboot: one spawned item per
    /// stored row.
    async fn reloaded_swords(&self) -> usize {
        let mut fresh = world_with_sword();
        let room = fresh.spawn_empty().id();
        for row in self.rows().await {
            fresh
                .resource_mut::<ObjectPrototypes>()
                .by_key
                .entry((row.object_zone_id, row.object_id))
                .or_insert_with(|| {
                    object_proto(row.object_zone_id, row.object_id, ObjectType::Weapon)
                });
            spawn_house_item(
                &mut fresh,
                row.id,
                row.object_zone_id,
                row.object_id,
                room,
                &row.custom(),
            );
        }
        fresh
            .query_filtered::<(), With<mud_world::HouseItem>>()
            .iter(&fresh)
            .count()
    }

    async fn end(self) {
        for sql in [
            "DELETE FROM player_houses WHERE character_id = $1",
            "DELETE FROM \"CharacterItems\" WHERE character_id = $1",
            "DELETE FROM \"Characters\" WHERE id = $1",
        ] {
            mud_db::sqlx::query(sql)
                .bind(&self.char_id)
                .execute(&self.pool)
                .await
                .unwrap();
        }
    }
}

fn owner_with_sword(db: &HouseDb) -> (World, Entity, Entity, super::test_support::Rx) {
    let (mut world, room, owner, rx) = house_world();
    world.insert_resource(super::DbPool(db.pool.clone()));
    world.insert_resource(crate::autosave::SaveCoordinator::default());
    world.entity_mut(room).insert(HouseRoom {
        house_id: db.house_id,
        local_index: 0,
    });
    world.entity_mut(owner).insert(mud_world::Account {
        user_id: String::new(),
        character_id: db.char_id.clone(),
        role: mud_db::enums::UserRole::Player,
        account_role: mud_db::enums::UserRole::Player,
        perms: Vec::new(),
    });
    (world, room, owner, rx)
}

fn carried_sword(world: &mut World, owner: Entity, pack_row: Option<i32>) -> Entity {
    let sword = world
        .spawn((
            Item,
            Named {
                name: "a plain sword".into(),
            },
            Keywords(vec!["sword".into()]),
            WorldKey { zone: 30, id: 7 },
            Located(owner),
        ))
        .id();
    if let Some(id) = pack_row {
        world
            .entity_mut(sword)
            .insert(mud_world::PersistedItemId(id));
    }
    sword
}

/// The exploit: `house place sword; get sword`, repeated, used to leave N
/// rows behind and rebuild N+1 swords after a reboot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn place_get_reload_leaves_exactly_one_sword() {
    let Some(db) = house_db().await else { return };
    // The sword's object must exist for the FK on the house row.
    let Ok(Some((oz, oid))) = mud_db::sqlx::query_as::<_, (i32, i32)>(
        "SELECT zone_id, id FROM \"Objects\" ORDER BY zone_id, id LIMIT 1",
    )
    .fetch_optional(&db.pool)
    .await
    else {
        db.end().await;
        return;
    };
    let (mut world, _room, owner, _rx) = owner_with_sword(&db);
    world
        .resource_mut::<mud_world::ObjectPrototypes>()
        .by_key
        .insert((oz, oid), {
            let mut p =
                super::test_support::object_proto(oz, oid, mud_db::enums::ObjectType::Weapon);
            p.name = "a plain sword".into();
            p.keywords = vec!["sword".into()];
            p
        });
    let pack_row: i32 = mud_db::sqlx::query_scalar(
        "INSERT INTO \"CharacterItems\" (character_id, object_zone_id, object_id, updated_at) \
         VALUES ($1, $2, $3, NOW()) RETURNING id",
    )
    .bind(&db.char_id)
    .bind(oz)
    .bind(oid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let sword = carried_sword(&mut world, owner, Some(pack_row));
    world
        .entity_mut(sword)
        .insert(WorldKey { zone: oz, id: oid });

    // Place: one house row, and the pack row is gone with it.
    cmd_house_place(&mut world, owner, &db.summary(), "sword");
    assert!(db.wait_for(|| async { db.rows().await.len() == 1 }).await);
    assert_eq!(
        db.pack_rows().await,
        0,
        "pack row removed with the placement"
    );
    assert_eq!(db.reloaded_swords().await, 1);

    // Plain get: the house row goes, so a reboot does not rebuild a copy.
    cmd_get(&mut world, owner, "sword");
    assert!(db.wait_for(|| async { db.rows().await.is_empty() }).await);
    assert_eq!(db.reloaded_swords().await, 0, "no ghost copy after get");

    // And again, repeatedly: still never more than the one sword in play.
    for _ in 0..3 {
        cmd_house_place(&mut world, owner, &db.summary(), "sword");
        assert!(db.wait_for(|| async { db.rows().await.len() == 1 }).await);
        cmd_get(&mut world, owner, "sword");
        assert!(db.wait_for(|| async { db.rows().await.is_empty() }).await);
    }
    assert_eq!(db.reloaded_swords().await, 0);

    // Place and get in the same breath, before the insert has finished.
    let seq_before = db.id_sequence().await;
    cmd_house_place(&mut world, owner, &db.summary(), "sword");
    cmd_get(&mut world, owner, "sword");
    assert!(
        db.wait_for(|| async { db.id_sequence().await > seq_before })
            .await,
        "the insert never ran"
    );
    assert!(
        db.wait_for(|| async { db.rows().await.is_empty() }).await,
        "the raced insert left a ghost row"
    );
    assert_eq!(db.reloaded_swords().await, 0);
    assert_eq!(world.get::<Located>(sword).unwrap().0, owner);
    db.end().await;
}

// ---------------------------------------------------------------------------
// Containers: contents are not persisted with a placed item
// ---------------------------------------------------------------------------

use super::info::cmd_put;

fn bag_world() -> (World, Entity, Entity, super::test_support::Rx) {
    let (mut world, room, owner, rx) = house_world();
    let mut bag = object_proto(30, 8, ObjectType::Container);
    bag.name = "a leather bag".into();
    bag.keywords = vec!["bag".into()];
    world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .insert((30, 8), bag);
    (world, room, owner, rx)
}

fn bag_at(world: &mut World, holder: Entity) -> Entity {
    world
        .spawn((
            Item,
            Named {
                name: "a leather bag".into(),
            },
            Keywords(vec!["bag".into()]),
            WorldKey { zone: 30, id: 8 },
            Located(holder),
        ))
        .id()
}

fn sword_at(world: &mut World, holder: Entity) -> Entity {
    world
        .spawn((
            Item,
            Named {
                name: "a plain sword".into(),
            },
            Keywords(vec!["sword".into()]),
            WorldKey { zone: 30, id: 7 },
            Located(holder),
        ))
        .id()
}

#[test]
fn a_container_with_contents_cannot_be_placed() {
    let (mut world, room, owner, mut rx) = bag_world();
    let bag = bag_at(&mut world, owner);
    let sword = sword_at(&mut world, bag);
    cmd_house_place(&mut world, owner, &house_summary(), "bag");
    let out = drain(&mut rx);
    assert!(out.contains("Empty it first"), "{out}");
    assert_eq!(world.get::<Located>(bag).unwrap().0, owner);
    assert!(world.get::<HousePlacement>(bag).is_none());
    assert_eq!(world.get::<Located>(sword).unwrap().0, bag);

    // Once emptied it places fine.
    world.entity_mut(sword).insert(Located(owner));
    cmd_house_place(&mut world, owner, &house_summary(), "bag");
    assert_eq!(world.get::<Located>(bag).unwrap().0, room);
}

#[test]
fn nothing_can_be_put_into_a_placed_container() {
    let (mut world, room, owner, mut rx) = bag_world();
    for marker in [
        Some(HouseItem(3)),
        None, // placed this session: carries a HousePlacement instead
    ] {
        let bag = bag_at(&mut world, room);
        match marker {
            Some(m) => world.entity_mut(bag).insert(m),
            None => world.entity_mut(bag).insert(HousePlacement::pending()),
        };
        let sword = sword_at(&mut world, owner);
        cmd_put(&mut world, owner, "sword bag");
        let out = drain(&mut rx);
        assert!(out.contains("can't hold anything"), "{out}");
        assert_eq!(world.get::<Located>(sword).unwrap().0, owner);
        cmd_put(&mut world, owner, "all bag");
        assert!(drain(&mut rx).contains("can't hold anything"));
        assert_eq!(world.get::<Located>(sword).unwrap().0, owner);
        world.despawn(bag);
        world.despawn(sword);
    }
}

#[test]
fn an_ordinary_carried_bag_still_takes_items() {
    let (mut world, _room, owner, mut rx) = bag_world();
    let bag = bag_at(&mut world, owner);
    let sword = sword_at(&mut world, owner);
    cmd_put(&mut world, owner, "sword bag");
    assert!(drain(&mut rx).contains("You put"));
    assert_eq!(world.get::<Located>(sword).unwrap().0, bag);
}
