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
//!   room. Mob corpses are in-memory only. The delete (like a
//!   resurrection's retirement) waits while any looter's save is still
//!   uncommitted, see [`CorpseLootLedger`]: the cascade would otherwise
//!   take item rows the looter's pending save has yet to re-home.
//! * **Item decay.** An item with a `CharacterItems` row that is destroyed
//!   inside a settled corpse has its row (and its nested rows) deleted
//!   through the [`CorpseDb`] writer, or boot would bring it back.
//! * **Purge.** Staff `purge` never touches a player corpse in bulk; an
//!   explicit one is removed with its contents ([`purge_player_corpse`]).

use bevy_ecs::prelude::*;
use mud_db::sqlx::PgPool;
use mud_world::{
    Corpse, CorpseDecay, CorpseOriginLevel, Item, Keywords, Located, Named, PersistedItemId,
    PlayerCorpse, PlayerCorpseId, WorldKey, WorldKeyIndex,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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

/// Character id of the dead player a player corpse belongs to, so
/// resurrection finds exactly their corpse (never a name look-alike).
#[derive(Component, Debug, Clone)]
pub(crate) struct PlayerCorpseOwner(pub(crate) String);

/// Corpses a resurrected player has taken everything from and that must
/// be deleted in that player's next save (`PlayerCorpses.id`s), in the
/// same transaction that re-homes the items and credits the coins.
#[derive(Component, Debug, Clone, Default)]
pub(crate) struct PendingCorpseRetire(pub(crate) Vec<i32>);

/// How long a looter's mark outlives their entity: the quit-save retry
/// schedule (see `autosave::QUIT_RETRY_BACKOFF`, ~2.6 minutes) can still
/// commit after the player is gone, so their marks hold the corpse until
/// then. Past it the save has been given up on and the mark is dropped.
const LOOT_ORPHAN_GRACE: Duration = Duration::from_secs(600);

/// Which players have taken items or coins out of a settled player corpse
/// without that take being committed yet. Looting writes nothing to the
/// corpse itself: the looter's own save re-homes the rows. Deleting the
/// corpse row first would cascade those rows away, so decay, retirement
/// and purge wait for the entry to clear (the looter's commit,
/// [`settle_loot`] from `apply_commit`).
///
/// Keyed by `(looter, PlayerCorpses.id)`; the value is a per-ledger
/// sequence number (so a take made after a snapshot isn't cleared by that
/// snapshot's commit) and when it was recorded.
#[derive(Resource, Default, Debug)]
pub(crate) struct CorpseLootLedger {
    next_seq: u64,
    pending: HashMap<(Entity, i32), (u64, Instant)>,
}

/// Remember that `player` took something out of player corpse `corpse`
/// (settled ones only; an unsettled corpse refuses looting). Cleared by
/// the commit of a save snapshotted after this call.
pub(crate) fn note_loot(world: &mut World, player: Entity, corpse: Entity) {
    let Some(id) = world.get::<PlayerCorpseId>(corpse).map(|c| c.0) else {
        return;
    };
    // Only characters that get saved can ever clear a mark.
    if world.get::<mud_world::Account>(player).is_none() {
        return;
    }
    let mut ledger = world.get_resource_or_insert_with(CorpseLootLedger::default);
    ledger.next_seq += 1;
    let seq = ledger.next_seq;
    ledger.pending.insert((player, id), (seq, Instant::now()));
}

/// The marks `player` holds right now, captured into their save snapshot.
pub(crate) fn loot_marks(world: &World, player: Entity) -> Vec<(i32, u64)> {
    world
        .get_resource::<CorpseLootLedger>()
        .map(|l| {
            l.pending
                .iter()
                .filter(|((p, _), _)| *p == player)
                .map(|((_, id), (seq, _))| (*id, *seq))
                .collect()
        })
        .unwrap_or_default()
}

/// A save carrying `committed` marks has just committed: clear them,
/// keeping any take made after that snapshot.
pub(crate) fn settle_loot(world: &mut World, player: Entity, committed: &[(i32, u64)]) {
    if committed.is_empty() {
        return;
    }
    let Some(mut ledger) = world.get_resource_mut::<CorpseLootLedger>() else {
        return;
    };
    for (id, seq) in committed {
        if ledger
            .pending
            .get(&(player, *id))
            .is_some_and(|(now, _)| now == seq)
        {
            ledger.pending.remove(&(player, *id));
        }
    }
}

/// True when a player other than `except` still has an uncommitted take
/// from corpse `corpse_id`. Marks of players who have left and whose
/// final save is long past retrying are ignored.
pub(crate) fn has_pending_loot(world: &World, corpse_id: i32, except: Option<Entity>) -> bool {
    let Some(ledger) = world.get_resource::<CorpseLootLedger>() else {
        return false;
    };
    ledger.pending.iter().any(|((player, id), (_, at))| {
        *id == corpse_id
            && Some(*player) != except
            && (world.get_entity(*player).is_ok() || at.elapsed() < LOOT_ORPHAN_GRACE)
    })
}

/// The in-world corpse was emptied by a resurrection: its rows are
/// deleted by the revived player's save, not by the despawn observer
/// (which would cascade-delete items the save hasn't re-homed yet).
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct RetiredCorpse;

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

/// Subtract the retirements a save just committed.
pub(crate) fn settle_retired(world: &mut World, player: Entity, committed: &[i32]) {
    if committed.is_empty() {
        return;
    }
    let Ok(mut em) = world.get_entity_mut(player) else {
        return;
    };
    let Some(mut pending) = em.take::<PendingCorpseRetire>().map(|p| p.0) else {
        return;
    };
    pending.retain(|id| !committed.contains(id));
    if !pending.is_empty() {
        em.insert(PendingCorpseRetire(pending));
    }
}

/// The player corpses that belong to `owner`, by character id (corpses
/// restored or created without an owner id fall back to the exact
/// `the corpse of <name>` title). Corpses already being decayed away are
/// not offered.
pub(crate) fn corpses_of(world: &mut World, owner: Entity) -> Vec<Entity> {
    let owner_id = world
        .get::<mud_world::Account>(owner)
        .map(|a| a.character_id.clone());
    let title = world
        .get::<Named>(owner)
        .map(|n| format!("the corpse of {}", n.name));
    let mut q = world.query_filtered::<(
        Entity,
        &Named,
        Option<&PlayerCorpseOwner>,
    ), (With<PlayerCorpse>, Without<DecayDeleting>)>();
    q.iter(world)
        .filter(|(_, named, corpse_owner)| match (corpse_owner, &owner_id) {
            (Some(o), Some(id)) => &o.0 == id,
            (Some(_), None) => false,
            (None, _) => title
                .as_deref()
                .is_some_and(|t| named.name.eq_ignore_ascii_case(t)),
        })
        .map(|(e, _, _)| e)
        .collect()
}

/// Resurrection: give `player` everything in `corpse` (items with their
/// nesting, coins) and remove the corpse. Persists exactly like an owner
/// loot: the items' rows are re-homed by `player`'s next save, the coins
/// are debited from the corpse in that save's transaction and the now
/// empty corpse row is deleted there too. Returns how many items moved.
/// The corpse must be settled (see [`is_unsettled`]).
pub(crate) fn hand_over(world: &mut World, player: Entity, corpse: Entity) -> usize {
    let items: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == corpse)
            .map(|(e, _)| e)
            .collect()
    };
    for &it in &items {
        if let Ok(mut em) = world.get_entity_mut(it) {
            em.insert(Located(player));
            em.remove::<mud_world::EquippedSlot>();
        }
    }
    let coins = world
        .get::<mud_world::CoinPile>(corpse)
        .map_or(0, |p| p.0.max(0));
    if coins > 0 {
        let held = world.get::<mud_world::Wealth>(player).map_or(0, |w| w.0);
        if let Ok(mut em) = world.get_entity_mut(player) {
            em.insert(mud_world::Wealth(held.saturating_add(coins)));
        }
        note_coin_take(world, player, corpse, coins);
    }
    if let Some(id) = world.get::<PlayerCorpseId>(corpse).map(|c| c.0) {
        if let Ok(mut em) = world.get_entity_mut(player) {
            let mut pending = em.take::<PendingCorpseRetire>().unwrap_or_default().0;
            pending.push(id);
            em.insert(PendingCorpseRetire(pending));
        }
        if let Ok(mut em) = world.get_entity_mut(corpse) {
            em.insert(RetiredCorpse);
        }
    }
    if let Ok(mut em) = world.get_entity_mut(corpse) {
        em.remove::<mud_world::CoinPile>();
    }
    if let Ok(em) = world.get_entity_mut(corpse) {
        em.despawn();
    }
    items.len()
}

