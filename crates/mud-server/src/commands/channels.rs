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
    Account, Category, ColorMode, Command, Connection, Help, Prevent, effect_prevents,
    has_flag, name_of, render_color_tags, send_to,
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
        names: &["qsay"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "qsay <message>",
            summary: "Talk on the quest channel.",
            long: "Visible to every online player not Deaf and not \
                   ignoring you. Conventionally used to coordinate \
                   quest progress with other questers. Same shape as \
                   `gossip` — separate channel mostly for ear cleanliness.",
        },
        run: cmd_qsay,
    }
}

inventory::submit! {
    Command {
        names: &["qecho"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "qecho <message>",
            summary: "Admin echo to the quest channel.",
            long: "Builder+. Drops a flavor announcement onto the \
                   quest channel without quoting the speaker (e.g. \
                   \"<msg>\" rather than \"X qsays, '<msg>'\"). \
                   Useful for run-of-show quest narration.",
        },
        run: cmd_qecho,
    }
}

inventory::submit! {
    Command {
        names: &["lastgossips", "lastgos"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "lastgossips [<n>]",
            summary: "Replay recent gossip channel lines.",
            long: "Shows the last <n> gossip utterances (default 10, \
                   capped at 50). In-memory only — clears on server \
                   restart, like the legacy ring buffer.",
        },
        run: cmd_lastgossips,
    }
}

inventory::submit! {
    Command {
        names: &["lastshouts"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "lastshouts [<n>]",
            summary: "Replay recent shout channel lines.",
            long: "Shows the last <n> shouts (default 10, capped at 50).",
        },
        run: cmd_lastshouts,
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
#[allow(clippy::too_many_arguments)]
fn broadcast_global(
    world: &mut World,
    player: Entity,
    args: &str,
    verb_self: &str,
    verb_other: &str,
    refusal: &str,
    channel_tag: &str,
    channel_kind: &'static str,
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
    if world
        .get::<mud_world::PlayerFlags>(player)
        .is_some_and(|pf| pf.has(PlayerFlag::Muted))
    {
        send_to(
            world,
            player,
            "Your voice has been muted by staff. Speak privately if you must.\r\n",
        );
        return;
    }
    let player_name = name_of(world, player);
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world).collect()
    };
    // GMCP Comm.Channel.Text: pre-build the JSON payload once
    // so each recipient gets the same frame. Mudlet (and the
    // major web clients) route this into a dedicated chat
    // window via the dotted package name's third segment, so
    // `gossip` lands in the gossip tab, `shout` in shouts, etc.
    // Per the IRE spec, text is the *plain rendered* form
    // (color tags stripped) so client-side display can re-style.
    let plain_speaker = render_color_tags(&player_name, ColorMode::Strip);
    let plain_message = render_color_tags(message, ColorMode::Strip);
    let gmcp_text = format!(
        "{}{}, \"{}\"",
        plain_speaker,
        match channel_kind {
            "gossip" => " gossips",
            "shout" => " shouts",
            "music" => " sings",
            "quest" => " quest-says",
            _ => "",
        },
        plain_message
    );
    let gmcp_payload = format!(
        "{{\"channel\":\"{}\",\"talker\":\"{}\",\"text\":\"{}\"}}",
        channel_kind,
        plain_speaker
            .replace('\\', "\\\\")
            .replace('"', "\\\""),
        gmcp_text
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    );
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
        // Wizinvis: hide the speaker entirely from observers below
        // their invis level. Sender always sees their own line.
        if t != player && !crate::commands::can_see_player(world, t, player) {
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
        // Push the GMCP companion frame to clients with a
        // Connection — listeners see it in their chat split,
        // sender sees it (so client-side echo doesn't have to
        // synthesize). Sent AFTER the text line so any client
        // race between text + GMCP renders the text first.
        if let Some(conn) = world.get::<Connection>(t) {
            let _ = conn.0.try_send(mud_net::gmcp_packet(
                "Comm.Channel.Text",
                &gmcp_payload,
            ));
        }
    }
    // Capture into the channel history ring buffer so `lastgossips`
    // / `lastshouts` can replay recent traffic to a player who
    // missed it. Push after broadcast so a spammer's own line is
    // already visible to listeners by the time it lands here.
    if !world.contains_resource::<mud_world::ChannelHistory>() {
        world.insert_resource(mud_world::ChannelHistory::default());
    }
    world
        .resource_mut::<mud_world::ChannelHistory>()
        .push(mud_world::ChannelEntry {
            at: std::time::SystemTime::now(),
            channel: channel_kind,
            speaker: player_name,
            body: message.to_string(),
        });
}

