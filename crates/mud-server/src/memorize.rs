//! Spell-slot recovery tick. Decrements `secs_remaining` on the head of the
//! recovery queue (`SpellSlots::in_flight`) for every online caster, and for every
//! caster mob running spell AI (a mob carries `MobAi` only with
//! `mob_ai.enabled` on, so with it off this tick sees players alone). A mob
//! recovers on the same ladder: sitting doubles the pace, and a meditating mob
//! (see `mob_ai::mob_idle_ai_tick`) doubles it again.
//!
//! Rate model:
//!   posture base    Standing/Kneeling=1, Sitting=2, Resting=3, Sleeping=4
//!     × Meditating  ×2 (only valid in Sitting/Resting/Kneeling per `cmd_meditate`)
//!     × Focus       × (100 + Focus) / 100 — each focus point is +1% recover speed
//!     ÷ Fighting    halved (rounded up) so a caster in a brawl still
//!                   trickles back slots; pairs with the user-flagged
//!                   "you don't become useless in a group" intent.
//!
//! Recovery is one queue per caster, in cast order, across all circles
//! (legacy `spellcasts` vector): only the head slot's timer runs, so three
//! casts take three recoveries back to back (30/60/90 for circle 1) rather
//! than all completing on the same tick.
//!
//! Mirrors legacy `charge_mem` + the regen event in `spell_mem.cpp`;
//! the legacy `get_spellslot_restore_rate` formula's race/class branches
//! aren't modeled, but the multiplier surface (posture / meditate /
//! focus / combat) is now richer than the early MVP.

use bevy_ecs::prelude::*;
use mud_world::{Fighting, Focus, Meditating, MobAi, Online, Posture, PostureKind, SpellSlots};

use crate::TickCount;
use crate::commands::{Concentrating, send_to};

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
        let mut q = world.query_filtered::<(
            Entity,
            &Posture,
            &mut SpellSlots,
            Option<&Meditating>,
            Option<&Focus>,
            Option<&Fighting>,
            Option<&Concentrating>,
        ), Or<(With<Online>, With<MobAi>)>>();
        for (entity, posture, mut slots, meditating, focus, fighting, concentrating) in
            q.iter_mut(world)
        {
            if slots.in_flight.is_empty() {
                continue;
            }
            let base = recover_seconds_per_tick(posture.0);
            let delta = effective_delta(
                base,
                meditating.is_some(),
                focus.map_or(0, |f| f.0)
                    + concentrating
                        .filter(|_| meditating.is_some())
                        .map_or(0, |c| c.0),
                fighting.is_some(),
            );
            // One global recovery queue, as in legacy: only the head slot's
            // timer runs, whatever its circle; the next starts after it.
            let just_restored: Vec<i32> = slots.recover_head(delta).into_iter().collect();
            if !just_restored.is_empty() {
                restored.push((entity, just_restored));
            }
        }
    }

    for (entity, circles) in restored {
        // A mob has no one to tell.
        if world.get::<Online>(entity).is_none() {
            continue;
        }
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

    // ---- sequential recovery queue (#133) ----

    use mud_world::{Online, SpellCooldown};

    fn cd(circle: i32, secs: i32) -> SpellCooldown {
        SpellCooldown {
            circle,
            secs_remaining: secs,
            total_secs: secs,
        }
    }

    fn slots_of(queue: Vec<SpellCooldown>) -> SpellSlots {
        let mut s = SpellSlots::default();
        s.in_flight = queue;
        s
    }

    /// A standing caster (1 s drained per tick) with the given queue.
    fn caster_with(world: &mut World, queue: Vec<SpellCooldown>) -> Entity {
        let slots = slots_of(queue);
        world
            .spawn((Online, Posture(PostureKind::Standing), slots))
            .id()
    }

    /// Run one recovery second (every `RECOVER_PERIOD_TICKS`th tick).
    fn one_second(world: &mut World, n: u64) {
        world.insert_resource(TickCount(n * RECOVER_PERIOD_TICKS));
        memorize_tick(world);
    }

    fn queue(world: &World, e: Entity) -> Vec<(i32, i32)> {
        world
            .get::<SpellSlots>(e)
            .unwrap()
            .in_flight
            .iter()
            .map(|c| (c.circle, c.secs_remaining))
            .collect()
    }

    #[test]
    fn three_casts_recover_one_after_another() {
        let mut world = World::new();
        let e = caster_with(&mut world, vec![cd(1, 30), cd(1, 30), cd(1, 30)]);
        let mut restored_at = Vec::new();
        for sec in 1..=95u64 {
            one_second(&mut world, sec);
            let left = world.get::<SpellSlots>(e).unwrap().in_flight.len();
            while restored_at.len() < 3 - left {
                restored_at.push(sec);
            }
        }
        // Total time is ~3x a single recovery, not 1x.
        assert_eq!(restored_at, vec![30, 60, 90]);
        assert!(queue(&world, e).is_empty());
    }

    #[test]
    fn only_the_head_timer_runs_and_the_next_starts_fresh() {
        let mut world = World::new();
        let e = caster_with(&mut world, vec![cd(1, 30), cd(1, 30), cd(2, 35)]);
        for sec in 1..=10u64 {
            one_second(&mut world, sec);
        }
        assert_eq!(queue(&world, e), vec![(1, 20), (1, 30), (2, 35)]);
        for sec in 11..=30u64 {
            one_second(&mut world, sec);
        }
        // The head came back; the second slot only now begins counting.
        assert_eq!(queue(&world, e), vec![(1, 30), (2, 35)]);
        one_second(&mut world, 31);
        assert_eq!(queue(&world, e), vec![(1, 29), (2, 35)]);
    }

    #[test]
    fn the_queue_spans_circles_in_cast_order() {
        let mut world = World::new();
        // A circle 3 slot cast first blocks the circle 1 slot behind it.
        let e = caster_with(&mut world, vec![cd(3, 50), cd(1, 30)]);
        for sec in 1..=49u64 {
            one_second(&mut world, sec);
        }
        assert_eq!(queue(&world, e), vec![(3, 1), (1, 30)]);
        one_second(&mut world, 50);
        assert_eq!(queue(&world, e), vec![(1, 30)]);
    }

    #[test]
    fn meditating_speeds_the_head_slot_and_overflow_is_not_carried() {
        let mut world = World::new();
        let e = caster_with(&mut world, vec![cd(1, 5), cd(1, 30)]);
        // Standing + meditating drains 2 s per tick: 5 s -> 3 -> 1 -> restored.
        world.entity_mut(e).insert(Meditating);
        for sec in 1..=3u64 {
            one_second(&mut world, sec);
        }
        assert_eq!(queue(&world, e), vec![(1, 30)], "overflow second is lost");
    }

    #[test]
    fn recover_head_reports_the_restored_circle() {
        let mut s = slots_of(vec![cd(4, 2), cd(1, 30)]);
        assert_eq!(s.recover_head(1), None);
        assert_eq!(s.recover_head(1), Some(4));
        assert_eq!(s.in_flight.len(), 1);
        assert_eq!(SpellSlots::default().recover_head(5), None);
    }

    #[test]
    fn queue_order_survives_a_save_round_trip() {
        let s = slots_of(vec![cd(3, 40), cd(1, 30), cd(2, 12)]);
        let back: SpellSlots = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        let order: Vec<i32> = back.in_flight.iter().map(|c| c.circle).collect();
        assert_eq!(order, vec![3, 1, 2]);
    }
}
