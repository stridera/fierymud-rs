//! Legacy liquid numbering for Lua `val2` of a drink container.
//!
//! `CircleMUD` scripts compare the liquid of a container by its position in the
//! legacy liquid table (`object.val2 == 11` is tea). That position is data now:
//! `"Liquids".legacy_index`, with `legacy_code` the label drink-container
//! objects store (`WHISKY`, `DARKALE`, `FIREBRT`). [`LiquidLegacyIndex`] answers
//! "which number is this container's liquid":
//!
//! 1. the liquid's `legacy_code` from the database;
//! 2. the built-in [`BUILTIN_CODES`] position (the fallback when the database
//!    has no legacy indices, i.e. the schema or patch is not deployed yet);
//! 3. the liquid's alias or display name from the database;
//! 4. otherwise 0, as before.
//!
//! A liquid added in Muditor has no `legacy_index`; it gets a stable number
//! after the highest one in use (rows without one are numbered in id order).

use std::collections::HashMap;

use bevy_ecs::prelude::Resource;

/// Legacy liquid labels in `CircleMUD` table order. Only consulted when the
/// database carries no `legacy_index` values; kept so a game binary deployed
/// before `fierylib/data/sql/2026-10-12-liquid-index.sql` still numbers the 42
/// legacy liquids exactly as before.
pub const BUILTIN_CODES: [&str; 42] = [
    "WATER",
    "BEER",
    "WINE",
    "ALE",
    "DARKALE",
    "WHISKY",
    "LEMONADE",
    "FIREBRT",
    "LOCALSPC",
    "SLIME",
    "MILK",
    "TEA",
    "COFFEE",
    "BLOOD",
    "SALTWATER",
    "RUM",
    "NECTAR",
    "SAKE",
    "CIDER",
    "TOMATOSOUP",
    "POTATOSOUP",
    "CHAI",
    "APPLEJUICE",
    "ORNGJUICE",
    "PNAPLJUICE",
    "GRAPEJUICE",
    "POMJUICE",
    "MELONAE",
    "COCOA",
    "ESPRESSO",
    "CAPPUCCINO",
    "MANGOLASSI",
    "ROSEWATER",
    "GREENTEA",
    "CHAMOMILE",
    "GIN",
    "BRANDY",
    "MEAD",
    "CHAMPAGNE",
    "VODKA",
    "TEQUILA",
    "ABSINTHE",
];

/// One `Liquids` row as the numbering needs it.
#[derive(Debug, Clone)]
pub struct LegacyLiquidRow {
    pub id: i32,
    pub alias: String,
    pub name: String,
    pub legacy_index: Option<i32>,
    pub legacy_code: Option<String>,
}

#[derive(Resource, Debug, Default, Clone)]
pub struct LiquidLegacyIndex {
    /// Lowercase `legacy_code` -> index.
    by_code: HashMap<String, i64>,
    /// Lowercase alias / display name -> index.
    by_name: HashMap<String, i64>,
}

impl LiquidLegacyIndex {
    /// Build from the database rows. When no row carries a `legacy_index`
    /// (column missing or patch not applied) the result is empty and
    /// [`index_of`](Self::index_of) uses [`BUILTIN_CODES`] alone.
    #[must_use]
    pub fn from_rows(rows: &[LegacyLiquidRow]) -> Self {
        let mut out = Self::default();
        let Some(max_indexed) = rows.iter().filter_map(|r| r.legacy_index).max() else {
            return out;
        };
        // New liquids number after both the DB maximum and the built-in table, so
        // they can never collide with a legacy number a script compares against.
        let builtin_max = i64::try_from(BUILTIN_CODES.len() - 1).unwrap_or(0);
        let mut next = i64::from(max_indexed).max(builtin_max) + 1;
        let mut ordered: Vec<&LegacyLiquidRow> = rows.iter().collect();
        ordered.sort_by_key(|r| r.id);
        for r in ordered {
            let idx = if let Some(i) = r.legacy_index {
                i64::from(i)
            } else {
                let i = next;
                next += 1;
                i
            };
            if let Some(code) = r.legacy_code.as_deref().map(norm).filter(|c| !c.is_empty()) {
                out.by_code.entry(code).or_insert(idx);
            }
            out.by_name.entry(norm(&r.alias)).or_insert(idx);
            out.by_name.entry(norm(&r.name)).or_insert(idx);
        }
        out
    }

