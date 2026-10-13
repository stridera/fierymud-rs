//! Summon-mount tiers: which mount a Paladin / Anti-Paladin gets from
//! `summonmount`, and the base pools its HP and movement are rolled around.
//!
//! The tiers are data: the `"MountType"` table (one row per tier and
//! alignment side), editable in Muditor and seeded by
//! `fierylib/data/sql/2026-10-13-mount-types.sql`. [`BUILTIN`] (the legacy
//! `mount_types` table of `act.other.cpp`) is the fallback for a side the
//! table has no usable rows for (or a database that predates the table), so
//! behaviour is identical with or without the patch.
//!
//! Selection: a side's rows sorted by tier; the first is the base mount and
//! always applies, each later tier applies while the summoner meets its level
//! and alignment floors (the walk stops at the first tier that is not met).

use bevy_ecs::prelude::Resource;
use mud_db::mount_types::MountTypeRow;

/// One tier of one alignment side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountTier {
    pub tier: i32,
    pub min_level: i32,
    /// Alignment magnitude: good needs `alignment >= this`, evil
    /// `alignment <= -this`.
    pub min_alignment: i32,
    /// Prototype `(zone, id)` of the mount mob.
    pub proto: (i32, i32),
    pub hp_bonus: i32,
    pub mv_bonus: i32,
}

/// `(tier, min_level, min_alignment, good mount, evil mount, hp, mv)`: the
/// legacy table, best tier last. gelding/mare, rouncy/destrier,
/// warhorse/nightmare, courser/shadowmare.
type BuiltinRow = (i32, i32, i32, (i32, i32), (i32, i32), i32, i32);

pub const BUILTIN: [BuiltinRow; 4] = [
    (1, 0, 0, (0, 48), (0, 47), 0, 0),
    (2, 30, 700, (0, 49), (0, 50), 0, 0),
    (3, 60, 900, (0, 45), (0, 46), 50, 50),
    (4, 90, 1000, (0, 51), (0, 52), 100, 100),
];

fn builtin_side(evil: bool) -> Vec<MountTier> {
    BUILTIN
        .iter()
        .map(
            |&(tier, min_level, min_alignment, good, bad, hp_bonus, mv_bonus)| MountTier {
                tier,
                min_level,
                min_alignment,
                proto: if evil { bad } else { good },
                hp_bonus,
                mv_bonus,
            },
        )
        .collect()
}

/// Which mount, and the base pools `cmd_summonmount` rolls around.
#[derive(Debug, PartialEq, Eq)]
pub struct MountPlan {
    pub proto: (i32, i32),
    /// Added to a 50..=100 roll for the mount's max HP.
    pub base_hp: i32,
    /// Added to a 0..=50 roll for the mount's max movement.
    pub base_mv: i32,
}

/// The mount tiers in effect, per alignment side. `Default` is the built-in
/// table.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct MountTypes {
    good: Vec<MountTier>,
    evil: Vec<MountTier>,
}

impl Default for MountTypes {
    fn default() -> Self {
        Self {
            good: builtin_side(false),
            evil: builtin_side(true),
        }
    }
}

impl MountTypes {
    /// Build from `"MountType"` rows. Rows with an unknown side or a negative
    /// level / alignment / bonus are skipped, and a side left with no usable
    /// row keeps its built-in tiers.
    #[must_use]
    pub fn from_rows<I>(rows: I) -> Self
    where
        I: IntoIterator<Item = MountTypeRow>,
    {
        let mut good = Vec::new();
        let mut evil = Vec::new();
        for r in rows {
            if r.min_level < 0 || r.min_alignment < 0 || r.hp_bonus < 0 || r.mv_bonus < 0 {
                continue;
            }
            let tier = MountTier {
                tier: r.tier,
                min_level: r.min_level,
                min_alignment: r.min_alignment,
                proto: (r.mob_zone_id, r.mob_id),
                hp_bonus: r.hp_bonus,
                mv_bonus: r.mv_bonus,
            };
            match r.alignment_side.trim().to_ascii_lowercase().as_str() {
                "good" => good.push(tier),
                "evil" => evil.push(tier),
                _ => {}
            }
        }
        let defaults = Self::default();
        for side in [&mut good, &mut evil] {
            side.sort_by_key(|t| t.tier);
        }
        Self {
            good: if good.is_empty() { defaults.good } else { good },
            evil: if evil.is_empty() { defaults.evil } else { evil },
        }
    }

    /// Number of tiers in effect across both sides.
    #[must_use]
    pub fn len(&self) -> usize {
        self.good.len() + self.evil.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.good.is_empty() && self.evil.is_empty()
    }

