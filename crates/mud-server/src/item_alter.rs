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
    ItemAlterDirty, ItemApplies, ItemBarredAlignments, WeaponDiceSizeAdjust,
};
use mud_world::{ObjectFlags, ObjectPrototypes, ObjectRestrictions, WorldKey};

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
    let proto_flags = proto_flags(world, item);
    let mut flags_added: Vec<ObjectFlag> = Vec::new();
    for f in world
        .get::<ObjectFlags>(item)
        .map(|f| f.0.as_slice())
        .unwrap_or_default()
    {
        if !proto_flags.contains(f) && !flags_added.contains(f) {
            flags_added.push(*f);
        }
    }
    ItemAlter {
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
    }
    if dirty {
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
