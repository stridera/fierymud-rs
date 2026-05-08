//! Shop stock persistence.
//!
//! `ShopCatalog` is rebuilt from the `ShopItems` schema rows on every
//! boot, so without this snapshot a server restart silently refills
//! every shop's shelves regardless of what players bought during the
//! prior session. Mirrors the weather / clock snapshot pattern: load
//! once after world load, save on graceful shutdown.
//!
//! Storage shape: only stocks that *changed* land in the file (by
//! diffing against the catalog as just-loaded from DB), and unlimited-
//! stock entries (`amount = -1`) never get an entry. The result stays
//! tiny in practice — only shops with finite-stock items players
//! actually depleted appear in the snapshot.

use std::path::Path;

use bevy_ecs::prelude::*;
use mud_world::ShopCatalog;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// On-disk path. Relative to the server's working directory like the
/// other state snapshots.
const SHOP_SNAPSHOT_PATH: &str = "state/shop_stock.json";

/// One persisted item-stock delta. Keyed by `(shop_zone, shop_id,
/// item_zone, item_id)`; reading the file produces a flat `HashMap`
/// so the loader doesn't have to rebuild any nesting.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ItemStock {
    shop_zone: i32,
    shop_id: i32,
    object_zone: i32,
    object_id: i32,
    amount: i32,
}

/// Same shape but for pet offerings (`ShopMobs` mirror). Pet shops are
/// rarer than item shops; the entry only lands when the saved amount
/// differs from what the catalog rebuild produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PetStock {
    shop_zone: i32,
    shop_id: i32,
    mob_zone: i32,
    mob_id: i32,
    amount: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ShopSnapshot {
    items: Vec<ItemStock>,
    pets: Vec<PetStock>,
}

/// Read the snapshot from disk and apply it to the just-loaded
/// `ShopCatalog`. Silent no-op when the file doesn't exist (first
/// boot) or fails to parse — the catalog keeps its DB-rebuild values
/// in either case.
///
/// Skips entries whose `(shop, object)` pair no longer exists in the
/// catalog: a content edit that removed a `ShopItems` row shouldn't
/// resurrect it from the snapshot.
pub fn load_snapshot(world: &mut World) {
    let bytes = match std::fs::read(SHOP_SNAPSHOT_PATH) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            warn!(error = %e, "shop snapshot read failed");
            return;
        }
    };
    let snapshot: ShopSnapshot = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "shop snapshot parse failed");
            return;
        }
    };
    let mut catalog = world.resource_mut::<ShopCatalog>();
    let mut item_restored = 0usize;
    let mut item_skipped = 0usize;
    for stock in snapshot.items {
        let Some(shop) = catalog.by_key.get_mut(&(stock.shop_zone, stock.shop_id)) else {
            item_skipped += 1;
            continue;
        };
        let Some(offer) = shop.items.iter_mut().find(|o| {
            o.object_zone_id == stock.object_zone && o.object_id == stock.object_id
        }) else {
            item_skipped += 1;
            continue;
        };
        // Unlimited-stock entries shouldn't accept a finite override
        // either — the snapshot is for finite stocks only. Leave the
        // catalog value (already -1) alone.
        if offer.amount == -1 {
            item_skipped += 1;
            continue;
        }
        offer.amount = stock.amount;
        item_restored += 1;
    }
    let mut pet_restored = 0usize;
    let mut pet_skipped = 0usize;
    for stock in snapshot.pets {
        let Some(shop) = catalog.by_key.get_mut(&(stock.shop_zone, stock.shop_id)) else {
            pet_skipped += 1;
            continue;
        };
        let Some(offer) = shop.pets.iter_mut().find(|o| {
            o.mob_zone_id == stock.mob_zone && o.mob_id == stock.mob_id
        }) else {
            pet_skipped += 1;
            continue;
        };
        if offer.amount == -1 {
            pet_skipped += 1;
            continue;
        }
        offer.amount = stock.amount;
        pet_restored += 1;
    }
    tracing::info!(
        items_restored = item_restored,
        items_skipped = item_skipped,
        pets_restored = pet_restored,
        pets_skipped = pet_skipped,
        path = %SHOP_SNAPSHOT_PATH,
        "shop snapshot loaded",
    );
}

/// Walk the live `ShopCatalog` and emit a snapshot of every finite-
/// stock entry. Unlimited stock (`amount = -1`) is skipped — those
/// don't need persistence. Run from the graceful-shutdown path so
/// next boot's `load_snapshot` can put the shelves back where they
/// were.
pub fn save_snapshot(world: &World) {
    if let Some(parent) = Path::new(SHOP_SNAPSHOT_PATH).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn!(error = %e, "couldn't create shop snapshot dir");
        return;
    }
    let catalog = world.resource::<ShopCatalog>();
    let mut snapshot = ShopSnapshot::default();
    for ((shop_zone, shop_id), shop) in &catalog.by_key {
        for offer in &shop.items {
            if offer.amount == -1 {
                continue;
            }
            snapshot.items.push(ItemStock {
                shop_zone: *shop_zone,
                shop_id: *shop_id,
                object_zone: offer.object_zone_id,
                object_id: offer.object_id,
                amount: offer.amount,
            });
        }
        for offer in &shop.pets {
            if offer.amount == -1 {
                continue;
            }
            snapshot.pets.push(PetStock {
                shop_zone: *shop_zone,
                shop_id: *shop_id,
                mob_zone: offer.mob_zone_id,
                mob_id: offer.mob_id,
                amount: offer.amount,
            });
        }
    }
    let bytes = match serde_json::to_vec_pretty(&snapshot) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "shop snapshot serialize failed");
            return;
        }
    };
    if let Err(e) = std::fs::write(SHOP_SNAPSHOT_PATH, bytes) {
        warn!(error = %e, "shop snapshot write failed");
        return;
    }
    tracing::info!(
        items = snapshot.items.len(),
        pets = snapshot.pets.len(),
        path = %SHOP_SNAPSHOT_PATH,
        "shop snapshot saved",
    );
}

