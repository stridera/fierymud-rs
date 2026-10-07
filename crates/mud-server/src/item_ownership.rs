//! Keeps `PersistedItemId` honest as items change hands.
//!
//! `PersistedItemId` names the `CharacterItems` row an item entity was
//! loaded from / last saved to. Items move between characters, rooms,
//! corpses and shops through hundreds of call sites, so instead of
//! patching each one this system watches `Located` and enforces one rule
//! once per tick:
//!
//! * An item still rooted at a player (directly carried, worn, or nested
//!   in a carried container, including another player's) keeps its id.
//!   The new holder's next save re-homes the row
//!   (`save_inventory_diff` sets `character_id`), and the previous
//!   holder's save no longer matches it.
//! * An item that has left player ownership entirely (dropped on the
//!   ground, handed to a mob or shop, junked, in a room container, moved
//!   to nowhere) loses its id. The previous owner's next save deletes the
//!   row (it is no longer in their snapshot), and a later pick-up INSERTs
//!   a fresh row instead of chasing a row that is gone.
//!
//! Decayed items are despawned outright, so they need no handling here.

use bevy_ecs::prelude::*;
use mud_world::{Account, Contents, Item, Located, PersistedItemId, Player};

/// Upper bound on the `Located` chain walked per item. Real nesting is a
/// handful of bags; the cap only guards against a malformed cycle.
const MAX_CHAIN: usize = 32;

type PlayerRoots<'w, 's> = Query<'w, 's, (), Or<(With<Account>, With<Player>)>>;
type MovedPersistedItems<'w, 's> =
    Query<'w, 's, Entity, (With<Item>, With<PersistedItemId>, Changed<Located>)>;

/// True when `item`'s `Located` chain ends at a player.
fn rooted_at_player(item: Entity, located: &Query<&Located>, players: &PlayerRoots) -> bool {
    let mut cur = item;
    for _ in 0..MAX_CHAIN {
        let Ok(loc) = located.get(cur) else {
            return false;
        };
        cur = loc.0;
        if players.contains(cur) {
            return true;
        }
    }
    false
}

/// Drop the stale row id from items that left player ownership.
pub fn release_unowned_item_ids(
    mut commands: Commands,
    moved: MovedPersistedItems,
    mut detached: RemovedComponents<Located>,
    persisted: Query<(), (With<Item>, With<PersistedItemId>)>,
    located: Query<&Located>,
    players: PlayerRoots,
    contents: Query<&Contents>,
) {
    let mut candidates: Vec<Entity> = moved.iter().collect();
    candidates.extend(detached.read().filter(|e| persisted.contains(*e)));
    for item in candidates {
        if rooted_at_player(item, &located, &players) {
            continue;
        }
        // Release the item and everything nested inside it: a container
        // leaving player ownership takes its contents with it, and their
        // own `Located` never changed so they would not be seen here.
        let mut stack = vec![item];
        while let Some(e) = stack.pop() {
            commands.entity(e).remove::<PersistedItemId>();
            if let Ok(inner) = contents.get(e) {
                stack.extend(inner.iter());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(world: &mut World) {
        let mut schedule = Schedule::default();
        schedule.add_systems(release_unowned_item_ids);
        schedule.run(world);
    }

    #[test]
    fn dropped_item_loses_id_and_carried_item_keeps_it() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let player = world.spawn((Player, Located(room))).id();
        let bag = world
            .spawn((Item, PersistedItemId(1), Located(player)))
            .id();
        let in_bag = world.spawn((Item, PersistedItemId(2), Located(bag))).id();
        let kept = world
            .spawn((Item, PersistedItemId(3), Located(player)))
            .id();
        let dropped = world.spawn((Item, PersistedItemId(4), Located(room))).id();
        run(&mut world);
        assert!(world.get::<PersistedItemId>(bag).is_some());
        assert!(world.get::<PersistedItemId>(in_bag).is_some());
        assert!(world.get::<PersistedItemId>(kept).is_some());
        assert!(world.get::<PersistedItemId>(dropped).is_none());

        // Dropping the whole bag releases it and its contents even though
        // the contents' own `Located` did not change.
        world.entity_mut(kept).insert(Located(room));
        world.entity_mut(bag).insert(Located(room));
        run(&mut world);
        assert!(world.get::<PersistedItemId>(kept).is_none());
        assert!(world.get::<PersistedItemId>(bag).is_none());
        assert!(world.get::<PersistedItemId>(in_bag).is_none());
    }

    #[test]
    fn given_item_keeps_id_under_the_new_holder() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let a = world.spawn((Player, Located(room))).id();
        let b = world.spawn((Player, Located(room))).id();
        let item = world.spawn((Item, PersistedItemId(7), Located(a))).id();
        run(&mut world);
        world.entity_mut(item).insert(Located(b));
        run(&mut world);
        assert_eq!(world.get::<PersistedItemId>(item).unwrap().0, 7);
        // Handed to a mob (no Player/Account): released.
        let mob = world.spawn(Located(room)).id();
        world.entity_mut(item).insert(Located(mob));
        run(&mut world);
        assert!(world.get::<PersistedItemId>(item).is_none());
    }
}
