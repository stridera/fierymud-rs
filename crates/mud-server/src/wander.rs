//! Mob wandering tick. Once every `WANDER_PERIOD_TICKS`, every
//! eligible mob picks a random open exit and walks one room. The
//! schema's `MobBehavior` flags gate participation:
//!
//! - `Sentinel` mobs never wander.
//! - `StayZone` mobs only walk through exits that stay in the
//!   same zone (zone match on the destination room's `WorldKey`).
//! - Mobs in combat (`Fighting` component) never wander.
//! - Mounts being ridden (`RiddenBy`) never wander — the rider
//!   moves them via the cardinal-direction commands.
//!
//! Cadence is loose (~30 game seconds) so a player walking through
//! a populated zone doesn't see mobs constantly migrating; tight
//! enough that a long sit watches the world breathe.

use bevy_ecs::prelude::*;
use mud_db::enums::{ExitState, MobBehavior};
use mud_world::{
    AttachedTriggers, ExitData, Exits, Fighting, Item, Located, Mob, MobBehaviors, Named,
    RiddenBy, WorldKey,
};

use crate::TickCount;
use crate::commands::{broadcast_room_except_players_rendered, direction_name, opposite};

/// One wander check every 300 ticks (= 30s real-time at 10Hz).
/// Each tick a fixed fraction of eligible mobs actually move —
/// see `WANDER_CHANCE_DENOM`.
const WANDER_PERIOD_TICKS: u64 = 300;
/// Per-eligible-mob chance to actually wander on a wander tick.
/// 1 in 4 means roughly one move every 2 minutes per mob —
/// ambient, not chaotic.
const WANDER_CHANCE_DENOM: u32 = 4;

/// Scavenger pickup tick fires every 100 ticks (= 10s real-time).
/// Each Scavenger-flagged mob in a room with a free-floor item
/// picks one up — at most one per tick per mob, so a busy zone
/// doesn't see mobs hoover the floor in a single frame.
const SCAVENGER_PERIOD_TICKS: u64 = 100;

pub fn wander_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(WANDER_PERIOD_TICKS) {
        return;
    }
    // Snapshot every eligible mob with its current room. The full
    // gate runs in Rust here; the per-step exit roll happens later.
    let candidates: Vec<(Entity, Entity)> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, Option<&MobBehaviors>, Option<&AttachedTriggers>),
            (With<Mob>, Without<Fighting>, Without<RiddenBy>),
        >();
        q.iter(world)
            .filter(|(_, _, beh, _)| !beh.is_some_and(|b| b.has(MobBehavior::Sentinel)))
            .map(|(e, l, _, _)| (e, l.0))
            .collect()
    };
    if candidates.is_empty() {
        return;
    }
    let mut moves: Vec<(Entity, Entity, Entity, mud_db::enums::Direction)> = Vec::new();
    for (mob, room) in candidates {
        if rand::random_range(0..WANDER_CHANCE_DENOM) != 0 {
            continue;
        }
        // StayZone-aware exit pool: the destination must be in the
        // same zone for StayZone-flagged mobs. Other mobs walk
        // wherever an open exit leads.
        let stay_zone = world
            .get::<MobBehaviors>(mob)
            .is_some_and(|b| b.has(MobBehavior::StayZone));
        let mob_zone = world.get::<WorldKey>(mob).map(|k| k.zone);
        let candidates_dir: Vec<(mud_db::enums::Direction, Entity)> = world
            .get::<Exits>(room)
            .map(|exits| {
                exits
                    .0
                    .iter()
                    .filter_map(|(dir, ed): (_, &ExitData)| {
                        if ed.state != ExitState::Open {
                            return None;
                        }
                        let to = ed.to?;
                        if stay_zone {
                            let target_zone = world.get::<WorldKey>(to).map(|k| k.zone);
                            if target_zone != mob_zone {
                                return None;
                            }
                        }
                        Some((*dir, to))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if candidates_dir.is_empty() {
            continue;
        }
        let pick = rand::random_range(0..candidates_dir.len());
        let (dir, target) = candidates_dir[pick];
        moves.push((mob, room, target, dir));
    }
    // Apply moves. Done in a separate pass so the candidate
    // snapshot's borrows are gone before we mutate `Located`.
    for (mob, from_room, target_room, dir) in moves {
        let mob_name = world
            .get::<Named>(mob)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        if mob_name.is_empty() {
            continue;
        }
        broadcast_room_except_players_rendered(
            world,
            from_room,
            &[mob],
            &format!("{mob_name} leaves {}.\r\n", direction_name(dir)),
        );
        if let Some(mut l) = world.get_mut::<Located>(mob) {
            l.0 = target_room;
        }
        let arrival_dir =
            opposite(dir).map_or("nearby".to_string(), |d| format!("the {}", direction_name(d)));
        broadcast_room_except_players_rendered(
            world,
            target_room,
            &[mob],
            &format!("{mob_name} arrives from {arrival_dir}.\r\n"),
        );
    }
}

/// Mob `Scavenger` behavior: every Scavenger-flagged mob picks up
/// one floor item per tick from its current room. Items already
/// inside containers (Located on something other than the room)
/// or worn (`EquippedSlot` set) are skipped. Cadence is 10s so
/// even a Scavenger-heavy zone doesn't strip the floor in one
/// frame; a player dropping a stack still gets to grab some back.
pub fn scavenger_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(SCAVENGER_PERIOD_TICKS) {
        return;
    }
    let scavengers: Vec<(Entity, Entity)> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &MobBehaviors),
            (With<Mob>, Without<Fighting>),
        >();
        q.iter(world)
            .filter(|(_, _, beh)| beh.has(MobBehavior::Scavenger))
            .map(|(e, l, _)| (e, l.0))
            .collect()
    };
    for (mob, room) in scavengers {
        // Pick the first free-floor item in the room — items
        // Located on other actors or inside containers stay put.
        let target_item: Option<(Entity, String)> = {
            let mut q = world.query_filtered::<(Entity, &Located, &Named), With<Item>>();
            q.iter(world)
                .find(|(_, l, _)| l.0 == room)
                .map(|(e, _, n)| (e, n.name.clone()))
        };
        let Some((item, item_name)) = target_item else {
            continue;
        };
        if let Some(mut l) = world.get_mut::<Located>(item) {
            l.0 = mob;
        }
        let mob_name = world
            .get::<Named>(mob)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        if !mob_name.is_empty() {
            broadcast_room_except_players_rendered(
                world,
                room,
                &[],
                &format!("{mob_name} picks up {item_name}.\r\n"),
            );
        }
    }
}