/// Why [`purge_player_corpse`] declined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PurgeRefusal {
    /// The death write hasn't committed (or the decay delete is running):
    /// the rows can't be deleted yet, and despawning now would let the
    /// pending write recreate the corpse at the next boot.
    Settling,
    /// A looter's save from this corpse hasn't committed; deleting the
    /// corpse row now would cascade their item rows away.
    LootPending,
}

/// Staff purge of one player corpse: the corpse and everything inside it
/// (nested bags included) leave the world and the database. The corpse row
/// goes through the [`CorpseDb`] writer exactly like a decay (the
/// `Remove<PlayerCorpseId>` observer), its item rows by cascade. Returns
/// how many items went with it.
pub(crate) fn purge_player_corpse(
    world: &mut World,
    corpse: Entity,
) -> Result<usize, PurgeRefusal> {
    if is_unsettled(world, corpse) || world.get::<DecayDeleting>(corpse).is_some() {
        return Err(PurgeRefusal::Settling);
    }
    if let Some(id) = world.get::<PlayerCorpseId>(corpse).map(|c| c.0)
        && has_pending_loot(world, id, None)
    {
        return Err(PurgeRefusal::LootPending);
    }
    let mut tree: Vec<Entity> = Vec::new();
    let mut frontier = vec![corpse];
    while let Some(parent) = frontier.pop() {
        if let Some(contents) = world.get::<mud_world::Contents>(parent) {
            for e in contents.iter() {
                if world.get::<Item>(e).is_some() {
                    tree.push(e);
                    frontier.push(e);
                }
            }
        }
    }
    // Corpse first: its row delete is queued while the contents are still
    // in place, and the contents then despawn as unattached entities (no
    // redundant per-item deletes; the cascade covers them).
    if let Ok(em) = world.get_entity_mut(corpse) {
        em.despawn();
    }
    for &e in &tree {
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
    Ok(tree.len())
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
    /// Rows of items destroyed inside corpse `corpse` (see
    /// `mud_db::character_items::delete_corpse_item_rows`).
    DeleteItems {
        corpse: i32,
        ids: Vec<i32>,
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
                    Op::DeleteItems { corpse, ids } => {
                        if let Err(e) =
                            mud_db::character_items::delete_corpse_item_rows(&pool, corpse, &ids)
                                .await
                        {
                            tracing::warn!(error = %e, corpse_id = corpse,
                                "couldn't delete a destroyed corpse item's rows");
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
        |on: On<Remove, PlayerCorpseId>,
         ids: Query<(&PlayerCorpseId, Has<RetiredCorpse>)>,
         db: Option<Res<CorpseDb>>| {
            if let (Ok((id, retired)), Some(db)) = (ids.get(on.entity), db)
                && !retired
            {
                let _ = db.tx.send(Op::Delete {
                    id: id.0,
                    waiting: None,
                });
            }
        },
    );
    // An item with a row that despawns inside a settled corpse takes its
    // row (and the rows of anything still nested in it) with it, or the
    // next boot would put it back.
    world.add_observer(
        |on: On<Remove, PersistedItemId>,
         rows: Query<&PersistedItemId>,
         located: Query<&Located>,
         contents: Query<&mud_world::Contents>,
         items: Query<(), With<Item>>,
         corpses: Query<(&PlayerCorpseId, Has<RetiredCorpse>, Has<DecayDeleting>)>,
         db: Option<Res<CorpseDb>>| {
            let Some(db) = db else { return };
            // The settled, live corpse the item sits in (directly or nested).
            // Corpses already retired or being deleted are handled elsewhere.
            let mut corpse_id = None;
            let mut cur = on.entity;
            // Real nesting is a handful of bags; the cap only guards
            // against a malformed `Located` cycle.
            for _ in 0..32 {
                let Ok(loc) = located.get(cur) else { break };
                cur = loc.0;
                if let Ok((id, retired, deleting)) = corpses.get(cur) {
                    if !retired && !deleting {
                        corpse_id = Some(id.0);
                    }
                    break;
                }
            }
            let Some(corpse_id) = corpse_id else { return };
            // The item first, then everything still nested inside it.
            let mut ids = Vec::new();
            let mut frontier = vec![on.entity];
            while let Some(e) = frontier.pop() {
                if let Ok(row) = rows.get(e) {
                    ids.push(row.0);
                }
                if let Ok(c) = contents.get(e) {
                    frontier.extend(c.iter().filter(|k| items.contains(*k)));
                }
            }
            if !ids.is_empty() {
                let _ = db.tx.send(Op::DeleteItems {
                    corpse: corpse_id,
                    ids,
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
    // Someone's loot from this corpse hasn't been saved yet: the delete
    // would cascade away their item rows. Hold the corpse (not marked, so
    // the decay tick asks again) until their save commits.
    if has_pending_loot(world, id, None) {
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
        let corpse = spawn_corpse(world, room_entity, &row);
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
    row: &mud_db::player_corpses::PlayerCorpseRow,
) -> Entity {
    let owner_name = &row.owner_name;
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
                remaining_secs: row.remaining_secs.max(1),
            },
        ))
        .id();
    if let Ok(mut em) = world.get_entity_mut(corpse) {
        em.insert(PlayerCorpse);
        em.insert(PlayerCorpseId(row.id));
        em.insert(PlayerCorpseOwner(row.owner_id.clone()));
        em.insert(CorpseOriginLevel(row.owner_level.max(1)));
        if row.coins > 0 {
            em.insert(mud_world::CoinPile(row.coins));
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

    #[tokio::test]
    async fn resurrection_hands_over_the_exact_owners_corpse_and_loses_nothing() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (bob_id, bob) = temp_char(&pool, "rez").await;
        // A look-alike whose name merely starts with Bob's.
        let bobby = format!("{bob}by");
        let bobby_id = format!("{bob_id}-by");
        mud_db::sqlx::query(
            "INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())",
        )
        .bind(&bobby_id)
        .bind(&bobby)
        .execute(&pool)
        .await
        .unwrap();
        let (mut world, room) = live_world(&[keys.0, keys.1]);
        register_observers(&mut world);
        world.insert_resource(CorpseDb::spawn(pool.clone()));
        let bob_e = spawn_player(&mut world, &bob_id, &bob, room, UserRole::Player, 500);
        let bobby_e = spawn_player(&mut world, &bobby_id, &bobby, room, UserRole::Player, 70);
        let kit = equip_and_save(&mut world, &pool, bob_e, keys).await;
        equip_and_save(&mut world, &pool, bobby_e, keys).await;
        for (id, coins) in [(&bob_id, 500), (&bobby_id, 70)] {
            mud_db::sqlx::query("UPDATE \"Characters\" SET wealth = $2 WHERE id = $1")
                .bind(id)
                .bind(coins)
                .execute(&pool)
                .await
                .unwrap();
        }
        crate::combat::handle_death(&mut world, bob_e, &bob, room);
        crate::combat::handle_death(&mut world, bobby_e, &bobby, room);
        assert!(save_player(&mut world, bob_e, &pool).await.committed);
        assert!(save_player(&mut world, bobby_e, &pool).await.committed);
        let bob_corpse = corpse_of(&mut world, &bob);

        // Exactly Bob's corpse, not Bobby's.
        assert_eq!(corpses_of(&mut world, bob_e), vec![bob_corpse]);

        let moved = hand_over(&mut world, bob_e, bob_corpse);
        assert_eq!(moved, 2, "sword and bag (the gem stays inside the bag)");
        assert!(world.get_entity(bob_corpse).is_err());
        assert_eq!(world.get::<Wealth>(bob_e).unwrap().0, 500);
        assert_eq!(world.get::<Located>(kit.gem).unwrap().0, kit.bag);
        // The despawn must not have cascade-deleted anything yet.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(item_rows(&pool, &bob_id).await.len(), 3);
        assert_eq!(corpse_rows(&pool, &bob_id).await.len(), 1);

        let out = save_player(&mut world, bob_e, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        let rows = item_rows(&pool, &bob_id).await;
        assert_eq!(rows.len(), 3, "no row lost");
        assert!(rows.iter().all(|r| r.3.is_none()));
        let sword_id = world
            .get::<mud_world::PersistedItemId>(kit.sword)
            .unwrap()
            .0;
        assert_eq!(
            rows.iter().find(|r| r.0 == sword_id).unwrap().4.as_deref(),
            Some("Fancy"),
            "instance fields survive the resurrection"
        );
        assert_eq!(wealth_of(&pool, &bob_id).await, 500, "coins credited");
        assert!(
            corpse_rows(&pool, &bob_id).await.is_empty(),
            "corpse row retired"
        );
        assert!(world.get::<PendingCorpseRetire>(bob_e).is_none());
        assert!(world.get::<PendingCorpseCoinTakes>(bob_e).is_none());
        // Bobby's corpse and gear were never touched.
        assert_eq!(corpse_rows(&pool, &bobby_id).await.len(), 1);
        let theirs = item_rows(&pool, &bobby_id).await;
        assert_eq!(theirs.len(), 3);
        assert!(theirs.iter().all(|r| r.3.is_some()));
        assert_eq!(corpse_rows(&pool, &bobby_id).await[0].1, 70);
        cleanup(&pool, &[&bob_id, &bobby_id]).await;
    }

    #[test]
    fn corpses_of_matches_the_exact_owner_only() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let named = |world: &mut World, name: &str| {
            world
                .spawn((
                    Player,
                    Named {
                        name: name.to_string(),
                    },
                    Located(room),
                ))
                .id()
        };
        let bob = named(&mut world, "Bob");
        let bobby = named(&mut world, "Bobby");
        let corpse = |world: &mut World, title: &str| {
            world
                .spawn((
                    Item,
                    Corpse,
                    PlayerCorpse,
                    Named {
                        name: title.to_string(),
                    },
                    Located(room),
                ))
                .id()
        };
        let c_bob = corpse(&mut world, "the corpse of Bob");
        let c_bobby = corpse(&mut world, "the corpse of Bobby");
        assert_eq!(corpses_of(&mut world, bob), vec![c_bob]);
        assert_eq!(corpses_of(&mut world, bobby), vec![c_bobby]);
        // An owner id on the corpse wins over the title.
        world
            .entity_mut(c_bob)
            .insert(PlayerCorpseOwner("x".into()));
        assert!(corpses_of(&mut world, bob).is_empty());
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

    /// Poll `check` for up to ~5s (the corpse writer is a background task).
    async fn eventually<F, Fut>(mut check: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..100 {
            if check().await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// A dead player with a settled corpse (sword, bag, gem + `coins`) in
    /// a world that has the delete observers and the corpse writer.
    struct Dead {
        world: World,
        room: Entity,
        cid: String,
        name: String,
        player: Entity,
        corpse: Entity,
        corpse_id: i32,
        kit: Kit,
    }

    async fn settled_corpse(
        pool: &PgPool,
        keys: ((i32, i32), (i32, i32)),
        tag: &str,
        coins: i64,
    ) -> Dead {
        let (cid, name) = temp_char(pool, tag).await;
        let (mut world, room) = live_world(&[keys.0, keys.1]);
        register_observers(&mut world);
        world.insert_resource(CorpseDb::spawn(pool.clone()));
        let player = spawn_player(&mut world, &cid, &name, room, UserRole::Player, coins);
        let kit = equip_and_save(&mut world, pool, player, keys).await;
        mud_db::sqlx::query("UPDATE \"Characters\" SET wealth = $2 WHERE id = $1")
            .bind(&cid)
            .bind(coins)
            .execute(pool)
            .await
            .unwrap();
        crate::combat::handle_death(&mut world, player, &name, room);
        assert!(save_player(&mut world, player, pool).await.committed);
        let corpse = corpse_of(&mut world, &name);
        let corpse_id = world.get::<PlayerCorpseId>(corpse).unwrap().0;
        Dead {
            world,
            room,
            cid,
            name,
            player,
            corpse,
            corpse_id,
            kit,
        }
    }

    #[tokio::test]
    async fn loot_then_immediate_decay_waits_for_the_loots_commit() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let mut d = settled_corpse(&pool, keys, "ldv", 300).await;
        let (lid, lname) = temp_char(&pool, "ldl").await;
        let looter = spawn_player(&mut d.world, &lid, &lname, d.room, UserRole::Builder, 0);

        crate::commands::info::cmd_get(&mut d.world, looter, "all corpse");
        assert!(has_pending_loot(&d.world, d.corpse_id, None));
        assert!(!has_pending_loot(&d.world, d.corpse_id, Some(looter)));

        // The corpse rots before the looter's save lands.
        d.world
            .get_mut::<CorpseDecay>(d.corpse)
            .unwrap()
            .remaining_secs = 1;
        crate::combat::corpse_decay_tick(&mut d.world);
        assert!(
            d.world.get::<DecayDeleting>(d.corpse).is_none(),
            "delete must wait for the looter's commit"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(corpse_rows(&pool, &d.cid).await.len(), 1);
        let held = item_rows(&pool, &d.cid).await;
        assert_eq!(held.len(), 3, "the looted rows still exist");

        // The commit clears the mark; the next decay pass deletes.
        let out = save_player(&mut d.world, looter, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        assert!(!has_pending_loot(&d.world, d.corpse_id, None));
        crate::combat::corpse_decay_tick(&mut d.world);
        assert!(d.world.get::<DecayDeleting>(d.corpse).is_some());
        assert!(eventually(|| async { corpse_rows(&pool, &d.cid).await.is_empty() }).await);

        let theirs = item_rows(&pool, &lid).await;
        assert_eq!(theirs.len(), 3, "the looter's rows survive the delete");
        assert!(theirs.iter().all(|r| r.3.is_none()));
        assert_eq!(wealth_of(&pool, &lid).await, 300);
        assert!(item_rows(&pool, &d.cid).await.is_empty());
        cleanup(&pool, &[&d.cid, &lid]).await;
    }

    #[tokio::test]
    async fn resurrection_retire_waits_for_another_looters_commit() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let mut d = settled_corpse(&pool, keys, "rtv", 0).await;
        let (lid, lname) = temp_char(&pool, "rtl").await;
        let looter = spawn_player(&mut d.world, &lid, &lname, d.room, UserRole::Builder, 0);

        // A staff member takes one thing; then the owner is raised.
        crate::commands::info::cmd_get(&mut d.world, looter, "object corpse");
        assert!(has_pending_loot(&d.world, d.corpse_id, None));
        hand_over(&mut d.world, d.player, d.corpse);
        assert!(d.world.get_entity(d.corpse).is_err());

        let out = save_player(&mut d.world, d.player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        assert_eq!(
            corpse_rows(&pool, &d.cid).await.len(),
            1,
            "the corpse row stays while the looter's take is unsaved"
        );
        assert_eq!(
            d.world.get::<PendingCorpseRetire>(d.player).unwrap().0,
            vec![d.corpse_id]
        );
        let stranded: Vec<DbRow> = item_rows(&pool, &d.cid)
            .await
            .into_iter()
            .filter(|r| r.3.is_some())
            .collect();
        assert!(!stranded.is_empty(), "the looted rows are still filed");

        let out = save_player(&mut d.world, looter, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        let out = save_player(&mut d.world, d.player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        assert!(corpse_rows(&pool, &d.cid).await.is_empty(), "now retired");
        assert!(d.world.get::<PendingCorpseRetire>(d.player).is_none());
        let theirs = item_rows(&pool, &lid).await;
        assert!(!theirs.is_empty() && theirs.iter().all(|r| r.3.is_none()));
        let mine = item_rows(&pool, &d.cid).await;
        assert_eq!(mine.len() + theirs.len(), 3, "no row lost");
        cleanup(&pool, &[&d.cid, &lid]).await;
    }

    #[tokio::test]
    async fn item_decay_inside_a_corpse_deletes_its_row_and_keeps_released_contents() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let mut d = settled_corpse(&pool, keys, "idc", 0).await;
        let bag_row = d.world.get::<PersistedItemId>(d.kit.bag).unwrap().0;
        let gem_row = d.world.get::<PersistedItemId>(d.kit.gem).unwrap().0;
        let sword_row = d.world.get::<PersistedItemId>(d.kit.sword).unwrap().0;
        d.world.entity_mut(d.kit.bag).insert(mud_world::ItemTimer {
            remaining_secs: 1,
            decompose_window_secs: 0,
        });

        crate::item_decay::item_decay_tick(&mut d.world);
        assert!(d.world.get_entity(d.kit.bag).is_err());
        assert!(
            eventually(|| async {
                !item_rows(&pool, &d.cid)
                    .await
                    .iter()
                    .any(|r| r.0 == bag_row)
            })
            .await,
            "the decayed bag's row must go"
        );
        let rows = item_rows(&pool, &d.cid).await;
        let gem = rows.iter().find(|r| r.0 == gem_row).expect("gem row kept");
        assert_eq!(gem.2, None, "released to the corpse, no dangling bag");
        assert_eq!(gem.3, Some(d.corpse_id));
        assert!(rows.iter().any(|r| r.0 == sword_row));
        assert_eq!(d.world.get::<Located>(d.kit.gem).unwrap().0, d.corpse);
        assert_eq!(corpse_rows(&pool, &d.cid).await.len(), 1);
        cleanup(&pool, &[&d.cid]).await;
    }

    #[tokio::test]
    async fn despawning_a_bag_inside_a_corpse_deletes_the_nested_rows_too() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let mut d = settled_corpse(&pool, keys, "idn", 0).await;
        let sword_row = d.world.get::<PersistedItemId>(d.kit.sword).unwrap().0;
        d.world.entity_mut(d.kit.bag).despawn();
        assert!(
            eventually(|| async { item_rows(&pool, &d.cid).await.len() == 1 }).await,
            "bag and gem rows must go"
        );
        let rows = item_rows(&pool, &d.cid).await;
        assert_eq!(rows[0].0, sword_row);
        assert_eq!(rows[0].3, Some(d.corpse_id));
        cleanup(&pool, &[&d.cid]).await;
    }

    #[tokio::test]
    async fn despawning_a_carried_item_leaves_its_row_to_the_save_diff() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (cid, name) = temp_char(&pool, "idp").await;
        let (mut world, room) = live_world(&[keys.0, keys.1]);
        register_observers(&mut world);
        world.insert_resource(CorpseDb::spawn(pool.clone()));
        let player = spawn_player(&mut world, &cid, &name, room, UserRole::Player, 0);
        let kit = equip_and_save(&mut world, &pool, player, keys).await;
        world.entity_mut(kit.sword).despawn();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(item_rows(&pool, &cid).await.len(), 3);
        assert!(save_player(&mut world, player, &pool).await.committed);
        assert_eq!(item_rows(&pool, &cid).await.len(), 2);
        cleanup(&pool, &[&cid]).await;
    }

    #[tokio::test]
    async fn purging_a_settled_corpse_removes_contents_and_deletes_the_row() {
        let Some((pool, _lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(keys) = object_keys(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let mut d = settled_corpse(&pool, keys, "pgs", 40).await;
        assert_eq!(purge_player_corpse(&mut d.world, d.corpse), Ok(3));
        for e in [d.corpse, d.kit.sword, d.kit.bag, d.kit.gem] {
            assert!(d.world.get_entity(e).is_err(), "{e:?} should be gone");
        }
        assert!(
            eventually(|| async {
                corpse_rows(&pool, &d.cid).await.is_empty()
                    && item_rows(&pool, &d.cid).await.is_empty()
            })
            .await,
            "row and (by cascade) item rows deleted"
        );
        let _ = (&d.name, d.player);
        cleanup(&pool, &[&d.cid]).await;
    }

    fn bare_corpse(world: &mut World, room: Entity, id: Option<i32>) -> (Entity, Entity) {
        let corpse = world
            .spawn((Item, Corpse, PlayerCorpse, Located(room)))
            .id();
        if let Some(id) = id {
            world.entity_mut(corpse).insert(PlayerCorpseId(id));
        }
        let held = world.spawn((Item, Located(corpse))).id();
        (corpse, held)
    }

    #[test]
    fn purge_refuses_an_unsettled_corpse_and_changes_nothing() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (corpse, held) = bare_corpse(&mut world, room, None);
        assert_eq!(
            purge_player_corpse(&mut world, corpse),
            Err(PurgeRefusal::Settling)
        );
        assert!(world.get_entity(corpse).is_ok() && world.get_entity(held).is_ok());
        // A corpse mid-decay-delete is refused too.
        let (rotting, _) = bare_corpse(&mut world, room, Some(4));
        world.entity_mut(rotting).insert(DecayDeleting);
        assert_eq!(
            purge_player_corpse(&mut world, rotting),
            Err(PurgeRefusal::Settling)
        );
    }

    #[test]
    fn purge_refuses_while_a_looters_save_is_pending() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (corpse, held) = bare_corpse(&mut world, room, Some(11));
        let looter = world
            .spawn(Account {
                user_id: String::new(),
                character_id: "x".into(),
                role: UserRole::Player,
                account_role: UserRole::Player,
                perms: vec![],
            })
            .id();
        note_loot(&mut world, looter, corpse);
        assert_eq!(
            purge_player_corpse(&mut world, corpse),
            Err(PurgeRefusal::LootPending)
        );
        assert!(world.get_entity(held).is_ok());
        // The looter's commit clears it.
        let marks = loot_marks(&world, looter);
        assert_eq!(marks.len(), 1);
        settle_loot(&mut world, looter, &marks);
        assert_eq!(purge_player_corpse(&mut world, corpse), Ok(1));
        assert!(world.get_entity(corpse).is_err() && world.get_entity(held).is_err());
    }

    #[test]
    fn loot_marks_keep_takes_made_after_the_snapshot() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (corpse, _) = bare_corpse(&mut world, room, Some(21));
        let (unsettled, _) = bare_corpse(&mut world, room, None);
        let acct = |cid: &str| Account {
            user_id: String::new(),
            character_id: cid.into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        };
        let a = world.spawn(acct("a")).id();
        let mob = world.spawn_empty().id();

        note_loot(&mut world, a, unsettled);
        note_loot(&mut world, mob, corpse);
        assert!(
            !has_pending_loot(&world, 21, None),
            "nothing to wait for yet"
        );

        note_loot(&mut world, a, corpse);
        let snap = loot_marks(&world, a);
        note_loot(&mut world, a, corpse); // taken after the snapshot
        settle_loot(&mut world, a, &snap);
        assert!(
            has_pending_loot(&world, 21, None),
            "the later take is still unsaved"
        );
        assert!(!has_pending_loot(&world, 21, Some(a)), "own marks ignored");
        let snap = loot_marks(&world, a);
        settle_loot(&mut world, a, &snap);
        assert!(!has_pending_loot(&world, 21, None));
    }
}