    /// Number of liquids with a database-assigned index.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// The legacy number of the liquid named `liquid` (a container's liquid
    /// label or alias), 0 when unknown.
    #[must_use]
    pub fn index_of(&self, liquid: &str) -> i64 {
        let key = norm(liquid);
        if let Some(i) = self.by_code.get(&key) {
            return *i;
        }
        if let Some(i) = BUILTIN_CODES
            .iter()
            .position(|c| c.eq_ignore_ascii_case(&key))
        {
            return i64::try_from(i).unwrap_or(0);
        }
        self.by_name.get(&key).copied().unwrap_or(0)
    }
}

fn norm(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        id: i32,
        alias: &str,
        name: &str,
        idx: Option<i32>,
        code: Option<&str>,
    ) -> LegacyLiquidRow {
        LegacyLiquidRow {
            id,
            alias: alias.into(),
            name: name.into(),
            legacy_index: idx,
            legacy_code: code.map(str::to_string),
        }
    }

    /// The 42 legacy liquids as `2026-10-12-liquid-index.sql` leaves them.
    fn patched_rows() -> Vec<LegacyLiquidRow> {
        BUILTIN_CODES
            .iter()
            .enumerate()
            .map(|(i, code)| {
                let alias = code.to_ascii_lowercase();
                row(
                    i32::try_from(i).unwrap() + 1,
                    &alias,
                    &alias,
                    Some(i32::try_from(i).unwrap()),
                    Some(code),
                )
            })
            .collect()
    }

    #[test]
    fn unpatched_database_numbers_the_legacy_liquids_by_the_builtin_table() {
        let none = LiquidLegacyIndex::from_rows(&[row(1, "water", "water", None, None)]);
        assert!(none.is_empty());
        for (i, code) in BUILTIN_CODES.iter().enumerate() {
            assert_eq!(none.index_of(code), i64::try_from(i).unwrap(), "{code}");
            assert_eq!(
                none.index_of(&code.to_ascii_lowercase()),
                i64::try_from(i).unwrap()
            );
        }
        assert_eq!(none.index_of("nonsense"), 0);
    }

    #[test]
    fn patched_database_matches_the_builtin_numbers_for_the_42_legacy_liquids() {
        let db = LiquidLegacyIndex::from_rows(&patched_rows());
        let none = LiquidLegacyIndex::default();
        for code in BUILTIN_CODES {
            assert_eq!(db.index_of(code), none.index_of(code), "{code}");
        }
        assert_eq!(db.index_of("TEA"), 11);
        assert_eq!(db.index_of(" whisky "), 5);
    }

    #[test]
    fn alias_and_name_resolve_for_poured_containers() {
        let mut rows = patched_rows();
        rows[11] = row(12, "black-tea", "black tea", Some(11), Some("TEA"));
        let db = LiquidLegacyIndex::from_rows(&rows);
        assert_eq!(db.index_of("black-tea"), 11);
        assert_eq!(db.index_of("Black Tea"), 11);
    }

    #[test]
    fn liquids_without_an_index_number_after_the_max_in_id_order() {
        let mut rows = patched_rows();
        rows.push(row(60, "mulled-wine", "mulled wine", None, None));
        rows.push(row(50, "root-beer", "root beer", None, Some("ROOTBEER")));
        let db = LiquidLegacyIndex::from_rows(&rows);
        assert_eq!(db.index_of("root-beer"), 42);
        assert_eq!(db.index_of("ROOTBEER"), 42);
        assert_eq!(db.index_of("mulled wine"), 43);
        // Legacy numbers are untouched.
        assert_eq!(db.index_of("WINE"), 2);
    }

    #[test]
    fn database_index_wins_over_builtin_position_for_code_lookups() {
        let rows = vec![row(1, "water", "water", Some(100), Some("WATER"))];
        let db = LiquidLegacyIndex::from_rows(&rows);
        assert_eq!(db.index_of("WATER"), 100);
        // A hole in the builtin order stays reachable for unknown codes only.
        assert_eq!(db.index_of("BEER"), 1);
    }
}
