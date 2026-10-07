//! `call` — summon your mob followers (hired hands, charmed pets) to
//! your room. Ported from the C++ `cmd_call`: every follower not already
//! with you "heeds the call" and is moved to your location. Only NPC
//! followers are pulled; players following you keep their own agency.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Fighting, Follower, Located, Mob, RiddenBy};

use crate::commands::{
    Category, Command, Help, broadcast_room_except_players_rendered, name_of, send_to, try_remove,
};

inventory::submit! {
    Command {
        names: &["call"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "call",
            summary: "Call your followers to your location.",
            long: "Every mob following you (pets, hirelings, charmed \
                   creatures) that isn't already in your room leaves \
                   whatever it was doing and joins you. Mounts with a \
                   rider are left alone.",
        },
        run: cmd_call,
    }
}

fn cmd_call(world: &mut World, player: Entity, _args: &str) {
    let Some(here) = world.get::<Located>(player).map(|l| l.0) else {
        return;
    };
    let followers: Vec<(Entity, Option<Entity>)> = {
        let mut q = world
            .query_filtered::<(Entity, &Follower, Option<&Located>, Option<&RiddenBy>), With<Mob>>(
            );
        q.iter(world)
            .filter(|(_, f, _, rider)| f.0 == player && rider.is_none())
            .map(|(e, _, loc, _)| (e, loc.map(|l| l.0)))
            .collect()
    };
    if followers.is_empty() {
        send_to(world, player, "You have no followers to call.\r\n");
        return;
    }
    let caller = name_of(world, player);
    let mut count = 0;
    for (follower, old_room) in followers {
        if old_room == Some(here) {
            continue;
        }
        // A pet the room refuses stays where it is (the caller is in the
        // destination, so a staff caller admits their own followers).
        if !crate::room_access::entry_allowed_following(world, follower, here, Some(player), false)
        {
            continue;
        }
        let follower_name = name_of(world, follower);
        if let Some(old) = old_room {
            broadcast_room_except_players_rendered(
                world,
                old,
                &[follower],
                &format!("{follower_name} heeds the call of {caller}.\r\n"),
            );
        }
        try_remove::<Fighting>(world, follower);
        if world.get::<Located>(follower).is_some() {
            world.entity_mut(follower).insert(Located(here));
        } else if let Ok(mut em) = world.get_entity_mut(follower) {
            em.insert(Located(here));
        }
        send_to(
            world,
            follower,
            format!("You heed the call of {caller}.\r\n"),
        );
        count += 1;
    }
    if count == 0 {
        send_to(world, player, "All your followers are already here.\r\n");
        return;
    }
    send_to(world, player, "You call for your followers.\r\n");
    broadcast_room_except_players_rendered(
        world,
        here,
        &[player],
        &format!("{caller} calls for their followers.\r\n"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{drain, player_in};
    use mud_world::Named;

    fn pet(world: &mut World, room: Entity, owner: Entity, name: &str) -> Entity {
        world
            .spawn((
                Mob,
                Named {
                    name: name.to_string(),
                },
                Located(room),
                Follower(owner),
            ))
            .id()
    }

    #[test]
    fn pulls_distant_followers_and_skips_present_ones() {
        let mut world = World::new();
        let here = world.spawn_empty().id();
        let away = world.spawn_empty().id();
        let (player, mut rx) = player_in(&mut world, here);
        let far = pet(&mut world, away, player, "a wolf");
        let near = pet(&mut world, here, player, "a hawk");
        let stranger = world
            .spawn((
                Mob,
                Located(away),
                Named {
                    name: "a bear".to_string(),
                },
            ))
            .id();
        world.entity_mut(far).insert(Fighting(stranger));

        cmd_call(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("You call for your followers."), "{out}");
        assert_eq!(world.get::<Located>(far).unwrap().0, here);
        assert_eq!(world.get::<Located>(near).unwrap().0, here);
        assert!(world.get::<Fighting>(far).is_none());
        // Unrelated mob stays put.
        assert_eq!(world.get::<Located>(stranger).unwrap().0, away);

        cmd_call(&mut world, player, "");
        assert!(drain(&mut rx).contains("All your followers are already here."));
    }

    #[test]
    fn no_followers_message() {
        let mut world = World::new();
        let here = world.spawn_empty().id();
        let (player, mut rx) = player_in(&mut world, here);
        cmd_call(&mut world, player, "");
        assert!(drain(&mut rx).contains("You have no followers to call."));
    }
}
