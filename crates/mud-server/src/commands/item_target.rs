//! The one resolver for "which item did the player mean?" (issue #56).
//!
//! Every command that names an item goes through [`find_item`] and states
//! which [`ItemClass`] it belongs to. The class fixes *where* to look and
//! in *what order*, so two commands of the same kind can never disagree
//! about whether the sword on the floor, the one in the pack or the one
//! being worn wins.
//!
//! | Class           | Search order                      | Commands |
//! |-----------------|-----------------------------------|----------|
//! | `Room`          | room                              | `get`/`take` (the item), `drag`, `enter` |
//! | `Inventory`     | pack (unworn)                     | `wear`, `wield`, `eat`, `drink`, `quaff`, `sell`, `junk`, `donate`, `drop`, `give`, `put` (the item), `deposit`, `palm` |
//! | `Equipment`     | worn                              | `remove` |
//! | `Carried`       | pack, then worn                   | `use`, `hold`, `light`, `identify`, `compare`, `pour`, `repair`, `taste`, `iedit` |
//! | `RoomFirst`     | room, then pack, then worn        | `look`/`examine`, `read`, `value`, `point`, `search`, `get ... from`, `look in` |
//! | `CarriedFirst`  | pack, then worn, then room        | `put ... in`, `write`, `fill`/`drink` sources, spells/wands aimed at an item |
//!
//! The rule of thumb: things you *take from or look at* prefer what is in
//! front of you (the room); things you *use, spend or fill* prefer what you
//! already have. Within a pack, newest arrival first (as `inventory`
//! lists it); within the room, newest first (as `look` lists it); worn
//! items follow `equipment`'s slot order. The `N.` counter restarts for
//! each place searched (legacy `universal_find` copies the find context),
//! so `2.ring` with one ring on the finger goes on to look for the second
//! ring in the pack.

use bevy_ecs::prelude::{Entity, World};
use mud_world::{EquippedSlot, Item, Keywords, Located, Named, Slot};

use super::{matches, parse_indexed_needle, sort_newest_first};

/// A place an item can be found, relative to the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    Room,
    /// Carried but not worn.
    Pack,
    /// Worn or held (has an equipment slot).
    Worn,
}

/// What kind of command is naming the item; see the module table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemClass {
    Room,
    Inventory,
    Equipment,
    Carried,
    RoomFirst,
    CarriedFirst,
}

impl ItemClass {
    /// Every class, for table-driven tests and docs.
    #[cfg(test)]
    pub(crate) const ALL: [ItemClass; 6] = [
        ItemClass::Room,
        ItemClass::Inventory,
        ItemClass::Equipment,
        ItemClass::Carried,
        ItemClass::RoomFirst,
        ItemClass::CarriedFirst,
    ];

    /// The places searched, in order. This is the documented table.
    pub(crate) const fn order(self) -> &'static [Place] {
        match self {
            ItemClass::Room => &[Place::Room],
            ItemClass::Inventory => &[Place::Pack],
            ItemClass::Equipment => &[Place::Worn],
            ItemClass::Carried => &[Place::Pack, Place::Worn],
            ItemClass::RoomFirst => &[Place::Room, Place::Pack, Place::Worn],
            ItemClass::CarriedFirst => &[Place::Pack, Place::Worn, Place::Room],
        }
    }
}

/// Resolve `[N.]needle` (hyphenated `x-y-z` included, see
/// `mud_world::targeting`) to an item for `player`, searching the places
/// of `class` in order.
pub(crate) fn find_item(
    world: &mut World,
    player: Entity,
    needle: &str,
    class: ItemClass,
) -> Option<Entity> {
    let (index, needle) = parse_indexed_needle(needle);
    let needle = needle.to_ascii_lowercase();
    let room = world.get::<Located>(player).map(|l| l.0);
    class
        .order()
        .iter()
        .find_map(|place| find_in_place(world, player, room, &needle, index, *place))
}

