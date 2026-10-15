//! Gods (immortals) are never held back by class, race or alignment
//! rules, and they know every ability in the catalog at full proficiency.
//! Legacy gated these on `GET_LEVEL(ch) < LVL_IMMORT` (handler.cpp
//! `invalid_align` / `invalid_class`, `spell_parser.cpp`). Rules that are not
//! about what a character *is* (peaceful rooms, PK, consent) are left to
//! their own checks.

use std::borrow::Cow;

use bevy_ecs::prelude::{Entity, World};

use crate::components::{Account, KnownAbilities, MortalMode};
use crate::resources::AbilityCatalog;

/// Proficiency a god has in every ability (full mastery).
pub const GOD_PROFICIENCY: i32 = 1000;

/// True for a god: a character whose effective rank is `Immortal`
/// (`LVL_IMMORT`, level 100) or above. `Account.role` is the cached
/// effective rank (the higher of the website role and the level-implied
/// role), so a level-100+ character counts even without a linked account.
/// `DevMode` deliberately does not count: this is about who the character is.
/// A god who turned on `mortal` ([`MortalMode`]) is treated as a mortal here;
/// their role and command permissions are untouched. This is the check for
/// game rules (water, fly, slots, `NO_DROP`, ...). Visibility and admin or
/// inspection output use [`has_immortal_role`], which ignores `mortal`.
#[must_use]
pub fn bypasses_restrictions(world: &World, entity: Entity) -> bool {
    !is_mortal_mode(world, entity) && has_immortal_role(world, entity)
}

/// True when `entity`'s effective rank is `Immortal` or above, whether or not
/// the `mortal` toggle is on. For visibility (god zones), admin tools and
/// inspection output, which must not degrade in mortal mode.
#[must_use]
pub fn has_immortal_role(world: &World, entity: Entity) -> bool {
    world
        .get::<Account>(entity)
        .is_some_and(|a| a.role.at_least(mud_db::enums::UserRole::Immortal))
}

/// True while `entity` has the `mortal` toggle on.
#[must_use]
pub fn is_mortal_mode(world: &World, entity: Entity) -> bool {
    world.get::<MortalMode>(entity).is_some()
}

/// The abilities `entity` knows, as the lookup layer sees them. Mortals get
/// their stored rows. A god gets a virtual list built from the catalog:
/// every ability, known, at [`GOD_PROFICIENCY`]. Nothing is written to the
/// component, so no god rows reach the database.
#[must_use]
pub fn known_abilities_of(world: &World, entity: Entity) -> Option<Cow<'_, KnownAbilities>> {
    let stored = world.get::<KnownAbilities>(entity);
    if bypasses_restrictions(world, entity)
        && let Some(catalog) = world.get_resource::<AbilityCatalog>()
    {
        let mut ids: Vec<i32> = catalog.by_name.values().map(|d| d.id).collect();
        if let Some(k) = stored {
            ids.extend(k.entries.iter().map(|(id, _, _)| *id));
        }
        ids.sort_unstable();
        ids.dedup();
        return Some(Cow::Owned(KnownAbilities {
            entries: ids
                .into_iter()
                .map(|id| (id, GOD_PROFICIENCY, true))
                .collect(),
        }));
    }
    stored.map(Cow::Borrowed)
}

/// `entity`'s stored proficiency in `ability_id` (0-1000), `None` when it has
/// no row for it. Gods have every ability at [`GOD_PROFICIENCY`].
#[must_use]
pub fn proficiency_of(world: &World, entity: Entity, ability_id: i32) -> Option<i32> {
    if bypasses_restrictions(world, entity) && world.contains_resource::<AbilityCatalog>() {
        return Some(GOD_PROFICIENCY);
    }
    world
        .get::<KnownAbilities>(entity)?
        .entries
        .iter()
        .find(|(id, _, _)| *id == ability_id)
        .map(|(_, p, _)| *p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_db::enums::UserRole;

    fn account(role: UserRole) -> Account {
        Account {
            user_id: String::new(),
            character_id: String::new(),
            role,
            account_role: role,
            perms: Vec::new(),
        }
    }

    #[test]
    fn immortal_and_up_bypass_mortals_do_not() {
        let mut world = World::new();
        let player = world.spawn(account(UserRole::Player)).id();
        let imm = world.spawn(account(UserRole::Immortal)).id();
        let imp = world.spawn(account(UserRole::Implementor)).id();
        let nobody = world.spawn_empty().id();
        assert!(!bypasses_restrictions(&world, player));
        assert!(bypasses_restrictions(&world, imm));
        assert!(bypasses_restrictions(&world, imp));
        assert!(!bypasses_restrictions(&world, nobody));
    }
}
