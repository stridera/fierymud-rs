//! `recall` / `home` — teleport to the player's bound recall room.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Fighting, Located, Mounted, Profile, RaceDefaults, RecallPoint, WorldKeyIndex};

use crate::commands::{Category, Command, Help, cmd_look, name_of, send_to, try_remove};

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
                   bound one yet you're told so — `touch <touchstone>` in a \
                   sanctuary room to bind your recall there. Builders can \
                   use `setrecall` from any room.",
        },
        run: cmd_recall,
    }
}

/// The room `recall` takes `player` to: the bound `RecallPoint`, or (for
/// someone who never bound one) their race's start room, the same fallback
/// the respawn chain uses.
pub(crate) fn recall_room(world: &World, player: Entity) -> Option<Entity> {
    if let Some(r) = world.get::<RecallPoint>(player).map(|r| r.0)
        && world.get_entity(r).is_ok()
    {
        return Some(r);
    }
    let race = world.get::<Profile>(player).map(|p| p.race.clone())?;
    let key = world
        .get_resource::<RaceDefaults>()?
        .start_room_by_race
        .get(&race)
        .copied()?;
    world
        .get_resource::<WorldKeyIndex>()?
        .rooms
        .get(&key)
        .copied()
}

fn cmd_recall(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't recall while fighting!\r\n");
        return;
    }
    // NoRecallRoom gate — `Room.allows_recall = false` blocks the
    // teleport home. Checked before the recall-point lookup so a
    // player who hasn't bound one yet still gets the dead-magic
    // refusal rather than the "no recall point" message that
    // would imply touching a touchstone here might help.
    if let Some(located) = world.get::<Located>(player)
        && world.get::<mud_world::NoRecallRoom>(located.0).is_some()
    {
        send_to(world, player, "Some power blocks your recall.\r\n");
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
    if crate::room_access::refuse_entry(world, player, target) {
        return;
    }

    let mover_name = name_of(world, player);
    let mover_capped = crate::commands::cap_sentence_start(&mover_name);

    crate::commands::broadcast_room_visible(
        world,
        from_room,
        player,
        &[player],
        &format!("{mover_capped} fades away in a flash of light.\r\n"),
    );

    let mount = world.get::<Mounted>(player).map(|m| m.0);
    if world.get::<Located>(player).is_some() {
        world.entity_mut(player).insert(Located(target));
    }
    if let Some(mount) = mount
        && world.get::<Located>(mount).is_some()
    {
        world.entity_mut(mount).insert(Located(target));
    }

    crate::commands::broadcast_room_visible(
        world,
        target,
        player,
        &[player],
        &format!("{mover_capped} appears in a flash of light.\r\n"),
    );

    send_to(world, player, "The world swirls around you...\r\n");
    cmd_look(world, player, "");
}
