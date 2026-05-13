//! Tells, replies, ignore lists, and the recent-tells history
//! readout. Persistence into `tell_message` lives here too — the
//! receiver's `TellLog` is hydrated at login from the same table.

use bevy_ecs::prelude::*;
use mud_db::enums::{PlayerFlag, UserRole};
use mud_world::{IgnoreList, LastTeller, Named, Online, Player, TellLog};

use crate::commands::{
    Account, Category, Command, DbPool, Help, Prevent, effect_prevents, format_idle,
    has_flag, name_approval_gate, name_of, send_comm_channel_text, send_rendered, send_to,
    try_insert,
};

inventory::submit! {
    Command {
        names: &["tell", "t"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "tell <player> <message>",
            summary: "Send a private message to one online player.",
            long: "Reaches one player by name. Refused if the target \
                   is offline, has set their `notell` flag, or is \
                   ignoring you. Stamped on the receiver as their \
                   most-recent teller for `reply` and appended to \
                   their bounded `lasttells` log + the persistent \
                   `tell_message` table.",
        },
        run: cmd_tell,
    }
}

inventory::submit! {
    Command {
        names: &["reply", "r"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "reply <message>",
            summary: "Send a tell to whoever last tell'd you.",
            long: "Forwards through `tell` so the LastTeller stamp \
                   lands on them too — start a back-and-forth without \
                   typing the name.",
        },
        run: cmd_reply,
    }
}

inventory::submit! {
    Command {
        names: &["ignore"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "ignore [<name> | -<name> | clear]",
            summary: "Block tells / channel msgs from a player.",
            long: "No arg: list current entries. `<name>`: add. \
                   `-<name>`: remove. `clear`: drop the whole list. \
                   Per-session today; persistence is a follow-up.",
        },
        run: cmd_ignore,
    }
}

inventory::submit! {
    Command {
        names: &["unignore"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "unignore <name>",
            summary: "Remove a name from your ignore list.",
            long: "Equivalent to `ignore -<name>`. No-op if the name \
                   isn't currently being ignored.",
        },
        run: cmd_unignore,
    }
}

inventory::submit! {
    Command {
        names: &["lasttells", "lt"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "lasttells",
            summary: "Show your bounded recent-tells history.",
            long: "Reads `TellLog`, hydrated at login from the \
                   `tell_message` table. Each row shows the sender \
                   and how long ago the tell arrived.",
        },
        run: cmd_lasttells,
    }
}

fn cmd_tell(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[1].trim().is_empty() {
        send_to(
            world,
            player,
            "Usage: tell <player>[,<player2>,...] <message>\r\n",
        );
        return;
    }
    if name_approval_gate(world, player) {
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let names_raw = parts[0].trim();
    let message = parts[1].trim();
    // Comma-separated recipient list — `tell foo,bar,baz hi`.
    // Single-name form is the common case and reads as a list of
    // one. Per-recipient feedback is sent inline (refusals,
    // AFK hints, "isn't online") so the sender knows what
    // landed.
    let names: Vec<&str> = names_raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        send_to(world, player, "Tell whom?\r\n");
        return;
    }
    for name in &names {
        deliver_tell(world, player, name, message);
    }
}

/// Resolve and deliver one tell. Factored out of `cmd_tell` so the
/// comma-separated list path can iterate. All per-recipient
/// feedback (off-line, `NoTell`, `IgnoreList`, AFK hint) renders
/// inline to the sender.
fn deliver_tell(world: &mut World, player: Entity, target_name: &str, message: &str) {
    let target_lower = target_name.to_ascii_lowercase();
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| {
                n.name.eq_ignore_ascii_case(target_name)
                    || n.name.to_ascii_lowercase() == target_lower
            })
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_rendered(world, player, &format!("'{target_name}' isn't online.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "You mutter quietly to yourself.\r\n");
        return;
    }
    if has_flag(world, target, PlayerFlag::NoTell) {
        let actual = name_of(world, target);
        send_rendered(
            world,
            player,
            &format!("{actual} is not accepting tells right now.\r\n"),
        );
        return;
    }
    let player_name_for_check = name_of(world, player);
    if world
        .get::<IgnoreList>(target)
        .is_some_and(|l| l.contains(&player_name_for_check))
    {
        let actual = name_of(world, target);
        send_rendered(
            world,
            player,
            &format!("{actual} is not accepting tells right now.\r\n"),
        );
        return;
    }

    let player_name = name_of(world, player);
    let target_name = name_of(world, target);

    send_rendered(
        world,
        player,
        &format!("<cyan>You tell <b:cyan>{target_name}</>, \"{message}\"</>\r\n"),
    );
    if has_flag(world, target, PlayerFlag::Afk) {
        send_rendered(
            world,
            player,
            &format!("<dim>({target_name} is AFK and may not respond right away.)</>\r\n"),
        );
    }
    send_rendered(
        world,
        target,
        &format!("<cyan><b:cyan>{player_name}</> tells you, \"{message}\"</>\r\n"),
    );

    // GMCP companion frame for both ends of the conversation —
    // sender and receiver land in the Tells tab via the same
    // third-person body. Bystanders don't exist for a tell, so
    // there's no information leak to worry about.
    let gmcp_text = format!("{player_name} tells {target_name}, \"{message}\"");
    send_comm_channel_text(world, player, "tells", &player_name, &gmcp_text);
    send_comm_channel_text(world, target, "tells", &player_name, &gmcp_text);

    try_insert(world, target, LastTeller(player));
    let player_name_owned = player_name.clone();
    if let Some(mut log) = world.get_mut::<TellLog>(target) {
        log.push(player_name_owned);
    } else {
        let mut log = TellLog::with_cap(10);
        log.push(player_name_owned);
        try_insert(world, target, log);
    }
    let recipient_id = world.get::<Account>(target).map(|a| a.character_id.clone());
    if let (Some(rid), Some(pool)) = (
        recipient_id,
        world.get_resource::<DbPool>().map(|p| p.0.clone()),
    ) {
        let sender_name = player_name.clone();
        let body = message.to_string();
        tokio::spawn(async move {
            if let Err(e) =
                mud_db::tell_messages::record(&pool, &rid, &sender_name, &body).await
            {
                tracing::warn!(error = %e, "tell persist failed");
            }
        });
    }
}

