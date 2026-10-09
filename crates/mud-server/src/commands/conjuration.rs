//! Data-driven conjuration: which mob a `summon` effect spawns and what a
//! `create` effect conjures. Both used to be hard-coded tables in
//! `invoke_ability_with`; builders now set them in the database.
//!
//! * `summon`: the `AbilityEffect.override_params` carry `mobZone` / `mobId`.
//! * `create`: the `CreationRecipe` table (Minor Creation keywords, Create
//!   Food by caster class), loaded into [`CreationRecipes`].

use mud_db::enums::ObjectType;
use mud_world::{CreationRecipes, ObjectPrototypes};

/// The mob prototype `(zone, id)` a `summon` effect spawns, from its
/// override params. The error is a builder-facing message naming the ability
/// and what to set; the caller logs it.
pub(super) fn summon_mob_key(
    ability: &str,
    params: Option<&serde_json::Value>,
) -> Result<(i32, i32), String> {
    let int = |key: &str| {
        params
            .and_then(|p| p.get(key))
            .and_then(serde_json::Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
    };
    if let (Some(zone), Some(id)) = (int("mobZone"), int("mobId")) {
        return Ok((zone, id));
    }
    let mob_type = params
        .and_then(|p| p.get("mobType"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    Err(format!(
        "summon effect of ability {ability} (mobType '{mob_type}') has no integer \
         mobZone/mobId in its override_params; set them on the AbilityEffect row \
         (Muditor, or a fierylib SQL patch) so the spell knows which mob to summon"
    ))
}

/// What a `create` spell conjures, per its `CreationRecipe` rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Conjured {
    /// This exact object `(zone, id)`.
    Fixed(i32, i32),
    /// Any FOOD object in this zone, picked by the caster's skill.
    FoodIn(i32),
}

/// The recipe row for a cast: the typed `word` (abbreviation of a keyword
/// row) wins, else the keyword-less row for the caster's class, else the
/// all-classes row. `None` when the ability has no matching row.
pub(super) fn creation_choice(
    recipes: &CreationRecipes,
    ability: &str,
    word: Option<&str>,
    class_id: Option<i32>,
) -> Option<Conjured> {
    let row = word
        .and_then(|w| recipes.for_keyword(ability, w))
        .or_else(|| recipes.for_class(ability, class_id))?;
    Some(match row.object_id {
        Some(id) => Conjured::Fixed(row.object_zone_id, id),
        None => Conjured::FoodIn(row.object_zone_id),
    })
}

/// FOOD prototypes in `zone`, by id. The imported per-class zones mix food
/// with armor and fountains, so only typed FOOD entries qualify.
pub(super) fn foods_in_zone(protos: &ObjectPrototypes, zone: i32) -> Vec<(i32, i32)> {
    let mut foods: Vec<(i32, i32)> = protos
        .by_key
        .iter()
        .filter(|((z, _), p)| *z == zone && p.r#type == ObjectType::Food)
        .map(|(&key, _)| key)
        .collect();
    foods.sort_by_key(|&(_, id)| id);
    foods
}

/// Pick a food from `foods` (sorted): caster skill 0..=100 maps onto the
/// list (better casters reach the later entries), and a small `jitter` keeps
/// back-to-back casts from duplicating. Legacy: `skill / 16 + random`.
pub(super) fn pick_food(foods: &[(i32, i32)], skill: i32, jitter: usize) -> Option<(i32, i32)> {
    let last = foods.len().checked_sub(1)?;
    let skill = usize::try_from(skill.clamp(0, 100)).unwrap_or(0);
    let base = skill.saturating_mul(foods.len()) / 101;
    foods.get((base + jitter).min(last)).copied()
}
