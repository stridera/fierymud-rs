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

use bevy_ecs::prelude::*;
use mud_world::{HouseItem, HousePlacement, HouseRoom, Located};

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

/// The one way an item stops being a house item: strips its markers and
/// deletes its row in the background. Used by `house take` and by the
/// `Located` observer, so every pickup behaves identically.
pub(crate) fn release_house_item(world: &mut World, item: Entity) -> bool {
    let id = match detach_house_item(world, item) {
        Detached::NotHouseItem => return false,
        Detached::InFlight => None,
        Detached::Row(id) => Some(id),
    };
    if let Some(id) = id
        && let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone())
    {
        tokio::spawn(async move {
            delete_row_with_retry(&pool, id).await;
        });
    }
    true
}

/// Delete a house item row, retrying a few times: a row that survives a
/// pickup is a duplicate after the next boot.
pub(crate) async fn delete_row_with_retry(pool: &mud_db::sqlx::PgPool, id: i32) {
    for attempt in 0..4_u32 {
        match mud_db::housing::remove_item(pool, id).await {
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(error = %e, id, attempt, "house item remove failed");
                tokio::time::sleep(std::time::Duration::from_millis(250 << attempt)).await;
            }
        }
    }
    tracing::error!(
        id,
        "house item row could not be removed; it will reappear on reload"
    );
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

/// Background-write a placement. Takes the character's save-order turn first
/// (like the account chest) so no in-flight save can re-insert the pack row
/// afterwards, then inserts the house row and deletes the pack row in one
/// transaction. No-op without a database (unit tests).
pub(crate) fn persist_placement(world: &World, w: PlacementWrite) {
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let coordinator = world
        .get_resource::<SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    tokio::spawn(async move {
        let mut ordered = coordinator.begin_ordered(&w.character_id).await;
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
                drop(ordered);
                tracing::debug!(item_id = id, "house item placed");
                if !w.placement.settle(id) {
                    // Picked up before the insert finished.
                    delete_row_with_retry(&pool, id).await;
                }
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