fn find_in_place(
    world: &mut World,
    player: Entity,
    room: Option<Entity>,
    needle: &str,
    index: usize,
    place: Place,
) -> Option<Entity> {
    match place {
        Place::Room => {
            let room = room?;
            let mut q = world
                .query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), bevy_ecs::prelude::With<Item>>();
            let mut hits: Vec<Entity> = q
                .iter(world)
                .filter(|(_, l, n, kw)| l.0 == room && matches(needle, n, *kw))
                .map(|(e, _, _, _)| e)
                .collect();
            sort_newest_first(world, room, &mut hits, |e| *e);
            hits.get(index - 1).copied()
        }
        Place::Pack | Place::Worn => {
            let mut q = world.query_filtered::<(
                Entity,
                &Located,
                &Named,
                Option<&Keywords>,
                Option<&EquippedSlot>,
            ), bevy_ecs::prelude::With<Item>>();
            let mut worn: Vec<(Entity, Slot)> = Vec::new();
            let mut packed: Vec<Entity> = Vec::new();
            for (e, l, n, kw, eq) in q.iter(world) {
                if l.0 != player || !matches(needle, n, kw) {
                    continue;
                }
                match eq {
                    Some(slot) => worn.push((e, slot.0)),
                    None => packed.push(e),
                }
            }
            if place == Place::Worn {
                worn.sort_by_key(|(_, s)| slot_rank(*s));
                worn.get(index - 1).map(|(e, _)| *e)
            } else {
                sort_newest_first(world, player, &mut packed, |e| *e);
                packed.get(index - 1).copied()
            }
        }
    }
}

/// Position of `slot` in `equipment`'s display order.
pub(crate) fn slot_rank(slot: Slot) -> usize {
    Slot::ORDER
        .iter()
        .position(|x| *x == slot)
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::player_in;
    use mud_world::{Room, Slot};

    fn sword(world: &mut World, holder: Entity, worn: bool) -> Entity {
        let e = world
            .spawn((
                Item,
                Named {
                    name: "a rusty sword".into(),
                },
                Keywords(vec!["sword".into()]),
                Located(holder),
            ))
            .id();
        if worn {
            world.entity_mut(e).insert(EquippedSlot(Slot::Wield));
        }
        e
    }

    /// Room, pack and worn each hold one `sword`; for every class the
    /// order the swords are found in (removing each winner in turn) must
    /// equal the documented `order()` and the table here.
    #[test]
    fn every_class_searches_in_its_documented_order() {
        let table: [(ItemClass, &[Place]); 6] = [
            (ItemClass::Room, &[Place::Room]),
            (ItemClass::Inventory, &[Place::Pack]),
            (ItemClass::Equipment, &[Place::Worn]),
            (ItemClass::Carried, &[Place::Pack, Place::Worn]),
            (
                ItemClass::RoomFirst,
                &[Place::Room, Place::Pack, Place::Worn],
            ),
            (
                ItemClass::CarriedFirst,
                &[Place::Pack, Place::Worn, Place::Room],
            ),
        ];
        assert_eq!(table.len(), ItemClass::ALL.len());
        for (class, expected) in table {
            assert_eq!(class.order(), expected, "{class:?} order drifted");
            let mut world = World::new();
            let room = world.spawn(Room).id();
            let (player, _rx) = player_in(&mut world, room);
            let in_room = sword(&mut world, room, false);
            let in_pack = sword(&mut world, player, false);
            let on_body = sword(&mut world, player, true);
            let entity_of = |p: Place| match p {
                Place::Room => in_room,
                Place::Pack => in_pack,
                Place::Worn => on_body,
            };
            for place in expected {
                let found = find_item(&mut world, player, "sword", class);
                assert_eq!(found, Some(entity_of(*place)), "{class:?} first hit");
                // Take the winner out of play and search again.
                world.despawn(found.unwrap());
            }
            assert_eq!(find_item(&mut world, player, "sword", class), None);
        }
    }

    #[test]
    fn hyphenated_needle_resolves_through_every_class() {
        let mut world = World::new();
        let room = world.spawn(Room).id();
        let (player, _rx) = player_in(&mut world, room);
        let e = world
            .spawn((
                Item,
                Named {
                    name: "a steel short sword".into(),
                },
                Keywords(vec!["sword".into(), "short".into(), "steel".into()]),
                Located(player),
            ))
            .id();
        assert_eq!(
            find_item(
                &mut world,
                player,
                "steel-short-sword",
                ItemClass::Inventory
            ),
            Some(e)
        );
        assert_eq!(
            find_item(&mut world, player, "short-axe", ItemClass::Inventory),
            None
        );
    }
}
