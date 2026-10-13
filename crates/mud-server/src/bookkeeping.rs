//! Bookkeeping components that exist from spawn instead of first use.
//!
//! Every distinct set of components is a bevy archetype, and bevy never
//! frees one. Each costs tens of KB (its table plus edge caches sized by the
//! number of bundle types), and every `world.query()` built from scratch
//! walks the archetypes holding its rarest component. Code that adds a
//! bookkeeping component on first use (the first kill adds `Trophy`, the
//! first queued line `InputQueue`, the first player swing a mob's
//! `HateList`) sends each entity through a different chain of archetypes
//! depending on the order things happen to it, so the archetype count grew
//! with play and never came back down: a 28-player soak created about 110 a
//! minute (RSS +2 MB/min, tick p50 1.2 to 3 ms in 25 minutes, still there
//! after everyone logged out).
//!
//! [`register`] makes these required components of [`Player`] / [`Mob`], so
//! they arrive with the entity. Each one's `Default` is its "absent" state
//! (an empty list, a lag that ended at tick 0, no recorded damager), and the
//! code that used to remove one puts it back to default with [`reset`]
//! instead. Components whose presence *is* the state (`Fighting`,
//! `Hiddenness`, spell flags) stay structural.

use bevy_ecs::component::Mutable;
use bevy_ecs::prelude::*;
use mud_world::{Cooldowns, Mob, Player, Resistances, ScriptVars, Trophy};

use crate::combat::{DamagedBy, HateList, MobMemory};
use crate::commands::ReengageLag;
use crate::commands::info::HideLag;
use crate::commands::input_queue::{InputQueue, WaitState};
use crate::idle::IdleCombatSeen;

/// Declare the bookkeeping components as required by `Player` / `Mob`.
/// Call once on a fresh world, before anything spawns a player or a mob
/// (bevy refuses a requirement for a component that is already in an
/// archetype).
pub(crate) fn register(world: &mut World) {
    // Players: per-character records and lags that used to appear on the
    // first kill / cooldown / queued line / fight / hide.
    world.register_required_components::<Player, Trophy>();
    world.register_required_components::<Player, ScriptVars>();
    world.register_required_components::<Player, Cooldowns>();
    world.register_required_components::<Player, Resistances>();
    world.register_required_components::<Player, InputQueue>();
    // The idle scan reads "last seen fighting" as activity; from spawn it
    // equals the login time, which the scan already counts as activity.
    world.register_required_components_with::<Player, IdleCombatSeen>(|| {
        IdleCombatSeen(std::time::Instant::now())
    });
    // Mobs: grudge lists, filled by the first player that swings at them.
    world.register_required_components::<Mob, HateList>();
    world.register_required_components::<Mob, MobMemory>();
    // Both: lags and the kill-credit stamp.
    world.register_required_components::<Player, WaitState>();
    world.register_required_components::<Mob, WaitState>();
    world.register_required_components::<Player, ReengageLag>();
    world.register_required_components::<Mob, ReengageLag>();
    world.register_required_components::<Player, HideLag>();
    world.register_required_components::<Mob, HideLag>();
    world.register_required_components::<Player, DamagedBy>();
    world.register_required_components::<Mob, DamagedBy>();
}

/// Put `e`'s `C` back to its default in place (no archetype move). No-op
/// when `e` is gone or never had one.
pub(crate) fn reset<C>(world: &mut World, e: Entity)
where
    C: Component<Mutability = Mutable> + Default,
{
    if let Some(mut c) = world.get_mut::<C>(e) {
        *c = C::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::{Health, Located, Room};

    fn world() -> World {
        let mut world = World::new();
        register(&mut world);
        world.insert_resource(crate::TickCount(100));
        world
    }

    fn archetype_of(world: &World, e: Entity) -> bevy_ecs::archetype::ArchetypeId {
        world.entity(e).archetype().id()
    }

    #[test]
    fn players_and_mobs_spawn_with_their_bookkeeping() {
        let mut world = world();
        let p = world.spawn(Player).id();
        let m = world.spawn(Mob).id();
        let pe = world.entity(p);
        assert!(pe.contains::<Trophy>() && pe.contains::<ScriptVars>());
        assert!(pe.contains::<Cooldowns>() && pe.contains::<Resistances>());
        assert!(pe.contains::<InputQueue>() && pe.contains::<IdleCombatSeen>());
        let me = world.entity(m);
        assert!(me.contains::<HateList>() && me.contains::<MobMemory>());
        for e in [p, m] {
            let em = world.entity(e);
            assert!(em.contains::<WaitState>() && em.contains::<ReengageLag>());
            assert!(em.contains::<HideLag>() && em.contains::<DamagedBy>());
        }
        // The defaults read as "nothing recorded".
        assert!(world.get::<DamagedBy>(m).unwrap().0.is_none());
        assert!(!crate::commands::input_queue::wait_active(&world, p));
        assert!(!crate::commands::input_queue::has_queued_input(&world, p));
    }

    /// A fight's bookkeeping (grudges, kill credit, lags) and its cleanup
    /// on death must leave both sides in the archetype they spawned in.
    #[test]
    fn fight_bookkeeping_and_cleanup_keep_the_archetype() {
        let mut world = world();
        let room = world.spawn(Room).id();
        let p = world
            .spawn((Player, Located(room), Health { hp: 10, max: 10 }))
            .id();
        let m = world
            .spawn((Mob, Located(room), Health { hp: 10, max: 10 }))
            .id();
        let (pa, ma) = (archetype_of(&world, p), archetype_of(&world, m));

        crate::combat::remember_attacker(&mut world, m, p);
        world.get_mut::<HateList>(m).unwrap().push(p);
        crate::combat::record_damager(&mut world, m, p);
        crate::commands::input_queue::impose_wait(&mut world, p, 30);
        crate::commands::input_queue::impose_wait(&mut world, m, 30);
        assert!(crate::commands::input_queue::wait_active(&world, m));
        assert_eq!(
            world
                .get::<DamagedBy>(m)
                .and_then(|d| d.0)
                .map(|d| d.attacker),
            Some(p)
        );
        assert_eq!(archetype_of(&world, p), pa);
        assert_eq!(archetype_of(&world, m), ma);

        crate::commands::input_queue::clear_wait(&mut world, m);
        reset::<DamagedBy>(&mut world, m);
        reset::<HateList>(&mut world, m);
        reset::<MobMemory>(&mut world, m);
        reset::<ReengageLag>(&mut world, p);
        assert!(!crate::commands::input_queue::wait_active(&world, m));
        assert!(world.get::<HateList>(m).unwrap().0.is_empty());
        assert!(world.get::<DamagedBy>(m).unwrap().0.is_none());
        assert_eq!(archetype_of(&world, p), pa);
        assert_eq!(archetype_of(&world, m), ma);
    }

    /// Scripts make mobs run commands; only players keep an idle clock.
    #[test]
    fn only_players_get_an_input_stamp() {
        let mut world = world();
        let p = world.spawn(Player).id();
        let m = world.spawn(Mob).id();
        crate::commands::dispatch(&mut world, m, "");
        crate::commands::dispatch(&mut world, p, "");
        assert!(world.get::<mud_world::LastInputAt>(m).is_none());
        assert!(world.get::<mud_world::LastInputAt>(p).is_some());
    }

    #[test]
    fn reset_ignores_entities_without_the_component() {
        let mut world = world();
        let e = world.spawn_empty().id();
        reset::<HateList>(&mut world, e);
        assert!(world.get::<HateList>(e).is_none());
        world.despawn(e);
        reset::<HateList>(&mut world, e);
    }
}
