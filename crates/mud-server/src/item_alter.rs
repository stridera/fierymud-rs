//! Persistence of spell-altered item state (Curse / Remove Curse, legacy
//! `mag_alter_obj`; Enchant Weapon, legacy `spell_enchant_weapon`): the
//! restriction change, the weapon-die shrink, and the enchantment (stat
//! applies, the `MAGIC` flag, barred alignments).
//!
//! Live state is the item's [`ObjectRestrictions`], [`WeaponDiceSizeAdjust`],
//! [`ItemApplies`], [`ObjectFlags`] and [`ItemBarredAlignments`]. What is stored is the delta against the
//! prototype ([`ItemAlter`], the `curse` key of `CharacterItems.custom_values`
//! or the account-chest row), so later edits to the prototype's own
//! restrictions still reach the instance. Only a changed ([`ItemAlterDirty`])
//! item overwrites an existing row on save, like
//! [`mud_world::ItemCustomization`]; an INSERT always writes it.

use bevy_ecs::prelude::*;
use mud_db::character_items::{ItemAlter, ItemApply};
use mud_db::enums::{ObjectFlag, ObjectRestriction};
use mud_world::components::{
    FoodPoisoned, ItemAddedFlags, ItemAlterDirty, ItemApplies, ItemBarredAlignments,
    ItemRemovedFlags, WeaponDiceSizeAdjust,
};
use mud_world::{LiquidContainer, ObjectFlags, ObjectPrototypes, ObjectRestrictions, WorldKey};

/// DB / JSON spelling of a restriction.
fn db_name(r: ObjectRestriction) -> &'static str {
    match r {
        ObjectRestriction::NoDrop => "NO_DROP",
        ObjectRestriction::NoTake => "NO_TAKE",
        ObjectRestriction::NoSell => "NO_SELL",
        ObjectRestriction::NoBurn => "NO_BURN",
        ObjectRestriction::NoLocate => "NO_LOCATE",
        ObjectRestriction::NoInvisible => "NO_INVISIBLE",
    }
}

fn parse_all(names: &[String]) -> Vec<ObjectRestriction> {
    names
        .iter()
        .filter_map(|n| ObjectRestriction::from_db_str(n))
        .collect()
}

fn proto_restrictions(world: &World, item: Entity) -> Vec<ObjectRestriction> {
    let Some(key) = world.get::<WorldKey>(item) else {
        return Vec::new();
    };
    world
        .get_resource::<ObjectPrototypes>()
        .and_then(|p| p.by_key.get(&(key.zone, key.id)))
        .map(|p| p.restrictions.clone())
        .unwrap_or_default()
}

fn proto_flags(world: &World, item: Entity) -> Vec<ObjectFlag> {
    let Some(key) = world.get::<WorldKey>(item) else {
        return Vec::new();
    };
    world
        .get_resource::<ObjectPrototypes>()
        .and_then(|p| p.by_key.get(&(key.zone, key.id)))
        .map(|p| p.flags.clone())
        .unwrap_or_default()
}

/// Mark `item` as changed by a spell so the next save writes it.
pub(crate) fn mark_dirty(world: &mut World, item: Entity) {
    if let Ok(mut em) = world.get_entity_mut(item) {
        em.insert(ItemAlterDirty);
    }
}

/// Record that an alteration put `flag` on `item` and set it (a no-op for a
/// flag the item already has). The record is what `snapshot` persists.
pub(crate) fn add_flag(world: &mut World, item: Entity, flag: ObjectFlag) {
    let Ok(mut em) = world.get_entity_mut(item) else {
        return;
    };
    let mut flags = em
        .get::<ObjectFlags>()
        .map(|f| f.0.clone())
        .unwrap_or_default();
    if flags.contains(&flag) {
        return;
    }
    flags.push(flag);
    em.insert(ObjectFlags(flags));
    let mut added = em
        .get::<ItemAddedFlags>()
        .map(|a| a.0.clone())
        .unwrap_or_default();
    added.push(flag);
    em.insert(ItemAddedFlags(added));
}

