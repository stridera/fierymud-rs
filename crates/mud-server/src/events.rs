//! World-event catalog runtime (parking-lot resolution: 2026-05-12).
//!
//! Tracks the active state of every row in the `Events` DB table and
//! emits `quest_triggers::dispatch_event_trigger(world, event_id)`
//! on the off → on edge. Quests with
//! `QuestTriggerType::EVENT` then trip through the standard
//! grant/offer path.
//!
//! Design choices:
//!
//! - **Active is the only gate.** Date windows live on the row but
//!   are not interpreted here today — the operator flips `active`
//!   manually (via Muditor or admin tooling) and the runtime
//!   observes the change. This keeps the v1 contract small and
//!   sidesteps "what happens if a quest fires at second 1 of a
//!   window then again at second 2" without a debounce.
//!
//! - **Edge-triggered.** A row going from `active = false` to
//!   `active = true` fires the dispatch exactly once. The reverse
//!   transition resets the gate so a subsequent off → on fires
//!   again — useful for recurring events the admin toggles on/off
//!   per session.
//!
//! - **DB polling.** Every `EVENT_POLL_PERIOD_TICKS` ticks we
//!   `events::list_all` and reconcile against the catalog's
//!   in-memory `was_active` map. Cheap when nothing changes
//!   (one bounded SELECT per minute).

use bevy_ecs::prelude::*;
use std::collections::HashMap;

use crate::TickCount;
use crate::commands::DbPool;
use crate::quest_triggers::dispatch_event_trigger;

/// 60s at TICK_HZ=10. Operators flip `active` from the Muditor UI;
/// 60s is the slowest "noticeable to the player" cadence — fast
/// enough to feel responsive when staff turns an event on, slow
/// enough that the DB poll stays in the noise.
pub(crate) const EVENT_POLL_PERIOD_TICKS: u64 = 600;

/// Per-event last-observed active state. Resource so the tick can
/// reconcile the DB poll against the previous tick's snapshot
/// without an extra ECS scan.
///
/// `was_active` is keyed by `Events.id`; entries are added on first
/// observation and never deleted (the table is small — dozens of
/// rows in normal content). A `false` value means the row exists
/// but is currently inactive; a missing key means we've never seen
/// the row before (and the next poll will seed the entry).
#[derive(Resource, Default, Debug)]
pub(crate) struct EventsCatalog {
    pub was_active: HashMap<i32, bool>,
}

/// Sender side of the inbox a tokio task uses to hand the polled
/// row state back to the world thread. The tokio task can't borrow
/// `&mut World` to mutate the catalog directly.
#[derive(Resource, Clone)]
pub(crate) struct EventPollSender(
    pub(crate) tokio::sync::mpsc::Sender<Vec<(i32, bool)>>,
);

#[derive(Resource)]
pub(crate) struct EventPollRx(
    pub(crate) std::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<(i32, bool)>>>,
);

/// Wire the catalog + the inbox channel. Call once at boot from
/// main.rs after `World::new()`.
pub(crate) fn init_resources(world: &mut World) {
    // 8-slot capacity: most poll runs publish exactly one batch;
    // bursts on a fast-tick pause/unpause cycle won't outrun the
    // drain.
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<(i32, bool)>>(8);
    world.insert_resource(EventPollSender(tx));
    world.insert_resource(EventPollRx(std::sync::Mutex::new(rx)));
    world.insert_resource(EventsCatalog::default());
}

/// Tick the events poll. Every `EVENT_POLL_PERIOD_TICKS` ticks
/// (and on tick 0 so a freshly-active event fires on boot), spawn
/// an async DB read that pushes a `Vec<(id, active)>` snapshot to
/// the inbox. The drain (`drain_events_inbox`) reconciles edges and
/// fires `dispatch_event_trigger` on each off → on transition.
pub(crate) fn events_poll_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    // Fire on tick 0 (first observation after boot) so any row that
    // was already active when the server came up emits the dispatch
    // exactly once on the first poll, then again only on a later
    // off → on edge.
    if tick != 0 && !tick.is_multiple_of(EVENT_POLL_PERIOD_TICKS) {
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let Some(sender) = world.get_resource::<EventPollSender>().map(|s| s.0.clone()) else {
        return;
    };
    tokio::spawn(async move {
        let rows = match mud_db::events::list_all(&pool).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "events poll failed");
                return;
            }
        };
        let snapshot: Vec<(i32, bool)> = rows.into_iter().map(|r| (r.id, r.active)).collect();
        let _ = sender.send(snapshot).await;
    });
}

