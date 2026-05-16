//! `release` — end the ghost cycle, restore HP, return to recall.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Ghost, Health, Located, Profile, RaceDefaults, RecallPoint, WorldKeyIndex};

use crate::commands::{
    Category, Command, Help, cmd_look, send_to, try_insert, try_remove,
};

inventory::submit! {
    Command {
        names: &["release"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: Help {
            usage: "release",
            summary: "End the ghost cycle: restore HP, return to recall.",
            long: "When you die, your spirit is left where you fell while \
                   your corpse drops at the death scene. `release` returns \
                   you to your recall point at full HP. Refused when you \
                   aren't currently dead.",
        },
        run: cmd_release,
    }
}

fn cmd_release(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Ghost>(player).is_none() {
        send_to(
            world,
            player,
            "You aren't dead. Nothing to release.\r\n",
        );
        return;
    }
    // Stamina is intentionally left at whatever it was when they died —
    // the death-corpse cycle's recovery cost is the move back to recall
    // plus losing stamina momentum, not a hard reset.
    if let Some(mut hp) = world.get_mut::<Health>(player) {
        hp.hp = hp.max;
    }
    try_remove::<Ghost>(world, player);
    // Respawn precedence (G4.2). Save_location (rented/camped) is
    // not modeled yet — when it lands, prepend it to this chain.
    //   1. Last touchstone (RecallPoint)
    //   2. Race home room (Races.start_room_*)
    //   3. The Void — last-resort fallback, should never fire in
    //      practice if races have start_room rows authored.
    let target = world.get::<RecallPoint>(player).map(|r| r.0);
    let target = target.or_else(|| {
        let race = world.get::<Profile>(player).map(|p| p.race.clone())?;
        let (zone, id) = world
            .get_resource::<RaceDefaults>()?
            .start_room_by_race
            .get(&race)
            .copied()?;
        world
            .resource::<WorldKeyIndex>()
            .rooms
            .get(&(zone, id))
            .copied()
    });
    let target = target.or_else(|| {
        world
            .resource::<WorldKeyIndex>()
            .rooms
            .get(&(0, 0))
            .copied()
    });
    let Some(target) = target else {
        send_to(
            world,
            player,
            "Your spirit has nowhere to return to. Speak with an immortal.\r\n",
        );
        return;
    };
    if world.get_entity(target).is_err() {
        send_to(
            world,
            player,
            "Your recall point has vanished. Speak with an immortal.\r\n",
        );
        return;
    }
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    } else {
        try_insert(world, player, Located(target));
    }
    send_to(
        world,
        player,
        "Your spirit settles back into your body. You return, alive once more.\r\n",
    );
    cmd_look(world, player, "");
}
