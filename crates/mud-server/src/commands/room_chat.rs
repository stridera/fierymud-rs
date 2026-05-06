//! Room-scoped communication: `say`, `emote`, `ask`, `whisper`,
//! `insult`. Plus `gsay` (group-only). All in one file because
//! they share the `find_actor_in_room` / room-broadcast pattern
//! and are read together in code review.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Located, Mob, Player, WorldKey};

use crate::commands::{
    Category, Command, Help, Prevent, broadcast_room_except_players_rendered,
    bump_talk_quest_progress, effect_prevents, find_actor_in_room, group_members,
    group_root, name_of, send_rendered, send_to,
};

inventory::submit! {
    Command {
        names: &["say", "'"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "say <message>",
            summary: "Speak to everyone in the room.",
            long: "Visible to every player in your current room. \
                   Triggers SPEECH-flagged Lua bodies on mobs / \
                   objects in the room.",
        },
        run: cmd_say,
    }
}

inventory::submit! {
    Command {
        names: &["emote", ":"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "emote <action>",
            summary: "Perform a third-person action visible to the room.",
            long: "Your name is prepended. `emote smiles broadly.` \
                   shows everyone (including you): \
                   `Strider smiles broadly.`",
        },
        run: cmd_emote,
    }
}

inventory::submit! {
    Command {
        names: &["ask"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "ask <mob> <topic>",
            summary: "Ask a mob about a topic — fires SPEECH triggers.",
            long: "Targets a single mob in the room and fires its \
                   SPEECH-flagged Lua bodies with the topic in the \
                   `speech` Lua global. Bystanders see that you \
                   asked something but not what.",
        },
        run: cmd_ask,
    }
}

inventory::submit! {
    Command {
        names: &["whisper"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "whisper <target> <message>",
            summary: "Quietly say something to one person in the room.",
            long: "Both you and the target see the full text. Other \
                   players in the room see that you whispered to \
                   them but not what.",
        },
        run: cmd_whisper,
    }
}

inventory::submit! {
    Command {
        names: &["insult"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "insult <target>",
            summary: "Hurl a random insult at someone in the room.",
            long: "Picks a random insult and emits it to you, the \
                   target, and the rest of the room. Self-targeting \
                   leaves you feeling insulted at yourself.",
        },
        run: cmd_insult,
    }
}

inventory::submit! {
    Command {
        names: &["greport"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "greport",
            summary: "Broadcast your hp/stamina to your group.",
            long: "Sends a one-line vitals snapshot to every member \
                   of your current group regardless of room. Used \
                   to coordinate healing / retreat without reading \
                   raw `who` numbers.",
        },
        run: cmd_greport,
    }
}

inventory::submit! {
    Command {
        names: &["gsay", "gtell", "gt"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "gsay <message>",
            summary: "Speak privately to your group.",
            long: "Reaches every member of your current group \
                   regardless of room. Players outside the group \
                   never see it. Refused when you're not grouped.",
        },
        run: cmd_gsay,
    }
}

fn cmd_say(world: &mut World, player: Entity, message: &str) {
    let message = message.trim();
    if message.is_empty() {
        send_to(world, player, "Say what?\r\n");
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let speaker = name_of(world, player);
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == located.0)
            .map(|(e, _)| e)
            .collect()
    };
    for target in targets {
        // Say/says verb framed in green (room-local speech reads
        // friendly / open vs the louder yellow/red wide channels).
        // Speaker name emphasized so the eye lands on who's
        // talking; message body inherits authored color.
        let line = if target == player {
            format!("<green>You say,</> \"{message}\"\r\n")
        } else {
            format!("<b:green>{speaker}</> <green>says,</> \"{message}\"\r\n")
        };
        send_rendered(world, target, &line);
    }
    crate::triggers::fire_speech_in_room(world, player, located.0, message);
}

fn cmd_emote(world: &mut World, player: Entity, args: &str) {
    let action = args.trim();
    if action.is_empty() {
        send_to(world, player, "Emote what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let player_name = name_of(world, player);
    let line = format!("{player_name} {action}\r\n");
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == located.0)
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        send_to(world, t, line.clone());
    }
}

