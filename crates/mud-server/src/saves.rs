//! Saving throws: the single implementation of legacy `mag_savingthrow`
//! (`magic.cpp`), shared by spell saves (`save_action_for`) and fear.
//!
//! `save = base(type) - level / 2 + GET_SAVE(victim, type)`; the victim
//! saves when `max(1, save) < d(0..99)`. Lower is better, so level and
//! negative gear modifiers both raise the chance to save.
//!
//! Follow-ups: the base values below are the legacy `get_base_saves`
//! defaults (no per-class or per-race table, no legacy race adjustments).
//! Per Data over Code they should move to the DB (class / race base saves).

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_world::SavingThrows;

/// The legacy save categories a rolled save uses. Legacy `ROD` (base 115) and
/// `PETRI` (base 105) have no `AbilitySavingThrow` type of their own; the
/// modern types fold into the three below (see [`SaveKind::for_ability`]),
/// so `SavingThrows::rod` / `petri` modifiers are not rolled against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveKind {
    Para,
    Breath,
    Spell,
}

/// Legacy `get_base_saves` defaults, before the level reduction.
const BASE_PARA: i32 = 105;
const BASE_BREATH: i32 = 110;
const BASE_SPELL: i32 = 110;

impl SaveKind {
    /// Pick the legacy category an ability's `AbilitySavingThrow` rolls
    /// against. Legacy rolls every cast spell against `SAVING_SPELL`
    /// (`spell_parser.cpp`) and only breath weapons against `SAVING_BREATH`,
    /// so for abilities that are not skills the modern `save_type` does not
    /// matter:
    /// - breath weapons (plain name ends in `BREATH`; no ability data flags
    ///   them, so this is a name match) -> breath;
    /// - every other spell / chant / song -> spell.
    ///
    /// Skills (bash, trip up) keep the schema's reading of the modern types:
    /// `REFLEX` -> breath, `FORTITUDE` -> paralysis (also covers
    /// petrification, same base), `WILL` -> spell. Unknown labels on a skill
    /// return `None` (no save is rolled).
    pub(crate) fn for_ability(
        save_type: &str,
        kind: AbilityKind,
        plain_name: &str,
    ) -> Option<Self> {
        if kind != AbilityKind::Skill {
            return Some(if plain_name.to_ascii_uppercase().ends_with("BREATH") {
                Self::Breath
            } else {
                Self::Spell
            });
        }
        match save_type.to_ascii_uppercase().as_str() {
            "REFLEX" => Some(Self::Breath),
            "FORTITUDE" => Some(Self::Para),
            "WILL" => Some(Self::Spell),
            _ => None,
        }
    }

    fn base(self) -> i32 {
        match self {
            Self::Para => BASE_PARA,
            Self::Breath => BASE_BREATH,
            Self::Spell => BASE_SPELL,
        }
    }

    fn modifier(self, s: &SavingThrows) -> i32 {
        match self {
            Self::Para => s.para,
            Self::Breath => s.breath,
            Self::Spell => s.spell,
        }
    }
}

/// The victim's save number for `kind`: base, improved by one point per two
/// levels, plus the victim's own modifier. Lower is better.
pub(crate) fn save_number(world: &World, victim: Entity, kind: SaveKind) -> i32 {
    let level = mud_world::effective_level(world, victim);
    let modifier = world
        .get::<SavingThrows>(victim)
        .map_or(0, |s| kind.modifier(s));
    kind.base() - level / 2 + modifier
}

/// Legacy `mag_savingthrow`: `max(1, save) < roll` where `roll` is the
/// `random(0..99)` draw (a parameter so tests can pin it).
pub(crate) fn saving_throw(world: &World, victim: Entity, kind: SaveKind, roll: i32) -> bool {
    save_number(world, victim, kind).max(1) < roll
}

/// [`saving_throw`] with a fresh `random(0..99)` roll.
pub(crate) fn roll_saving_throw(world: &World, victim: Entity, kind: SaveKind) -> bool {
    #[cfg(test)]
    if let Some(roll) = test_pin::pinned() {
        return saving_throw(world, victim, kind, roll);
    }
    saving_throw(world, victim, kind, rand::random_range(0..=99))
}

/// Test hook: pin the d(0..99) that [`roll_saving_throw`] draws on this
/// thread, so spell-pipeline tests can force a made or failed save.
#[cfg(test)]
pub(crate) mod test_pin {
    use std::cell::Cell;

    thread_local!(static PIN: Cell<Option<i32>> = const { Cell::new(None) });

    pub(crate) fn pinned() -> Option<i32> {
        PIN.with(Cell::get)
    }

    /// Pin the roll until the guard drops. `0` always fails the save; `99`
    /// saves whenever the victim's number is below 99.
    pub(crate) fn pin(roll: i32) -> Guard {
        PIN.with(|p| p.set(Some(roll)));
        Guard
    }

    pub(crate) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            PIN.with(|p| p.set(None));
        }
    }
}

#[cfg(test)]
mod tests;
