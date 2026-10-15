//! Query states kept between ticks for the per-tick sweeps.
//!
//! `world.query()` / `world.query_filtered()` build a fresh `QueryState`,
//! and building one matches the query against every archetype that holds
//! its rarest *fetched* component, or against every archetype in the world
//! when it fetches no component (`Entity` plus `With` / `Without` filters:
//! filters do not narrow the walk). A live world has a few thousand
//! archetypes, so each fresh query costs tens of microseconds and grows
//! with the archetype count: on an idle boot the tick's ~27 fresh queries
//! were about 0.7 ms of a 1.2 ms tick, and 1,900 extra (empty) mob
//! archetypes took the idle tick from 1.2 to 2.1 ms.
//!
//! [`with_query`] keeps one `QueryState` per query type in a resource. A
//! cached state only matches archetypes created since its last use, so a
//! sweep costs its iteration and nothing per archetype.

use bevy_ecs::prelude::*;
use bevy_ecs::query::{QueryData, QueryFilter};

#[derive(Resource)]
struct Cached<D: QueryData + 'static, F: QueryFilter + 'static>(QueryState<D, F>);

/// Run `f` with the world's cached `QueryState<D, F>` (built on first use).
/// `QueryState::iter` / `get` bring the state up to date with archetypes
/// created since the previous call.
///
/// The state is lent out of the world for the call, so a nested `with_query`
/// of the same `D, F` inside `f` finds none and builds its own. Whichever
/// scope finishes first stores its state; a scope that finishes to find one
/// already stored drops its own, so the world never gets a second insert.
/// The nested call is correct, just not cached for that call.
pub(crate) fn with_query<D, F, R>(
    world: &mut World,
    f: impl FnOnce(&mut World, &mut QueryState<D, F>) -> R,
) -> R
where
    D: QueryData + 'static,
    F: QueryFilter + 'static,
{
    let mut cached = match world.remove_resource::<Cached<D, F>>() {
        Some(cached) => cached,
        None => Cached(world.query_filtered::<D, F>()),
    };
    let out = f(world, &mut cached.0);
    // A nested call of the same type may have stored its state meanwhile;
    // keep that one (inserting again would panic in debug builds).
    if !world.contains_resource::<Cached<D, F>>() {
        world.insert_resource(cached);
    }
    out
}

/// The entities matching `F`, in query order, through a cached state.
pub(crate) fn entities<F>(world: &mut World) -> Vec<Entity>
where
    F: QueryFilter + 'static,
{
    with_query::<Entity, F, _>(world, |world, q| q.iter(world).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Component)]
    struct A(u32);
    #[derive(Component)]
    struct B;
    #[derive(Component)]
    struct C;

    #[test]
    fn cached_query_sees_entities_and_archetypes_added_later() {
        let mut world = World::new();
        let a = world.spawn(A(1)).id();
        assert_eq!(entities::<With<A>>(&mut world), vec![a]);
        // A new archetype after the state was cached.
        let ab = world.spawn((A(2), B)).id();
        let mut got = entities::<With<A>>(&mut world);
        got.sort();
        let mut want = vec![a, ab];
        want.sort();
        assert_eq!(got, want);
        assert_eq!(entities::<(With<A>, With<B>)>(&mut world), vec![ab]);
        // Moving an entity between archetypes is tracked too.
        world.entity_mut(a).insert(C);
        world.entity_mut(ab).remove::<A>();
        assert_eq!(entities::<With<A>>(&mut world), vec![a]);
        let sum: u32 =
            with_query::<&A, (), _>(&mut world, |world, q| q.iter(world).map(|x| x.0).sum());
        assert_eq!(sum, 1);
    }

    #[test]
    fn nested_use_of_the_same_query_still_answers() {
        let mut world = World::new();
        world.spawn(A(1));
        let n = with_query::<&A, (), _>(&mut world, |world, q| {
            let outer = q.iter(world).count();
            outer + entities::<With<A>>(world).len()
        });
        assert_eq!(n, 2);
        assert_eq!(entities::<With<A>>(&mut world).len(), 1);
    }

    #[test]
    fn nested_use_of_the_same_query_type_leaves_one_stored_state() {
        let mut world = World::new();
        world.spawn(A(1));
        // Cold: the inner call builds the state the outer would have built.
        let n = with_query::<&A, (), _>(&mut world, |world, q| {
            let inner = with_query::<&A, (), _>(world, |world, q2| {
                q2.iter(world).map(|x| x.0).sum::<u32>()
            });
            // Spawning in between adds an archetype the outer state must catch up on.
            world.spawn((A(10), B));
            inner + q.iter(world).map(|x| x.0).sum::<u32>()
        });
        assert_eq!(n, 1 + 11);
        assert!(world.contains_resource::<Cached<&A, ()>>());
        // Warm: both scopes borrow around a stored state.
        let n = with_query::<&A, (), _>(&mut world, |world, q| {
            let inner = with_query::<&A, (), _>(world, |world, q2| q2.iter(world).count());
            inner + q.iter(world).count()
        });
        assert_eq!(n, 4);
        assert_eq!(
            with_query::<&A, (), _>(&mut world, |w, q| q.iter(w).count()),
            2
        );
    }

    #[test]
    fn the_state_is_stored_in_the_world() {
        let mut world = World::new();
        world.spawn(A(1));
        assert!(!world.contains_resource::<Cached<Entity, With<A>>>());
        let _ = entities::<With<A>>(&mut world);
        assert!(world.contains_resource::<Cached<Entity, With<A>>>());
        // Still there after a call that saw a new archetype.
        world.spawn((A(2), B));
        assert_eq!(entities::<With<A>>(&mut world).len(), 2);
        assert!(world.contains_resource::<Cached<Entity, With<A>>>());
    }
}