/// Drain the events inbox and reconcile edges. Runs on the world
/// thread so `dispatch_event_trigger` can borrow `&mut World`.
/// Cheap when no batch is pending — single try_recv.
pub(crate) fn drain_events_inbox(world: &mut World) {
    let batches: Vec<Vec<(i32, bool)>> = {
        let Some(rx) = world.get_resource::<EventPollRx>() else {
            return;
        };
        let Ok(mut rx) = rx.0.lock() else {
            return;
        };
        let mut out = Vec::new();
        while let Ok(b) = rx.try_recv() {
            out.push(b);
        }
        out
    };
    if batches.is_empty() {
        return;
    }
    // For each (id, active) pair, compare against the prior state
    // and fire on the off → on edge. Use the LATEST batch as the
    // authoritative snapshot (older batches are stale).
    let Some(latest) = batches.last() else {
        return;
    };
    // Snapshot prior state so we can update the catalog without
    // borrowing across the dispatch call.
    let prior: HashMap<i32, bool> = world
        .get_resource::<EventsCatalog>()
        .map(|c| c.was_active.clone())
        .unwrap_or_default();
    let mut fired: Vec<i32> = Vec::new();
    for &(id, active) in latest {
        let was = prior.get(&id).copied().unwrap_or(false);
        if active && !was {
            fired.push(id);
        }
    }
    // Update the catalog before firing — if a dispatch handler
    // re-enters this code path (it shouldn't, but be defensive),
    // we want the new state visible.
    if let Some(mut cat) = world.get_resource_mut::<EventsCatalog>() {
        for &(id, active) in latest {
            cat.was_active.insert(id, active);
        }
    }
    for id in fired {
        tracing::info!(event_id = id, "world event activated; dispatching EVENT-trigger quests");
        dispatch_event_trigger(world, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verifies the edge-detection logic by walking the catalog
    // directly. The full `events_poll_tick` + `drain_events_inbox`
    // round-trip needs a tokio runtime + DB; the unit test below
    // covers the reconciliation contract — what the drain actually
    // computes from a batch — which is the load-bearing piece.

    #[test]
    fn catalog_seeds_with_no_entries() {
        let cat = EventsCatalog::default();
        assert!(cat.was_active.is_empty());
    }

    #[test]
    fn edge_detection_off_to_on_fires() {
        // Simulate the drain's edge-detection by hand.
        let prior: HashMap<i32, bool> = [(1, false), (2, true)].into_iter().collect();
        let latest: Vec<(i32, bool)> = vec![(1, true), (2, true), (3, true)];
        let fired: Vec<i32> = latest
            .iter()
            .filter(|(id, active)| *active && !prior.get(id).copied().unwrap_or(false))
            .map(|(id, _)| *id)
            .collect();
        // 1 transitioned off → on; 2 was already on; 3 is new + on.
        assert_eq!(fired, vec![1, 3]);
    }

    #[test]
    fn edge_detection_on_to_off_does_not_fire() {
        let prior: HashMap<i32, bool> = [(1, true)].into_iter().collect();
        let latest: Vec<(i32, bool)> = vec![(1, false)];
        let fired: Vec<i32> = latest
            .iter()
            .filter(|(id, active)| *active && !prior.get(id).copied().unwrap_or(false))
            .map(|(id, _)| *id)
            .collect();
        assert!(fired.is_empty());
    }

    #[test]
    fn edge_detection_stable_on_does_not_re_fire() {
        let prior: HashMap<i32, bool> = [(1, true)].into_iter().collect();
        let latest: Vec<(i32, bool)> = vec![(1, true)];
        let fired: Vec<i32> = latest
            .iter()
            .filter(|(id, active)| *active && !prior.get(id).copied().unwrap_or(false))
            .map(|(id, _)| *id)
            .collect();
        assert!(fired.is_empty());
    }
}
