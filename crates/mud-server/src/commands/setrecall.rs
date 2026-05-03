//! Recall-binding commands.
//!
//! `setrecall` is the admin shortcut. The mortal-facing path is
//! `touch <touchstone>`: the runtime checks the named object's
//! prototype `ObjectType` and writes the player's
//! `RecallPoint` to the current room when the type is
//! `Touchstone`.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole};
use mud_world::{Item, Keywords, Located, Named, ObjectPrototypes, RecallPoint, WorldKey};

use crate::commands::{
    Category, Command, Help, broadcast_room_except_players_rendered, matches, name_of, name_or,
    send_to, try_insert,
};

inventory::submit! {
    Command {
        names: &["setrecall"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "setrecall",
            summary: "Bind your recall point to the current room (admin).",
            long: "Builder+. Saves the room you're standing in as \
                   your recall destination. Persists across logins. \
                   Mortals reset their recall via a touchstone item \
                   (see `touch <object>`).",
        },
        run: cmd_setrecall,
    }
}

inventory::submit! {
    Command {
        names: &["touch"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: Help {
            usage: "touch <object>",
            summary: "Touch an object — touchstones bind your recall.",
            long: "Looks up the named keyword in your inventory or \
                   the room. If the object's prototype `ObjectType` \
                   is `Touchstone`, your recall point binds to the \
                   room you're standing in. Other object types just \
                   refuse — touch is currently a recall-only verb.",
        },
        run: cmd_touch,
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

fn cmd_touch(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim().to_ascii_lowercase();
    if needle.is_empty() {
        send_to(world, player, "Touch what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    // Inventory match wins over room match (player can carry a
    // pocket-touchstone), but we accept either since touchstones are
    // typically placed in fixed rooms.
    let candidate: Option<Entity> = {
        let mut q =
            world.query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
        let in_inv = q
            .iter(world)
            .find(|(_, l, n, kw)| l.0 == player && matches(&needle, n, *kw))
            .map(|(e, _, _, _)| e);
        in_inv.or_else(|| {
            q.iter(world)
                .find(|(_, l, n, kw)| l.0 == located.0 && matches(&needle, n, *kw))
                .map(|(e, _, _, _)| e)
        })
    };
    let Some(item) = candidate else {
        send_to(world, player, format!("You don't see '{needle}' here.\r\n"));
        return;
    };
    let item_name = name_of(world, item);
    let kind = world.get::<WorldKey>(item).and_then(|k| {
        world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(k.zone, k.id))
            .map(|p| p.r#type)
    });
    if kind != Some(ObjectType::Touchstone) {
        send_to(
            world,
            player,
            format!("You touch {item_name}. Nothing happens.\r\n"),
        );
        return;
    }
    try_insert(world, player, RecallPoint(located.0));
    let room_name = name_or(world, located.0, "(unknown)");
    send_to(
        world,
        player,
        format!(
            "You touch {item_name}, and warmth blooms beneath your fingers. \
             Your recall now binds to {room_name}.\r\n"
        ),
    );
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        located.0,
        &[player],
        &format!("{player_name} touches {item_name} and pauses, eyes briefly distant.\r\n"),
    );
}
