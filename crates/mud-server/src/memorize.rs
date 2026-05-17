//! Spell-slot recovery tick. Decrements `secs_remaining` on the
//! oldest in-flight cooldown per circle for every online caster.
//!
//! Rate model:
//!   posture base    Standing/Kneeling=1, Sitting=2, Resting=3, Sleeping=4
//!     × Meditating  ×2 (only valid in Sitting/Resting/Kneeling per `cmd_meditate`)
//!     × Focus       × (100 + Focus) / 100 — each focus point is +1% recover speed
//!     ÷ Fighting    halved (rounded up) so a caster in a brawl still
//!                   trickles back slots; pairs with the user-flagged
//!                   "you don't become useless in a group" intent.
//!
//! Per-circle sequential queueing means casting seven circle-1 spells
//! at once spreads them into a 30/60/90/... recover ladder rather than
//! all completing on the same tick — strategy matters.
//!
//! Mirrors legacy `charge_mem` + the regen event in `spell_mem.cpp`;
//! the legacy `get_spellslot_restore_rate` formula's race/class branches
//! aren't modeled, but the multiplier surface (posture / meditate /
//! focus / combat) is now richer than the early MVP.

use bevy_ecs::prelude::*;
use mud_world::{Fighting, Focus, Meditating, Online, Posture, PostureKind, SpellSlots};

use crate::TickCount;
use crate::commands::send_to;

/// Tick once per second.
const RECOVER_PERIOD_TICKS: u64 = 10;

fn recover_seconds_per_tick(p: PostureKind) -> i32 {
    match p {
        PostureKind::Sleeping => 4,
        PostureKind::Resting => 3,
        PostureKind::Sitting => 2,
        PostureKind::Standing | PostureKind::Kneeling => 1,
    }
}

/// Apply the meditate/focus/combat multipliers to the posture base.
/// Returned delta is the number of cooldown-seconds drained on this
/// tick. Always ≥ 0; clamped to ≥ 1 for active casters (otherwise
/// a tiny base with the combat halver would zero out and casters
/// would have no in-combat trickle at all).
fn effective_delta(base: i32, meditating: bool, focus_pts: i32, fighting: bool) -> i32 {
    let mut delta = base;
    if meditating {
        delta = delta.saturating_mul(2);
    }
    if focus_pts > 0 {
        // (100 + focus) / 100 with integer math. Truncates fractional
        // gain; focus=10 → +10%, focus=100 → +100% (≈ meditate).
        delta = delta
            .saturating_mul(100i32.saturating_add(focus_pts))
            .saturating_div(100);
    }
    if fighting {
        // Round up so a base=1 combatant still drains 1 sec/tick
        // instead of zero. Pairs with the doc-note intent above.
        delta = (delta + 1) / 2;
    }
    delta.max(1)
}

#[allow(clippy::needless_pass_by_value)]
pub fn memorize_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(RECOVER_PERIOD_TICKS) {
        return;
    }

    // Per-entity vec of restored circles so we can emit the
    // "slot restored" line after the World mutation.
    let mut restored: Vec<(Entity, Vec<i32>)> = Vec::new();
    {
        let mut q = world.query_filtered::<
            (Entity, &Posture, &mut SpellSlots, Option<&Meditating>, Option<&Focus>, Option<&Fighting>),
            With<Online>,
        >();
        for (entity, posture, mut slots, meditating, focus, fighting) in q.iter_mut(world) {
            if slots.in_flight.is_empty() {
                continue;
            }
            let base = recover_seconds_per_tick(posture.0);
            let delta = effective_delta(
                base,
                meditating.is_some(),
                focus.map_or(0, |f| f.0),
                fighting.is_some(),
            );
            // Sequential per-circle recovery — see the head-of-file
            // doc note. Only the lowest-remaining entry per circle
            // is ticked; siblings in the same circle wait their turn.
            let mut indices_to_tick: std::collections::HashMap<i32, (usize, i32)> =
                std::collections::HashMap::new();
            for (i, cd) in slots.in_flight.iter().enumerate() {
                let entry = indices_to_tick
                    .entry(cd.circle)
                    .or_insert((i, cd.secs_remaining));
                if cd.secs_remaining < entry.1 {
                    *entry = (i, cd.secs_remaining);
                }
            }
            let tick_indices: std::collections::HashSet<usize> =
                indices_to_tick.values().map(|(i, _)| *i).collect();
            let mut just_restored: Vec<i32> = Vec::new();
            let mut idx = 0;
            slots.in_flight.retain_mut(|cd| {
                let keep = if tick_indices.contains(&idx) {
                    cd.secs_remaining -= delta;
                    if cd.secs_remaining <= 0 {
                        just_restored.push(cd.circle);
                        false
                    } else {
                        true
                    }
                } else {
                    true
                };
                idx += 1;
                keep
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Lock in the multiplier ladder so a tuning tweak doesn't
    /// silently drop in-combat recovery to zero or invert the
    /// meditate / focus relationship.
    #[test]
    fn effective_delta_multiplier_ladder() {
        // Standing + nothing → 1 sec/tick.
        assert_eq!(effective_delta(1, false, 0, false), 1);
        // Resting (base 3) + meditate → 6.
        assert_eq!(effective_delta(3, true, 0, false), 6);
        // Resting + meditate + focus 50 → 6 * 1.5 = 9.
        assert_eq!(effective_delta(3, true, 50, false), 9);
        // Standing + fighting → still 1 (rounded up from 0.5).
        assert_eq!(effective_delta(1, false, 0, true), 1);
        // Sitting (base 2) + fighting → 1.
        assert_eq!(effective_delta(2, false, 0, true), 1);
        // Resting (base 3) + fighting → 2 (rounded up).
        assert_eq!(effective_delta(3, false, 0, true), 2);
        // Resting + meditate + fighting → (6+1)/2 = 3. (Meditate
        // technically refuses while Fighting per cmd_meditate, but the
        // math should still resolve sensibly if the flags coexist.)
        assert_eq!(effective_delta(3, true, 0, true), 3);
        // Focus 100 ≈ meditate (doubles): standing base=1, +100% → 2.
        assert_eq!(effective_delta(1, false, 100, false), 2);
        // Always at least 1 when there's any recovery to do.
        assert_eq!(effective_delta(1, false, -50, true), 1);
    }
}
