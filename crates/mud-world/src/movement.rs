//! Shared room-change helpers. Kept in `mud-world` so the game server and the
//! Lua bindings (`mud-script`, which cannot depend on `mud-server`) move
//! characters the same way.

use bevy_ecs::prelude::*;

use crate::components::{
    Falling, Fighting, Flying, Ghost, Item, Located, Mob, MovementModeTag, Player, RoomSector,
};

/// Legacy `char_from_room`: leaving a room ends the mover's own fight and
/// every fight against them (`stop_fighting` + `stop_attackers`). Without
/// this the stale `Fighting` links survive until the next combat tick, so a
/// target that walked back in before the tick would resume the fight.
pub fn stop_fighting_both_ways(world: &mut World, who: Entity) {
    if let Ok(mut e) = world.get_entity_mut(who) {
        e.remove::<Fighting>();
    }
    let attackers: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Fighting)>();
        q.iter(world)
            .filter(|(_, f)| f.0 == who)
            .map(|(e, _)| e)
            .collect()
    };
    for a in attackers {
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
