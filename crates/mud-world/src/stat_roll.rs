//! Starting-stat roller for new characters (and the admin `reroll`).
//!
//! Core stats live on the game's 0..100 scale (`CoreStats::bonus` is
//! `(score - 50) / 5`, a legacy-imported character carries natural
//! scores of 30..100). This is a port of the legacy
//! `roll_natural_abils()` (`fierymud_legacy/src/chars.cpp`), NOT the
//! D&D 3d6 (3..18) roll, which on this scale lands every stat at a
//! -8..-10 bonus. The legacy distribution:
//!
//! * each of the six stats is `2 * (best 5 of 6 rolls of d3..d10)`
//!   (range 30..100);
//! * if the six sum to 450 or less, +5 is handed to random stats
//!   (never above 100) until the sum exceeds 450;
//! * the values are then assigned to stats by the class's priority
//!   order. We have no class priority table in the DB yet, so values
//!   are assigned in random order (follow-up: `Classes` stat order).
//!
//! Legacy then scaled the natural score by the race's attribute
//! scale. The DB's `Races.max_*` columns are those scales expressed
//! as a 0..100 ceiling, so we clamp the rolled value to the race cap.
//!
//! Muditor's web creator mirrors this in
//! `muditor/apps/api/src/characters/stat-roll.ts`; keep them in step.

use crate::components::CoreStats;
use crate::resources::{RaceCatalog, RaceStatCaps};

/// Top of the stat scale.
pub const STAT_SCALE_MAX: i32 = 100;
/// Legacy `roll_natural_abils`: raise the total to above this.
const MIN_TOTAL_EXCLUSIVE: i32 = 450;
/// Legacy only adds +5 to a stat that is at or below this value.
const BONUS_STEP_CEILING: i32 = 95;

/// Roll the six natural stat values, highest first. `roll(lo, hi)`
/// must return a uniform integer in `lo..=hi`.
pub fn roll_natural_pool(roll: &mut impl FnMut(i32, i32) -> i32) -> [i32; 6] {
    let mut pool = [0i32; 6];
    for slot in &mut pool {
        let mut dice = [0i32; 6];
        for d in &mut dice {
            *d = roll(3, 10);
        }
        let total: i32 = dice.iter().sum::<i32>() - dice.iter().copied().min().unwrap_or(0);
        *slot = total * 2;
    }
    let mut total: i32 = pool.iter().sum();
    while total <= MIN_TOTAL_EXCLUSIVE {
        let j = usize::try_from(roll(0, 5)).unwrap_or(0).min(5);
        if pool[j] <= BONUS_STEP_CEILING {
            pool[j] += 5;
            total += 5;
        }
    }
    pool.sort_unstable_by(|a, b| b.cmp(a));
    pool
}

/// Assign a rolled pool to the six stats in random order and clamp
/// each to the race's cap (itself capped at [`STAT_SCALE_MAX`]).
#[must_use]
pub fn assign_pool(
    mut pool: [i32; 6],
    caps: &RaceStatCaps,
    roll: &mut impl FnMut(i32, i32) -> i32,
) -> CoreStats {
    for i in (1..pool.len()).rev() {
        let j = usize::try_from(roll(0, i32::try_from(i).unwrap_or(0))).unwrap_or(0);
        pool.swap(i, j.min(i));
    }
    let cap = |limit: i32, v: i32| v.min(limit.min(STAT_SCALE_MAX));
    CoreStats {
        strength: cap(caps.strength, pool[0]),
        intelligence: cap(caps.intelligence, pool[1]),
        wisdom: cap(caps.wisdom, pool[2]),
        dexterity: cap(caps.dexterity, pool[3]),
        constitution: cap(caps.constitution, pool[4]),
        charisma: cap(caps.charisma, pool[5]),
    }
}

