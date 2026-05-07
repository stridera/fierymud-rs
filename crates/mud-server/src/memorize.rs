//! Spell-slot recovery tick. Decrements `secs_remaining` on every
//! `SpellSlots.in_flight` cooldown for online players, at a rate
//! that depends on posture: Sleeping fastest, Resting slower,
//! Sitting slowest, Standing/Kneeling/Fighting halt.
//!
//! When a slot's `secs_remaining` hits zero the cooldown is removed
//! (slot is free again) and the player gets a "Your circle N slot
//! is restored." line. `cast` reads the same component to check
//! `used_in_circle(C) < max_for(class, level, C)`.
//!
//! Mirrors legacy `charge_mem` + the regen event in `spell_mem.cpp` —
//! the recover-rate dependence on focus/race/class is approximated
//! today as a flat per-posture multiplier; the legacy
//! `get_spellslot_restore_rate` formula can be plumbed in later if
//! the tuning matters.

use bevy_ecs::prelude::*;
use mud_world::{Fighting, Meditating, Online, Posture, PostureKind, SpellSlots};

use crate::TickCount;
use crate::commands::send_to;

/// Tick once per second.
const RECOVER_PERIOD_TICKS: u64 = 10;

fn recover_seconds_per_tick(p: PostureKind) -> i32 {
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
    if !tick.is_multiple_of(RECOVER_PERIOD_TICKS) {
        return;
    }

    // Per-entity vec of restored circles so we can emit the
    // "slot restored" line after the World mutation. The
    // `Meditating` marker doubles the recover delta — same speed-up
    // intent as the legacy meditate skill multiplier.
    let mut restored: Vec<(Entity, Vec<i32>)> = Vec::new();
    {
        let mut q = world.query_filtered::<
            (Entity, &Posture, &mut SpellSlots, Option<&Meditating>),
            (With<Online>, Without<Fighting>),
        >();
        for (entity, posture, mut slots, meditating) in q.iter_mut(world) {
            let mut delta = recover_seconds_per_tick(posture.0);
            if delta == 0 {
                continue;
            }
            if meditating.is_some() {
                delta *= 2;
            }
            let mut just_restored: Vec<i32> = Vec::new();
            slots.in_flight.retain_mut(|cd| {
                cd.secs_remaining -= delta;
                if cd.secs_remaining <= 0 {
                    just_restored.push(cd.circle);
                    false
                } else {
                    true
                }
            });
            if !just_restored.is_empty() {
                restored.push((entity, just_restored));
            }
        }
    }

    for (entity, circles) in restored {
        for circle in circles {
            send_to(
                world,
                entity,
                format!("Your circle {circle} slot is restored.\r\n"),
            );
        }
    }
}
