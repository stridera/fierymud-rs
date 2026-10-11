//! Shared room-change helpers. Kept in `mud-world` so the game server and the
//! Lua bindings (`mud-script`, which cannot depend on `mud-server`) move
//! characters the same way.

use bevy_ecs::prelude::*;

use crate::components::{
    Falling, Fighting, Flying, Ghost, Item, Located, Mob, MovementModeTag, Player, RoomSector,
};

/// Cached scan for "who is fighting X". Building a `QueryState` walks every
/// archetype in the world, which cost milliseconds per move at 5k mobs when
/// done for each of them; the cached state only catches up on archetypes
/// created since the last use.
#[derive(Resource)]
struct FightingScan(QueryState<(Entity, &'static Fighting)>);

/// Everyone whose `Fighting` points at `who`.
fn attackers_of(world: &mut World, who: Entity) -> Vec<Entity> {
    if !world.contains_resource::<FightingScan>() {
        let state = world.query::<(Entity, &Fighting)>();
        world.insert_resource(FightingScan(state));
    }
    world.resource_scope(|world, mut scan: Mut<FightingScan>| {
        scan.0
            .iter(world)
            .filter(|(_, f)| f.0 == who)
            .map(|(e, _)| e)
            .collect()
    })
}

/// Legacy `char_from_room`: leaving a room ends the mover's own fight and
/// every fight against them (`stop_fighting` + `stop_attackers`). Without
/// this the stale `Fighting` links survive until the next combat tick, so a
/// target that walked back in before the tick would resume the fight.
pub fn stop_fighting_both_ways(world: &mut World, who: Entity) {
    if let Ok(mut e) = world.get_entity_mut(who) {
        e.remove::<Fighting>();
    }
    for a in attackers_of(world, who) {
        if let Ok(mut e) = world.get_entity_mut(a) {
            e.remove::<Fighting>();
        }
    }
}

/// Put `who` in `dest`, ending its fights first when the room actually
/// changes. Inserts `Located` even if `who` had none. Items never fight, so
/// they skip the fight cleanup. A despawned `who` is ignored.
pub fn move_to_room(world: &mut World, who: Entity, dest: Entity) {
    if world.get_entity(who).is_err() {
        return;
    }
    let here = world.get::<Located>(who).map(|l| l.0);
    if here != Some(dest) && world.get::<Item>(who).is_none() {
        stop_fighting_both_ways(world, who);
    }
    world.entity_mut(who).insert(Located(dest));
    if world.get::<Item>(who).is_none() {
        begin_fall_if_unsupported(world, who);
    }
}

/// Legacy `falling_check`: a player or mob standing in an air-sector room
/// with nothing holding it up starts to fall (the server's `gravity_tick`
/// does the falling and re-checks everything else: mounts, level, exits).
/// Called on every room entry and wherever flight is lost. No-op for items,
/// ghosts, fliers (the `Flying` marker, or a proto that flies) and anyone
/// already falling.
pub fn begin_fall_if_unsupported(world: &mut World, who: Entity) {
    if world.get::<Falling>(who).is_some()
        || world.get::<Flying>(who).is_some()
        || world.get::<Ghost>(who).is_some()
        || (world.get::<Player>(who).is_none() && world.get::<Mob>(who).is_none())
        || world
            .get::<MovementModeTag>(who)
            .is_some_and(|m| m.0 == mud_db::enums::MovementMode::Flying)
    {
        return;
    }
    let Some(room) = world.get::<Located>(who).map(|l| l.0) else {
        return;
    };
    if world
        .get::<RoomSector>(room)
        .is_some_and(|s| s.0 == mud_db::enums::Sector::Air)
    {
        world.entity_mut(who).insert(Falling {
            start_room: room,
            distance: 0,
            due_tick: 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_ends_the_movers_fight_and_every_fight_against_them() {
        let mut world = World::new();
        let (here, there) = (world.spawn_empty().id(), world.spawn_empty().id());
        let mover = world.spawn(Located(here)).id();
        let foe = world.spawn((Located(here), Fighting(mover))).id();
        let other_foe = world.spawn((Located(here), Fighting(mover))).id();
        let bystander = world.spawn((Located(here), Fighting(foe))).id();
        world.entity_mut(mover).insert(Fighting(foe));
        move_to_room(&mut world, mover, there);
        assert!(world.get::<Fighting>(mover).is_none());
        assert!(world.get::<Fighting>(foe).is_none());
        assert!(world.get::<Fighting>(other_foe).is_none());
        assert_eq!(world.get::<Fighting>(bystander).map(|f| f.0), Some(foe));
        assert_eq!(world.get::<Located>(mover).map(|l| l.0), Some(there));
    }

    #[test]
    fn an_unfought_mover_who_is_being_attacked_still_frees_the_attacker() {
        let mut world = World::new();
        let (here, there) = (world.spawn_empty().id(), world.spawn_empty().id());
        let mover = world.spawn(Located(here)).id();
        let attacker = world.spawn((Located(here), Fighting(mover))).id();
        move_to_room(&mut world, mover, there);
        assert!(world.get::<Fighting>(attacker).is_none());
    }

    macro_rules! markers {
        ($($m:ident),*) => { $( #[derive(Component)] struct $m; )* };
    }
    markers!(M0, M1, M2, M3, M4, M5, M6, M7);

    /// 5.5k mobs spread over 256 archetypes, one fight; 5k moves. Measures
    /// what the per-move fight cleanup costs.
    #[test]
    fn move_cleanup_scales_to_thousands_of_mobs() {
        const MOBS: usize = 5500;
        const MOVES: usize = 5000;
        let mut world = World::new();
        let rooms: Vec<Entity> = (0..50).map(|_| world.spawn_empty().id()).collect();
        let mut mobs = Vec::new();
        for i in 0..MOBS {
            let mut e = world.spawn((Mob, Located(rooms[i % rooms.len()])));
            if i & 1 != 0 {
                e.insert(M0);
            }
            if i & 2 != 0 {
                e.insert(M1);
            }
            if i & 4 != 0 {
                e.insert(M2);
            }
            if i & 8 != 0 {
                e.insert(M3);
            }
            if i & 16 != 0 {
                e.insert(M4);
            }
            if i & 32 != 0 {
                e.insert(M5);
            }
            if i & 64 != 0 {
                e.insert(M6);
            }
            if i & 128 != 0 {
                e.insert(M7);
            }
            mobs.push(e.id());
        }
        let (a, b) = (mobs[0], mobs[1]);
        world.entity_mut(a).insert(Fighting(b));
        world.entity_mut(b).insert(Fighting(a));
        let start = std::time::Instant::now();
        for i in 0..MOVES {
            let mob = mobs[2 + i % (MOBS - 2)];
            let dest = rooms[(i * 7 + 1) % rooms.len()];
            move_to_room(&mut world, mob, dest);
        }
        let took = start.elapsed();
        eprintln!(
            "move_to_room: {MOVES} moves among {MOBS} mobs in {took:?} ({:?} each)",
            took / u32::try_from(MOVES).unwrap(),
        );
        assert_eq!(world.get::<Fighting>(a).map(|f| f.0), Some(b));
        assert_eq!(world.get::<Fighting>(b).map(|f| f.0), Some(a));
    }
}