fn cmd_gossip(world: &mut World, player: Entity, args: &str) {
    broadcast_global(
        world,
        player,
        args,
        "gossip",
        "gossips",
        "Gossip what?\r\n",
        "<yellow>",
        "gossip",
    );
}

fn cmd_music(world: &mut World, player: Entity, args: &str) {
    broadcast_global(
        world,
        player,
        args,
        "sing",
        "sings",
        "Sing what?\r\n",
        "<magenta>",
        "music",
    );
}

fn cmd_shout(world: &mut World, player: Entity, args: &str) {
    broadcast_global(
        world, player, args, "shout", "shouts", "Shout what?\r\n", "<b:red>", "shout",
    );
}

fn cmd_qsay(world: &mut World, player: Entity, args: &str) {
    broadcast_global(
        world,
        player,
        args,
        "qsay",
        "qsays",
        "Quest-say what?\r\n",
        "<b:green>",
        "quest",
    );
}

fn cmd_qecho(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Quest-echo what?\r\n");
        return;
    }
    let player_name = name_of(world, player);
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world).collect()
    };
    let line = format!("<b:green>[Quest]</> {message}\r\n");
    for t in targets {
        send_to(world, t, line.clone());
    }
    if !world.contains_resource::<mud_world::ChannelHistory>() {
        world.insert_resource(mud_world::ChannelHistory::default());
    }
    world
        .resource_mut::<mud_world::ChannelHistory>()
        .push(mud_world::ChannelEntry {
            at: std::time::SystemTime::now(),
            channel: "quest",
            speaker: format!("{player_name} (echo)"),
            body: message.to_string(),
        });
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

fn cmd_lastgossips(world: &mut World, player: Entity, args: &str) {
    show_channel_history(world, player, args, "gossip", "<yellow>", "gossips");
}

fn cmd_lastshouts(world: &mut World, player: Entity, args: &str) {
    show_channel_history(world, player, args, "shout", "<b:red>", "shouts");
}

/// Body shared by `lastgossips` / `lastshouts`. Pulls the channel
/// subset from `ChannelHistory`, formats most-recent-first, and
/// hides authored color tags inside the message body so a heated
/// `<b:red>!!!</>` in someone's gossip line doesn't escape its
/// envelope. <n> defaults to 10, caps at 50 — the buffer holds
/// 200 cross-channel total entries.
fn show_channel_history(
    world: &mut World,
    player: Entity,
    args: &str,
    channel: &'static str,
    channel_tag: &str,
    plural_label: &str,
) {
    let n: usize = args
        .trim()
        .parse()
        .ok()
        .filter(|x: &usize| *x > 0)
        .map_or(10, |x| x.min(50));
    let Some(history) = world.get_resource::<mud_world::ChannelHistory>() else {
        send_to(world, player, format!("No recent {plural_label}.\r\n"));
        return;
    };
    let entries: Vec<&mud_world::ChannelEntry> = history.recent_on(channel).take(n).collect();
    if entries.is_empty() {
        send_to(world, player, format!("No recent {plural_label}.\r\n"));
        return;
    }
    let mut out = format!(
        "\r\n<b:cyan>Last {} {plural_label}:</>\r\n",
        entries.len(),
    );
    let now = std::time::SystemTime::now();
    // Render oldest-first so the player reads chronologically;
    // recent_on returns reverse-chronological. Reverse our take().
    let mut chrono: Vec<&mud_world::ChannelEntry> = entries.into_iter().collect();
    chrono.reverse();
    for entry in chrono {
        let secs_ago = now.duration_since(entry.at).map(|d| d.as_secs()).unwrap_or(0);
        out.push_str(&format!(
            "  <dim>{ago:>4}s ago</>  {channel_tag}{speaker}: \"{body}\"</>\r\n",
            ago = secs_ago,
            speaker = entry.speaker,
            body = entry.body.replace('"', "'"),
        ));
    }
    crate::commands::send_rendered(world, player, &out);
}
