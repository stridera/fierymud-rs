//! Shared room-change helpers. Kept in `mud-world` so the game server and the
//! Lua bindings (`mud-script`, which cannot depend on `mud-server`) move
//! characters the same way.

use bevy_ecs::prelude::*;

use crate::components::{Fighting, Item, Located};

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
}