fn cmd_ask(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[1].trim().is_empty() {
        send_to(world, player, "Usage: ask <mob> <topic>\r\n");
        return;
    }
    let target_word = parts[0].trim();
    let topic = parts[1].trim();
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(target) = find_actor_in_room(world, target_word, located.0, player) else {
        send_rendered(
            world,
            player,
            &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    let target_name = name_of(world, target);
    let player_name = name_of(world, player);
    send_to(
        world,
        player,
        format!("You ask {target_name} about \"{topic}\".\r\n"),
    );
    broadcast_room_except_players_rendered(
        world,
        located.0,
        &[player],
        &format!("{player_name} asks {target_name} about something.\r\n"),
    );
    crate::triggers::fire_speech_at(world, target, player, topic);
    if world.get::<Mob>(target).is_some()
        && let Some(key) = world.get::<WorldKey>(target).copied()
    {
        bump_talk_quest_progress(world, player, key.zone, key.id);
    }
}

fn cmd_whisper(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[1].trim().is_empty() {
        send_to(world, player, "Usage: whisper <target> <message>\r\n");
        return;
    }
    let target_word = parts[0].trim();
    let message = parts[1].trim();
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(target) = find_actor_in_room(world, target_word, located.0, player) else {
        send_rendered(
            world,
            player,
            &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    let speaker = name_of(world, player);
    let target_name = name_of(world, target);
    send_rendered(
        world,
        player,
        &format!("You whisper to {target_name}, \"{message}\"\r\n"),
    );
    send_to(
        world,
        target,
        format!("{speaker} whispers to you, \"{message}\"\r\n"),
    );
    broadcast_room_except_players_rendered(
        world,
        located.0,
        &[player, target],
        &format!("{speaker} whispers something to {target_name}.\r\n"),
    );
}

const INSULT_LINES: &[&str] = &[
    "You smell like a troll's armpit!",
    "Your mother was a bugbear!",
    "You fight like a dairy farmer!",
    "I've seen better-looking rust monsters!",
    "Even a gelatinous cube has more personality!",
    "Your sword is dull and your wits are duller!",
    "I've met kobolds with sharper tongues!",
    "Your aim is as bad as your cooking!",
];

fn cmd_insult(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "You feel insulted.\r\n");
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "You feel insulted.\r\n");
        return;
    }
    let line = INSULT_LINES[rand::random_range(0..INSULT_LINES.len())];
    let actor_name = name_of(world, player);
    let target_name = name_of(world, target);
    send_to(world, player, format!("You insult {target_name}: {line}\r\n"));
    send_to(world, target, format!("{actor_name} insults you: {line}\r\n"));
    let bystanders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| l.0 == located.0 && *e != player && *e != target)
            .map(|(e, _)| e)
            .collect()
    };
    let line_room = format!("{actor_name} insults {target_name}.\r\n");
    for e in bystanders {
        send_to(world, e, line_room.clone());
    }
}

fn cmd_greport(world: &mut World, player: Entity, _args: &str) {
    let root = group_root(world, player);
    let members = group_members(world, root);
    if members.len() <= 1 {
        send_to(
            world,
            player,
            "You're not in a group — nobody to report to.\r\n",
        );
        return;
    }
    let speaker = name_of(world, player);
    let hp = world.get::<mud_world::Health>(player).copied();
    let stamina = world.get::<mud_world::Stamina>(player).copied();
    let hp_str = hp.map_or_else(|| String::from("?/?"), |h| format!("{}/{}", h.hp, h.max));
    let st_str = stamina.map_or_else(
        || String::from("?/?"),
        |s| format!("{}/{}", s.current, s.max),
    );
    for m in members {
        let line = if m == player {
            format!("You report: {hp_str} hp, {st_str} stamina.\r\n")
        } else {
            format!("({speaker} reports: {hp_str} hp, {st_str} stamina.)\r\n")
        };
        send_rendered(world, m, &line);
    }
}

fn cmd_gsay(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Group-say what?\r\n");
        return;
    }
    let root = group_root(world, player);
    let members = group_members(world, root);
    if members.len() <= 1 {
        send_to(
            world,
            player,
            "You're not in a group — nobody to say that to.\r\n",
        );
        return;
    }
    let speaker = name_of(world, player);
    for m in members {
        let line = if m == player {
            format!("You group-say, \"{message}\"\r\n")
        } else {
            format!("({speaker} group-says) \"{message}\"\r\n")
        };
        send_rendered(world, m, &line);
    }
}
