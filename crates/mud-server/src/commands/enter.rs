//! `enter <portal>` — step into a portal in the room and teleport
//! to its destination.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole};
use mud_world::{Fighting, Item, Keywords, Located, Named, ObjectPrototypes, WorldKey, WorldKeyIndex};

use crate::commands::{
    Category, Command, Help, broadcast_room_except_players_rendered, cmd_look, matches,
    name_of, send_rendered, send_to,
};

inventory::submit! {
    Command {
        names: &["leave"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: Help {
            usage: "leave",
            summary: "Exit your current vehicle / mount.",
            long: "Inverse of `enter` for vehicles. Today the only \
                   in-vehicle state we model is mounted, so `leave` \
                   is a synonym for `dismount`. When boats / \
                   carriages land it'll cover those too.",
        },
        run: cmd_leave,
    }
}

inventory::submit! {
    Command {
        names: &["enter"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: Help {
            usage: "enter <portal>",
            summary: "Step into a portal in the room.",
            long: "Reads the portal's `Destination` and teleports you to \
                   the matching room. Refused while fighting. Portals \
                   with a missing or unresolved destination shimmer \
                   harmlessly.",
        },
        run: cmd_enter,
    }
}

fn cmd_leave(world: &mut World, player: Entity, _args: &str) {
    use mud_world::Mounted;
    if world.get::<Mounted>(player).is_some() {
        // Defer to the existing dismount path so the broadcast +
        // RiddenBy cleanup stay in one place.
        crate::commands::info::cmd_dismount(world, player, "");
        return;
    }
    send_to(
        world,
        player,
        "You aren't inside any vehicle or mount to leave.\r\n",
    );
}

#[allow(clippy::too_many_lines)]
fn cmd_enter(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Enter what?\r\n");
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't slip into a portal mid-fight.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let from_room = located.0;
    let lc = needle.to_ascii_lowercase();
    let portal_match = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &Named, Option<&Keywords>, &WorldKey),
            With<Item>,
        >();
        q.iter(world)
            .find(|(_, l, n, kw, _)| l.0 == from_room && matches(&lc, n, *kw))
            .map(|(e, _, _, _, k)| (e, *k))
    };
    let Some((portal, key)) = portal_match else {
        send_rendered(
            world,
            player,
            &format!("There's no portal called '{needle}' here.\r\n"),
        );
        return;
    };
    let proto = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(key.zone, key.id))
        .cloned();
    let Some(proto) = proto else {
        send_to(world, player, "That portal's prototype is missing.\r\n");
        return;
    };
    if !matches!(proto.r#type, ObjectType::Portal) {
        let portal_name = name_of(world, portal);
        send_rendered(
            world,
            player,
            &format!("{portal_name} isn't something you can enter.\r\n"),
        );
        return;
    }
    let Some(vnum) = proto.portal_destination_vnum else {
        send_rendered(
            world,
            player,
            &format!("{} leads nowhere right now.\r\n", proto.name),
        );
        return;
    };
    let dest_key = world
        .resource::<WorldKeyIndex>()
        .legacy_vnums
        .get(&vnum)
        .copied();
    let dest_room = dest_key.and_then(|k| {
        world
            .resource::<WorldKeyIndex>()
            .rooms
            .get(&k)
            .copied()
    });
    let Some(dest) = dest_room else {
        send_rendered(
            world,
            player,
            &format!("{} shimmers, but the destination is gone.\r\n", proto.name),
        );
        return;
    };
    if dest == from_room {
        send_to(world, player, "It would just spit you back out where you are.\r\n");
        return;
    }
    let mover_name = name_of(world, player);
    let mover_capped = crate::commands::cap_sentence_start(&mover_name);
    let portal_name = proto.name.clone();
    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[player],
        &format!("{mover_capped} steps into {portal_name} and vanishes.\r\n"),
    );
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = dest;
    }
    broadcast_room_except_players_rendered(
        world,
        dest,
        &[player],
        &format!("{mover_capped} steps out of a swirling portal.\r\n"),
    );
    send_rendered(
        world,
        player,
        &format!("You step into {portal_name}...\r\n"),
    );
    cmd_look(world, player, "");
}