/// Roll a full starting block for `race` (character creation, the
/// creation-time `reroll` prompt, and the admin `reroll` command).
/// An unknown race is only limited by the scale ceiling.
#[must_use]
pub fn roll_starting_stats(race_catalog: &RaceCatalog, race: &str) -> CoreStats {
    let caps = RaceStatCaps {
        strength: race_catalog.stat_cap(race, "strength", STAT_SCALE_MAX),
        dexterity: race_catalog.stat_cap(race, "dexterity", STAT_SCALE_MAX),
        constitution: race_catalog.stat_cap(race, "constitution", STAT_SCALE_MAX),
        intelligence: race_catalog.stat_cap(race, "intelligence", STAT_SCALE_MAX),
        wisdom: race_catalog.stat_cap(race, "wisdom", STAT_SCALE_MAX),
        charisma: race_catalog.stat_cap(race, "charisma", STAT_SCALE_MAX),
    };
    let mut roll = |lo: i32, hi: i32| rand::random_range(lo..=hi);
    let pool = roll_natural_pool(&mut roll);
    assign_pool(pool, &caps, &mut roll)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift so the tests need no seeded RNG type.
    fn lcg(seed: u64) -> impl FnMut(i32, i32) -> i32 {
        let mut s = seed | 1;
        move |lo, hi| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let span = u64::try_from(hi - lo + 1).unwrap();
            lo + i32::try_from(s % span).unwrap()
        }
    }

    #[test]
    fn pool_is_on_the_zero_to_hundred_scale() {
        let mut roll = lcg(7);
        for _ in 0..2000 {
            let pool = roll_natural_pool(&mut roll);
            assert!(pool.windows(2).all(|w| w[0] >= w[1]), "sorted high to low");
            for v in pool {
                assert!((30..=100).contains(&v), "value {v} out of 30..=100");
            }
            assert!(pool.iter().sum::<i32>() > 450, "legacy minimum total");
        }
    }

    #[test]
    fn worst_dice_still_get_the_legacy_floor_bonus() {
        // Every die 3: each stat 2 * 15 = 30, total 180, topped up to > 450.
        let mut next_bonus = 0;
        let mut roll = |lo: i32, hi: i32| {
            if (lo, hi) == (3, 10) {
                3
            } else {
                next_bonus += 1;
                (next_bonus - 1) % 6
            }
        };
        let pool = roll_natural_pool(&mut roll);
        assert!(pool.iter().sum::<i32>() > 450);
        assert!(pool.iter().all(|&v| v <= 100));
    }

    #[test]
    fn best_dice_hit_the_ceiling() {
        let mut roll = |lo: i32, hi: i32| if (lo, hi) == (3, 10) { 10 } else { 0 };
        assert_eq!(roll_natural_pool(&mut roll), [100; 6]);
    }

    #[test]
    fn assignment_clamps_to_race_caps() {
        let caps = RaceStatCaps {
            strength: 64,
            dexterity: 80,
            constitution: 64,
            intelligence: 88,
            wisdom: 86,
            charisma: 78,
        };
        let mut roll = lcg(11);
        for _ in 0..500 {
            let pool = roll_natural_pool(&mut roll);
            let s = assign_pool(pool, &caps, &mut roll);
            assert!(s.strength <= 64 && s.constitution <= 64);
            assert!(s.dexterity <= 80 && s.intelligence <= 88);
            assert!(s.wisdom <= 86 && s.charisma <= 78);
            let mut got = [
                s.strength,
                s.dexterity,
                s.constitution,
                s.intelligence,
                s.wisdom,
                s.charisma,
            ];
            got.sort_unstable();
            assert!(got[0] >= 30);
        }
    }

    #[test]
    fn unknown_race_is_limited_only_by_the_scale() {
        let catalog = RaceCatalog::default();
        for _ in 0..200 {
            let s = roll_starting_stats(&catalog, "no-such-race");
            for v in [
                s.strength,
                s.intelligence,
                s.wisdom,
                s.dexterity,
                s.constitution,
                s.charisma,
            ] {
                assert!((30..=100).contains(&v));
                // Average-or-better bonus: never the old 3d6 -8..-10.
                assert!(CoreStats::bonus(v) >= -4);
            }
        }
    }

    #[test]
    fn new_character_bonuses_are_not_deeply_negative() {
        let mut roll = lcg(99);
        let mut sum = 0i32;
        let n = 3000;
        for _ in 0..n {
            for v in roll_natural_pool(&mut roll) {
                sum += CoreStats::bonus(v);
            }
        }
        let mean = f64::from(sum) / f64::from(n * 6);
        assert!(mean > 1.0, "mean bonus {mean} should be positive");
    }
}
