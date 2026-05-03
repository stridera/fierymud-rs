//! `recall` / `home` — teleport to the player's bound recall room.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Fighting, Located, Mounted, RecallPoint};

use crate::commands::{
    Category, Command, Help, broadcast_room_except_players_rendered, cmd_look, name_of,
    send_to, try_remove,
};

inventory::submit! {
    Command {
        names: &["recall", "home"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: Help {
            usage: "recall",
            summary: "Teleport to your recall point.",
            long: "Move instantly to your saved recall room. If you haven't \
                   set one, you're told so. Use `setrecall` in your current \
                   room to bind it as your recall point.",
        },
        run: cmd_recall,
    }
}

fn cmd_recall(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't recall while fighting!\r\n");
        return;
    }
    let Some(target) = world.get::<RecallPoint>(player).map(|r| r.0) else {
        send_to(
            world,
            player,
            "You have no recall point set. Touch a touchstone room object \
             with `touch <object>` to bind one. (Builders can use `setrecall` \
             from any room.)\r\n",
        );
        return;
    };
    if world.get_entity(target).is_err() {
        send_to(world, player, "Your recall point has vanished.\r\n");
        try_remove::<RecallPoint>(world, player);
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; can't recall.\r\n");
        return;
    };
    let from_room = located.0;
    if from_room == target {
        send_to(world, player, "You're already at your recall point.\r\n");
        return;
    }

    let mover_name = name_of(world, player);

    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[player],
        &format!("{mover_name} fades away in a flash of light.\r\n"),
    );

    let mount = world.get::<Mounted>(player).map(|m| m.0);
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    }
    if let Some(mount) = mount
        && let Some(mut l) = world.get_mut::<Located>(mount)
    {
        l.0 = target;
    }

    broadcast_room_except_players_rendered(
        world,
        target,
        &[player],
        &format!("{mover_name} appears in a flash of light.\r\n"),
    );

    send_to(world, player, "The world swirls around you...\r\n");
    cmd_look(world, player, "");
}