fn cmd_reply(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Reply with what?\r\n");
        return;
    }
    if name_approval_gate(world, player) {
        return;
    }
    let Some(LastTeller(last)) = world.get::<LastTeller>(player).copied() else {
        send_to(world, player, "Nobody has tell'd you recently.\r\n");
        return;
    };
    if world.get_entity(last).is_err() || world.get::<Online>(last).is_none() {
        send_to(world, player, "They're no longer online.\r\n");
        return;
    }
    let last_name = name_of(world, last);
    cmd_tell(world, player, &format!("{last_name} {message}"));
}

fn cmd_ignore(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        let entries = world
            .get::<IgnoreList>(player)
            .map(|l| l.0.clone())
            .unwrap_or_default();
        if entries.is_empty() {
            send_to(world, player, "You're ignoring nobody.\r\n");
        } else {
            let mut out = format!("\r\nIgnoring {} player(s):\r\n", entries.len());
            for n in &entries {
                out.push_str(&format!("  {n}\r\n"));
            }
            send_to(world, player, out);
        }
        return;
    }
    if arg.eq_ignore_ascii_case("clear") {
        if let Ok(mut e) = world.get_entity_mut(player) {
            e.remove::<IgnoreList>();
        }
        send_to(world, player, "Ignore list cleared.\r\n");
        return;
    }
    if let Some(name) = arg.strip_prefix('-') {
        let name = name.trim();
        if name.is_empty() {
            send_to(world, player, "Unignore whom?\r\n");
            return;
        }
        cmd_unignore(world, player, name);
        return;
    }
    let added = if let Some(mut list) = world.get_mut::<IgnoreList>(player) {
        list.add(arg)
    } else {
        let mut l = IgnoreList::default();
        let added = l.add(arg);
        try_insert(world, player, l);
        added
    };
    if added {
        send_to(world, player, format!("You will now ignore {arg}.\r\n"));
    } else {
        send_to(world, player, format!("You're already ignoring {arg}.\r\n"));
    }
}

fn cmd_unignore(world: &mut World, player: Entity, args: &str) {
    let name = args.trim();
    if name.is_empty() {
        send_to(world, player, "Unignore whom?\r\n");
        return;
    }
    let removed = world
        .get_mut::<IgnoreList>(player)
        .is_some_and(|mut l| l.remove(name));
    if removed {
        send_to(world, player, format!("You no longer ignore {name}.\r\n"));
    } else {
        send_to(world, player, format!("You aren't ignoring {name}.\r\n"));
    }
}

fn cmd_lasttells(world: &mut World, player: Entity, _args: &str) {
    let entries: Vec<(String, u64)> = world
        .get::<TellLog>(player)
        .map(|log| {
            log.entries
                .iter()
                .map(|(name, when)| {
                    let secs = when.elapsed().map(|d| d.as_secs()).unwrap_or(0);
                    (name.clone(), secs)
                })
                .collect()
        })
        .unwrap_or_default();
    if entries.is_empty() {
        send_to(world, player, "No recent tells.\r\n");
        return;
    }
    let mut out = format!("\r\nRecent tells ({}):\r\n", entries.len());
    for (name, secs_ago) in &entries {
        out.push_str(&format!("  {:<20} ({} ago)\r\n", name, format_idle(*secs_ago)));
    }
    send_to(world, player, out);
}
