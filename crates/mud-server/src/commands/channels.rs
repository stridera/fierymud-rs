//! Global broadcast channels — every online player receives, with
//! per-target Deaf flag + `IgnoreList` filtering. `wiznet` is the
//! staff-only variant.
//!
//! Communication category — first batch of the migration. Four
//! commands with near-identical broadcast loops; the differences
//! are the verb (gossip/sing/shout) and (for wiznet) the
//! recipient filter.

use bevy_ecs::prelude::*;
use mud_db::enums::{PlayerFlag, UserRole};
use mud_world::{IgnoreList, Online, Player};

use crate::commands::{
    Account, Category, Command, Help, Prevent, effect_prevents, has_flag, name_of,
    send_to,
};

inventory::submit! {
    Command {
        names: &["gossip", "/"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "gossip <message>",
            summary: "Talk on the global gossip channel.",
            long: "Visible to every online player who hasn't toggled \
                   their `deaf` flag and isn't ignoring you. The \
                   slash alias (`/`) lets seasoned players stack \
                   gossips without arrowing back to type the verb.",
        },
        run: cmd_gossip,
    }
}

inventory::submit! {
    Command {
        names: &["music"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "music <message>",
            summary: "Sing on the global music channel.",
            long: "RP-flavored counterpart to `gossip`. Same Deaf / \
                   ignore filtering. Convention: in-character song \
                   lyrics or melodic flavor.",
        },
        run: cmd_music,
    }
}

inventory::submit! {
    Command {
        names: &["shout"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "shout <message>",
            summary: "Shout to every online player.",
            long: "Higher-volume cousin of `say` (which is room-only). \
                   Same Deaf / ignore filtering as `gossip`.",
        },
        run: cmd_shout,
    }
}

inventory::submit! {
    Command {
        names: &["wiznet", ";"],
        min_role: UserRole::Immortal,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "wiznet <message>",
            summary: "Chat on the staff-only wiznet channel.",
            long: "Immortal+. Sent to every online staff member \
                   (Immortal or higher). Players never see wiznet \
                   traffic. Convention: out-of-character coordination \
                   between staff during play.",
        },
        run: cmd_wiznet,
    }
}

/// Shared body for gossip / music / shout. Picks a self-form +
/// other-form verb pair for the user-visible line plus a
/// `channel_tag` open-color so each channel reads in its own hue
/// — gossip yellow, music magenta, shout bold red. Filters out
/// Deaf recipients and anyone who's `IgnoreList`-blocked the
/// sender.
fn broadcast_global(
    world: &mut World,
    player: Entity,
    args: &str,
    verb_self: &str,
    verb_other: &str,
    refusal: &str,
    channel_tag: &str,
) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, refusal);
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let player_name = name_of(world, player);
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world).collect()
    };
    for t in targets {
        if t != player && has_flag(world, t, PlayerFlag::Deaf) {
            continue;
        }
        if t != player
            && world
                .get::<IgnoreList>(t)
                .is_some_and(|l| l.contains(&player_name))
        {
            continue;
        }
        // Channel tag colors the verb + body together so the line
        // is unmistakable in a busy log. Speaker name keeps any
        // authored color via render-on-send.
        let line = if t == player {
            format!("{channel_tag}You {verb_self}, \"{message}\"</>\r\n")
        } else {
            format!(
                "{channel_tag}{player_name} {verb_other}, \"{message}\"</>\r\n"
            )
        };
        send_to(world, t, line);
    }
}

fn cmd_gossip(world: &mut World, player: Entity, args: &str) {
    broadcast_global(
        world, player, args, "gossip", "gossips", "Gossip what?\r\n", "<yellow>",
    );
}

fn cmd_music(world: &mut World, player: Entity, args: &str) {
    broadcast_global(
        world, player, args, "sing", "sings", "Sing what?\r\n", "<magenta>",
    );
}

fn cmd_shout(world: &mut World, player: Entity, args: &str) {
    broadcast_global(
        world, player, args, "shout", "shouts", "Shout what?\r\n", "<b:red>",
    );
}

fn cmd_wiznet(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Wiznet what?\r\n");
        return;
    }
    let player_name = name_of(world, player);
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Account), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(_, a)| a.role.at_least(UserRole::Immortal))
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        // Wiznet reads bold cyan to match its staff-only role —
        // distinct from the public channels (gossip/shout/music)
        // and unmistakable in a mixed log.
        let line = if t == player {
            format!("<b:cyan>[wiznet]</> <b:white>You:</> {message}\r\n")
        } else {
            format!(
                "<b:cyan>[wiznet]</> <b:white>{player_name}:</> {message}\r\n"
            )
        };
        send_to(world, t, line);
    }
}
