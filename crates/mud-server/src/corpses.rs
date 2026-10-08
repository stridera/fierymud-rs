//! Player corpse persistence. A dead player's gear and purse live in the
//! database, never in a side file:
//!
//! * **Death.** `handle_death` moves the items into the corpse entity and
//!   marks the player [`PendingDeath`]. The player's very next save
//!   (every snapshot carries the marker until one commits) runs ONE
//!   transaction that inserts the `PlayerCorpses` row, tags the moved
//!   items' `CharacterItems` rows with its id, zeroes the carried wealth
//!   and writes the rest of the save (`login::write_snapshot`). A crash
//!   on either side of that commit loses nothing and duplicates nothing.
//! * **Loot.** Looting needs no corpse write at all: the looter's own
//!   save re-homes the item rows (`character_id` = looter, `corpse_id`
//!   cleared), and coins taken ride along as [`PendingCorpseCoinTakes`],
//!   debited from the corpse inside the same transaction that credits
//!   the looter's wealth.
//! * **Boot.** [`load_from_db`] rebuilds every corpse, nested bags
//!   included, from the rows via the same loader player inventories use.
//! * **Decay.** The `PlayerCorpses` row (and its items, by cascade) is
//!   deleted first; only once that commits do the contents drop to the
//!   room. Mob corpses are in-memory only.

use bevy_ecs::prelude::*;
use mud_db::sqlx::PgPool;
use mud_world::{
    Corpse, CorpseDecay, CorpseOriginLevel, Item, Keywords, Located, Named, PlayerCorpse,
    PlayerCorpseId, WorldKey, WorldKeyIndex,
};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Where a corpse is filed when its room carries no `WorldKey` and the
/// owner has no recall point: the Void (zone 0, room 0).
const FALLBACK_ROOM: (i32, i32) = (0, 0);

/// Marks a player whose corpse has not been committed yet. Carries the
/// corpse entity. Every save snapshot of the player includes the death
/// transaction until one commits, so a failed or racing earlier save can
/// never delete the moved items from the database.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct PendingDeath(pub(crate) Entity);

/// Coins a player has taken from player corpses and not yet saved:
/// `(PlayerCorpses.id, copper)`. A save debits the corpses in the same
/// transaction that writes the player's wealth, then subtracts what it
/// committed.
#[derive(Component, Debug, Clone, Default)]
pub(crate) struct PendingCorpseCoinTakes(pub(crate) Vec<(i32, i64)>);

/// Decay has deleted (or is deleting) the corpse's database rows; the
/// in-world corpse waits for that to commit before releasing its contents.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct DecayDeleting;

/// What the death transaction needs to know about the corpse, captured
/// when the player's snapshot is taken.
#[derive(Debug, Clone)]
pub(crate) struct DeathPersist {
    pub(crate) corpse: Entity,
    pub(crate) room_zone: i32,
    pub(crate) room_id: i32,
    /// Copper in the corpse's purse.
    pub(crate) coins: i64,
    pub(crate) decay_secs: i32,
}

/// Capture the pending death of `player`, if any. `None` when the player
/// has no [`PendingDeath`] or its corpse is gone.
pub(crate) fn pending_death(world: &World, player: Entity) -> Option<DeathPersist> {
    let corpse = world.get::<PendingDeath>(player)?.0;
    world.get_entity(corpse).ok()?;
    let room_key = world
        .get::<Located>(corpse)
        .and_then(|l| world.get::<WorldKey>(l.0).copied())
        .or_else(|| {
            world
                .get::<mud_world::RecallPoint>(player)
                .and_then(|r| world.get::<WorldKey>(r.0).copied())
        });
    let (room_zone, room_id) = room_key.map_or(FALLBACK_ROOM, |k| (k.zone, k.id));
    Some(DeathPersist {
        corpse,
        room_zone,
        room_id,
        coins: world
            .get::<mud_world::CoinPile>(corpse)
            .map_or(0, |p| p.0.max(0)),
        decay_secs: world
            .get::<CorpseDecay>(corpse)
            .map_or(1, |d| d.remaining_secs.max(1)),
    })
}

/// `(item count, coin)` held by `container`: lets callers detect
/// whether a looting command actually changed a corpse's contents.
pub(crate) fn contents_fingerprint(world: &mut World, container: Entity) -> (usize, i64) {
    let items = {
        let mut q = world.query_filtered::<&Located, With<Item>>();
        q.iter(world).filter(|l| l.0 == container).count()
    };
    let coin = world
        .get::<mud_world::CoinPile>(container)
        .map_or(0, |p| p.0);
    (items, coin)
}

