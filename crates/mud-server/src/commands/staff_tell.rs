//! `ptell` (answer a petition) and `page` — legacy `do_ptell`
//! (`act.wizard.cpp`) and `do_page` (`act.comm.cpp`).
//!
//! `petition` (mortal) broadcasts to online immortals. `ptell` is the
//! staff answer: the petitioner gets it privately, and every other
//! immortal who is not mid-edit sees who answered whom. `page` rings a
//! player's terminal bell with a short message.

use bevy_ecs::prelude::*;
use mud_db::enums::{PlayerFlag, UserRole};
use mud_world::{BoardDraft, MailDraft, Named, Online, Player, Posture, PostureKind, Profile};

use super::grants::authority_level;
use super::look_auras::pronouns;
use crate::commands::{Category, Command, Help, has_flag, name_of, send_to};

const NO_PERSON: &str = "There is no one by that name here.\r\n";

/// Legacy `LVL_IMMORT`.
const LVL_IMMORT: i32 = 100;
/// Legacy `LVL_GOD`; `page all` needs a level above it.
const LVL_GOD: i32 = 101;

inventory::submit! {
    Command {
        names: &["ptell"],
        min_role: UserRole::Immortal,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "ptell <player> <message>",
            summary: "Answer a player's petition.",
            long: "Immortal+. Sends <message> privately to the petitioner \
                   and shows the other online immortals who answered whom. \
                   Refused for staff targets (use wiznet) and for a player \
                   who is writing a message.",
        },
        run: cmd_ptell,
    }
}

inventory::submit! {
    Command {
        names: &["page"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "page <player|all> <message>",
            summary: "Beep a player with a message.",
            long: "Builder+. Sends '*<you>* <message>' with two terminal \
                   bells to one player anywhere in the game. 'page all' \
                   reaches everyone and needs a level above 101.",
        },
        run: cmd_page,
    }
}

/// True while the player is composing a mail or board post (legacy
/// `PLR_WRITING` / `EDITING`).
fn is_writing(world: &World, e: Entity) -> bool {
    world.get::<MailDraft>(e).is_some() || world.get::<BoardDraft>(e).is_some()
}

fn is_asleep(world: &World, e: Entity) -> bool {
    world
        .get::<Posture>(e)
        .is_some_and(|p| p.0 == PostureKind::Sleeping)
}

fn gender_pronoun(world: &World, e: Entity) -> &'static str {
    let gender = world
        .get::<Profile>(e)
        .map_or("neutral", |p| p.gender.as_str());
    pronouns(gender).0
}

fn cmd_ptell(world: &mut World, player: Entity, args: &str) {
    let args = args.trim();
    let (target_word, message) = args
        .split_once(char::is_whitespace)
        .map_or((args, ""), |(a, b)| (a, b.trim()));
    if target_word.is_empty() || message.is_empty() {
        send_to(world, player, "Who do you wish to ptell??\r\n");
        return;
    }
    let Some(target) = super::admin_world::find_actor_anywhere(world, player, target_word) else {
        send_to(world, player, NO_PERSON);
        return;
    };
    // Mobs are not petitioners; the lookup finds them like any character.
    if world.get::<Player>(target).is_none() || world.get::<Online>(target).is_none() {
        send_to(world, player, NO_PERSON);
        return;
    }
    if target == player {
        send_to(
            world,
            player,
            "You need mental help. Try ptelling someone besides yourself.\r\n",
        );
        return;
    }
    if authority_level(world, target) >= LVL_IMMORT {
        send_to(world, player, "Just use wiznet!\r\n");
        return;
    }
    if is_writing(world, target) {
        let he = gender_pronoun(world, target);
        send_to(
            world,
            player,
            format!("{he}'s writing a message right now; try again later.\r\n"),
        );
        return;
    }

    let sender = name_of(world, player);
    let target_name = name_of(world, target);

    if has_flag(world, player, PlayerFlag::NoRepeat) {
        send_to(world, player, "Ok.\r\n");
    } else {
        send_to(
            world,
            player,
            format!("<cyan>You respond to {target_name}, '</><b:cyan>{message}</><cyan>'</>\r\n"),
        );
    }
    if has_flag(world, target, PlayerFlag::Afk) {
        send_to(
            world,
            target,
            "You received the previous message while AFK.\r\n",
        );
        send_to(
            world,
            player,
            "That person is AFK right now but received your message.\r\n",
        );
    }

    let line = format!(
        "<cyan>{sender} responds to {target_name}'s petition, '</><b:cyan>{message}</><cyan>'</>\r\n"
    );
    let staff: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|&e| {
                e != player && authority_level(world, e) >= LVL_IMMORT && !is_writing(world, e)
            })
            .collect()
    };
    for e in staff {
        send_to(world, e, line.clone());
    }

    send_to(
        world,
        target,
        format!("<cyan>{sender} responds to your petition, '</><b:cyan>{message}</><cyan>'</>\r\n"),
    );
}

fn cmd_page(world: &mut World, player: Entity, args: &str) {
    let args = args.trim();
    let (target_word, message) = args
        .split_once(char::is_whitespace)
        .map_or((args, ""), |(a, b)| (a, b.trim()));
    if world.get::<Player>(player).is_none() {
        send_to(world, player, "Monsters can't page.. go away.\r\n");
        return;
    }
    if target_word.is_empty() {
        send_to(world, player, "Whom do you wish to page?\r\n");
        return;
    }
    let sender = name_of(world, player);
    let line = format!("*{sender}* {message}\r\n");

    if target_word.eq_ignore_ascii_case("all") {
        if authority_level(world, player) > LVL_GOD {
            let everyone: Vec<Entity> = {
                let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
                q.iter(world).collect()
            };
            for e in everyone {
                deliver_page(world, e, &line);
            }
        } else {
            send_to(
                world,
                player,
                "You will never be godly enough to do that!\r\n",
            );
        }
        return;
    }

    let Some(target) = super::admin_world::find_actor_anywhere(world, player, target_word) else {
        send_to(world, player, "There is no such person in the game!\r\n");
        return;
    };
    deliver_page(world, target, &line);
    if has_flag(world, player, PlayerFlag::NoRepeat) {
        send_to(world, player, "Ok.\r\n");
    } else {
        deliver_page(world, player, &line);
    }
}

/// Legacy `act(..., TO_VICT)` delivery with the two terminal bells in
/// front: a sleeping or message-writing recipient does not get it. Sent
/// raw because rendering strips control bytes (the bell included); the
/// text itself still goes through the colour renderer.
fn deliver_page(world: &World, to: Entity, line: &str) {
    if world.get::<Named>(to).is_none() || is_asleep(world, to) || is_writing(world, to) {
        return;
    }
    let rendered =
        crate::commands::render_color_tags(line, crate::commands::color_mode_for(world, to));
    crate::commands::send_raw(world, to, format!("\x07\x07{rendered}"));
}
