//! `call` — whistle for your mob followers (hired hands, charmed pets).
//! Ported from legacy `do_call`: followers elsewhere rush to your room,
//! ones already here look at you curiously. Only NPC followers are
//! pulled; players following you keep their own agency, and a mount you
//! are riding is simply reported as such.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Follower, Located, Mob, RiddenBy};

use crate::commands::{
    Category, Command, Help, broadcast_room_except_players_rendered, cap_sentence_start, name_of,
    send_to,
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
                   whatever it was doing and joins you. A mount you are \
                   riding is left alone.",
        },
        run: cmd_call,
    }
}

fn cmd_call(world: &mut World, player: Entity, _args: &str) {
    let Some(here) = world.get::<Located>(player).map(|l| l.0) else {
        return;
    };
    // Legacy `do_call`: every charmed/pet follower, ridden or not.
    let followers: Vec<(Entity, Option<Entity>, bool)> = {
        let mut q = world
            .query_filtered::<(Entity, &Follower, Option<&Located>, Option<&RiddenBy>), With<Mob>>(
            );
        q.iter(world)
            .filter(|(_, f, _, _)| f.0 == player)
            .map(|(e, _, loc, rider)| (e, loc.map(|l| l.0), rider.is_some_and(|r| r.0 == player)))
            .collect()
    };
    if followers.is_empty() {
        send_to(world, player, "You don't have a pet to call out to.\r\n");
        return;
    }
    // A mount you are sitting on is already with you; say so rather than
    // whistling for it (and never drag it away from under you).
    if followers.iter().all(|&(_, _, ridden)| ridden) {
        let mount = followers[0].0;
        let mount_name = name_of(world, mount);
        send_to(
            world,
            player,
            format!("You're already riding {mount_name}.\r\n"),
        );
        return;
    }
    let caller = name_of(world, player);
    let him = super::look_auras::pronouns(&super::look_auras::target_gender(world, player)).2;
    send_to(world, player, "You whistle sharply.\r\n");
    for (follower, old_room, ridden) in followers {
        if ridden {
            continue;
        }
        let follower_name = name_of(world, follower);
        if old_room == Some(here) {
            send_to(
                world,
                player,
                cap_sentence_start(&format!("{follower_name} looks at you curiously.\r\n")),
            );
            send_to(
                world,
                follower,
                format!("You look at {caller} as they whistle for you.\r\n"),
            );
            broadcast_room_except_players_rendered(
                world,
                here,
                &[player, follower],
                &cap_sentence_start(&format!(
                    "{caller} whistles at {follower_name}, who looks at {him} curiously.\r\n"
                )),
            );
            continue;
        }
        // A pet the room refuses stays where it is (the caller is in the
        // destination, so a staff caller admits their own followers).
        if !crate::room_access::entry_allowed_following(world, follower, here, Some(player), false)
        {
            continue;
        }
        // Calling a pet away ends its fights both ways (and everyone
        // fighting it), like any other room change.
        mud_world::movement::move_to_room(world, follower, here);
        send_to(
            world,
            player,
            cap_sentence_start(&format!(
                "{follower_name} rushes into the room and looks at you expectantly.\r\n"
            )),
        );
        send_to(
            world,
            follower,
            format!("You hear {caller} whistle in the distance and rush to rejoin them.\r\n"),
        );
        broadcast_room_except_players_rendered(
            world,
            here,
            &[player, follower],
            &cap_sentence_start(&format!(
                "{caller} whistles sharply and {follower_name} rushes in to join {him}.\r\n"
            )),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{drain, player_in};
    use mud_world::{Fighting, Named};

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
        world.entity_mut(stranger).insert(Fighting(far));

        cmd_call(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("You whistle sharply."), "{out}");
        assert!(
            out.contains("A wolf rushes into the room and looks at you expectantly."),
            "{out}"
        );
        assert_eq!(world.get::<Located>(far).unwrap().0, here);
        assert_eq!(world.get::<Located>(near).unwrap().0, here);
        assert!(world.get::<Fighting>(far).is_none());
        // Whoever was fighting the pet stops too.
        assert!(world.get::<Fighting>(stranger).is_none());
        // Unrelated mob stays put.
        assert_eq!(world.get::<Located>(stranger).unwrap().0, away);

        cmd_call(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("A wolf looks at you curiously."), "{out}");
        assert!(out.contains("A hawk looks at you curiously."), "{out}");
        assert!(!out.contains("rushes into the room"), "{out}");
    }

    #[test]
    fn no_followers_message() {
        let mut world = World::new();
        let here = world.spawn_empty().id();
        let (player, mut rx) = player_in(&mut world, here);
        cmd_call(&mut world, player, "");
        assert!(drain(&mut rx).contains("You don't have a pet to call out to."));
    }

    #[test]
    fn calling_while_riding_says_so_and_leaves_the_mount_alone() {
        let mut world = World::new();
        let here = world.spawn_empty().id();
        let (player, mut rx) = player_in(&mut world, here);
        let mare = pet(&mut world, here, player, "a stout mare");
        world.entity_mut(mare).insert(RiddenBy(player));
        world.entity_mut(player).insert(mud_world::Mounted(mare));
        cmd_call(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("You're already riding a stout mare."), "{out}");
        assert!(!out.contains("no followers"), "{out}");
        assert_eq!(world.get::<Located>(mare).unwrap().0, here);
    }

    #[test]
    fn a_ridden_mount_is_skipped_but_other_pets_still_come() {
        let mut world = World::new();
        let here = world.spawn_empty().id();
        let away = world.spawn_empty().id();
        let (player, mut rx) = player_in(&mut world, here);
        let mare = pet(&mut world, here, player, "a stout mare");
        world.entity_mut(mare).insert(RiddenBy(player));
        let wolf = pet(&mut world, away, player, "a wolf");
        cmd_call(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("A wolf rushes into the room"), "{out}");
        assert!(!out.contains("mare"), "{out}");
        assert_eq!(world.get::<Located>(wolf).unwrap().0, here);
    }
}