/// A player corpse whose death write has not committed yet: its contents
/// are not in the database, so it can't be looted or moved safely.
pub(crate) fn is_unsettled(world: &World, corpse: Entity) -> bool {
    world.get::<PlayerCorpse>(corpse).is_some() && world.get::<PlayerCorpseId>(corpse).is_none()
}

/// Remember that `player` took `amount` copper from player corpse
/// `corpse`; the player's next save settles it with the corpse.
pub(crate) fn note_coin_take(world: &mut World, player: Entity, corpse: Entity, amount: i64) {
    let Some(id) = world.get::<PlayerCorpseId>(corpse).map(|c| c.0) else {
        return;
    };
    if amount <= 0 {
        return;
    }
    let Ok(mut em) = world.get_entity_mut(player) else {
        return;
    };
    let mut takes = em.take::<PendingCorpseCoinTakes>().unwrap_or_default().0;
    match takes.iter_mut().find(|(cid, _)| *cid == id) {
        Some((_, total)) => *total = total.saturating_add(amount),
        None => takes.push((id, amount)),
    }
    em.insert(PendingCorpseCoinTakes(takes));
}

/// Subtract `committed` (what a save just wrote) from the player's
/// pending coin takes, keeping anything taken since the snapshot.
pub(crate) fn settle_coin_takes(world: &mut World, player: Entity, committed: &[(i32, i64)]) {
    if committed.is_empty() {
        return;
    }
    let Ok(mut em) = world.get_entity_mut(player) else {
        return;
    };
    let Some(mut takes) = em.take::<PendingCorpseCoinTakes>().map(|t| t.0) else {
        return;
    };
    for (id, done) in committed {
        if let Some(entry) = takes.iter_mut().find(|(cid, _)| cid == id) {
            entry.1 -= done;
        }
    }
    takes.retain(|(_, amount)| *amount > 0);
    if !takes.is_empty() {
        em.insert(PendingCorpseCoinTakes(takes));
    }
}

// ---------------------------------------------------------------------
// Ordered corpse-row writes (drag, decay, despawn)
// ---------------------------------------------------------------------

enum Op {
    SetRoom {
        id: i32,
        zone: i32,
        room: i32,
    },
    Delete {
        id: i32,
        /// The decaying corpse waiting on this delete; `None` for a
        /// fire-and-forget cleanup.
        waiting: Option<Entity>,
    },
}

enum Finished {
    Deleted(Entity),
    DeleteFailed(Entity),
}

/// Handle to the single task that applies corpse-row writes in the order
/// the world issued them (a drag followed by a decay can't reorder).
#[derive(Resource, Clone)]
pub(crate) struct CorpseDb {
    tx: mpsc::UnboundedSender<Op>,
    finished: Arc<Mutex<Vec<Finished>>>,
}

impl CorpseDb {
    /// Spawn the writer task. Must be called inside a tokio runtime.
    pub(crate) fn spawn(pool: PgPool) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Op>();
        let finished: Arc<Mutex<Vec<Finished>>> = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::clone(&finished);
        tokio::spawn(async move {
            while let Some(op) = rx.recv().await {
                match op {
                    Op::SetRoom { id, zone, room } => {
                        if let Err(e) =
                            mud_db::player_corpses::set_room(&pool, id, zone, room).await
                        {
                            tracing::warn!(error = %e, corpse_id = id,
                                "couldn't record the dragged corpse's room");
                        }
                    }
                    Op::Delete { id, waiting } => {
                        let result = mud_db::player_corpses::delete(&pool, id).await;
                        if let Err(e) = &result {
                            tracing::warn!(error = %e, corpse_id = id,
                                "couldn't delete the player corpse rows");
                        }
                        if let Some(entity) = waiting {
                            done.lock()
                                .expect("corpse finished lock")
                                .push(if result.is_ok() {
                                    Finished::Deleted(entity)
                                } else {
                                    Finished::DeleteFailed(entity)
                                });
                        }
                    }
                }
            }
        });
        Self { tx, finished }
    }

    #[cfg(test)]
    pub(crate) fn pending_finished(&self) -> usize {
        self.finished.lock().expect("corpse finished lock").len()
    }
}

