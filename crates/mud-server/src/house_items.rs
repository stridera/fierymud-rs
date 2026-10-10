//! Persistence bookkeeping for items placed in a player house.
//!
//! A placed item lives in two places at once: the ECS entity in the house
//! room and its `PlayerHouseItem` row (what rebuilds the house after a
//! reboot). Whenever the entity leaves the house the row has to go with
//! it, or the next boot rebuilds a second copy. Every way out (`get`,
//! `get all`, `house take`, a guest or a mob picking it up, `give`, ...)
//! ends in the item's `Located` pointing somewhere that is not a house
//! room, so an observer on `Located` catches them all instead of each
//! pickup path having to remember to call a helper.
//!
//! Items loaded from the database carry [`HouseItem`] (row id known).
//! Items placed this session carry [`HousePlacement`], a shared slot the
//! background insert settles once it has the row id; a pickup that races
//! the insert marks the slot released and the insert task deletes the row
//! it just wrote.
//!
//! The row delete is tied to the picker's persistence. When a player (or a
//! player's pet) takes the item, the row id rides on the player as
//! [`PendingHouseDeletes`] and the player's next save snapshot carries it:
//! the save deletes the house row in the same transaction that inserts the
//! item into the pack, so the item is never in both places and never in
//! neither. A picker that is not a player's (a scavenger mob) has no save to
//! ride on; its delete is a tracked task that retries until it succeeds and
//! that shutdown waits for.

use bevy_ecs::prelude::*;
use mud_world::{Account, Follower, HouseItem, HousePlacement, HouseRoom, Located, PersistentPet};

use crate::autosave::SaveCoordinator;
use crate::commands::DbPool;

/// Query filter for entities tracked as house items.
pub(crate) type HouseTracked = Or<(With<HouseItem>, With<HousePlacement>)>;

/// Outcome of [`detach_house_item`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Detached {
    /// The item was not a house item.
    NotHouseItem,
    /// It was, but its insert is still in flight (the insert task will
    /// clean up after itself).
    InFlight,
    /// It was; this `PlayerHouseItem` row must be deleted.
    Row(i32),
}

/// Take `item` out of the house bookkeeping: strip its markers and report
/// the `PlayerHouseItem` row to delete, if one is known yet.
pub(crate) fn detach_house_item(world: &mut World, item: Entity) -> Detached {
    let loaded = world.get::<HouseItem>(item).map(|h| h.0);
    let placement = world.get::<HousePlacement>(item).cloned();
    if loaded.is_none() && placement.is_none() {
        return Detached::NotHouseItem;
    }
    let mut row = loaded;
    if let Some(p) = &placement
        && let Some(id) = p.release()
    {
        row = Some(id);
    }
    if let Ok(mut e) = world.get_entity_mut(item) {
        e.remove::<HouseItem>();
        e.remove::<HousePlacement>();
    }
    row.map_or(Detached::InFlight, Detached::Row)
}

/// `PlayerHouseItem` rows a player has picked up, not yet deleted in the
/// database. The player's save snapshot carries them and deletes them in the
/// transaction that writes their pack; [`settle_deletes`] clears what a
/// committed save carried. Rows stay until a save commits, so a failed save
/// simply carries them into the next attempt.
#[derive(Component, Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PendingHouseDeletes(pub Vec<i32>);

/// The player whose save should carry the row delete for `item`: whoever
/// holds it (directly or inside carried containers), or the owner of the
/// persistent pet holding it. `None` when no player's save will include it.
fn save_owner(world: &World, item: Entity) -> Option<Entity> {
    let mut cur = item;
    // Real nesting is a handful of bags; the cap guards a malformed cycle.
    for _ in 0..16 {
        let holder = world.get::<Located>(cur)?.0;
        if world.get::<Account>(holder).is_some() {
            return Some(holder);
        }
        if world.get::<PersistentPet>(holder).is_some()
            && let Some(owner) = world.get::<Follower>(holder).map(|f| f.0)
            && world.get::<Account>(owner).is_some()
        {
            return Some(owner);
        }
        cur = holder;
    }
    None
}

/// The one way an item stops being a house item: strips its markers and
/// arranges for its row to be deleted (see the module docs). Used by
/// `house take` and by the `Located` observer, so every pickup behaves
/// identically.
pub(crate) fn release_house_item(world: &mut World, item: Entity) -> bool {
    let id = match detach_house_item(world, item) {
        Detached::NotHouseItem => return false,
        Detached::InFlight => None,
        Detached::Row(id) => Some(id),
    };
    if let Some(id) = id {
        match save_owner(world, item) {
            Some(owner) => {
                let mut e = world.entity_mut(owner);
                match e.get_mut::<PendingHouseDeletes>() {
                    Some(mut pending) => pending.0.push(id),
                    None => {
                        e.insert(PendingHouseDeletes(vec![id]));
                    }
                }
            }
            None => delete_row_tracked(world, id),
        }
    }
    true
}

