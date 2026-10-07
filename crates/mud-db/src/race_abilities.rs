//! Race-innate ability rows. Each `(race, ability_id)` says "members
//! of this race start with this ability at the given proficiency".
//! The runtime lists them with `innate` and grants them to the
//! character's ability set at login ([`merge_innates`]) — the ability
//! catalog still gates the actual runtime behavior.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::character_abilities::CharacterAbilityRow;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaceAbilityRow {
    /// `Race` enum value as raw text (HUMAN / ELF / etc.).
    pub race: String,
    pub ability_id: i32,
    /// Display name from the joined `Ability.plain_name`.
    pub ability_name: String,
    /// `SkillCategory` (PRIMARY / SECONDARY / ...) as raw text.
    pub category: String,
    pub bonus: i32,
    pub proficiency_cap: i32,
}

/// Race-innate abilities for a single race, sorted by ability name.
pub async fn list_for_race(pool: &PgPool, race: &str) -> sqlx::Result<Vec<RaceAbilityRow>> {
    sqlx::query_as!(
        RaceAbilityRow,
        r#"
        SELECT
            ra.race::text       AS "race!: String",
            ra.ability_id,
            a.plain_name        AS "ability_name!: String",
            ra.category::text   AS "category!: String",
            ra.bonus,
            ra.proficiency_cap
        FROM "RaceAbilities" ra
        JOIN "Ability" a ON a.id = ra.ability_id
        WHERE ra.race::text = $1
        ORDER BY a.plain_name
        "#,
        race,
    )
    .fetch_all(pool)
    .await
}

/// Proficiency a granted innate starts at: the row's `proficiency_cap`
/// (0-100, the schema's "max trainable") on the 0-1000 scale
/// `CharacterAbilities` uses. Innates are not trained, so they begin
/// at their ceiling (legacy gave racial breath weapons proficiency
/// 1000).
#[must_use]
pub const fn innate_proficiency(row: &RaceAbilityRow) -> i32 {
    let p = row.proficiency_cap.saturating_mul(10);
    if p < 0 {
        0
    } else if p > 1000 {
        1000
    } else {
        p
    }
}

/// Grant a race's innate abilities to a character's ability rows.
/// Missing abilities are added as known at [`innate_proficiency`]; an
/// existing not-known row (e.g. a placeholder at proficiency 0) becomes
/// known at `max(existing, innate_proficiency)` so a granted innate
/// isn't left unusable at 0; an already-known row is untouched (its
/// trained proficiency is never lowered or reset). Rows stay
/// sorted by `ability_id` (matching `list_for`). Idempotent. Returns
/// how many rows were added or changed.
pub fn merge_innates(rows: &mut Vec<CharacterAbilityRow>, innates: &[RaceAbilityRow]) -> usize {
    let mut changed = 0;
    for innate in innates {
        if let Some(existing) = rows.iter_mut().find(|r| r.ability_id == innate.ability_id) {
            if !existing.known {
                existing.known = true;
                existing.proficiency = existing.proficiency.max(innate_proficiency(innate));
                changed += 1;
            }
        } else {
            rows.push(CharacterAbilityRow {
                ability_id: innate.ability_id,
                known: true,
                proficiency: innate_proficiency(innate),
            });
            changed += 1;
        }
    }
    if changed > 0 {
        rows.sort_by_key(|r| r.ability_id);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn innate(id: i32, cap: i32) -> RaceAbilityRow {
        RaceAbilityRow {
            race: "ELF".into(),
            ability_id: id,
            ability_name: format!("A{id}"),
            category: "PRIMARY".into(),
            bonus: 0,
            proficiency_cap: cap,
        }
    }

    fn row(id: i32, known: bool, prof: i32) -> CharacterAbilityRow {
        CharacterAbilityRow {
            ability_id: id,
            known,
            proficiency: prof,
        }
    }

    #[test]
    fn missing_innates_are_added_known_at_their_cap() {
        let mut rows = vec![row(5, true, 300)];
        let n = merge_innates(&mut rows, &[innate(9, 100), innate(2, 60)]);
        assert_eq!(n, 2);
        let ids: Vec<i32> = rows.iter().map(|r| r.ability_id).collect();
        assert_eq!(ids, vec![2, 5, 9], "sorted by ability id");
        assert_eq!((rows[0].known, rows[0].proficiency), (true, 600));
        assert_eq!((rows[2].known, rows[2].proficiency), (true, 1000));
    }

    #[test]
    fn unknown_rows_become_known_at_max_of_existing_and_start() {
        let mut rows = vec![
            row(9, false, 0),
            row(7, false, 120),
            row(6, false, 900),
            row(2, true, 400),
        ];
        let innates = [innate(9, 100), innate(7, 50), innate(6, 50), innate(2, 100)];
        let n = merge_innates(&mut rows, &innates);
        assert_eq!(n, 3, "only the not-known rows changed");
        let by = |id: i32| rows.iter().find(|r| r.ability_id == id).unwrap();
        assert_eq!(
            (by(9).known, by(9).proficiency),
            (true, 1000),
            "0 raised to start"
        );
        assert_eq!(
            (by(7).known, by(7).proficiency),
            (true, 500),
            "120 raised to start"
        );
        assert_eq!(
            (by(6).known, by(6).proficiency),
            (true, 900),
            "never lowered"
        );
        assert_eq!(
            by(2).proficiency,
            400,
            "known rows keep trained proficiency"
        );
    }

    #[test]
    fn merging_twice_is_a_no_op() {
        let mut rows = Vec::new();
        let innates = [innate(3, 100)];
        assert_eq!(merge_innates(&mut rows, &innates), 1);
        assert_eq!(merge_innates(&mut rows, &innates), 0);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn out_of_range_caps_are_clamped() {
        assert_eq!(innate_proficiency(&innate(1, 500)), 1000);
        assert_eq!(innate_proficiency(&innate(1, -4)), 0);
    }
}