/// Take `flag` off `item` (a no-op for a flag it does not have). A flag the
/// prototype carries is recorded as lost so `snapshot` persists it; one a
/// spell had added is simply forgotten.
pub(crate) fn remove_flag(world: &mut World, item: Entity, flag: ObjectFlag) {
    let from_proto = proto_flags(world, item).contains(&flag);
    let Ok(mut em) = world.get_entity_mut(item) else {
        return;
    };
    let Some(mut flags) = em.get::<ObjectFlags>().map(|f| f.0.clone()) else {
        return;
    };
    if !flags.contains(&flag) {
        return;
    }
    flags.retain(|f| *f != flag);
    em.insert(ObjectFlags(flags));
    if let Some(added) = em.get::<ItemAddedFlags>() {
        let kept: Vec<ObjectFlag> = added.0.iter().copied().filter(|f| *f != flag).collect();
        em.insert(ItemAddedFlags(kept));
    }
    if from_proto {
        let mut removed = em
            .get::<ItemRemovedFlags>()
            .map(|r| r.0.clone())
            .unwrap_or_default();
        if !removed.contains(&flag) {
            removed.push(flag);
        }
        em.insert(ItemRemovedFlags(removed));
    }
}

/// Whether a food or drink instance is poisoned right now: a drink
/// container's own state, else the per-instance food override, else the
/// prototype's.
pub(crate) fn is_poisoned(world: &World, item: Entity) -> bool {
    if let Some(l) = world.get::<LiquidContainer>(item) {
        return l.poisoned;
    }
    if let Some(p) = world.get::<FoodPoisoned>(item) {
        return p.0;
    }
    proto_food_poisoned(world, item)
}

fn proto_food_poisoned(world: &World, item: Entity) -> bool {
    let Some(key) = world.get::<WorldKey>(item) else {
        return false;
    };
    world
        .get_resource::<ObjectPrototypes>()
        .and_then(|p| p.by_key.get(&(key.zone, key.id)))
        .is_some_and(|p| p.food_poisoned)
}

fn proto_liquid_poisoned(world: &World, item: Entity) -> bool {
    let Some(key) = world.get::<WorldKey>(item) else {
        return false;
    };
    world
        .get_resource::<ObjectPrototypes>()
        .and_then(|p| p.by_key.get(&(key.zone, key.id)))
        .and_then(|p| p.liquid.as_ref())
        .is_some_and(|l| l.poisoned)
}

/// Set the poisoned state of a food or drink instance.
pub(crate) fn set_poisoned(world: &mut World, item: Entity, poisoned: bool) {
    let Ok(mut em) = world.get_entity_mut(item) else {
        return;
    };
    if let Some(mut l) = em.get_mut::<LiquidContainer>() {
        l.poisoned = poisoned;
    } else {
        em.insert(FoodPoisoned(poisoned));
    }
}