/// Keep the database in step when a persisted corpse leaves the world
/// by any route (decay, purge, ...): its rows go too, or boot would
/// bring it back.
pub(crate) fn register_observers(world: &mut World) {
    world.add_observer(
        |on: On<Remove, PlayerCorpseId>, ids: Query<&PlayerCorpseId>, db: Option<Res<CorpseDb>>| {
            if let (Ok(id), Some(db)) = (ids.get(on.entity), db) {
                let _ = db.tx.send(Op::Delete {
                    id: id.0,
                    waiting: None,
                });
            }
        },
    );
}

/// A dragged player corpse now lies in `room`: record it.
pub(crate) fn queue_set_room(world: &World, corpse: Entity, room: Entity) {
    let (Some(id), Some(key), Some(db)) = (
        world.get::<PlayerCorpseId>(corpse).map(|c| c.0),
        world.get::<WorldKey>(room).copied(),
        world.get_resource::<CorpseDb>(),
    ) else {
        return;
    };
    let _ = db.tx.send(Op::SetRoom {
        id,
        zone: key.zone,
        room: key.id,
    });
}

/// Decay of a persisted player corpse: delete its rows first and hold the
/// in-world corpse until that commits. Returns `true` when the corpse is
/// now waiting (the caller must not release it yet); `false` for a corpse
/// with no rows (mob corpses, or no database).
pub(crate) fn begin_decay_delete(world: &mut World, corpse: Entity) -> bool {
    let (Some(id), Some(db)) = (
        world.get::<PlayerCorpseId>(corpse).map(|c| c.0),
        world.get_resource::<CorpseDb>().cloned(),
    ) else {
        return false;
    };
    if world.get::<DecayDeleting>(corpse).is_some() {
        return true;
    }
    if let Ok(mut em) = world.get_entity_mut(corpse) {
        em.insert(DecayDeleting);
    }
    let _ = db.tx.send(Op::Delete {
        id,
        waiting: Some(corpse),
    });
    true
}

/// Corpses whose decay delete has committed this tick (ready to release
/// their contents). Failed deletes are un-marked so the next decay tick
/// retries them.
pub(crate) fn take_decayed(world: &mut World) -> Vec<Entity> {
    let Some(db) = world.get_resource::<CorpseDb>().cloned() else {
        return Vec::new();
    };
    let finished = std::mem::take(&mut *db.finished.lock().expect("corpse finished lock"));
    let mut ready = Vec::new();
    for f in finished {
        match f {
            Finished::Deleted(e) => ready.push(e),
            Finished::DeleteFailed(e) => {
                if let Ok(mut em) = world.get_entity_mut(e) {
                    em.remove::<DecayDeleting>();
                }
            }
        }
    }
    ready
}

// ---------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------

/// Recreate every persisted player corpse after the world has loaded
/// (needs prototypes and `WorldKeyIndex`). Items come back as the very
/// rows the owner's inventory uses, so charges, liquids and every
/// editor-owned column survive; bags nest as stored. Expired corpses
/// come back with one second left and decay through the normal path
/// (delete the rows, then drop the contents). A corpse whose room has
/// been removed is skipped with a warning and its rows are left alone.
pub async fn load_from_db(world: &mut World, pool: &PgPool) {
    let rows = match mud_db::player_corpses::list_all(pool).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "couldn't load player corpses");
            return;
        }
    };
    let mut restored = 0usize;
    let mut restored_items = 0usize;
    let mut skipped_rooms = 0usize;
    for row in rows {
        let Some(room_entity) = world
            .resource::<WorldKeyIndex>()
            .rooms
            .get(&(row.room_zone_id, row.room_id))
            .copied()
        else {
            tracing::warn!(corpse_id = row.id, owner = %row.owner_name,
                room_zone = row.room_zone_id, room_id = row.room_id,
                "player corpse's room no longer exists; leaving its rows alone");
            skipped_rooms += 1;
            continue;
        };
        let item_rows = match mud_db::character_items::list_for_corpse(pool, row.id).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, corpse_id = row.id,
                    "couldn't load player corpse items");
                continue;
            }
        };
        let corpse = spawn_corpse(
            world,
            room_entity,
            &row.owner_name,
            row.owner_level,
            row.remaining_secs,
            row.coins,
            row.id,
        );
        restored_items += crate::login::spawn_inventory(world, corpse, &item_rows);
        restored += 1;
    }
    tracing::info!(
        restored,
        restored_items,
        skipped_rooms,
        "player corpses loaded"
    );
}

