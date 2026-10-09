//! Room-scoped communication: `say`, `emote`, `ask`, `whisper`,
//! `insult`. Plus `gsay` (group-only). All in one file because
//! they share the `find_actor_in_room` / room-broadcast pattern
//! and are read together in code review.

use bevy_ecs::prelude::*;
use mud_db::enums::{PlayerFlag, UserRole};
use mud_world::{Located, Mob, Player, WorldKey};

use crate::commands::{
    Category, Command, Help, Prevent, broadcast_room_except_players_rendered,
    bump_talk_quest_progress, effect_prevents, find_actor_in_room, group_members, group_root,
    has_flag, name_approval_gate, name_of, send_comm_channel_text, send_rendered, send_to,
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
            long: "Your name is prepended. 'emote smiles broadly.' \
                   shows everyone (including you): \
                   'Strider smiles broadly.'",
        },
        run: cmd_emote,
    }
}

inventory::submit! {
    Command {
        names: &["emote's"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "emote's <action>",
            summary: "Perform a possessive third-person action visible to the room.",
            long: "Your name plus 's is prepended. 'emote's eyes widen.' \
                   shows everyone (including you): \
                   'Strider's eyes widen.'",
        },
        run: cmd_emotes,
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
                   'speech' Lua global. Bystanders see that you \
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
                   raw 'who' numbers.",
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
    if name_approval_gate(world, player) {
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
    let gmcp_text = format!("{speaker} says, \"{message}\"");
    let norepeat = has_flag(world, player, PlayerFlag::NoRepeat);
    for target in targets {
        // Say/says verb framed in green (room-local speech reads
        // friendly / open vs the louder yellow/red wide channels).
        // Speaker name emphasized so the eye lands on who's
        // talking; message body inherits authored color.
        let line = if target == player && norepeat {
            "Ok.\r\n".to_string()
        } else if target == player {
            format!("<green>You say,</> \"{message}\"\r\n")
        } else {
            format!("<b:green>{speaker}</> <green>says,</> \"{message}\"\r\n")
        };
        send_rendered(world, target, &line);
        send_comm_channel_text(world, target, "say", &speaker, &gmcp_text);
    }
    crate::triggers::fire_speech_in_room(world, player, located.0, message);
}

fn cmd_emote(world: &mut World, player: Entity, args: &str) {
    emote_as(world, player, args, "");
}

/// `emote's <action>`: the possessive form, "Strider's eyes widen."
fn cmd_emotes(world: &mut World, player: Entity, args: &str) {
    emote_as(world, player, args, "'s");
}

/// Shared body of `emote` and `emote's` (legacy `do_echo`): the speaker's
/// name, `suffix` (`'s` for the possessive), a space, then the action.
fn emote_as(world: &mut World, player: Entity, args: &str, suffix: &str) {
    let action = args.trim();
    if action.is_empty() {
        send_to(world, player, "Emote what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let player_name = name_of(world, player);
    let shown = format!("{player_name}{suffix}");
    let line = format!("{shown} {action}\r\n");
    // Emote body is already third-person ("Strider smiles."),
    // so the GMCP frame is just the line minus its trailing
    // CRLF — every recipient sees the same thing in the chat
    // tab regardless of whether they're the speaker.
    let gmcp_text = format!("{shown} {action}");
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == located.0)
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        send_to(world, t, line.clone());
        send_comm_channel_text(world, t, "emote", &player_name, &gmcp_text);
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
    if has_flag(world, player, PlayerFlag::NoRepeat) {
        send_to(world, player, "Ok.\r\n");
    } else {
        send_to(
            world,
            player,
            format!("You ask {target_name} about \"{topic}\".\r\n"),
        );
    }
    broadcast_room_except_players_rendered(
        world,
        located.0,
        &[player],
        &format!("{player_name} asks {target_name} about something.\r\n"),
    );
    // GMCP only to participants — the topic stays private to
    // the speaker and addressee, matching the "asks something"
    // rendering bystanders see in the main window. Target may
    // be a mob (no Connection); the helper no-ops cleanly.
    let gmcp_text = format!("{player_name} asks {target_name} about \"{topic}\"");
    send_comm_channel_text(world, player, "ask", &player_name, &gmcp_text);
    send_comm_channel_text(world, target, "ask", &player_name, &gmcp_text);
    crate::triggers::fire_speech_at(world, target, player, topic);
    if world.get::<Mob>(target).is_some()
        && let Some(key) = world.get::<WorldKey>(target).copied()
    {
        // Already in a conversation with this mob? Walk the dialogue
        // tree in-band (no DB round-trip); nothing else counts.
        let mob = (key.zone, key.id);
        // The quest giver: asking the mob offers its quests.
        crate::quest_triggers::dispatch_giver_trigger(world, player, mob);
        if let Some(open) =
            crate::quest_dialogue::try_advance_active_tree(world, player, mob, topic)
        {
            crate::quest_dialogue::say_reply(world, player, &target_name, &open);
        } else {
            // Opening a conversation: TALK_TO_NPC objectives bound to a
            // dialogue only advance when `topic` matches its keywords,
            // and the mob answers (entering the tree, if linked).
            bump_talk_quest_progress(world, player, key.zone, key.id, &target_name, topic);
        }
    }
}

fn cmd_whisper(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[1].trim().is_empty() {
        send_to(world, player, "Usage: whisper <target> <message>\r\n");
        return;
    }
    if name_approval_gate(world, player) {
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
    if has_flag(world, player, PlayerFlag::NoRepeat) {
        send_to(world, player, "Ok.\r\n");
    } else {
        send_rendered(
            world,
            player,
            &format!("You whisper to {target_name}, \"{message}\"\r\n"),
        );
    }
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
    // GMCP only to the two participants. Bystanders see the
    // muffled "whispers something" line in the main window and
    // get no GMCP frame — keeps the message body private to
    // its intended audience.
    let gmcp_text = format!("{speaker} whispers to {target_name}, \"{message}\"");
    send_comm_channel_text(world, player, "whisper", &speaker, &gmcp_text);
    send_comm_channel_text(world, target, "whisper", &speaker, &gmcp_text);
}

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
    // The `insult_lines` SystemMessage row is the pool to pick from.
    let Some(line) = world
        .get_resource::<mud_world::SystemMessages>()
        .and_then(|m| m.pick("insult_lines"))
        .map(str::to_string)
    else {
        send_to(world, player, "You can't think of an insult.\r\n");
        return;
    };
    let actor_name = name_of(world, player);
    let target_name = name_of(world, target);
    send_to(
        world,
        player,
        format!("You insult {target_name}: {line}\r\n"),
    );
    send_to(
        world,
        target,
        crate::commands::cap_sentence_start(&format!("{actor_name} insults you: {line}\r\n")),
    );
    let bystanders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| l.0 == located.0 && *e != player && *e != target)
            .map(|(e, _)| e)
            .collect()
    };
    let line_room =
        crate::commands::cap_sentence_start(&format!("{actor_name} insults {target_name}.\r\n"));
    for e in bystanders {
        send_to(world, e, line_room.clone());
    }
    // GMCP only to the two participants — the full insult line
    // is content; bystanders see the muffled action in the main
    // window and don't need it in their chat tab.
    let gmcp_text = format!("{actor_name} insults {target_name}: {line}");
    send_comm_channel_text(world, player, "insult", &actor_name, &gmcp_text);
    send_comm_channel_text(world, target, "insult", &actor_name, &gmcp_text);
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
    if name_approval_gate(world, player) {
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
    let gmcp_text = format!("{speaker} group-says, \"{message}\"");
    let norepeat = has_flag(world, player, PlayerFlag::NoRepeat);
    for m in members {
        let line = if m == player && norepeat {
            "Ok.\r\n".to_string()
        } else if m == player {
            format!("You group-say, \"{message}\"\r\n")
        } else {
            format!("({speaker} group-says) \"{message}\"\r\n")
        };
        send_rendered(world, m, &line);
        send_comm_channel_text(world, m, "group", &speaker, &gmcp_text);
    }
}

#[cfg(test)]
mod tests {
    use bevy_ecs::prelude::*;
    use mud_world::{Exits, Named, Room, SystemMessages};

    use super::cmd_insult;
    use crate::commands::test_support::{Rx, drain, player_in};

    fn setup() -> (World, Entity, Rx, Rx) {
        let mut world = World::new();
        let room = world
            .spawn((
                Room,
                Named {
                    name: "A hall".into(),
                },
                Exits::default(),
            ))
            .id();
        let (speaker, rx) = player_in(&mut world, room);
        let (target, target_rx) = player_in(&mut world, room);
        world
            .entity_mut(target)
            .insert(Named { name: "Bob".into() });
        (world, speaker, rx, target_rx)
    }

    #[test]
    fn insult_picks_from_the_system_message_row() {
        let (mut world, speaker, mut rx, _target_rx) = setup();
        let mut m = SystemMessages::default();
        m.by_key
            .insert("insult_lines".into(), vec!["Your hat is silly!".into()]);
        world.insert_resource(m);
        cmd_insult(&mut world, speaker, "bob");
        let out = drain(&mut rx);
        assert!(
            out.contains("You insult Bob: Your hat is silly!"),
            "{out:?}"
        );
    }

    #[test]
    fn insult_without_lines_says_so_instead_of_panicking() {
        let (mut world, speaker, mut rx, _target_rx) = setup();
        cmd_insult(&mut world, speaker, "bob");
        assert!(drain(&mut rx).contains("You can't think of an insult."));
        world.insert_resource(SystemMessages::default());
        cmd_insult(&mut world, speaker, "bob");
        assert!(drain(&mut rx).contains("You can't think of an insult."));
    }
}
