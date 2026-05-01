//! Spell memorization tick. Decrements `prep_secs_remaining` on
//! every `MemorizedSpells.entries` for online, non-fighting players,
//! at a rate that depends on posture: Sleeping fastest, Resting
//! slower, Sitting slowest, Standing/Kneeling/Fighting halt.
//!
//! When a slot's `prep_secs_remaining` hits zero, the slot flips to
//! `ready=true` and the player gets a "you've finished memorizing X"
//! line. `cast` consumes only `ready=true` entries (see commands.rs
//! `invoke_ability`'s memorization gate).

use bevy_ecs::prelude::*;
use mud_world::{
    AbilityCatalog, Fighting, MemorizedSpells, Online, Posture, PostureKind,
};

use crate::commands::send_to;
use crate::TickCount;

/// Tick once per second.
const MEM_PERIOD_TICKS: u64 = 10;

fn prep_seconds_per_tick(p: PostureKind) -> i32 {
    match p {
        PostureKind::Sleeping => 3,
        PostureKind::Resting => 2,
        PostureKind::Sitting => 1,
        PostureKind::Standing | PostureKind::Kneeling => 0,
    }
}

#[allow(clippy::needless_pass_by_value)]
pub fn memorize_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(MEM_PERIOD_TICKS) {
        return;
    }

    // Collect entries to advance + which slot indexes flip ready.
    // Per-entity vec of newly-ready ability_ids so we can emit the
    // "finished memorizing" line after the World mutation.
    let mut newly_ready: Vec<(Entity, Vec<i32>)> = Vec::new();
    {
        let mut q = world
            .query_filtered::<
                (Entity, &Posture, &mut MemorizedSpells),
                (With<Online>, Without<Fighting>),
            >();
        for (entity, posture, mut mem) in q.iter_mut(world) {
            let delta = prep_seconds_per_tick(posture.0);
            if delta == 0 {
                continue;
            }
            let mut just_ready: Vec<i32> = Vec::new();
            for e in &mut mem.entries {
                if e.ready {
                    continue;
                }
                e.prep_secs_remaining -= delta;
                if e.prep_secs_remaining <= 0 {
                    e.ready = true;
                    e.prep_secs_remaining = 0;
                    just_ready.push(e.ability_id);
                }
            }
            if !just_ready.is_empty() {
                newly_ready.push((entity, just_ready));
            }
        }
    }

    if newly_ready.is_empty() {
        return;
    }
    // Resolve ability names for the just-finished slots.
    let names: Vec<(Entity, Vec<String>)> = newly_ready
        .into_iter()
        .map(|(e, ids)| {
            let catalog = world.resource::<AbilityCatalog>();
            let names: Vec<String> = ids
                .iter()
                .map(|id| {
                    catalog
                        .by_name
                        .values()
                        .find(|d| d.id == *id)
                        .map_or_else(String::new, |d| d.plain_name.clone())
                })
                .collect();
            (e, names)
        })
        .collect();
    for (entity, names) in names {
        for name in names {
            send_to(
                world,
                entity,
                format!("You finish memorizing {name}.\r\n"),
            );
        }
    }
}