/// Spawn the in-world corpse entity for a persisted corpse.
fn spawn_corpse(
    world: &mut World,
    room: Entity,
    owner_name: &str,
    owner_level: i32,
    remaining_secs: i32,
    coins: i64,
    corpse_id: i32,
) -> Entity {
    let corpse = world
        .spawn((
            Item,
            Corpse,
            Named {
                name: format!("the corpse of {owner_name}"),
            },
            Keywords(vec!["corpse".to_string(), owner_name.to_ascii_lowercase()]),
            Located(room),
            CorpseDecay {
                remaining_secs: remaining_secs.max(1),
            },
        ))
        .id();
    if let Ok(mut em) = world.get_entity_mut(corpse) {
        em.insert(PlayerCorpse);
        em.insert(PlayerCorpseId(corpse_id));
        em.insert(CorpseOriginLevel(owner_level.max(1)));
        if coins > 0 {
            em.insert(mud_world::CoinPile(coins));
        }
    }
    corpse
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autosave::SaveCoordinator;
    use crate::commands::test_support::{db_test_lock, db_test_pool_settings, object_proto};
    use crate::login::{save_player, spawn_background_save};
    use mud_db::enums::{ObjectType, UserRole};
    use mud_world::{
        Account, EquippedSlot, Health, ObjectAbilityCatalog, ObjectPrototypes, Player, Posture,
        PostureKind, Room, Slot, TriggerCatalog, Wealth,
    };
    use std::time::Duration;

    type DbRow = (i32, String, Option<i32>, Option<i32>, Option<String>);

    /// Dev-database pool (small, serialised); `None` when it isn't
    /// reachable or lacks the corpse tables, which skips the test.
    async fn live_pool() -> Option<(PgPool, tokio::sync::MutexGuard<'static, ()>)> {
        let lock = db_test_lock().await;
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
        let pool = tokio::time::timeout(
            Duration::from_secs(3),
            mud_db::connect_with(&url, db_test_pool_settings()),
        )
        .await
        .ok()?
        .ok()?;
        mud_db::sqlx::query("SELECT corpse_id FROM \"CharacterItems\" LIMIT 1")
            .execute(&pool)
            .await
            .ok()?;
        mud_db::sqlx::query("SELECT 1 FROM \"PlayerCorpses\" LIMIT 1")
            .execute(&pool)
            .await
            .ok()?;
        Some((pool, lock))
    }

    /// Two real `Objects` keys to satisfy the item FK.
    async fn object_keys(pool: &PgPool) -> Option<((i32, i32), (i32, i32))> {
        let keys: Vec<(i32, i32)> = mud_db::sqlx::query_as(
            "SELECT zone_id, id FROM \"Objects\" ORDER BY zone_id, id LIMIT 2",
        )
        .fetch_all(pool)
        .await
        .ok()?;
        (keys.len() == 2).then(|| (keys[0], keys[1]))
    }

    async fn temp_char(pool: &PgPool, tag: &str) -> (String, String) {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("Zc{tag}{}", suffix % 1_000_000_000_000);
        let id = format!("zc-{tag}-{suffix}");
        mud_db::sqlx::query(
            "INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())",
        )
        .bind(&id)
        .bind(&name)
        .execute(pool)
        .await
        .unwrap();
        (id, name)
    }

    async fn cleanup(pool: &PgPool, ids: &[&str]) {
        let ids: Vec<String> = ids.iter().map(|s| (*s).to_string()).collect();
        for sql in [
            "DELETE FROM \"PlayerCorpses\" WHERE owner_id = ANY($1)",
            "DELETE FROM \"CharacterItems\" WHERE character_id = ANY($1)",
            "DELETE FROM \"Characters\" WHERE id = ANY($1)",
        ] {
            mud_db::sqlx::query(sql)
                .bind(&ids)
                .execute(pool)
                .await
                .unwrap();
        }
    }

    async fn item_rows(pool: &PgPool, cid: &str) -> Vec<DbRow> {
        mud_db::sqlx::query_as(
            "SELECT id, character_id, container_id, corpse_id, custom_name \
             FROM \"CharacterItems\" WHERE character_id = $1 ORDER BY id",
        )
        .bind(cid)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn corpse_rows(pool: &PgPool, cid: &str) -> Vec<(i32, i64, i32, i32)> {
        mud_db::sqlx::query_as(
            "SELECT id, coins, room_zone_id, room_id FROM \"PlayerCorpses\" \
             WHERE owner_id = $1 ORDER BY id",
        )
        .bind(cid)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn wealth_of(pool: &PgPool, cid: &str) -> i64 {
        mud_db::sqlx::query_scalar("SELECT wealth FROM \"Characters\" WHERE id = $1")
            .bind(cid)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn live_world(keys: &[(i32, i32)]) -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(crate::TickCount(0));
        world.insert_resource(WorldKeyIndex::default());
        world.insert_resource(TriggerCatalog::default());
        world.insert_resource(ObjectAbilityCatalog::default());
        let mut protos = ObjectPrototypes::default();
        for &(z, i) in keys {
            protos
                .by_key
                .insert((z, i), object_proto(z, i, ObjectType::Container));
        }
        world.insert_resource(protos);
        let room = world.spawn((Room, WorldKey { zone: 30, id: 45 })).id();
        world
            .resource_mut::<WorldKeyIndex>()
            .rooms
            .insert((30, 45), room);
        (world, room)
    }

    fn spawn_player(
        world: &mut World,
        cid: &str,
        name: &str,
        room: Entity,
        role: UserRole,
        coins: i64,
    ) -> Entity {
        world
            .spawn((
                Player,
                Named {
                    name: name.to_string(),
                },
                Keywords(vec![name.to_ascii_lowercase()]),
                Account {
                    user_id: String::new(),
                    character_id: cid.to_string(),
                    role,
                    account_role: role,
                    perms: vec![],
                },
                Health { hp: 50, max: 100 },
                Posture(PostureKind::Standing),
                Located(room),
                Wealth(coins),
            ))
            .id()
    }

    fn spawn_item(world: &mut World, key: (i32, i32), parent: Entity) -> Entity {
        world
            .spawn((
                Item,
                Named {
                    name: "a test object".into(),
                },
                Keywords(vec!["object".into()]),
                WorldKey {
                    zone: key.0,
                    id: key.1,
                },
                Located(parent),
            ))
            .id()
    }

    /// Carried sword (worn), a bag, and a gem inside the bag; saved once so
    /// every item owns a `CharacterItems` row, then the sword is given
    /// per-instance state only the database knows about.
    struct Kit {
        sword: Entity,
        bag: Entity,
        gem: Entity,
    }

    async fn equip_and_save(
        world: &mut World,
        pool: &PgPool,
        player: Entity,
        k: ((i32, i32), (i32, i32)),
    ) -> Kit {
        let sword = spawn_item(world, k.0, player);
        world
            .entity_mut(sword)
            .insert((EquippedSlot(Slot::Wield), mud_world::Charges(7)));
        let bag = spawn_item(world, k.1, player);
        let gem = spawn_item(world, k.0, bag);
        let out = save_player(world, player, pool).await;
        assert!(out.committed, "{:?}", out.error);
        let sword_row = world.get::<mud_world::PersistedItemId>(sword).unwrap().0;
        mud_db::sqlx::query(
            "UPDATE \"CharacterItems\" SET custom_name = 'Fancy', condition = 42 \
             WHERE id = $1",
        )
        .bind(sword_row)
        .execute(pool)
        .await
        .unwrap();
        Kit { sword, bag, gem }
    }

    fn corpse_of(world: &mut World, name: &str) -> Entity {
        let want = format!("the corpse of {name}");
        world
            .query_filtered::<(Entity, &Named), With<PlayerCorpse>>()
            .iter(world)
            .find(|(_, n)| n.name == want)
            .map(|(e, _)| e)
            .expect("player corpse entity")
    }

    #[tokio::test]
    async fn death_persists_corpse_items_and_zero_wealth_atomically() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (cid, name) = temp_char(&pool, "dead").await;
        let (mut world, room) = live_world(&[keys.0, keys.1]);
        let player = spawn_player(&mut world, &cid, &name, room, UserRole::Player, 500);
        let kit = equip_and_save(&mut world, &pool, player, keys).await;
        let before = item_rows(&pool, &cid).await;
        assert_eq!(before.len(), 3);
        // The carried purse is only on the character row after a save.
        mud_db::sqlx::query("UPDATE \"Characters\" SET wealth = 500 WHERE id = $1")
            .bind(&cid)
            .execute(&pool)
            .await
            .unwrap();

        crate::combat::handle_death(&mut world, player, &name, room);
        assert!(world.get::<PendingDeath>(player).is_some());
        let corpse = corpse_of(&mut world, &name);
        assert!(is_unsettled(&world, corpse));

        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);

        // Corpse row: room, coins; wealth zeroed in the same commit.
        let corpses = corpse_rows(&pool, &cid).await;
        assert_eq!(corpses.len(), 1);
        let (corpse_id, coins, zone, rid) = corpses[0];
        assert_eq!((coins, zone, rid), (500, 30, 45));
        assert_eq!(wealth_of(&pool, &cid).await, 0);
        // Same rows (instance state intact), all filed under the corpse,
        // bag nesting preserved.
        let after = item_rows(&pool, &cid).await;
        assert_eq!(after.len(), 3);
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(b.0, a.0, "rows are re-filed, never recreated");
            assert_eq!(a.3, Some(corpse_id));
        }
        let bag_row = world.get::<mud_world::PersistedItemId>(kit.bag).unwrap().0;
        let gem_row = world.get::<mud_world::PersistedItemId>(kit.gem).unwrap().0;
        assert_eq!(
            after.iter().find(|r| r.0 == gem_row).unwrap().2,
            Some(bag_row)
        );
        let sword_row = after
            .iter()
            .find(|r| {
                r.0 == world
                    .get::<mud_world::PersistedItemId>(kit.sword)
                    .unwrap()
                    .0
            })
            .unwrap();
        assert_eq!(sword_row.4.as_deref(), Some("Fancy"));
        // In-memory side: committed id on the corpse, marker cleared.
        assert_eq!(world.get::<PlayerCorpseId>(corpse).unwrap().0, corpse_id);
        assert!(world.get::<PendingDeath>(player).is_none());
        // Carried set is empty: the wearer's own listing excludes the corpse.
        assert!(
            mud_db::character_items::list_for(&pool, &cid)
                .await
                .unwrap()
                .is_empty()
        );

        cleanup(&pool, &[&cid]).await;
    }

    #[tokio::test]
    async fn saves_racing_a_death_never_delete_or_duplicate_the_corpse() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (cid, name) = temp_char(&pool, "race").await;
        let (mut world, room) = live_world(&[keys.0, keys.1]);
        let player = spawn_player(&mut world, &cid, &name, room, UserRole::Player, 10);
        equip_and_save(&mut world, &pool, player, keys).await;
        world.insert_resource(crate::commands::DbPool(pool.clone()));

        // handle_death queues the background death save...
        crate::combat::handle_death(&mut world, player, &name, room);
        let coordinator = world.resource::<SaveCoordinator>().clone();
        // ...an autosave arriving meanwhile is refused (in flight) or
        // carries the same death; either way the quit-save behind it must
        // find the corpse committed.
        let _ = spawn_background_save(&mut world, player, &pool);
        assert!(coordinator.flush(&mut world, Duration::from_secs(10)).await);
        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);

        assert_eq!(corpse_rows(&pool, &cid).await.len(), 1, "one corpse row");
        let rows = item_rows(&pool, &cid).await;
        assert_eq!(rows.len(), 3, "no item row deleted or duplicated");
        assert!(rows.iter().all(|r| r.3.is_some()));
        cleanup(&pool, &[&cid]).await;
    }

    #[tokio::test]
    async fn owner_loot_moves_rows_with_instance_fields_and_coins_atomically() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (cid, name) = temp_char(&pool, "own").await;
        let (mut world, room) = live_world(&[keys.0, keys.1]);
        let player = spawn_player(&mut world, &cid, &name, room, UserRole::Player, 500);
        let kit = equip_and_save(&mut world, &pool, player, keys).await;
        mud_db::sqlx::query("UPDATE \"Characters\" SET wealth = 500 WHERE id = $1")
            .bind(&cid)
            .execute(&pool)
            .await
            .unwrap();
        crate::combat::handle_death(&mut world, player, &name, room);
        assert!(save_player(&mut world, player, &pool).await.committed);
        let corpse = corpse_of(&mut world, &name);
        let corpse_id = world.get::<PlayerCorpseId>(corpse).unwrap().0;

        crate::commands::info::cmd_get(&mut world, player, "all corpse");
        assert_eq!(world.get::<Wealth>(player).unwrap().0, 500);
        // Nothing is written to the database until the looter saves.
        assert_eq!(corpse_rows(&pool, &cid).await[0].1, 500);
        assert!(item_rows(&pool, &cid).await.iter().all(|r| r.3.is_some()));
        assert_eq!(
            world.get::<PendingCorpseCoinTakes>(player).unwrap().0,
            vec![(corpse_id, 500)]
        );

        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        let rows = item_rows(&pool, &cid).await;
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.3.is_none() && r.1 == cid));
        let sword_id = world
            .get::<mud_world::PersistedItemId>(kit.sword)
            .unwrap()
            .0;
        assert_eq!(
            rows.iter().find(|r| r.0 == sword_id).unwrap().4.as_deref(),
            Some("Fancy"),
            "customName survives the loot"
        );
        let (condition, charges): (i32, i32) = mud_db::sqlx::query_as(
            "SELECT condition, charges FROM \"CharacterItems\" WHERE id = $1",
        )
        .bind(sword_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(condition, 42);
        assert_eq!(charges, 7, "charges survive");
        assert_eq!(
            corpse_rows(&pool, &cid).await[0].1,
            0,
            "corpse purse debited"
        );
        assert_eq!(wealth_of(&pool, &cid).await, 500, "looter credited");
        assert!(world.get::<PendingCorpseCoinTakes>(player).is_none());
        let _ = kit.gem;
        cleanup(&pool, &[&cid]).await;
    }

    #[tokio::test]
    async fn another_players_loot_moves_the_items_and_coins_to_them() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (cid, name) = temp_char(&pool, "vic").await;
        let (lid, lname) = temp_char(&pool, "lot").await;
        let (mut world, room) = live_world(&[keys.0, keys.1]);
        let victim = spawn_player(&mut world, &cid, &name, room, UserRole::Player, 300);
        equip_and_save(&mut world, &pool, victim, keys).await;
        mud_db::sqlx::query("UPDATE \"Characters\" SET wealth = 300 WHERE id = $1")
            .bind(&cid)
            .execute(&pool)
            .await
            .unwrap();
        let looter = spawn_player(&mut world, &lid, &lname, room, UserRole::Builder, 0);
        crate::combat::handle_death(&mut world, victim, &name, room);
        assert!(save_player(&mut world, victim, &pool).await.committed);

        crate::commands::info::cmd_get(&mut world, looter, "all corpse");
        assert!(save_player(&mut world, looter, &pool).await.committed);

        let theirs = item_rows(&pool, &lid).await;
        assert_eq!(theirs.len(), 3);
        assert!(theirs.iter().all(|r| r.3.is_none()));
        assert!(theirs.iter().any(|r| r.4.as_deref() == Some("Fancy")));
        assert!(item_rows(&pool, &cid).await.is_empty());
        assert_eq!(wealth_of(&pool, &lid).await, 300);
        assert_eq!(corpse_rows(&pool, &cid).await[0].1, 0);
        // The dead player's later save can't claw the items back.
        assert!(save_player(&mut world, victim, &pool).await.committed);
        assert_eq!(item_rows(&pool, &lid).await.len(), 3);
        cleanup(&pool, &[&cid, &lid]).await;
    }

    #[tokio::test]
    async fn boot_restores_the_corpse_with_nested_contents_in_the_right_room() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (cid, name) = temp_char(&pool, "boot").await;
        let (mut world, room) = live_world(&[keys.0, keys.1]);
        let player = spawn_player(&mut world, &cid, &name, room, UserRole::Player, 250);
        let kit = equip_and_save(&mut world, &pool, player, keys).await;
        mud_db::sqlx::query("UPDATE \"Characters\" SET wealth = 250 WHERE id = $1")
            .bind(&cid)
            .execute(&pool)
            .await
            .unwrap();
        crate::combat::handle_death(&mut world, player, &name, room);
        assert!(save_player(&mut world, player, &pool).await.committed);
        let _ = kit;

        // A fresh server: nothing in memory, rooms and prototypes loaded.
        let (mut fresh, fresh_room) = live_world(&[keys.0, keys.1]);
        load_from_db(&mut fresh, &pool).await;
        let corpse = corpse_of(&mut fresh, &name);
        assert_eq!(fresh.get::<Located>(corpse).unwrap().0, fresh_room);
        assert_eq!(fresh.get::<mud_world::CoinPile>(corpse).unwrap().0, 250);
        assert!(fresh.get::<PlayerCorpseId>(corpse).is_some());
        assert!(!is_unsettled(&fresh, corpse));
        let direct: Vec<Entity> = {
            let mut q = fresh.query_filtered::<(Entity, &Located), With<Item>>();
            q.iter(&fresh)
                .filter(|(_, l)| l.0 == corpse)
                .map(|(e, _)| e)
                .collect()
        };
        assert_eq!(direct.len(), 2, "sword and bag directly in the corpse");
        let bag = direct
            .iter()
            .copied()
            .find(|e| fresh.get::<WorldKey>(*e).map(|k| (k.zone, k.id)) == Some(keys.1))
            .expect("bag");
        let nested: Vec<Entity> = {
            let mut q = fresh.query_filtered::<(Entity, &Located), With<Item>>();
            q.iter(&fresh)
                .filter(|(_, l)| l.0 == bag)
                .map(|(e, _)| e)
                .collect()
        };
        assert_eq!(nested.len(), 1, "gem restored inside the bag");
        assert!(fresh.get::<mud_world::PersistedItemId>(nested[0]).is_some());
        // A second boot of the same rows doesn't duplicate anything in the DB.
        assert_eq!(item_rows(&pool, &cid).await.len(), 3);
        cleanup(&pool, &[&cid]).await;
    }

    #[tokio::test]
    async fn decay_deletes_the_rows_then_drops_the_contents() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (cid, name) = temp_char(&pool, "rot").await;
        let (mut world, room) = live_world(&[keys.0, keys.1]);
        let player = spawn_player(&mut world, &cid, &name, room, UserRole::Player, 80);
        equip_and_save(&mut world, &pool, player, keys).await;
        mud_db::sqlx::query("UPDATE \"Characters\" SET wealth = 80 WHERE id = $1")
            .bind(&cid)
            .execute(&pool)
            .await
            .unwrap();
        crate::combat::handle_death(&mut world, player, &name, room);
        assert!(save_player(&mut world, player, &pool).await.committed);
        // The corpse rotted while the server was down.
        mud_db::sqlx::query(
            "UPDATE \"PlayerCorpses\" SET decay_at = NOW() - INTERVAL '1 hour' \
             WHERE owner_id = $1",
        )
        .bind(&cid)
        .execute(&pool)
        .await
        .unwrap();

        let (mut fresh, fresh_room) = live_world(&[keys.0, keys.1]);
        load_from_db(&mut fresh, &pool).await;
        register_observers(&mut fresh);
        fresh.insert_resource(CorpseDb::spawn(pool.clone()));
        fresh.insert_resource(crate::TickCount(10));
        let corpse = corpse_of(&mut fresh, &name);
        assert_eq!(fresh.get::<CorpseDecay>(corpse).unwrap().remaining_secs, 1);

        // First pass: the timer expires and the delete is issued; the
        // contents stay put until it commits.
        crate::combat::corpse_decay_tick(&mut fresh);
        assert!(fresh.get::<DecayDeleting>(corpse).is_some());
        let db = fresh.resource::<CorpseDb>().clone();
        for _ in 0..100 {
            if db.pending_finished() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(db.pending_finished() > 0, "delete never reported");
        assert!(corpse_rows(&pool, &cid).await.is_empty());
        assert!(item_rows(&pool, &cid).await.is_empty(), "items cascade");
        assert!(
            fresh.get_entity(corpse).is_ok(),
            "contents not yet released"
        );

        crate::combat::corpse_decay_tick(&mut fresh);
        assert!(fresh.get_entity(corpse).is_err(), "corpse despawned");
        let on_floor = {
            let mut q = fresh.query_filtered::<&Located, With<Item>>();
            q.iter(&fresh).filter(|l| l.0 == fresh_room).count()
        };
        assert!(
            on_floor >= 3,
            "sword, bag and the coin pile drop to the room"
        );
        cleanup(&pool, &[&cid]).await;
    }

    #[test]
    fn coin_takes_accumulate_and_settle() {
        let mut world = World::new();
        let player = world.spawn_empty().id();
        let corpse = world.spawn(PlayerCorpseId(9)).id();
        note_coin_take(&mut world, player, corpse, 100);
        note_coin_take(&mut world, player, corpse, 50);
        assert_eq!(
            world.get::<PendingCorpseCoinTakes>(player).unwrap().0,
            vec![(9, 150)]
        );
        // A save committed 100 of it; a later take stays pending.
        note_coin_take(&mut world, player, corpse, 25);
        settle_coin_takes(&mut world, player, &[(9, 150)]);
        assert_eq!(
            world.get::<PendingCorpseCoinTakes>(player).unwrap().0,
            vec![(9, 25)]
        );
        settle_coin_takes(&mut world, player, &[(9, 25)]);
        assert!(world.get::<PendingCorpseCoinTakes>(player).is_none());
        // Mob corpses (no id) never queue anything.
        let mob_corpse = world.spawn(Corpse).id();
        note_coin_take(&mut world, player, mob_corpse, 10);
        assert!(world.get::<PendingCorpseCoinTakes>(player).is_none());
    }
}