    /// Legacy `do_summon_mount`: the highest tier whose level and alignment
    /// floors the summoner meets; HP is `hp_bonus + 2 * level`, movement
    /// `mv_bonus + align/2 - 100` for a Paladin and `mv_bonus + 150 -
    /// align/4` for an Anti-Paladin.
    #[must_use]
    pub fn plan(&self, anti: bool, level: i32, alignment: i32) -> MountPlan {
        let tiers = if anti { &self.evil } else { &self.good };
        let mut chosen = &tiers[0];
        for t in &tiers[1..] {
            let aligned = if anti {
                -t.min_alignment >= alignment
            } else {
                t.min_alignment <= alignment
            };
            if t.min_level <= level && aligned {
                chosen = t;
            } else {
                break;
            }
        }
        let base_mv = if anti {
            chosen.mv_bonus + 150 - alignment / 4
        } else {
            chosen.mv_bonus + alignment / 2 - 100
        };
        MountPlan {
            proto: chosen.proto,
            base_hp: chosen.hp_bonus + 2 * level,
            base_mv,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rows the seed patch writes, one per tier and side.
    fn seeded_rows() -> Vec<MountTypeRow> {
        let mut rows = Vec::new();
        for &(tier, min_level, min_alignment, good, evil, hp, mv) in &BUILTIN {
            for (side, (zone, id)) in [("good", good), ("evil", evil)] {
                rows.push(MountTypeRow {
                    tier,
                    alignment_side: side.to_owned(),
                    min_level,
                    min_alignment,
                    mob_zone_id: zone,
                    mob_id: id,
                    hp_bonus: hp,
                    mv_bonus: mv,
                });
            }
        }
        rows
    }

    #[test]
    fn builtin_matches_the_legacy_table() {
        let t = MountTypes::default();
        assert_eq!(t.len(), 8);
        let p = t.plan(false, 15, 500);
        assert_eq!(p.proto, (0, 48));
        assert_eq!((p.base_hp, p.base_mv), (30, 500 / 2 - 100));
        assert_eq!(t.plan(false, 40, 600).proto, (0, 48));
        assert_eq!(t.plan(false, 40, 700).proto, (0, 49));
        let w = t.plan(false, 60, 900);
        assert_eq!(w.proto, (0, 45));
        assert_eq!((w.base_hp, w.base_mv), (50 + 120, 50 + 450 - 100));
        assert_eq!(t.plan(false, 100, 1000).proto, (0, 51));
        let a = t.plan(true, 15, -1000);
        assert_eq!(a.proto, (0, 47));
        assert_eq!(a.base_mv, 150 + 250);
        assert_eq!(t.plan(true, 40, -700).proto, (0, 50));
        assert_eq!(t.plan(true, 40, -699).proto, (0, 47));
        assert_eq!(t.plan(true, 100, -1000).proto, (0, 52));
    }

    #[test]
    fn seeded_rows_select_and_roll_like_the_builtin_table() {
        let db = MountTypes::from_rows(seeded_rows());
        let builtin = MountTypes::default();
        assert_eq!(db, builtin);
        for anti in [false, true] {
            for level in [0, 1, 15, 29, 30, 59, 60, 89, 90, 100, 105] {
                for step in -20..=20 {
                    let alignment = step * 50;
                    assert_eq!(
                        db.plan(anti, level, alignment),
                        builtin.plan(anti, level, alignment),
                        "anti={anti} level={level} alignment={alignment}"
                    );
                }
            }
        }
    }

    #[test]
    fn row_order_does_not_matter() {
        let mut rows = seeded_rows();
        rows.reverse();
        assert_eq!(MountTypes::from_rows(rows), MountTypes::default());
    }

    #[test]
    fn edited_rows_change_the_choice() {
        let mut rows = seeded_rows();
        for r in &mut rows {
            if r.tier == 2 && r.alignment_side == "good" {
                r.min_level = 20;
                r.mob_id = 99;
                r.hp_bonus = 7;
            }
        }
        let t = MountTypes::from_rows(rows);
        let p = t.plan(false, 20, 700);
        assert_eq!(p.proto, (0, 99));
        assert_eq!(p.base_hp, 7 + 40);
        // The evil side is untouched.
        assert_eq!(t.plan(true, 20, -700).proto, (0, 47));
    }

    #[test]
    fn a_side_without_usable_rows_keeps_the_builtin_tiers() {
        let rows: Vec<_> = seeded_rows()
            .into_iter()
            .filter(|r| r.alignment_side == "good")
            .collect();
        assert_eq!(MountTypes::from_rows(rows), MountTypes::default());
        let junk = vec![MountTypeRow {
            tier: 1,
            alignment_side: "neutral".to_owned(),
            min_level: 0,
            min_alignment: 0,
            mob_zone_id: 0,
            mob_id: 1,
            hp_bonus: 0,
            mv_bonus: 0,
        }];
        assert_eq!(MountTypes::from_rows(junk), MountTypes::default());
        let negative = vec![MountTypeRow {
            tier: 1,
            alignment_side: "good".to_owned(),
            min_level: -1,
            min_alignment: 0,
            mob_zone_id: 0,
            mob_id: 1,
            hp_bonus: 0,
            mv_bonus: 0,
        }];
        assert_eq!(MountTypes::from_rows(negative), MountTypes::default());
    }
}