/// A committed save wrote `deleted` (its snapshot's [`PendingHouseDeletes`]):
/// forget exactly those, keeping rows queued since the snapshot.
pub(crate) fn settle_deletes(world: &mut World, player: Entity, deleted: &[i32]) {
    if deleted.is_empty() {
        return;
    }
    let Some(mut pending) = world.get_mut::<PendingHouseDeletes>(player) else {
        return;
    };
    pending.0.retain(|id| !deleted.contains(id));
    if pending.0.is_empty() {
        world.entity_mut(player).remove::<PendingHouseDeletes>();
    }
}

/// Delete a house row with no player save to carry it: a task shutdown waits
/// for, retrying until the row is gone (a row that survives a pickup is a
/// duplicate after the next boot). No-op without a database (unit tests).
fn delete_row_tracked(world: &World, id: i32) {
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let coordinator = world
        .get_resource::<SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    coordinator.spawn_tracked(delete_row_until_gone(pool, id));
}

/// Retry `op` with a growing, capped delay until it succeeds.
pub(crate) async fn retry_until_ok<F, Fut, E>(what: &str, id: i32, mut op: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    let mut attempt = 0_u32;
    loop {
        match op().await {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(error = %e, id, attempt, "{what} failed; retrying");
                let delay_ms = (250_u64 << attempt.min(7)).min(30_000);
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

async fn delete_row_until_gone(pool: mud_db::sqlx::PgPool, id: i32) {
    retry_until_ok("house item row delete", id, || async {
        mud_db::housing::remove_item(&pool, id).await.map(|_| ())
    })
    .await;
}

/// What a placement needs to persist itself.
pub(crate) struct PlacementWrite {
    pub character_id: String,
    pub room_row_id: i32,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub custom: mud_db::housing::HouseItemCustom,
    /// The item's pack row, deleted in the same transaction.
    pub inventory_row_id: Option<i32>,
    pub placement: HousePlacement,
}

/// Background-write a placement. The character's save-order turn is queued
/// BEFORE this returns ([`SaveCoordinator::spawn_ordered`]), so a quit save
/// issued right after `house place` (or after `get` took the item back)
/// waits for the insert instead of snapshotting first: it can neither drop
/// the pack row while the house row does not exist yet, nor race the
/// placement's cleanup. Like the account chest, no in-flight save can
/// re-insert the pack row afterwards, and the house row insert and the pack
/// row delete share one transaction. The task counts as an unfinished write,
/// so shutdown waits for it. No-op without a database (unit tests).
pub(crate) fn persist_placement(world: &World, w: PlacementWrite) {
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let coordinator = world
        .get_resource::<SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    let character_id = w.character_id.clone();
    let tracker = coordinator.clone();
    coordinator.spawn_ordered(&character_id, move |mut ordered| async move {
        if w.placement.is_released() {
            // Picked up before this turn came: the item is back in a pack
            // (its stale pack row, if any, goes with the next save's diff)
            // and there is nothing to insert.
            return;
        }
        let result = mud_db::housing::place_item(
            &pool,
            w.room_row_id,
            w.object_zone_id,
            w.object_id,
            &w.custom,
            w.inventory_row_id,
        )
        .await;
        match result {
            Ok(id) => {
                // Snapshots taken before the placement still list the item
                // in the pack; none may land after its row is gone.
                ordered.supersede_earlier_snapshots();
                tracing::debug!(item_id = id, "house item placed");
                if !w.placement.settle(id) {
                    // Picked up while the insert was in flight. Remove the
                    // row before giving up the turn, so the picker's queued
                    // save cannot commit the pack row ahead of it; if the
                    // delete fails keep retrying where shutdown waits.
                    if mud_db::housing::remove_item(&pool, id).await.is_err() {
                        tracker.spawn_tracked(delete_row_until_gone(pool.clone(), id));
                    }
                }
                drop(ordered);
            }
            Err(e) => {
                tracing::warn!(error = %e, "house item place failed");
            }
        }
    });
}

/// Boot registers this once: a house item whose `Located` is set anywhere
/// but a house room has left the house.
pub(crate) fn register_observers(world: &mut World) {
    world.add_observer(
        |on: On<Insert, Located>,
         placed: Query<&Located, HouseTracked>,
         house_rooms: Query<(), With<HouseRoom>>,
         mut commands: Commands| {
            let Ok(located) = placed.get(on.entity) else {
                return;
            };
            if house_rooms.contains(located.0) {
                return;
            }
            let item = on.entity;
            commands.queue(move |world: &mut World| {
                release_house_item(world, item);
            });
        },
    );
}
