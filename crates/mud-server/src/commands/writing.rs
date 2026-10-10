//! `write <object> <message>` — scribble on a Note-type object.
//!
//! Mirrors the C++ `cmd_write`: the target must be a `Note` item found
//! in your inventory first, then the room. The message replaces the
//! item's examine text (the `Description` component that `read` /
//! `examine` print, and which character-item persistence already
//! round-trips as the custom examine description).

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole};
use mud_world::{Description, Located, ObjectPrototypes, WorldKey};

use crate::commands::{
    Category, Command, Help, ItemClass, broadcast_room_except_players_rendered, find_item, name_of,
    send_to,
};

inventory::submit! {
    Command {
        names: &["write"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "write <object> <message>",
            summary: "Write a message on a note or paper.",
            long: "Writes the message onto a note-type object you carry \
                   or that lies in the room, replacing whatever was \
                   there. Anyone can then 'read' it.",
        },
        run: cmd_write,
    }
}

/// Inventory first, then the floor of the player's room.
fn find_writable_target(world: &mut World, player: Entity, needle: &str) -> Option<Entity> {
    find_item(world, player, needle, ItemClass::CarriedFirst)
}

fn cmd_write(world: &mut World, player: Entity, args: &str) {
    let trimmed = mud_net::sanitize_text(args, false);
    let trimmed = trimmed.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let Some(target_kw) = parts.next().filter(|w| !w.is_empty()) else {
        send_to(
            world,
            player,
            "Usage: write <object> [message]\r\n\
             Examples:\r\n  write note Hello everyone!\r\n",
        );
        return;
    };
    let message = parts.next().map_or("", str::trim);
    let Some(target) = find_writable_target(world, player, target_kw) else {
        send_to(
            world,
            player,
            format!("You don't have '{target_kw}' and it's not here.\r\n"),
        );
        return;
    };
    let target_name = name_of(world, target);
    let is_note = world.get::<WorldKey>(target).is_some_and(|k| {
        world
            .get_resource::<ObjectPrototypes>()
            .and_then(|p| p.by_key.get(&(k.zone, k.id)))
            .is_some_and(|p| p.r#type == ObjectType::Note)
    });
    if !is_note {
        send_to(
            world,
            player,
            format!("You can't write on {target_name}.\r\n"),
        );
        return;
    }
    if message.is_empty() {
        send_to(
            world,
            player,
            format!("What do you want to write on {target_name}?\r\n"),
        );
        return;
    }
    world
        .entity_mut(target)
        .insert(Description(format!("It reads:\r\n  \"{message}\"")));
    send_to(
        world,
        player,
        format!("You write on {target_name}:\r\n  \"{message}\"\r\n"),
    );
    let who = name_of(world, player);
    if let Some(room) = world.get::<Located>(player).map(|l| l.0) {
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{who} writes something on {target_name}.\r\n"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{Rx, drain, object_proto, player_in};
    use mud_world::{Item, Keywords, Named};

    fn setup() -> (World, Entity, Entity, Rx) {
        let mut world = World::new();
        let mut protos = ObjectPrototypes::default();
        protos
            .by_key
            .insert((1, 1), object_proto(1, 1, ObjectType::Note));
        protos
            .by_key
            .insert((1, 2), object_proto(1, 2, ObjectType::Other));
        world.insert_resource(protos);
        let room = world.spawn_empty().id();
        let (player, rx) = player_in(&mut world, room);
        (world, room, player, rx)
    }

    fn spawn_item(world: &mut World, holder: Entity, name: &str, kw: &str, id: i32) -> Entity {
        world
            .spawn((
                Item,
                Named {
                    name: name.to_string(),
                },
                Keywords(vec![kw.to_string()]),
                WorldKey { zone: 1, id },
                Located(holder),
            ))
            .id()
    }

    #[test]
    fn writes_on_carried_note() {
        let (mut world, _room, player, mut rx) = setup();
        let note = spawn_item(&mut world, player, "a blank note", "note", 1);
        cmd_write(&mut world, player, "note Meet at noon");
        let out = drain(&mut rx);
        assert!(out.contains("You write on a blank note:"), "{out}");
        let desc = world.get::<Description>(note).unwrap();
        assert!(desc.0.contains("Meet at noon"));
    }

    #[test]
    fn writes_on_note_lying_in_room() {
        let (mut world, room, player, mut rx) = setup();
        let note = spawn_item(&mut world, room, "a crumpled note", "note", 1);
        cmd_write(&mut world, player, "note hi");
        assert!(drain(&mut rx).contains("You write on a crumpled note"));
        assert!(world.get::<Description>(note).is_some());
    }

    #[test]
    fn refuses_non_note_and_missing_message() {
        let (mut world, _room, player, mut rx) = setup();
        let rock = spawn_item(&mut world, player, "a rock", "rock", 2);
        cmd_write(&mut world, player, "rock hello");
        assert!(drain(&mut rx).contains("You can't write on a rock."));
        assert!(world.get::<Description>(rock).is_none());

        spawn_item(&mut world, player, "a blank note", "note", 1);
        cmd_write(&mut world, player, "note");
        assert!(drain(&mut rx).contains("What do you want to write on a blank note?"));
        cmd_write(&mut world, player, "quill hello");
        assert!(drain(&mut rx).contains("You don't have 'quill'"));
    }
}
