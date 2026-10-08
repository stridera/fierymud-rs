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
                   bound one yet you go to your race's start room — 'touch \
                   <touchstone>' in a sanctuary room to bind your recall \
                   there. Builders can use 'setrecall' from any room.",
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
    // A bound point whose room is gone is dropped; recall then falls back to
    // the race start room like the respawn chain.
    if let Some(bound) = world.get::<RecallPoint>(player).map(|r| r.0)
        && world.get_entity(bound).is_err()
    {
        try_remove::<RecallPoint>(world, player);
    }
    let Some(target) = recall_room(world, player) else {
        send_to(
            world,
            player,
            "You have no recall point set. Touch a touchstone room object \
             with 'touch <object>' to bind one. (Builders can use 'setrecall' \
             from any room.)\r\n",
        );
        return;
    };
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
    crate::combat::relocate(world, player, target);
    if let Some(mount) = mount {
        crate::combat::relocate(world, mount, target);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{drain, player_in};
    use mud_world::Named;

    #[test]
    fn unbound_player_recalls_to_the_race_start_room() {
        let mut world = World::new();
        let here = world
            .spawn(Named {
                name: "Here".into(),
            })
            .id();
        let start = world
            .spawn(Named {
                name: "The Temple".into(),
            })
            .id();
        let mut rooms = WorldKeyIndex::default();
        rooms.rooms.insert((30, 1), start);
        world.insert_resource(rooms);
        let mut defaults = RaceDefaults::default();
        defaults.start_room_by_race.insert("HUMAN".into(), (30, 1));
        world.insert_resource(defaults);
        let (player, mut rx) = player_in(&mut world, here);
        world.entity_mut(player).insert(Profile {
            level: 5,
            class_id: None,
            race: "HUMAN".into(),
            experience: 0,
            gender: "neutral".into(),
        });
        assert!(world.get::<RecallPoint>(player).is_none());
        cmd_recall(&mut world, player, "");
        assert_eq!(world.get::<Located>(player).map(|l| l.0), Some(start));
        let text = drain(&mut rx);
        assert!(text.contains("The world swirls around you"), "{text}");
        assert!(!text.contains("no recall point"), "{text}");
    }

    #[test]
    fn no_bound_point_and_no_race_start_room_is_refused() {
        let mut world = World::new();
        let here = world.spawn_empty().id();
        let (player, mut rx) = player_in(&mut world, here);
        cmd_recall(&mut world, player, "");
        assert_eq!(world.get::<Located>(player).map(|l| l.0), Some(here));
        assert!(drain(&mut rx).contains("no recall point"));
    }
}
