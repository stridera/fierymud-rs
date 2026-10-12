//! Class skills a character has earned by level.
//!
//! Legacy `update_skills` (skills.cpp) ran at character creation and on every
//! level gain: each non-quest skill whose class `min_level` the character has
//! reached is added at the low starting proficiency (50 of 1000) if they did
//! not have it yet. Here that is [`grant_class_skills`], called when a
//! character is created or logs in and when they gain a level, so `skills`,
//! `practice` and `study` see what the class table says the class can do.

use std::collections::HashSet;

use crate::components::KnownAbilities;
use crate::resources::{AbilityCatalog, ClassSkillsData};

/// Proficiency (0-1000) a newly earned class skill starts at (legacy:
/// `SET_SKILL(ch, skill, 50)` for skills that improve with use).
pub const STARTING_SKILL_PROFICIENCY: i32 = 50;

/// Add every SKILL row of `class_id` whose `min_level` is at most `level` and
/// that `known` does not have yet, at [`STARTING_SKILL_PROFICIENCY`]. Existing
/// entries (trained, learning or not) are never touched. Spells, chants and
/// songs are learned through `study`, not granted. Keeps `entries` sorted by
/// ability id. Skills flagged `quest_only` are never granted here (a quest
/// reward hands them out). Returns the ids added.
pub fn grant_class_skills(
    known: &mut KnownAbilities,
    class_id: i32,
    level: i32,
    class_skills: &ClassSkillsData,
    catalog: &AbilityCatalog,
) -> Vec<i32> {
    let skill_ids: HashSet<i32> = catalog
        .by_name
        .values()
        .filter(|d| d.kind == mud_db::abilities::AbilityKind::Skill && !d.quest_only)
        .map(|d| d.id)
        .collect();
    let mut due: Vec<i32> = class_skills
        .min_level
        .iter()
        .filter(|((cid, aid), min)| {
            *cid == class_id && **min <= level && skill_ids.contains(aid) && !known.has_any(*aid)
        })
        .map(|((_, aid), _)| *aid)
        .collect();
    due.sort_unstable();
    for aid in &due {
        known.entries.push((*aid, STARTING_SKILL_PROFICIENCY, true));
    }
    if !due.is_empty() {
        known.entries.sort_by_key(|(id, _, _)| *id);
    }
    due
}
