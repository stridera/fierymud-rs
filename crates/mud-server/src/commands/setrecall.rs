//! `setrecall` — bind the current room as the player's recall point.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Located, RecallPoint};

use crate::commands::{Category, Command, Help, name_or, send_to, try_insert};

inventory::submit! {
    Command {
        names: &["setrecall"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "setrecall",
            summary: "Bind your recall point to the current room.",
            long: "Saves the current room as your recall destination. \
                   Persists across logins.",
        },
        run: cmd_setrecall,
    }
}

fn cmd_setrecall(world: &mut World, player: Entity, _args: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; can't bind a recall point.\r\n");
        return;
    };
    try_insert(world, player, RecallPoint(located.0));
    let room_name = name_or(world, located.0, "(unknown)");
    send_to(
        world,
        player,
        format!("Recall point bound: {room_name}.\r\n"),
    );
}
