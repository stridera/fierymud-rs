//! Drowning. Players standing in an Underwater sector without the
//! Flying marker take steady HP damage every few ticks. Mobs are
//! exempt — sea creatures, drowned dead, etc. need to live there.
//! No swim ability gates this yet; once the schema models a Swim
//! skill the filter can grow a `Without<Swim>` clause.

use bevy_ecs::prelude::*;
use mud_db::enums::Sector;
use mud_world::{Flying, Frozen, Ghost, Located, Online, Player, RoomSector};

use crate::TickCount;
use crate::commands::{apply_damage, send_to};

/// One drowning tick = three seconds of held breath.
const DROWNING_PERIOD_TICKS: u64 = 30;
/// HP lost per drowning tick. A 100-HP player drowns in ~100s,
/// fast enough to be threatening, slow enough to swim out.
const DROWNING_DAMAGE: i32 = 3;

pub fn drowning_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(DROWNING_PERIOD_TICKS) {
        return;
    }
    // Snapshot every player whose current room is Underwater and
    // who isn't Flying. Ghosts / Frozen players skip — death isn't
    // re-entered, and frozen players are admin-protected.
    let drowners: Vec<(Entity, Entity, String)> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &mud_world::Named),
            (
                With<Player>,
                With<Online>,
                Without<Ghost>,
                Without<Frozen>,
                Without<Flying>,
            ),
        >();
        q.iter(world)
            .filter(|(_, l, _)| {
                world
                    .get::<RoomSector>(l.0)
                    .is_some_and(|s| matches!(s.0, Sector::Underwater))
            })
            .map(|(e, l, n)| (e, l.0, n.name.clone()))
            .collect()
    };
    for (entity, room, name) in drowners {
        send_to(world, entity, "You can't breathe! You're drowning!\r\n");
        let (dead, _) = apply_damage(world, entity, DROWNING_DAMAGE);
        if dead {
            crate::combat::handle_death(world, entity, &name, room);
        }
    }
}