/// The delta of `item` against its prototype.
pub(crate) fn snapshot(world: &World, item: Entity) -> ItemAlter {
    let proto = proto_restrictions(world, item);
    let current: Vec<ObjectRestriction> = world
        .get::<ObjectRestrictions>(item)
        .map(|r| r.0.clone())
        .unwrap_or_default();
    let mut restrictions_added: Vec<String> = Vec::new();
    for r in &current {
        let name = db_name(*r).to_string();
        if !proto.contains(r) && !restrictions_added.contains(&name) {
            restrictions_added.push(name);
        }
    }
    let mut restrictions_removed: Vec<String> = Vec::new();
    for r in &proto {
        let name = db_name(*r).to_string();
        if !current.contains(r) && !restrictions_removed.contains(&name) {
            restrictions_removed.push(name);
        }
    }
    // Only the flags a deliberate alteration (Enchant Weapon) recorded, never
    // a diff of the live flags: anything else that sets an object flag at
    // runtime must not become a permanent part of the saved instance.
    let proto_flags = proto_flags(world, item);
    let mut flags_added: Vec<ObjectFlag> = Vec::new();
    for f in world
        .get::<ItemAddedFlags>(item)
        .map(|f| f.0.as_slice())
        .unwrap_or_default()
    {
        if !proto_flags.contains(f) && !flags_added.contains(f) {
            flags_added.push(*f);
        }
    }
    // Poison state only when it differs from the prototype's.
    let proto_poisoned = if world.get::<LiquidContainer>(item).is_some() {
        proto_liquid_poisoned(world, item)
    } else {
        proto_food_poisoned(world, item)
    };
    let poisoned = (world.get::<LiquidContainer>(item).is_some()
        || world.get::<FoodPoisoned>(item).is_some())
    .then(|| is_poisoned(world, item))
    .filter(|now| *now != proto_poisoned);
    let flags_removed: Vec<ObjectFlag> = world
        .get::<ItemRemovedFlags>(item)
        .map(|r| {
            r.0.iter()
                .copied()
                .filter(|f| proto_flags.contains(f))
                .collect()
        })
        .unwrap_or_default();
    ItemAlter {
        flags_removed,
        poisoned,
        restrictions_added,
        restrictions_removed,
        weapon_dice_size: world.get::<WeaponDiceSizeAdjust>(item).map_or(0, |a| a.0),
        applies: world
            .get::<ItemApplies>(item)
            .map(|a| {
                a.0.iter()
                    .map(|(target, amount)| ItemApply {
                        target: target.clone(),
                        amount: *amount,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        flags_added,
        alignments_barred: world
            .get::<ItemBarredAlignments>(item)
            .map(|a| a.0.clone())
            .unwrap_or_default(),
    }
}

/// Re-apply a stored delta to a freshly spawned `item` (its restrictions
/// already hold the prototype's list). `dirty` queues a write on the next
/// save, for state whose row does not carry it yet.
pub(crate) fn restore(world: &mut World, item: Entity, alter: &ItemAlter, dirty: bool) {
    if alter.is_empty() {
        return;
    }
    let added = parse_all(&alter.restrictions_added);
    let removed = parse_all(&alter.restrictions_removed);
    let mut list = proto_restrictions(world, item);
    list.retain(|r| !removed.contains(r));
    for r in added {
        if !list.contains(&r) {
            list.push(r);
        }
    }
    let Ok(mut em) = world.get_entity_mut(item) else {
        return;
    };
    if list.is_empty() {
        em.remove::<ObjectRestrictions>();
    } else {
        em.insert(ObjectRestrictions(list));
    }
    if alter.weapon_dice_size != 0 {
        em.insert(WeaponDiceSizeAdjust(alter.weapon_dice_size));
    }
    if !alter.applies.is_empty() {
        em.insert(ItemApplies(
            alter
                .applies
                .iter()
                .map(|a| (a.target.clone(), a.amount))
                .collect(),
        ));
    }
    if !alter.alignments_barred.is_empty() {
        em.insert(ItemBarredAlignments(alter.alignments_barred.clone()));
    }
    if !alter.flags_added.is_empty() {
        let mut flags = em
            .get::<ObjectFlags>()
            .map(|f| f.0.clone())
            .unwrap_or_default();
        for f in &alter.flags_added {
            if !flags.contains(f) {
                flags.push(*f);
            }
        }
        em.insert(ObjectFlags(flags));
        em.insert(ItemAddedFlags(alter.flags_added.clone()));
    }
    if !alter.flags_removed.is_empty() {
        let mut flags = em
            .get::<ObjectFlags>()
            .map(|f| f.0.clone())
            .unwrap_or_default();
        flags.retain(|f| !alter.flags_removed.contains(f));
        em.insert(ObjectFlags(flags));
        em.insert(ItemRemovedFlags(alter.flags_removed.clone()));
    }
    if let Some(p) = alter.poisoned {
        set_poisoned(world, item, p);
    }
    if dirty && let Ok(mut em) = world.get_entity_mut(item) {
        em.insert(ItemAlterDirty);
    }
}

/// A save wrote `saved` for `item`: it is settled unless a spell changed it
/// again since the snapshot (then it stays dirty for the next save).
pub(crate) fn settle(world: &mut World, item: Entity, saved: &ItemAlter) {
    if world.get::<ItemAlterDirty>(item).is_some() && snapshot(world, item) == *saved {
        world.entity_mut(item).remove::<ItemAlterDirty>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::object_proto;
    use mud_db::enums::ObjectType;

    fn world_with_proto(restrictions: Vec<ObjectRestriction>) -> (World, Entity) {
        let mut world = World::new();
        let mut protos = ObjectPrototypes::default();
        let mut proto = object_proto(1, 1, ObjectType::Weapon);
        proto.weapon_dice_num = 1;
        proto.restrictions = restrictions.clone();
        protos.by_key.insert((1, 1), proto);
        world.insert_resource(protos);
        let mut item = world.spawn(WorldKey { zone: 1, id: 1 });
        if !restrictions.is_empty() {
            item.insert(ObjectRestrictions(restrictions));
        }
        let item = item.id();
        (world, item)
    }

    #[test]
    fn untouched_item_has_an_empty_delta() {
        let (world, item) = world_with_proto(vec![ObjectRestriction::NoSell]);
        assert!(snapshot(&world, item).is_empty());
    }

    #[test]
    fn delta_records_added_and_lifted_restrictions_and_survives_restore() {
        let (mut world, item) = world_with_proto(vec![ObjectRestriction::NoSell]);
        world
            .entity_mut(item)
            .insert(ObjectRestrictions(vec![ObjectRestriction::NoDrop]));
        world.entity_mut(item).insert(WeaponDiceSizeAdjust(-1));
        let delta = snapshot(&world, item);
        assert_eq!(delta.restrictions_added, vec!["NO_DROP"]);
        assert_eq!(delta.restrictions_removed, vec!["NO_SELL"]);
        assert_eq!(delta.weapon_dice_size, -1);

        let (mut fresh, other) = world_with_proto(vec![ObjectRestriction::NoSell]);
        restore(&mut fresh, other, &delta, true);
        let r = fresh.get::<ObjectRestrictions>(other).unwrap();
        assert_eq!(r.0, vec![ObjectRestriction::NoDrop]);
        assert_eq!(fresh.get::<WeaponDiceSizeAdjust>(other).unwrap().0, -1);
        assert!(fresh.get::<ItemAlterDirty>(other).is_some());
        assert_eq!(snapshot(&fresh, other), delta);
    }

    #[test]
    fn only_deliberately_added_flags_are_stored() {
        let (mut world, item) = world_with_proto(Vec::new());
        // Some other system sets a flag at runtime: not part of the delta.
        world
            .entity_mut(item)
            .insert(ObjectFlags(vec![ObjectFlag::Soulbound]));
        assert!(snapshot(&world, item).flags_added.is_empty());
        // Enchant records MAGIC explicitly.
        add_flag(&mut world, item, ObjectFlag::Magic);
        add_flag(&mut world, item, ObjectFlag::Magic);
        let delta = snapshot(&world, item);
        assert_eq!(delta.flags_added, vec![ObjectFlag::Magic]);
        let live = world.get::<ObjectFlags>(item).unwrap();
        assert!(live.has(ObjectFlag::Soulbound) && live.has(ObjectFlag::Magic));

        // Restoring puts MAGIC back and keeps it recorded for the next save.
        let (mut fresh, other) = world_with_proto(Vec::new());
        restore(&mut fresh, other, &delta, false);
        assert!(
            fresh
                .get::<ObjectFlags>(other)
                .unwrap()
                .has(ObjectFlag::Magic)
        );
        assert_eq!(snapshot(&fresh, other), delta);
    }

    #[test]
    fn a_proto_flag_taken_off_is_stored_and_stays_off_after_a_reload() {
        let (mut world, item) = world_with_proto(Vec::new());
        world
            .resource_mut::<ObjectPrototypes>()
            .by_key
            .get_mut(&(1, 1))
            .unwrap()
            .flags = vec![ObjectFlag::Invisible];
        world
            .entity_mut(item)
            .insert(ObjectFlags(vec![ObjectFlag::Invisible]));
        remove_flag(&mut world, item, ObjectFlag::Invisible);
        let delta = snapshot(&world, item);
        assert_eq!(delta.flags_removed, vec![ObjectFlag::Invisible]);
        assert!(
            !world
                .get::<ObjectFlags>(item)
                .unwrap()
                .has(ObjectFlag::Invisible)
        );

        let (mut fresh, other) = world_with_proto(Vec::new());
        fresh
            .resource_mut::<ObjectPrototypes>()
            .by_key
            .get_mut(&(1, 1))
            .unwrap()
            .flags = vec![ObjectFlag::Invisible];
        fresh
            .entity_mut(other)
            .insert(ObjectFlags(vec![ObjectFlag::Invisible]));
        restore(&mut fresh, other, &delta, false);
        assert!(
            !fresh
                .get::<ObjectFlags>(other)
                .unwrap()
                .has(ObjectFlag::Invisible)
        );
        assert_eq!(snapshot(&fresh, other), delta);
    }

    #[test]
    fn a_flag_a_spell_added_then_removed_leaves_no_trace() {
        let (mut world, item) = world_with_proto(Vec::new());
        add_flag(&mut world, item, ObjectFlag::Invisible);
        remove_flag(&mut world, item, ObjectFlag::Invisible);
        assert!(snapshot(&world, item).is_empty());
    }

    #[test]
    fn poison_state_is_stored_only_when_it_differs_from_the_prototype() {
        let (mut world, item) = world_with_proto(Vec::new());
        assert_eq!(snapshot(&world, item).poisoned, None);
        set_poisoned(&mut world, item, true);
        let delta = snapshot(&world, item);
        assert_eq!(delta.poisoned, Some(true));
        assert!(is_poisoned(&world, item));

        let (mut fresh, other) = world_with_proto(Vec::new());
        restore(&mut fresh, other, &delta, true);
        assert!(is_poisoned(&fresh, other));
        assert_eq!(snapshot(&fresh, other), delta);
        set_poisoned(&mut fresh, other, false);
        assert_eq!(snapshot(&fresh, other).poisoned, None);

        // A drink container carries it on the liquid state instead.
        let (mut w, it) = world_with_proto(Vec::new());
        w.entity_mut(it).insert(LiquidContainer {
            liquid: "water".into(),
            capacity: 5,
            remaining: 5,
            poisoned: false,
        });
        set_poisoned(&mut w, it, true);
        assert_eq!(snapshot(&w, it).poisoned, Some(true));
        assert!(w.get::<LiquidContainer>(it).unwrap().poisoned);
    }

    #[test]
    fn settle_keeps_the_marker_when_the_item_changed_again() {
        let (mut world, item) = world_with_proto(Vec::new());
        world.entity_mut(item).insert(WeaponDiceSizeAdjust(-1));
        mark_dirty(&mut world, item);
        let saved = snapshot(&world, item);
        world.entity_mut(item).insert(WeaponDiceSizeAdjust(0));
        settle(&mut world, item, &saved);
        assert!(world.get::<ItemAlterDirty>(item).is_some());
        world.entity_mut(item).insert(WeaponDiceSizeAdjust(-1));
        settle(&mut world, item, &saved);
        assert!(world.get::<ItemAlterDirty>(item).is_none());
    }
}
