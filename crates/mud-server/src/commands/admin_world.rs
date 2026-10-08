//! Admin commands for world manipulation: movement (goto /
//! transfer / teleport / summon / where), state mutation
//! (freeze / slay / restore / apply / purge / force), and
//! prototype loading (load / loadobj / dumpworld). Command
//! records and bodies both live here.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{
    Account, AppliedTo, Description, EffectCatalog, EffectInstance, EffectSource, Fighting, Frozen,
    Health, Item, Keywords, Located, Mob, MobPrototypes, Named, ObjectPrototypes, Online, Player,
    PlayerCorpse, PlayerFlags, Profile, Stamina, Wealth, WearableIn, WorldKey, WorldKeyIndex,
};
use tracing::info;

use crate::TickCount;
use crate::commands::{
    self, Category, Command, Help, broadcast_room_except_players_rendered, cap_sentence_start,
    cmd_look, find_actor_in_room, matches, matches_self, name_of, name_or, pad_visible,
    record_admin_action, send_rendered, send_to, try_insert, try_remove,
};

inventory::submit! {
    Command {
        names: &["where"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "where [<name> | all]",
            summary: "Show your location, a named player's location, or list all online.",
            long: "With no argument, prints your own current room \
                   (name, zone, id). With a player name, prints that \
                   online player's current room. 'where all' (Builder+ \
                   only) lists every online player and where they are.\r\n\
                   \r\n\
                   Immortal+: 'where <name>' also searches live mobs \
                   (M lines: [zone:id] name - room) and objects (O \
                   lines, including carried, worn and contained ones) \
                   whose name or keywords match.",
        },
        run: cmd_where,
    }
}

inventory::submit! {
    Command {
        names: &["goto", "go"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "goto <target>",
            summary: "Teleport to a room by id, or to a player/mob by name.",
            long: "Builder+ command. Three forms:\r\n\
                   \r\n\
                   \x20 goto <id>             — room <id> in your current zone\r\n\
                   \x20 goto <zone> <id>      — composite (zone, id)\r\n\
                   \x20 goto <name>           — teleport to a player or mob's room\r\n\
                   \x20 goto home            — your recall (home) room\r\n\
                   \r\n\
                   Bypasses exits, doors, and no-teleport rooms. Rooms \
                   restricted to the god ranks stay closed to lower staff. \
                   Your 'poofout' / 'poofin' lines are shown to the rooms \
                   you leave and arrive in.",
        },
        run: cmd_goto,
    }
}

inventory::submit! {
    Command {
        names: &["transfer"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "transfer <player | mob | all>",
            summary: "Pull an online player or a mob to your current room.",
            long: "Builder+ command. The target is a player or mob you \
                   can see, by name or keyword; 'N.name' picks the Nth \
                   match and '<zone>:<id>' (or 'N.<zone>:<id>') picks a \
                   spawned instance of that mob prototype. Moves it to \
                   wherever you are (ending any fight it is in). You \
                   cannot transfer a player of higher level than you. \
                   'transfer all' (level 102+) pulls every online \
                   player of lower level than you.",
        },
        run: cmd_transfer,
    }
}

inventory::submit! {
    Command {
        names: &["teleport"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "teleport <player | mob> <zone> <room> | <zone:id> | <id> | <target>",
            summary: "Send a player or mob to a room, or to wherever a target is.",
            long: "Builder+. Inverse of 'transfer' (which pulls them \
                   to you) and 'goto' (which moves you). The subject \
                   is a player or mob by name, 'N.name' or \
                   '<zone>:<id>' (a spawned mob instance). The \
                   destination is a room ('<zone> <id>', '<zone>:<id>', \
                   or a bare '<id>' in your zone) or any visible \
                   player, mob or object lying in a room: 'teleport \
                   bob 30:12', 'teleport 30:5 3.guard'. You cannot \
                   teleport a player of equal or higher level.",
        },
        run: cmd_teleport,
    }
}

inventory::submit! {
    Command {
        names: &["force"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "force <player> <command>",
            summary: "Make a player run a command as themselves.",
            long: "Implementor-only. Dispatches <command> with <player> \
                   as the actor — exactly as if they had typed it.",
        },
        run: cmd_force,
    }
}

inventory::submit! {
    Command {
        names: &["switch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "switch <mob>",
            summary: "Take control of a mob in your current room.",
            long: "Builder+. Future commands you type dispatch \
                   against the mob instead of yourself; output the \
                   mob would receive forwards to your connection. \
                   Useful for testing triggers from inside a mob \
                   and for running RP-as-NPC. 'return' ends the \
                   switch.",
        },
        run: cmd_switch,
    }
}

inventory::submit! {
    Command {
        names: &["return"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "return",
            summary: "End a 'switch' session and return to your own body.",
            long: "Builder+. Inverse of 'switch'. Always types as \
                   the puppeteer (not the mob) — the dispatcher \
                   keeps 'return' and 'switch' as escape hatches \
                   so a stuck switch can always be undone.",
        },
        run: cmd_return,
    }
}

inventory::submit! {
    Command {
        names: &["pain"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "pain <player>",
            summary: "Cosmetic divine wrath flourish on a single player.",
            long: "Builder+. Emits a roleplay-flavor pain line — no \
                   actual HP / stamina change. The target sees a \
                   personalized line; the room sees a third-person \
                   broadcast. Use to express divine displeasure \
                   during live events.",
        },
        run: cmd_pain,
    }
}

inventory::submit! {
    Command {
        names: &["rpain"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "rpain [<message>]",
            summary: "Cosmetic divine pain across the realm.",
            long: "Implementor-only. Same as 'pain' but reaches \
                   every online player. Pure flavor — no stat \
                   change. Optional <message> overrides the default \
                   line. Players see your line; the caster sees a \
                   confirmation count.",
        },
        run: cmd_rpain,
    }
}

inventory::submit! {
    Command {
        names: &["rrestore"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "rrestore [<message>]",
            summary: "Heal every online player + strip every effect.",
            long: "Implementor-only. Restores HP / stamina to max \
                   and clears every active EffectInstance for every \
                   online player. Optional <message> overrides the \
                   default flavor line. The realm-wide counterpart \
                   to 'restore'.",
        },
        run: cmd_rrestore,
    }
}

inventory::submit! {
    Command {
        names: &["echo"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "echo <message>",
            summary: "Speak a god-line into your current room.",
            long: "Builder+. Broadcasts the message verbatim (no \
                   speaker prefix) to everyone in the caster's \
                   room — useful for narrative prompts during \
                   live events. For a global broadcast, see 'gecho'.",
        },
        run: cmd_echo,
    }
}

inventory::submit! {
    Command {
        names: &["gecho"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "gecho <message>",
            summary: "Speak a god-line to every online player.",
            long: "Builder+. Bypasses Deaf / IgnoreList — gechoes \
                   are out-of-character announcements that should \
                   always reach the audience.",
        },
        run: cmd_gecho,
    }
}

inventory::submit! {
    Command {
        names: &["inctime"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "inctime <hours>",
            summary: "Force-advance the in-game clock.",
            long: "Implementor-only. Adds <hours> to 'MudClock.hour' \
                   modulo 24 — useful for testing day/night triggers \
                   and dawn/dusk weather transitions without \
                   waiting for real time.",
        },
        run: cmd_inctime,
    }
}

inventory::submit! {
    Command {
        names: &["dc"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "dc <player>",
            summary: "Disconnect a player by name.",
            long: "Implementor-only. Drops the player's Connection \
                   so the network task closes the socket cleanly. \
                   The autosave path runs on disconnect, so progress \
                   is preserved.",
        },
        run: cmd_dc,
    }
}

inventory::submit! {
    Command {
        names: &["send"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "send <player> <text>",
            summary: "Send raw text to a player's connection.",
            long: "Implementor-only. Skips the rendering pipeline \
                   entirely — bytes go straight to the descriptor. \
                   Used to surface diagnostic output that already \
                   carries ANSI escapes.",
        },
        run: cmd_send,
    }
}

inventory::submit! {
    Command {
        names: &["poofin"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "poofin [<message>]",
            summary: "Set your custom arrival message on goto / teleport.",
            long: "Builder+. Replaces the generic \"$n appears with an \
                   ear-splitting bang.\" with your own line ('$n' stands \
                   for your name). Bare \
                   'poofin' shows the current value; 'poofin clear' \
                   removes it.",
        },
        run: cmd_poofin,
    }
}

inventory::submit! {
    Command {
        names: &["poofout"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "poofout [<message>]",
            summary: "Set your custom departure message on goto / teleport.",
            long: "Builder+. Mirrors 'poofin'. Replaces the generic \
                   \"$n disappears in a puff of smoke.\" departure line.",
        },
        run: cmd_poofout,
    }
}

inventory::submit! {
    Command {
        names: &["cls", "clear"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "cls",
            summary: "Clear the terminal screen.",
            long: "Sends the ANSI clear-screen + cursor-home escape \
                   sequence. Cosmetic only; no game state changes.",
        },
        run: cmd_cls,
    }
}

inventory::submit! {
    Command {
        names: &["zreset"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "zreset [<zone_id>]",
            summary: "Despawn reset-spawned mobs/objects so respawn refills the zone.",
            long: "Builder+. With no arg, resets your current zone. \
                   Despawns every mob and object that came from a \
                   'MobResets' / 'ObjectResets' row in the named \
                   zone. The next respawn tick (~6s) refills the \
                   gaps. Admin-summoned / loadobj'd entities are \
                   preserved (no 'FromMobReset' / 'FromObjectReset' \
                   marker).",
        },
        run: cmd_zreset,
    }
}

inventory::submit! {
    Command {
        names: &["advance"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "advance <player> <level>",
            summary: "Set a player's level (admin level-up).",
            long: "Implementor-only. Bumps the target's XP to the \
                   threshold for <level> and runs the standard \
                   level-up loop, so HP/stamina max + practice \
                   points scale through the normal path. Refuses \
                   level decreases (those need a separate 'delevel' \
                   path).",
        },
        run: cmd_advance,
    }
}

inventory::submit! {
    Command {
        names: &["skillset"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "skillset <player> <ability> <proficiency>",
            summary: "Set a player's proficiency in a single ability.",
            long: "Builder+. Writes the row in the target's \
                   KnownAbilities. Inserts a new entry when the \
                   ability isn't already learned. Proficiency is \
                   0..=1000 (legacy convention).",
        },
        run: cmd_skillset,
    }
}

inventory::submit! {
    Command {
        names: &["reroll"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "reroll <player>",
            summary: "Reroll a player's six core stats (3d6 each).",
            long: "Implementor-only. Wipes CoreStats and rolls 3d6 \
                   per axis (STR / DEX / CON / INT / WIS / CHA). \
                   Sends the new roll to the target so they can \
                   verify it.",
        },
        run: cmd_reroll,
    }
}

inventory::submit! {
    Command {
        names: &["mute", "squelch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "mute <player>",
            summary: "Toggle a player's silence on global channels.",
            long: "Builder+. Toggles 'PlayerFlag::Muted' on the \
                   target. Muted players can't use gossip / shout / \
                   music / clan / quest channels — 'say' and 'tell' \
                   are unaffected so they can still play. Re-running \
                   'mute <name>' clears it.",
        },
        run: cmd_mute,
    }
}

inventory::submit! {
    Command {
        names: &["last"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "last <player>",
            summary: "Show last-login info for a character.",
            long: "Builder+. Looks up the character row by name and \
                   prints the last 'last_login' timestamp, level, \
                   race / class, and online-now status. Async DB \
                   call — output is delivered after the lookup \
                   returns.",
        },
        run: cmd_last,
    }
}

inventory::submit! {
    Command {
        names: &["wizinvis", "invis"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "wizinvis [<level> | off]",
            summary: "Become invisible to lower-level players.",
            long: "Builder+. With no arg, toggles invis at your own \
                   level. 'wizinvis <level>' sets to that exact \
                   level (capped at your own). 'wizinvis off' clears \
                   it. Players whose level is below yours (or \
                   below the explicit level) won't see you in \
                   'who' / 'look' / 'scan' listings.",
        },
        run: cmd_wizinvis,
    }
}

inventory::submit! {
    Command {
        names: &["snoop"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "snoop [<player>]",
            summary: "Mirror another player's output to your screen.",
            long: "Builder+. With a player name, starts mirroring \
                   their output (every line they receive prints to \
                   you with a dim '%' prefix). With no arg, stops \
                   the current snoop. Refuses snooping yourself, an \
                   equal-or-higher level account, or a player who's \
                   already being snooped — one snooper per target. \
                   Re-snooping a different target rewires cleanly.",
        },
        run: cmd_snoop,
    }
}

inventory::submit! {
    Command {
        names: &["peace"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "peace",
            summary: "Stop all combat in your current room.",
            long: "Builder+. Removes the 'Fighting' component from \
                   every entity in your room — useful when a brawl \
                   gets out of hand or a Lua trigger spawned a \
                   hostile mob you'd rather not pile on. Each \
                   disengaged combatant gets a quiet \"calm settles \
                   over the room\" line; a room broadcast confirms \
                   the action.",
        },
        run: cmd_peace,
    }
}

inventory::submit! {
    Command {
        names: &["unaffect"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "unaffect <target>",
            summary: "Strip every active effect from a target.",
            long: "Builder+. Despawns every 'EffectInstance' whose \
                   'AppliedTo' is the target. Reverses any modifier \
                   deltas via the standard expiry path so stat \
                   bumps walk back cleanly. <target> is a name in \
                   the current room.",
        },
        run: cmd_unaffect,
    }
}

inventory::submit! {
    Command {
        names: &["wizlock"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "wizlock [on|off]",
            summary: "Lock the mud to staff-only logins.",
            long: "Builder+. With no arg, prints the current state. \
                   'wizlock on' blocks non-staff (UserRole < Builder) \
                   from completing login; 'wizlock off' clears the \
                   gate. Reset to off on every server restart so a \
                   forgotten lock doesn't outlive the deploy.",
        },
        run: cmd_wizlock,
    }
}

inventory::submit! {
    Command {
        names: &["freeze"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "freeze <player>",
            summary: "Toggle a player's frozen state.",
            long: "Implementor-only. Frozen players can't input commands.",
        },
        run: cmd_freeze,
    }
}

inventory::submit! {
    Command {
        names: &["summon"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "summon <mob proto> [<count>]",
            summary: "Spawn one or more mob proto instances at your location.",
            long: "Builder+. Reads the (zone, id) MobProto and spawns \
                   'count' (default 1) instances Located on your room.",
        },
        run: cmd_summon,
    }
}

inventory::submit! {
    Command {
        names: &["apply"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "apply <player> <effect> [duration_secs]",
            summary: "Spawn an effect on a player.",
            long: "Implementor-only. Effect name is matched against \
                   the EffectCatalog. Default duration 60s.",
        },
        run: cmd_apply,
    }
}

inventory::submit! {
    Command {
        names: &["restore"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "restore <player>",
            summary: "Refill a player's HP and stamina to max.",
            long: "Implementor-only. No-op for offline players.",
        },
        run: cmd_restore,
    }
}

inventory::submit! {
    Command {
        names: &["slay"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "slay <player|mob>",
            summary: "Kill a target instantly, ignoring HP / armor.",
            long: "Implementor-only. Same death pipeline as combat — \
                   corpses, loot drops, triggers all fire normally.",
        },
        run: cmd_slay,
    }
}

inventory::submit! {
    Command {
        names: &["purge"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "purge [target]",
            summary: "Despawn the named target — or every non-player in the room.",
            long: "Implementor-only. With no arg, removes every mob / \
                   item in your current room. With a name, despawns \
                   that one entity.",
        },
        run: cmd_purge,
    }
}

inventory::submit! {
    Command {
        names: &["load"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "load <zone> <mob-id>",
            summary: "Spawn a mob proto into your current room.",
            long: "Builder+. Same as 'summon' for count=1, kept as a \
                   separate verb for muscle-memory.",
        },
        run: cmd_load,
    }
}

inventory::submit! {
    Command {
        names: &["loadobj", "loado"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "loadobj <zone> <obj-id>",
            summary: "Spawn an object proto onto the floor.",
            long: "Builder+. Materializes one instance of (zone, id) \
                   from 'ObjectPrototypes' Located on your current \
                   room.",
        },
        run: cmd_loadobj,
    }
}

inventory::submit! {
    Command {
        names: &["dumpworld"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "dumpworld [path]",
            summary: "Snapshot the entire entity store as JSON.",
            long: "Implementor-only. Useful for offline analysis. \
                   Path defaults to /tmp/world-dump-<ts>.json.",
        },
        run: cmd_dumpworld,
    }
}

// ---- handler bodies ----

pub(crate) fn cmd_where(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let role = world.get::<Account>(player).map(|a| a.role);
    let is_builder_plus = role.is_some_and(|r| r.at_least(UserRole::Builder));

    // No args: report the caller's own location with the same
    // (name, zone, id) format the listing form uses for parity.
    if arg.is_empty() {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        let name = name_or(world, located.0, "(unknown)");
        // A mortal standing in a god zone is told the room, never the
        // zone/id coordinates (god zones are not on any mortal map).
        if !crate::room_access::room_visible_to(world, player, located.0) {
            send_rendered(world, player, &format!("You are in: {name}\r\n"));
            return;
        }
        let (zone, id) = world
            .get::<WorldKey>(located.0)
            .map_or((-1, -1), |k| (k.zone, k.id));
        send_rendered(
            world,
            player,
            &format!("You are in: {name}  [{zone}:{id}]\r\n"),
        );
        return;
    }

    // `where all` / `where list` retains the original Builder+ listing
    // — every online player + their room. Mortals get the gate refusal
    // here rather than at the dispatcher so the help-text contract
    // matches the implementation contract.
    if arg.eq_ignore_ascii_case("all") || arg.eq_ignore_ascii_case("list") {
        if !is_builder_plus {
            send_to(
                world,
                player,
                "Only Builders+ can list every online player.\r\n",
            );
            return;
        }
        let mut rows: Vec<(String, String)> = {
            let mut q = world.query_filtered::<(&Named, &Located), (With<Player>, With<Online>)>();
            q.iter(world)
                .filter(|(_, l)| crate::room_access::room_visible_to(world, player, l.0))
                .map(|(n, l)| {
                    let room_name = name_or(world, l.0, "(unknown)");
                    (n.name.clone(), room_name)
                })
                .collect()
        };
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let mut out = format!("\r\n{} player(s) online:\r\n", rows.len());
        for (name, room) in &rows {
            // pad_visible: counts visible chars, skipping XML-Lite tags.
            let padded = pad_visible(name, 24);
            out.push_str(&format!("  {padded} {room}\r\n"));
        }
        send_to(world, player, out);
        return;
    }

    // Immortal+ (legacy `perform_immort_where`): players, then live
    // mobs, then objects wherever they sit.
    if crate::room_access::is_immortal(world, player) {
        staff_where(world, player, arg);
        return;
    }

    // `where <name>` — locate one online player. Match is case-
    // insensitive against `Characters.name`. Offline characters
    // intentionally fall through to "isn't online" rather than
    // disclosing their last-known room.
    let needle = arg.to_ascii_lowercase();
    let target = {
        let mut q =
            world.query_filtered::<(Entity, &Named, &Located), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(e, n, _)| {
                n.name.eq_ignore_ascii_case(&needle)
                    && crate::commands::can_see_player(world, player, *e)
            })
            .map(|(e, _, l)| (e, l.0))
    };
    // A target standing in a god zone reads as offline to mortals.
    let target =
        target.filter(|(_, room)| crate::room_access::room_visible_to(world, player, *room));
    let Some((target_entity, room)) = target else {
        send_to(world, player, format!("'{arg}' isn't online.\r\n"));
        return;
    };
    let target_name = name_of(world, target_entity);
    let room_name = name_or(world, room, "(unknown)");
    let (zone, id) = world
        .get::<WorldKey>(room)
        .map_or((-1, -1), |k| (k.zone, k.id));
    send_rendered(
        world,
        player,
        &format!(
            "{} is in: {room_name}  [{zone}:{id}]\r\n",
            cap_sentence_start(&target_name)
        ),
    );
}
/// Rows shown per section of a staff `where <name>` before the overflow
/// footer (a short needle like "a" would otherwise flood the screen).
const WHERE_SECTION_LIMIT: usize = 100;

/// Deepest container nesting `where` follows ("inside A at inside B at ...").
const WHERE_MAX_NESTING: usize = 8;

/// `room name [zone:id]` for a room entity.
fn room_label(world: &World, room: Entity) -> String {
    let name = name_or(world, room, "(unknown)");
    let (zone, id) = world
        .get::<WorldKey>(room)
        .map_or((-1, -1), |k| (k.zone, k.id));
    format!("{name} [{zone}:{id}]")
}

/// Where an item is, in legacy `print_object_location` words: in a room,
/// carried / worn by an actor (with that actor's room), or inside another
/// item (followed through to wherever that sits). `None` hides the row:
/// the holder is an actor the viewer cannot see.
fn item_location(world: &World, viewer: Entity, item: Entity, depth: usize) -> Option<String> {
    let Some(parent) = world.get::<Located>(item).map(|l| l.0) else {
        return Some("in an unknown location".to_string());
    };
    if world.get::<mud_world::Room>(parent).is_some() {
        return Some(room_label(world, parent));
    }
    if world.get::<Player>(parent).is_some() || world.get::<Mob>(parent).is_some() {
        if !crate::commands::can_see_player(world, viewer, parent) {
            return None;
        }
        let verb = if world.get::<mud_world::EquippedSlot>(item).is_some() {
            "worn by"
        } else {
            "carried by"
        };
        let holder = name_of(world, parent);
        let at = world
            .get::<Located>(parent)
            .map_or_else(|| "nowhere".to_string(), |l| room_label(world, l.0));
        return Some(format!("{verb} {holder} at {at}"));
    }
    if world.get::<Item>(parent).is_some() {
        let outer = name_of(world, parent);
        if depth >= WHERE_MAX_NESTING {
            return Some(format!("inside {outer}"));
        }
        let at = item_location(world, viewer, parent, depth + 1)?;
        return Some(format!("inside {outer} at {at}"));
    }
    Some("in an unknown location".to_string())
}

/// Immortal+ `where <name>` (legacy `perform_immort_where`): every visible
/// player, then live mob, then object whose name / keywords match. Mob and
/// object rows lead with the instance's prototype `[zone:id]`.
fn staff_where(world: &mut World, viewer: Entity, arg: &str) {
    let needle = arg.to_ascii_lowercase();
    let mut out = String::new();

    // Players: the original `X is in: room [zone:id]` line.
    let mut players: Vec<(String, Entity)> = {
        let mut q =
            world.query_filtered::<(Entity, &Named, &Located), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(e, n, _)| {
                matches(&needle, n, None) && crate::commands::can_see_player(world, viewer, *e)
            })
            .map(|(_, n, l)| (n.name.clone(), l.0))
            .collect()
    };
    players.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, room) in &players {
        out.push_str(&format!(
            "{} is in: {}\r\n",
            cap_sentence_start(name),
            room_label(world, *room)
        ));
    }

    // Live mobs, in a stable (proto key, spawn order) order.
    let mut mobs: Vec<(Entity, (i32, i32), String, Entity)> = {
        let mut q = world.query_filtered::<(
            Entity,
            &Named,
            Option<&Keywords>,
            &Located,
            Option<&WorldKey>,
        ), With<Mob>>();
        q.iter(world)
            .filter(|(e, n, kw, _, _)| {
                matches(&needle, n, *kw) && crate::commands::can_see_player(world, viewer, *e)
            })
            .map(|(e, n, _, l, k)| {
                (
                    e,
                    k.map_or((-1, -1), |k| (k.zone, k.id)),
                    n.name.clone(),
                    l.0,
                )
            })
            .collect()
    };
    mobs.sort_by_key(|(e, key, _, _)| (*key, e.index_u32()));
    for (i, (_, (zone, id), name, room)) in mobs.iter().take(WHERE_SECTION_LIMIT).enumerate() {
        out.push_str(&format!(
            "M{:>3}. [{zone}:{id}] {} - {}\r\n",
            i + 1,
            pad_visible(name, 25),
            room_label(world, *room)
        ));
    }
    if mobs.len() > WHERE_SECTION_LIMIT {
        out.push_str(&format!(
            "     ... {} more mob(s); narrow your search.\r\n",
            mobs.len() - WHERE_SECTION_LIMIT
        ));
    }

    // Objects, with carried / worn / contained locations.
    let mut items: Vec<(Entity, (i32, i32), String)> = {
        let mut q = world
            .query_filtered::<(Entity, &Named, Option<&Keywords>, Option<&WorldKey>), With<Item>>();
        q.iter(world)
            .filter(|(_, n, kw, _)| matches(&needle, n, *kw))
            .map(|(e, n, _, k)| (e, k.map_or((-1, -1), |k| (k.zone, k.id)), n.name.clone()))
            .collect()
    };
    items.sort_by_key(|(e, key, _)| (*key, e.index_u32()));
    let (mut shown, mut hidden_or_over) = (0usize, 0usize);
    for (item, (zone, id), name) in &items {
        let Some(location) = item_location(world, viewer, *item, 0) else {
            continue;
        };
        if shown >= WHERE_SECTION_LIMIT {
            hidden_or_over += 1;
            continue;
        }
        shown += 1;
        out.push_str(&format!(
            "O{shown:>3}. [{zone}:{id}] {} - {location}\r\n",
            pad_visible(name, 25)
        ));
    }
    if hidden_or_over > 0 {
        out.push_str(&format!(
            "     ... {hidden_or_over} more object(s); narrow your search.\r\n"
        ));
    }

    if out.is_empty() {
        send_to(world, viewer, "Couldn't find any such thing.\r\n");
    } else {
        send_rendered(world, viewer, &out);
    }
}

pub(crate) fn cmd_slay(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "slay", args);
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: slay <mob>\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    // Self-target: only Implementor can slay themselves, and only as a
    // way to test the death/release cycle without finding a willing
    // mob. find_actor_in_room excludes self, so the keyword check has
    // to happen first.
    let self_name = name_of(world, player);
    let target = if matches_self(&self_name, arg) {
        let role = world
            .get::<Account>(player)
            .map_or(mud_db::enums::UserRole::Player, |a| a.role);
        if role != mud_db::enums::UserRole::Implementor {
            send_to(
                world,
                player,
                "You can't slay yourself. (Only Implementor accounts may, for testing the \
                 death/release cycle.)\r\n",
            );
            return;
        }
        player
    } else if let Some(t) = find_actor_in_room(world, arg, located.0, player) {
        t
    } else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if target != player && world.get::<Player>(target).is_some() {
        send_to(
            world,
            player,
            "Slaying players is not allowed. Use 'restore' if they're in trouble.\r\n",
        );
        return;
    }
    let target_name = name_or(world, target, "(unknown)");

    // Notify the room before death.
    let admin_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        located.0,
        &[player],
        &format!("{admin_name} extends a hand and {target_name} crumbles to dust.\r\n"),
    );
    send_rendered(
        world,
        player,
        &format!("{target_name} crumbles to dust at your gesture.\r\n"),
    );

    // Briefly point the admin at the target so the kill payout's
    // first-Player-attacker walk credits them. handle_death sweeps
    // the Fighting component on the way out.
    try_insert(world, player, Fighting(target));
    crate::combat::handle_death(world, target, &target_name, located.0);
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_dumpworld(world: &mut World, player: Entity, args: &str) {
    let path = args.trim();
    let path = if path.is_empty() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("/tmp/world_dump_{stamp}.json")
    } else {
        path.to_string()
    };

    let tick = world.resource::<TickCount>().0;
    let clock = world.resource::<mud_world::MudClock>().clone();

    // Online players roster.
    let players: Vec<serde_json::Value> = {
        let mut q = world.query_filtered::<(
            &Named,
            &Account,
            Option<&Profile>,
            &Located,
            Option<&Health>,
            Option<&Stamina>,
            Option<&Wealth>,
        ), (With<Player>, With<Online>)>();
        q.iter(world)
            .map(|(name, acct, prof, loc, hp, st, wealth)| {
                let room_name = name_or(world, loc.0, "(unknown)");
                let room_key = world
                    .get::<WorldKey>(loc.0)
                    .map_or((-1, -1), |wk| (wk.zone, wk.id));
                serde_json::json!({
                    "name": name.name,
                    "role": acct.role.label(),
                    "level": prof.map_or(0, |p| p.level),
                    "race": prof.map(|p| p.race.clone()).unwrap_or_default(),
                    "room_name": room_name,
                    "room_zone": room_key.0,
                    "room_id": room_key.1,
                    "hp": hp.map_or(0, |h| h.hp),
                    "hp_max": hp.map_or(0, |h| h.max),
                    "stamina": st.map_or(0, |s| s.current),
                    "stamina_max": st.map_or(0, |s| s.max),
                    "wealth_copper": wealth.map_or(0, |w| w.0),
                })
            })
            .collect()
    };

    // Entity counts.
    let mob_count = {
        let mut q = world.query_filtered::<Entity, (With<Mob>, Without<Player>)>();
        q.iter(world).count()
    };
    let item_count = {
        let mut q = world.query::<&Item>();
        q.iter(world).count()
    };
    let effect_count = {
        let mut q = world.query::<&EffectInstance>();
        q.iter(world).count()
    };

    let trigger_catalog = world.resource::<mud_world::TriggerCatalog>();
    let triggers = serde_json::json!({
        "rows": trigger_catalog.by_key.len(),
        "mob_attachments": trigger_catalog.mob_attachments.len(),
        "object_attachments": trigger_catalog.object_attachments.len(),
        "room_attachments": trigger_catalog.room_attachments.len(),
    });

    let payload = serde_json::json!({
        "schema_version": 1,
        "captured_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        "tick": tick,
        "clock": {
            "year": clock.year,
            "month": clock.month,
            "day": clock.day,
            "hour": clock.hour,
            "minute": clock.minute,
            "stamp": clock.stamp,
        },
        "counts": {
            "online_players": players.len(),
            "mobs": mob_count,
            "items": item_count,
            "effect_instances": effect_count,
        },
        "players": players,
        "triggers": triggers,
    });

    let serialized = match serde_json::to_string_pretty(&payload) {
        Ok(s) => s,
        Err(e) => {
            send_to(world, player, format!("Serialization failed: {e}\r\n"));
            return;
        }
    };

    if let Err(e) = std::fs::write(&path, &serialized) {
        send_to(world, player, format!("Write failed ({path}): {e}\r\n"));
        return;
    }

    let bytes = serialized.len();
    let player_count = payload["counts"]["online_players"].as_u64().unwrap_or(0);
    send_to(
        world,
        player,
        format!("World dumped to {path} ({bytes} bytes, {player_count} player(s)).\r\n"),
    );
    info!(path = %path, bytes, "dumpworld checkpoint written");
}
/// `purge <player corpse>`: report what [`crate::corpses::purge_player_corpse`] did.
fn purge_one_player_corpse(world: &mut World, admin: Entity, corpse: Entity, name: &str) {
    let msg = match crate::corpses::purge_player_corpse(world, corpse) {
        Ok(items) => format!("You purge {name} and its {items} item(s).\r\n"),
        Err(crate::corpses::PurgeRefusal::Settling) => {
            format!("{name} is still settling; try again in a moment.\r\n")
        }
        Err(crate::corpses::PurgeRefusal::LootPending) => {
            format!("Someone's loot from {name} hasn't been saved yet; try again in a moment.\r\n")
        }
    };
    send_rendered(world, admin, &msg);
}

pub(crate) fn cmd_purge(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "purge", args);
    let arg = args.trim();
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;

    if !arg.is_empty() {
        // Single-target form: try mobs/items in the room (no players).
        let target = find_actor_in_room(world, arg, room, player)
            .filter(|e| world.get::<Player>(*e).is_none())
            .or_else(|| {
                let mut q = world
                    .query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
                q.iter(world)
                    .find(|(_, l, n, kw)| l.0 == room && matches(&arg.to_ascii_lowercase(), n, *kw))
                    .map(|(e, _, _, _)| e)
            });
        let Some(target) = target else {
            send_to(world, player, format!("No purge-able '{arg}' here.\r\n"));
            return;
        };
        let target_name = name_or(world, target, "(unknown)");
        // A player corpse is database-backed: it goes (with everything in
        // it) only once its rows are settled and no looter's save is
        // pending, and its row is deleted through the corpse writer.
        if world.get::<PlayerCorpse>(target).is_some() {
            purge_one_player_corpse(world, player, target, &target_name);
            return;
        }
        // Cascade-despawn: anything Located on the target (mob's gear /
        // container contents) goes too.
        let nested: Vec<Entity> = {
            let mut q = world.query::<(Entity, &Located)>();
            q.iter(world)
                .filter(|(_, l)| l.0 == target)
                .map(|(e, _)| e)
                .collect()
        };
        for n in nested {
            if let Ok(e) = world.get_entity_mut(n) {
                e.despawn();
            }
        }
        if let Ok(e) = world.get_entity_mut(target) {
            e.despawn();
        }
        send_rendered(world, player, &format!("You purge {target_name}.\r\n"));
        return;
    }

    // No-arg form: every mob + every item in the room.
    let mobs: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Mob>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == room)
            .map(|(e, _)| e)
            .collect()
    };
    // Player corpses hold a dead player's persisted gear: never swept.
    let room_items: Vec<(Entity, bool)> = {
        let mut q = world.query_filtered::<(Entity, &Located, Has<PlayerCorpse>), With<Item>>();
        q.iter(world)
            .filter(|(_, l, _)| l.0 == room)
            .map(|(e, _, pc)| (e, pc))
            .collect()
    };
    let corpses_left = room_items.iter().filter(|(_, pc)| *pc).count();
    let items: Vec<Entity> = room_items
        .into_iter()
        .filter(|(_, pc)| !*pc)
        .map(|(e, _)| e)
        .collect();
    let mob_count = mobs.len();
    let item_count = items.len();
    // Despawn nested children of mobs first (gear, contents).
    let nested_of_mobs: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located)>();
        q.iter(world)
            .filter(|(_, l)| mobs.contains(&l.0))
            .map(|(e, _)| e)
            .collect()
    };
    let nested_count = nested_of_mobs.len();
    for e in nested_of_mobs
        .into_iter()
        .chain(mobs.into_iter())
        .chain(items.into_iter())
    {
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
    let mut report =
        format!("Purged {mob_count} mob(s), {item_count} item(s), and {nested_count} nested.");
    if corpses_left > 0 {
        report.push_str(&format!(
            " {corpses_left} player corpse(s) left alone (use 'purge <corpse>' to remove one)."
        ));
    }
    send_to(world, player, format!("{report}\r\n"));
}
pub(crate) fn cmd_restore(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let target =
        if arg.is_empty() || arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
            player
        } else {
            let Some(located) = world.get::<Located>(player).copied() else {
                send_to(world, player, "You are nowhere.\r\n");
                return;
            };
            let Some(found) = find_actor_in_room(world, arg, located.0, player) else {
                send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
                return;
            };
            found
        };
    if let Some(mut h) = world.get_mut::<Health>(target) {
        h.hp = h.max;
    }
    if let Some(mut s) = world.get_mut::<Stamina>(target) {
        s.current = s.max;
    }
    let target_name = name_or(world, target, "(unknown)");
    if target == player {
        send_to(world, player, "You feel completely refreshed.\r\n");
        return;
    }
    let admin_name = name_of(world, player);
    send_rendered(world, player, &format!("You restore {target_name}.\r\n"));
    send_rendered(
        world,
        target,
        &format!("{admin_name} restores you. You feel completely refreshed.\r\n"),
    );
}
pub(crate) fn cmd_apply(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 || parts.len() > 3 {
        send_to(
            world,
            player,
            "Usage: apply <effect_name> <target> [seconds]\r\n",
        );
        return;
    }
    let effect_name = parts[0];
    let target_word = parts[1];
    let duration_s: i32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);

    let effect_def = world
        .resource::<EffectCatalog>()
        .find_by_name(effect_name)
        .cloned();
    let Some(effect_def) = effect_def else {
        send_to(world, player, format!("Unknown effect: {effect_name}\r\n"));
        return;
    };

    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let target =
        if target_word.eq_ignore_ascii_case("me") || target_word.eq_ignore_ascii_case("self") {
            Some(player)
        } else {
            let target_lower = target_word.to_ascii_lowercase();
            let mut q = world.query::<(Entity, &Located, &Named)>();
            q.iter(world)
                .find(|(e, l, n)| {
                    *e != player
                        && l.0 == located.0
                        && n.name.to_ascii_lowercase().contains(&target_lower)
                })
                .map(|(e, _, _)| e)
        };
    let Some(target) = target else {
        send_rendered(world, player, &format!("No '{target_word}' here.\r\n"));
        return;
    };

    world.spawn((
        EffectInstance {
            kind: effect_def.id,
            name: effect_def.name.clone(),
            strength: 1,
            remaining_secs: duration_s,
            source: EffectSource::Admin,
            ability_id: None,
        },
        AppliedTo(target),
    ));

    let target_name = name_or(world, target, "(unknown)");
    let dur_label = if duration_s < 0 {
        "permanently".to_string()
    } else {
        format!("for {duration_s}s")
    };
    send_to(
        world,
        player,
        format!(
            "Applied '{}' to {target_name} {dur_label}.\r\n",
            effect_def.name
        ),
    );
    if target != player {
        send_to(
            world,
            target,
            format!("You feel the effect of {}.\r\n", effect_def.name),
        );
    }
}
pub(crate) fn cmd_load(world: &mut World, player: Entity, args: &str) {
    let trimmed = args.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let kind = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    if rest.is_empty() {
        send_to(world, player, "Usage: load <obj|mob> <zone> <id>\r\n");
        return;
    }
    match kind.to_ascii_lowercase().as_str() {
        "obj" | "object" | "item" => cmd_loadobj(world, player, rest),
        "mob" | "mobile" | "npc" | "creature" => cmd_summon(world, player, rest),
        other => send_to(
            world,
            player,
            format!("Unknown load type '{other}'. Use obj or mob.\r\n"),
        ),
    }
}
pub(crate) fn cmd_loadobj(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "loadobj", args);
    let parts: Vec<&str> = args.split_whitespace().collect();
    let Some((zone, obj_id)) = super::admin_inspect::try_parse_zone_id(world, player, &parts)
    else {
        send_to(world, player, "Usage: loadobj [<zone_id>] <obj_id>\r\n");
        return;
    };

    let proto = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(zone, obj_id))
        .cloned();
    let Some(proto) = proto else {
        send_to(
            world,
            player,
            format!("No object prototype ({zone}, {obj_id}).\r\n"),
        );
        return;
    };

    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; can't load.\r\n");
        return;
    };
    let room = located.0;
    let proto_name = proto.name.clone();
    let proto_keywords = proto.keywords.clone();
    let examine = proto.examine_description.clone();

    let primary_slot = mud_world::wear_flags_primary_slot(&proto.wear_flags);
    // Spawn directly into the loader's inventory rather than the
    // floor — admin tooling shouldn't race with scavengers / mobs /
    // other players who could grab a freshly-loaded item before
    // the admin reacts. `get` and `drop` are still available if
    // the admin actually wants it on the floor.
    let mut bundle = world.spawn((
        Item,
        Named {
            name: proto_name.clone(),
        },
        Keywords(proto_keywords),
        WorldKey {
            zone: proto.zone_id,
            id: proto.id,
        },
        Located(player),
    ));
    if let Some(desc) = examine {
        bundle.insert(Description(desc));
    }
    if let Some(s) = primary_slot {
        bundle.insert(WearableIn(s));
    }
    if let Some(board_id) = proto.board_id {
        bundle.insert(mud_world::BoardLink(board_id));
    }
    if let Some(liq) = proto.liquid.clone() {
        bundle.insert(mud_world::LiquidContainer {
            liquid: liq.liquid,
            capacity: liq.capacity,
            remaining: liq.remaining,
            poisoned: liq.poisoned,
        });
    }
    if let Some(fuel) = proto.light_fuel {
        bundle.insert(mud_world::LightFuel {
            capacity: fuel.capacity,
            remaining: fuel.remaining,
        });
    }
    if !proto.flags.is_empty() {
        bundle.insert(mud_world::ObjectFlags(proto.flags.clone()));
    }
    if !proto.restrictions.is_empty() {
        bundle.insert(mud_world::ObjectRestrictions(proto.restrictions.clone()));
    }
    let item = bundle.id();
    crate::item_decay::attach_timer_if_decaying(world, item, &proto);
    // Populate Charges from the first ObjectAbilities binding
    // (wands and staves carry finite-use charges in the schema's
    // `charges` column). Items without a binding or without
    // charges set get no Charges component → treated as unlimited.
    if let Some(charges) = world
        .resource::<mud_world::ObjectAbilityCatalog>()
        .by_key
        .get(&(proto.zone_id, proto.id))
        .and_then(|v| v.first().and_then(|b| b.charges))
    {
        crate::commands::try_insert(world, item, mud_world::Charges(charges));
    }

    send_rendered(
        world,
        player,
        &format!("Loaded {proto_name} (entity {item:?}) into your inventory.\r\n"),
    );
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} produces {proto_name} from thin air.\r\n"),
    );
}
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_summon(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "summon", args);
    let parts: Vec<&str> = args.split_whitespace().collect();
    let Some((zone, mob_id)) = super::admin_inspect::try_parse_zone_id(world, player, &parts)
    else {
        send_to(world, player, "Usage: summon [<zone_id>] <mob_id>\r\n");
        return;
    };

    let proto = world
        .resource::<MobPrototypes>()
        .by_key
        .get(&(zone, mob_id))
        .cloned();
    let Some(proto) = proto else {
        send_rendered(
            world,
            player,
            &format!("No mob prototype ({zone}, {mob_id}).\r\n"),
        );
        return;
    };

    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; can't summon.\r\n");
        return;
    };
    let room = located.0;
    // NoSummonRoom gate — `Room.allows_summon = false` refuses any
    // mob materialization in this room (anti-summon wards, planar
    // boundaries). Admin staff still see the refusal; legacy
    // parity left even imm-tier verbs honoring the gate so that
    // sanctuary content stays inviolate. To override, builders
    // must clear the flag first.
    if world.get::<mud_world::NoSummonRoom>(room).is_some() {
        send_to(
            world,
            player,
            "An anti-summon ward in this room rebuffs the call.\r\n",
        );
        return;
    }

    let mob_entity = mud_world::spawn_mob_from_proto(world, &proto, room, None);
    let hp = world.get::<Health>(mob_entity).map_or(0, |h| h.max);
    let dmg = proto.avg_damage();
    let proto_name = proto.name.clone();

    send_rendered(
        world,
        player,
        &format!("Summoned {proto_name} (entity {mob_entity:?}) — HP {hp}, dmg avg {dmg}.\r\n"),
    );
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} summons {proto_name} from thin air.\r\n"),
    );
    // The new mob may be hostile to anyone else standing here.
    crate::commands::aggro_room_players(world, room);
}
pub(crate) fn cmd_switch(world: &mut World, player: Entity, args: &str) {
    use mud_world::{SwitchedFrom, SwitchedInto};
    record_admin_action(world, player, "switch", args);
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: switch <mob>\r\n");
        return;
    }
    if world.get::<SwitchedInto>(player).is_some() {
        send_to(
            world,
            player,
            "You're already controlling someone. Use 'return' first.\r\n",
        );
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let mob = find_actor_in_room(world, arg, located.0, player);
    let Some(mob) = mob else {
        send_to(world, player, format!("No '{arg}' here.\r\n"));
        return;
    };
    if world.get::<Player>(mob).is_some() {
        send_to(world, player, "You can't switch into another player.\r\n");
        return;
    }
    if world.get::<SwitchedFrom>(mob).is_some() {
        send_to(world, player, "Someone else is already in there.\r\n");
        return;
    }
    try_insert(world, player, SwitchedInto(mob));
    try_insert(world, mob, SwitchedFrom(player));
    let mob_name = name_of(world, mob);
    send_rendered(
        world,
        player,
        &format!("<dim>You slip into {mob_name}. Type 'return' to come back.</>\r\n"),
    );
}

pub(crate) fn cmd_return(world: &mut World, player: Entity, args: &str) {
    use mud_world::{SwitchedFrom, SwitchedInto};
    record_admin_action(world, player, "return", args);
    let mob = world.get::<SwitchedInto>(player).map(|s| s.0);
    let Some(mob) = mob else {
        send_to(world, player, "You aren't switched into anyone.\r\n");
        return;
    };
    try_remove::<SwitchedInto>(world, player);
    try_remove::<SwitchedFrom>(world, mob);
    let mob_name = name_of(world, mob);
    send_rendered(
        world,
        player,
        &format!("<dim>You slip out of {mob_name} and back into your own body.</>\r\n"),
    );
}

pub(crate) fn cmd_pain(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "pain", args);
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: pain <player>\r\n");
        return;
    }
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(arg))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{arg}' isn't online.\r\n"));
        return;
    };
    let target_name = name_of(world, target);
    let admin_name = name_of(world, player);
    send_rendered(
        world,
        player,
        &format!("You wreathe {target_name} in divine pain.\r\n"),
    );
    send_rendered(
        world,
        target,
        &format!(
            "<red>{admin_name} wreathes you in divine pain — your nerves scream with otherworldly fire!</>\r\n"
        ),
    );
    if let Some(located) = world.get::<Located>(target).copied() {
        broadcast_room_except_players_rendered(
            world,
            located.0,
            &[player, target],
            &format!("<red>{target_name} writhes as divine pain courses through them.</>\r\n"),
        );
    }
}

pub(crate) fn cmd_rpain(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "rpain", args);
    let custom = args.trim();
    let admin_name = name_of(world, player);
    let line = if custom.is_empty() {
        format!(
            "<red>{admin_name} spreads pain and pestilence across the realm — its harm reaches all in their path!</>\r\n"
        )
    } else {
        format!("<red>{custom}</>\r\n")
    };
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world).filter(|e| *e != player).collect()
    };
    let count = targets.len();
    for t in targets {
        send_rendered(world, t, &line);
    }
    send_rendered(
        world,
        player,
        &format!("Pain spread across the realm. {count} player(s) felt your wrath.\r\n"),
    );
}

pub(crate) fn cmd_rrestore(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "rrestore", args);
    let custom = args.trim();
    let admin_name = name_of(world, player);
    let line = if custom.is_empty() {
        format!(
            "<b:cyan>{admin_name} spreads healing energy across the realm, restoring all in its path.</>\r\n"
        )
    } else {
        format!("<b:cyan>{custom}</>\r\n")
    };
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world).collect()
    };
    let count = targets.len();
    for t in targets {
        if let Some(mut h) = world.get_mut::<Health>(t) {
            h.hp = h.max;
        }
        if let Some(mut s) = world.get_mut::<Stamina>(t) {
            s.current = s.max;
        }
        crate::commands::remove_all_effects_on(world, t);
        if t != player {
            send_rendered(world, t, &line);
        }
    }
    send_rendered(
        world,
        player,
        &format!("Restored {count} player(s) and cleansed every active effect.\r\n"),
    );
}

pub(crate) fn cmd_echo(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "echo", args);
    let msg = args.trim();
    if msg.is_empty() {
        send_to(world, player, "Echo what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let line = format!("{msg}\r\n");
    // Send to caster + every Player in the room. No speaker prefix —
    // god-lines are unattributed by convention.
    send_rendered(world, player, &line);
    broadcast_room_except_players_rendered(world, located.0, &[player], &line);
}

pub(crate) fn cmd_gecho(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "gecho", args);
    let msg = args.trim();
    if msg.is_empty() {
        send_to(world, player, "Gecho what?\r\n");
        return;
    }
    let line = format!("{msg}\r\n");
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<Online>)>();
        q.iter(world).collect()
    };
    for t in targets {
        send_rendered(world, t, &line);
    }
}

pub(crate) fn cmd_inctime(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "inctime", args);
    let arg = args.trim();
    let Ok(hours) = arg.parse::<i32>() else {
        send_to(world, player, "Usage: inctime <hours>\r\n");
        return;
    };
    let new_hour = {
        let mut clock = world.resource_mut::<mud_world::MudClock>();
        let h = (clock.hour + hours).rem_euclid(24);
        clock.hour = h;
        h
    };
    send_rendered(
        world,
        player,
        &format!("Clock advanced by {hours} hour(s); now hour {new_hour}.\r\n"),
    );
}

pub(crate) fn cmd_dc(world: &mut World, player: Entity, args: &str) {
    use crate::commands::Connection;
    record_admin_action(world, player, "dc", args);
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: dc <player>\r\n");
        return;
    }
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(arg))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{arg}' isn't online.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "Disconnecting yourself is silly.\r\n");
        return;
    }
    let target_name = name_of(world, target);
    send_rendered(
        world,
        target,
        "<red>You have been disconnected by an admin.</>\r\n",
    );
    // Drop the Connection — the network task sees the channel
    // close and exits. The disconnect handler runs the autosave
    // path on tear-down.
    if let Ok(mut em) = world.get_entity_mut(target) {
        em.remove::<Connection>();
    }
    send_to(world, player, format!("Disconnected {target_name}.\r\n"));
}

pub(crate) fn cmd_send(world: &mut World, player: Entity, args: &str) {
    use crate::commands::send_raw;
    record_admin_action(world, player, "send", args);
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].trim().is_empty() {
        send_to(world, player, "Usage: send <player> <text>\r\n");
        return;
    }
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(parts[0].trim()))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{}' isn't online.\r\n", parts[0]));
        return;
    };
    send_raw(world, target, format!("{}\r\n", parts[1]));
    let target_name = name_of(world, target);
    send_rendered(world, player, &format!("Sent to {target_name}.\r\n"));
}

pub(crate) fn cmd_poofin(world: &mut World, player: Entity, args: &str) {
    use mud_world::Poofs;
    record_admin_action(world, player, "poofin", args);
    let arg = args.trim();
    if arg.is_empty() {
        let current = world.get::<Poofs>(player).and_then(|p| p.poof_in.clone());
        match current {
            Some(s) => send_rendered(world, player, &format!("Your poofin is: {s}\r\n")),
            None => send_to(world, player, "You have no poofin set.\r\n"),
        }
        return;
    }
    if world.get::<Poofs>(player).is_none()
        && let Ok(mut em) = world.get_entity_mut(player)
    {
        em.insert(Poofs::default());
    }
    if arg.eq_ignore_ascii_case("clear") {
        if let Some(mut p) = world.get_mut::<Poofs>(player) {
            p.poof_in = None;
        }
        send_to(world, player, "Poofin cleared.\r\n");
        return;
    }
    if let Some(mut p) = world.get_mut::<Poofs>(player) {
        p.poof_in = Some(arg.to_string());
    }
    send_rendered(world, player, &format!("Poofin set to: {arg}\r\n"));
}

pub(crate) fn cmd_poofout(world: &mut World, player: Entity, args: &str) {
    use mud_world::Poofs;
    record_admin_action(world, player, "poofout", args);
    let arg = args.trim();
    if arg.is_empty() {
        let current = world.get::<Poofs>(player).and_then(|p| p.poof_out.clone());
        match current {
            Some(s) => send_rendered(world, player, &format!("Your poofout is: {s}\r\n")),
            None => send_to(world, player, "You have no poofout set.\r\n"),
        }
        return;
    }
    if world.get::<Poofs>(player).is_none()
        && let Ok(mut em) = world.get_entity_mut(player)
    {
        em.insert(Poofs::default());
    }
    if arg.eq_ignore_ascii_case("clear") {
        if let Some(mut p) = world.get_mut::<Poofs>(player) {
            p.poof_out = None;
        }
        send_to(world, player, "Poofout cleared.\r\n");
        return;
    }
    if let Some(mut p) = world.get_mut::<Poofs>(player) {
        p.poof_out = Some(arg.to_string());
    }
    send_rendered(world, player, &format!("Poofout set to: {arg}\r\n"));
}

pub(crate) fn cmd_cls(world: &mut World, player: Entity, _args: &str) {
    use crate::commands::send_raw;
    // ANSI: ESC[2J clears the screen, ESC[H homes the cursor. Most
    // terminals support both even when the player has color stripped.
    send_raw(world, player, "\x1b[2J\x1b[H");
}

pub(crate) fn cmd_zreset(world: &mut World, player: Entity, args: &str) {
    use mud_world::{FromMobReset, FromObjectReset};
    record_admin_action(world, player, "zreset", args);
    let arg = args.trim();
    let zone: i32 = if arg.is_empty() {
        let here = world
            .get::<Located>(player)
            .and_then(|l| world.get::<WorldKey>(l.0).map(|k| k.zone));
        let Some(zone) = here else {
            send_to(world, player, "Can't resolve current zone.\r\n");
            return;
        };
        zone
    } else if let Ok(z) = arg.parse::<i32>() {
        z
    } else {
        send_to(world, player, "Usage: zreset [<zone_id>]\r\n");
        return;
    };

    // Snapshot every reset-spawned mob and object whose WorldKey
    // zone matches. Admin-summoned entities lack the From* marker
    // and get preserved. Players (not Mob / Item) are also
    // skipped by the With<Mob>/With<Item> filters.
    let mob_targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &WorldKey, &FromMobReset), With<Mob>>();
        q.iter(world)
            .filter(|(_, k, _)| k.zone == zone)
            .map(|(e, _, _)| e)
            .collect()
    };
    let item_targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &WorldKey, &FromObjectReset), With<Item>>();
        q.iter(world)
            .filter(|(_, k, _)| k.zone == zone)
            .map(|(e, _, _)| e)
            .collect()
    };
    let mob_count = mob_targets.len();
    let item_count = item_targets.len();
    for e in mob_targets {
        // Disengage anyone still locked onto this mob so combat
        // doesn't dangle a Fighting reference into the void.
        crate::commands::disengage_attackers_of(world, e);
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
    for e in item_targets {
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
    let admin_name = name_of(world, player);
    send_rendered(
        world,
        player,
        &format!(
            "Reset zone {zone}: cleared {mob_count} mob(s), {item_count} item(s). \
             Respawn fills on the next tick.\r\n"
        ),
    );
    info!(admin = %admin_name, zone, mob_count, item_count, "zreset");
}

pub(crate) fn cmd_advance(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "advance", args);
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: advance <player> <level>\r\n");
        return;
    }
    let target_word = parts[0];
    let Ok(target_level) = parts[1].parse::<i32>() else {
        send_to(world, player, "Level must be an integer.\r\n");
        return;
    };
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(target_word))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{target_word}' isn't online.\r\n"));
        return;
    };
    let current_level = world.get::<Profile>(target).map_or(0, |p| p.level);
    if target_level <= current_level {
        send_to(
            world,
            player,
            "advance only raises levels — use a delevel path for the inverse.\r\n",
        );
        return;
    }
    // Rank rules (see `authorize_level_change`): target level strictly below
    // the actor's, and the target must not already be at/above the actor.
    let target_name = name_of(world, target);
    if let Err(denied) =
        crate::combat::authorize_level_change(world, player, target, &target_name, target_level)
    {
        send_to(world, player, denied.message());
        return;
    }
    // Look up the XP threshold for the target level. If the level
    // table doesn't have it we refuse rather than silently no-op.
    let target_class = world.get::<Profile>(target).and_then(|p| p.class_id);
    let threshold = mud_world::exp_to_reach(world, target_class, target_level);
    let Some(threshold) = threshold else {
        send_to(
            world,
            player,
            format!("Level {target_level} isn't defined in the level table.\r\n"),
        );
        return;
    };
    if let Some(mut p) = world.get_mut::<Profile>(target) {
        p.experience = p.experience.max(threshold);
    }
    // Capped at the requested level so a large pre-existing XP total
    // can't carry the target past what the caller authorized.
    crate::combat::level_up_to(world, target, target_level);
    crate::combat::after_level_change(world, target, current_level);
    send_rendered(
        world,
        player,
        &format!("{target_name} advanced to level {target_level}.\r\n"),
    );
}

pub(crate) fn cmd_skillset(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "skillset", args);
    let parts: Vec<&str> = args.splitn(3, char::is_whitespace).collect();
    if parts.len() != 3 {
        send_to(
            world,
            player,
            "Usage: skillset <player> <ability> <proficiency>\r\n",
        );
        return;
    }
    let target_word = parts[0];
    let ability_word = parts[1].trim();
    let Ok(prof) = parts[2].trim().parse::<i32>() else {
        send_to(world, player, "Proficiency must be an integer.\r\n");
        return;
    };
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(target_word))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{target_word}' isn't online.\r\n"));
        return;
    };
    // Look up the ability id by name (case-insensitive). The
    // catalog keys on the lower-cased canonical name.
    let ability_id = world
        .resource::<mud_world::AbilityCatalog>()
        .by_name
        .get(&ability_word.to_ascii_lowercase())
        .map(|d| d.id);
    let Some(ability_id) = ability_id else {
        send_to(
            world,
            player,
            format!("No ability named '{ability_word}'.\r\n"),
        );
        return;
    };
    if world.get::<mud_world::KnownAbilities>(target).is_none()
        && let Ok(mut em) = world.get_entity_mut(target)
    {
        em.insert(mud_world::KnownAbilities::default());
    }
    if let Some(mut known) = world.get_mut::<mud_world::KnownAbilities>(target) {
        if let Some(entry) = known
            .entries
            .iter_mut()
            .find(|(id, _, _)| *id == ability_id)
        {
            entry.1 = prof;
            entry.2 = prof > 0;
        } else {
            known.entries.push((ability_id, prof, prof > 0));
        }
    }
    let target_name = name_of(world, target);
    send_rendered(
        world,
        player,
        &format!("Set {target_name}'s {ability_word} proficiency to {prof}.\r\n"),
    );
}

pub(crate) fn cmd_reroll(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "reroll", args);
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: reroll <player>\r\n");
        return;
    }
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(arg))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{arg}' isn't online.\r\n"));
        return;
    };
    let roll_3d6 =
        || rand::random_range(1..=6) + rand::random_range(1..=6) + rand::random_range(1..=6);
    let new_stats = mud_world::CoreStats {
        strength: roll_3d6(),
        dexterity: roll_3d6(),
        constitution: roll_3d6(),
        intelligence: roll_3d6(),
        wisdom: roll_3d6(),
        charisma: roll_3d6(),
    };
    if let Some(mut cs) = world.get_mut::<mud_world::CoreStats>(target) {
        *cs = new_stats;
    } else if let Ok(mut em) = world.get_entity_mut(target) {
        em.insert(new_stats);
    }
    let target_name = name_of(world, target);
    let line = format!(
        "Rerolled {target_name}: STR {} DEX {} CON {} INT {} WIS {} CHA {}.\r\n",
        new_stats.strength,
        new_stats.dexterity,
        new_stats.constitution,
        new_stats.intelligence,
        new_stats.wisdom,
        new_stats.charisma,
    );
    send_rendered(world, player, &line);
    if target != player {
        send_rendered(
            world,
            target,
            &format!(
                "Your stats were rerolled by an admin: STR {} DEX {} CON {} INT {} WIS {} CHA {}.\r\n",
                new_stats.strength,
                new_stats.dexterity,
                new_stats.constitution,
                new_stats.intelligence,
                new_stats.wisdom,
                new_stats.charisma,
            ),
        );
    }
}

pub(crate) fn cmd_mute(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "mute", args);
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: mute <player>\r\n");
        return;
    }
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(arg))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{arg}' isn't online.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "Muting yourself would be silly.\r\n");
        return;
    }
    let target_name = name_of(world, target);
    let admin_name = name_of(world, player);
    // Mute is `PlayerFlag::Muted` in the flags set — same array
    // that round-trips through `Characters.player_flags`, so the
    // sanction survives a reconnect for free. Insert the
    // component if missing so the toggle path always has a Vec
    // to flip.
    if world.get::<PlayerFlags>(target).is_none() {
        try_insert(world, target, PlayerFlags::default());
    }
    let now_muted = world
        .get_mut::<PlayerFlags>(target)
        .is_some_and(|mut pf| pf.toggle(mud_db::enums::PlayerFlag::Muted));
    if now_muted {
        send_rendered(
            world,
            player,
            &format!("You mute {target_name} on global channels.\r\n"),
        );
        send_rendered(
            world,
            target,
            "<red>Your voice has been muted by staff. Channels won't carry your words.</>\r\n",
        );
        info!(admin = %admin_name, target = %target_name, "mute set");
    } else {
        send_rendered(
            world,
            player,
            &format!("You restore {target_name}'s voice.\r\n"),
        );
        send_rendered(
            world,
            target,
            "<b:white>Your voice has been restored — channels are open again.</>\r\n",
        );
        info!(admin = %admin_name, target = %target_name, "mute cleared");
    }
}

pub(crate) fn cmd_last(world: &mut World, player: Entity, args: &str) {
    use crate::commands::{Connection, DbPool};
    record_admin_action(world, player, "last", args);
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: last <player>\r\n");
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        send_to(world, player, "Database unavailable.\r\n");
        return;
    };
    // Snapshot online status before we cut the World borrow loose.
    let online_now = {
        let mut q = world.query_filtered::<&Named, (With<Player>, With<Online>)>();
        q.iter(world).any(|n| n.name.eq_ignore_ascii_case(arg))
    };
    let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
    let target_name = arg.to_string();
    tokio::spawn(async move {
        let Some(out) = outbound else { return };
        let Ok(Some(row)) = mud_db::characters::find_by_name(&pool, &target_name).await else {
            let _ = out.try_send(format!("No character named '{target_name}'.\r\n").into_bytes());
            return;
        };
        let last = row
            .last_login
            .map_or_else(|| String::from("(never)"), |t| t.to_string());
        let online_label = if online_now { " — online now" } else { "" };
        let class_label = row
            .class_id
            .map_or_else(|| String::from("Classless"), |id| format!("class id {id}"));
        let line = format!(
            "{} (L{} {} / {})\r\n  last login: {last}{online_label}\r\n",
            row.name, row.level, row.race, class_label,
        );
        let _ = out.try_send(line.into_bytes());
    });
}

pub(crate) fn cmd_wizinvis(world: &mut World, player: Entity, args: &str) {
    use mud_world::WizInvis;
    record_admin_action(world, player, "wizinvis", args);
    let arg = args.trim();
    let own_level = world.get::<Profile>(player).map_or(0, |p| p.level);
    let current = world.get::<WizInvis>(player).map(|w| w.0);

    // Resolve the target invis level from the arg.
    let new_level: Option<i32> = if arg.is_empty() {
        // Toggle: if currently invis, clear; else go invis at own level.
        if current.is_some() {
            Some(0)
        } else {
            Some(own_level)
        }
    } else if arg.eq_ignore_ascii_case("off") || arg == "0" {
        Some(0)
    } else if let Ok(n) = arg.parse::<i32>() {
        if n < 0 {
            send_to(world, player, "Invis level can't be negative.\r\n");
            return;
        }
        if n > own_level {
            send_to(
                world,
                player,
                "You can't go invisible above your own level.\r\n",
            );
            return;
        }
        Some(n)
    } else {
        send_to(world, player, "Usage: wizinvis [<level> | off]\r\n");
        return;
    };

    let Some(level) = new_level else {
        return;
    };
    if level == 0 {
        try_remove::<WizInvis>(world, player);
        send_rendered(world, player, "<dim>You fade back into view.</>\r\n");
    } else {
        try_insert(world, player, WizInvis(level));
        send_rendered(
            world,
            player,
            &format!("<dim>You vanish from sight (invis level {level}).</>\r\n"),
        );
    }
}

pub(crate) fn cmd_snoop(world: &mut World, player: Entity, args: &str) {
    use mud_world::{SnoopedBy, Snooping};
    record_admin_action(world, player, "snoop", args);
    let arg = args.trim();
    // No arg → stop snooping.
    if arg.is_empty() {
        let target = world.get::<Snooping>(player).map(|s| s.0);
        if let Some(target) = target {
            try_remove::<Snooping>(world, player);
            try_remove::<SnoopedBy>(world, target);
            let target_name = name_or(world, target, "(gone)");
            send_to(
                world,
                player,
                format!("You stop snooping {target_name}.\r\n"),
            );
        } else {
            send_to(world, player, "You aren't snooping anyone.\r\n");
        }
        return;
    }

    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(arg))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{arg}' isn't online.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "Snooping yourself? Don't be silly.\r\n");
        return;
    }

    // Same-or-higher role refuses — staff can't be snooped by
    // their peers. Mirrors legacy GET_LEVEL gate.
    let target_role = world
        .get::<Account>(target)
        .map_or(UserRole::Player, |a| a.role);
    let admin_role = world
        .get::<Account>(player)
        .map_or(UserRole::Player, |a| a.role);
    if target_role.rank() >= admin_role.rank() {
        send_to(
            world,
            player,
            "You can't snoop someone of equal or higher rank.\r\n",
        );
        return;
    }

    // One snooper per target. Refuse if someone else is already
    // watching this target.
    if let Some(SnoopedBy(other)) = world.get::<SnoopedBy>(target).copied() {
        if other == player {
            // Re-snooping the same target → no-op confirm.
            send_to(world, player, "You're already snooping that player.\r\n");
            return;
        }
        send_to(
            world,
            player,
            "Someone is already snooping that player.\r\n",
        );
        return;
    }

    // Clear any prior snoop on this admin first.
    if let Some(prev) = world.get::<Snooping>(player).map(|s| s.0) {
        try_remove::<SnoopedBy>(world, prev);
    }

    try_insert(world, player, Snooping(target));
    try_insert(world, target, SnoopedBy(player));
    let target_name = name_of(world, target);
    send_to(
        world,
        player,
        format!("You begin snooping {target_name}.\r\n"),
    );
}

pub(crate) fn cmd_peace(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "peace", args);
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;
    // Snapshot every fighting entity in the room before mutating.
    let combatants: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located, &Fighting)>();
        q.iter(world)
            .filter(|(_, l, _)| l.0 == room)
            .map(|(e, _, _)| e)
            .collect()
    };
    if combatants.is_empty() {
        send_to(world, player, "No combat to interrupt here.\r\n");
        return;
    }
    let count = combatants.len();
    for entity in &combatants {
        try_remove::<Fighting>(world, *entity);
        // Drop any mob hate-list / memory entries so the brawl
        // doesn't pick right back up on the next tick. Players
        // don't carry those components so the call is a no-op
        // for them.
        try_remove::<crate::combat::MobMemory>(world, *entity);
        try_remove::<crate::combat::HateList>(world, *entity);
    }
    let suffix = if count == 1 { "" } else { "s" };
    let admin_name = name_of(world, player);
    send_rendered(
        world,
        player,
        &format!(
            "You quell the violence — {count} combatant{suffix} disengage{}.\r\n",
            if count == 1 { "s" } else { "" }
        ),
    );
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("<b:white>A calm settles over the room as {admin_name} commands peace.</>\r\n"),
    );
    info!(admin = %admin_name, count, "peace cleared combat");
}

pub(crate) fn cmd_unaffect(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "unaffect", args);
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: unaffect <target>\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let target = if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        player
    } else {
        let Some(t) = find_actor_in_room(world, arg, located.0, player) else {
            send_to(world, player, format!("No '{arg}' here.\r\n"));
            return;
        };
        t
    };
    let target_name = name_of(world, target);
    let removed = commands::remove_all_effects_on(world, target);
    if removed == 0 {
        send_to(
            world,
            player,
            format!(
                "{} has no active effects.\r\n",
                cap_sentence_start(&target_name)
            ),
        );
    } else {
        let suffix = if removed == 1 { "" } else { "s" };
        send_rendered(
            world,
            player,
            &format!("Stripped {removed} effect{suffix} from {target_name}.\r\n"),
        );
        if target != player {
            send_rendered(
                world,
                target,
                "<b:white>You feel cleansed; every effect drains away.</>\r\n",
            );
        }
    }
}

pub(crate) fn cmd_wizlock(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "wizlock", args);
    let arg = args.trim().to_ascii_lowercase();
    let current = world
        .get_resource::<mud_world::WizLock>()
        .is_some_and(|w| w.active);
    let new_state = match arg.as_str() {
        "on" => Some(true),
        "off" => Some(false),
        "" => None, // just report
        _ => {
            send_to(world, player, "Usage: wizlock [on|off]\r\n");
            return;
        }
    };
    if let Some(new_state) = new_state {
        if !world.contains_resource::<mud_world::WizLock>() {
            world.insert_resource(mud_world::WizLock::default());
        }
        world.resource_mut::<mud_world::WizLock>().active = new_state;
        let admin_name = name_of(world, player);
        let label = if new_state { "ON" } else { "OFF" };
        send_rendered(
            world,
            player,
            &format!("Wizlock is now <b:cyan>{label}</>.\r\n"),
        );
        info!(admin = %admin_name, state = label, "wizlock toggled");
    } else {
        let label = if current { "ON" } else { "OFF" };
        send_rendered(
            world,
            player,
            &format!("Wizlock is currently <b:cyan>{label}</>.\r\n"),
        );
    }
}

pub(crate) fn cmd_freeze(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "freeze", args);
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: freeze <player>\r\n");
        return;
    }
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(arg))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{arg}' isn't online.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "Freezing yourself would be unwise.\r\n");
        return;
    }
    let admin_name = name_of(world, player);
    let target_name = name_of(world, target);
    let was_frozen = world.get::<Frozen>(target).is_some();
    if was_frozen {
        try_remove::<Frozen>(world, target);
        send_rendered(world, player, &format!("You thaw {target_name}.\r\n"));
        send_rendered(
            world,
            target,
            &format!("{admin_name} thaws you. You can move again.\r\n"),
        );
        info!(admin = %admin_name, target = %target_name, action = "thaw", "freeze toggle");
    } else {
        try_insert(world, target, Frozen);
        send_rendered(world, player, &format!("You freeze {target_name}.\r\n"));
        send_to(
            world,
            target,
            format!("{admin_name} freezes you in place. You cannot act until thawed.\r\n"),
        );
        info!(admin = %admin_name, target = %target_name, action = "freeze", "freeze toggle");
    }
}
pub(crate) fn cmd_force(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "force", args);
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 || parts[1].trim().is_empty() {
        send_to(world, player, "Usage: force <player> <command>\r\n");
        return;
    }
    let target_word = parts[0].trim();
    let cmd_text = parts[1].trim();
    let target = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(target_word))
            .map(|(e, _)| e)
    };
    let Some(target) = target else {
        send_to(world, player, format!("'{target_word}' isn't online.\r\n"));
        return;
    };
    let admin_name = name_of(world, player);
    let target_name = name_of(world, target);

    // Rank rule (mirrors `authorize_level_change`): you can only force
    // someone strictly below your own character level. Self is exempt.
    // Without this, DevMode (every account holds `force`) would let a
    // level-5 player make a god act.
    if target != player {
        let actor_level = world.get::<Profile>(player).map_or(0, |p| p.level);
        let target_level = world.get::<Profile>(target).map_or(0, |p| p.level);
        if target_level >= actor_level {
            record_admin_action(world, player, "force_denied", args);
            send_to(
                world,
                player,
                format!("{target_name} outranks or equals you; you can't force them.\r\n"),
            );
            return;
        }
    }

    send_rendered(
        world,
        player,
        &format!("You force {target_name} to: {cmd_text}\r\n"),
    );
    send_rendered(
        world,
        target,
        &format!("{admin_name} forces you to: {cmd_text}\r\n"),
    );
    info!(
        admin = %admin_name,
        target = %target_name,
        command = %cmd_text,
        "force"
    );
    // The forced line runs as `target` but on `player`'s initiative:
    // staff commands are refused (see `commands::command_permitted`).
    commands::with_command_origin(commands::CommandOrigin::Forced, || {
        commands::dispatch(world, target, cmd_text);
    });
}
/// A `(zone, id)` prototype key.
type WorldKeyPair = (i32, i32);

/// `zone:id` -> a mob prototype key (selects a spawned instance).
fn parse_instance_key(needle: &str) -> Option<(i32, i32)> {
    let (zone, id) = needle.split_once(':')?;
    Some((zone.parse().ok()?, id.parse().ok()?))
}

/// Legacy `find_char_around_char(ch, find_vis_by_name(ch, name))`: a
/// character or mob `viewer` can see, by name / keywords, `N.name`, or
/// `zone:id` (a spawned instance of that mob prototype; `N.zone:id` picks
/// the Nth). Candidates in the viewer's own room come first, in `look`
/// order; then online players; then mobs by (prototype, spawn order).
fn find_actor_anywhere(world: &mut World, viewer: Entity, token: &str) -> Option<Entity> {
    let (index, needle) = commands::parse_indexed_needle(token);
    let key = parse_instance_key(needle);
    let needle = needle.to_ascii_lowercase();
    let viewer_room = world.get::<Located>(viewer).map(|l| l.0);
    let mut here: Vec<Entity> = Vec::new();
    let mut elsewhere: Vec<(bool, Option<WorldKeyPair>, Entity)> = Vec::new();
    {
        let mut q = world.query::<(
            Entity,
            &Named,
            Option<&Keywords>,
            &Located,
            Option<&WorldKey>,
            Has<Player>,
            Has<Online>,
        )>();
        for (e, n, kw, loc, wk, is_player, online) in q.iter(world) {
            let is_mob = world.get::<Mob>(e).is_some();
            if !(is_mob || (is_player && online)) {
                continue;
            }
            let hit = match key {
                Some(k) => is_mob && wk.is_some_and(|w| (w.zone, w.id) == k),
                None => matches(&needle, n, kw),
            };
            if !hit || !commands::can_see_player(world, viewer, e) {
                continue;
            }
            if Some(loc.0) == viewer_room {
                here.push(e);
            } else {
                elsewhere.push((is_mob, wk.map(|w| (w.zone, w.id)), e));
            }
        }
    }
    if let Some(room) = viewer_room {
        commands::sort_newest_first(world, room, &mut here, |e| *e);
        here.sort_by_key(|e| commands::mobs_before_players_key(world, *e));
    }
    elsewhere.sort_by_key(|(is_mob, k, e)| (*is_mob, *k, e.index_u32()));
    here.into_iter()
        .chain(elsewhere.into_iter().map(|(_, _, e)| e))
        .nth(index - 1)
}

/// Legacy `NOPERSON`.
const NO_SUCH_ACTOR: &str = "There is no one by that name here.\r\n";

/// Legacy rank guard on moving another character: staff may not move a
/// player of higher level (`transfer`: strictly higher; `teleport`: equal
/// or higher). Mobs are always fair game.
fn outranks_for_move(world: &World, staff: Entity, target: Entity, or_equal: bool) -> bool {
    if world.get::<Player>(target).is_none() {
        return false;
    }
    let level = |e: Entity| world.get::<Profile>(e).map_or(0, |p| p.level);
    let (mine, theirs) = (level(staff), level(target));
    if or_equal {
        theirs >= mine
    } else {
        theirs > mine
    }
}

/// Players in `room` other than the excluded ones (message recipients).
fn players_in_room_except(world: &mut World, room: Entity, except: &[Entity]) -> Vec<Entity> {
    let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
    q.iter(world)
        .filter(|(e, l)| l.0 == room && !except.contains(e))
        .map(|(e, _)| e)
        .collect()
}

/// Where `teleport <target> <where...>` sends someone (legacy
/// `find_target_room`): a room (`zone id`, `zone:id`, or a bare id in the
/// staff member's zone), or wherever a visible character / mob / object
/// lying in a room currently is.
fn resolve_teleport_destination(
    world: &mut World,
    staff: Entity,
    words: &[&str],
) -> Result<Entity, String> {
    let room_at = |world: &World, zone: i32, id: i32| {
        world
            .resource::<WorldKeyIndex>()
            .rooms
            .get(&(zone, id))
            .copied()
            .ok_or_else(|| format!("No room ({zone}, {id}).\r\n"))
    };
    let dest = match words {
        [z, i] if z.parse::<i32>().is_ok() && i.parse::<i32>().is_ok() => {
            room_at(world, z.parse().unwrap_or(0), i.parse().unwrap_or(0))?
        }
        [one] if parse_instance_key(one).is_some() => {
            // `zone:id` names a room here (a mob instance is a name).
            let (zone, id) = parse_instance_key(one).unwrap_or((0, 0));
            room_at(world, zone, id)?
        }
        [one, ..] if one.parse::<i32>().is_ok() => {
            let id: i32 = one.parse().unwrap_or(0);
            let zone = world
                .get::<Located>(staff)
                .and_then(|l| world.get::<WorldKey>(l.0))
                .map(|k| k.zone)
                .ok_or_else(|| "Can't resolve current zone.\r\n".to_string())?;
            world
                .resource::<WorldKeyIndex>()
                .rooms
                .get(&(zone, id))
                .copied()
                .ok_or_else(|| format!("No room {id} in zone {zone}.\r\n"))?
        }
        [name, ..] => {
            // Legacy: one word, a character/mob first, then an object.
            if let Some(actor) = find_actor_anywhere(world, staff, name) {
                world
                    .get::<Located>(actor)
                    .map(|l| l.0)
                    .ok_or_else(|| "That creature is nowhere.\r\n".to_string())?
            } else {
                let (index, needle) = commands::parse_indexed_needle(name);
                let needle = needle.to_ascii_lowercase();
                let mut q = world
                    .query_filtered::<(Entity, &Named, Option<&Keywords>, &Located), With<Item>>();
                let mut rooms: Vec<(Entity, Entity)> = q
                    .iter(world)
                    .filter(|(_, n, kw, _)| matches(&needle, n, *kw))
                    .map(|(e, _, _, l)| (e, l.0))
                    .collect();
                rooms.sort_by_key(|(e, _)| e.index_u32());
                let Some((_, holder)) = rooms.get(index - 1).copied() else {
                    return Err("No such creature or object around.\r\n".to_string());
                };
                if world.get::<mud_world::Room>(holder).is_none() {
                    return Err("That object is not available.\r\n".to_string());
                }
                holder
            }
        }
        [] => return Err("Where do you wish to send this person?\r\n".to_string()),
    };
    if !crate::room_access::entry_allowed(world, staff, dest) {
        return Err("You are not godly enough to use that room!\r\n".to_string());
    }
    Ok(dest)
}

/// Legacy `LVL_GRGOD` (102), the rank `transfer all` needs. The role ladder
/// cannot tell 101 from 102 (both `Builder`), so go by level, or by a
/// head-builder-or-above role.
fn may_transfer_all(world: &World, staff: Entity) -> bool {
    let level = world.get::<Profile>(staff).map_or(0, |p| p.level);
    let role_ok = world
        .get::<Account>(staff)
        .is_some_and(|a| a.role.at_least(mud_db::enums::UserRole::HeadBuilder));
    level >= 102 || role_ok
}

/// Move `target` (somewhere else) into `staff`'s room with the transfer
/// announcements. Shared by `transfer <name>` and `transfer all`.
fn transfer_to_staff_room(world: &mut World, staff: Entity, target: Entity, dest: Entity) {
    let Some(src_loc) = world.get::<Located>(target).copied() else {
        return;
    };
    let admin_name = name_of(world, staff);
    let target_name = cap_sentence_start(&name_of(world, target));

    // Source-room bystanders (everyone but the target).
    for b in players_in_room_except(world, src_loc.0, &[target]) {
        send_rendered(
            world,
            b,
            &format!("{target_name} vanishes in a puff of smoke.\r\n"),
        );
    }

    // Move the target (clears its fights).
    crate::combat::relocate(world, target, dest);

    // Destination-room bystanders (everyone but admin and the just-arrived target).
    for b in players_in_room_except(world, dest, &[staff, target]) {
        send_rendered(
            world,
            b,
            &format!("{target_name} appears, summoned by {admin_name}.\r\n"),
        );
    }

    send_rendered(world, staff, &format!("You summon {target_name}.\r\n"));
    send_rendered(world, target, &format!("{admin_name} summons you.\r\n"));
    if world.get::<Player>(target).is_some() {
        cmd_look(world, target, "");
    }
}

/// `transfer all` (legacy `do_trans`, "Trans All" branch): `LVL_GRGOD`+
/// only ("I think not." otherwise); brings every connected player other
/// than the caller whose level is strictly below the caller's.
fn cmd_transfer_all(world: &mut World, player: Entity) {
    if !may_transfer_all(world, player) {
        send_to(world, player, "I think not.\r\n");
        return;
    }
    let Some(dest) = world.get::<Located>(player).map(|l| l.0) else {
        send_to(world, player, "You are nowhere — can't transfer here.\r\n");
        return;
    };
    let my_level = world.get::<Profile>(player).map_or(0, |p| p.level);
    let victims: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(e, l)| *e != player && l.0 != dest)
            .map(|(e, _)| e)
            .collect()
    };
    for victim in victims {
        let level = world.get::<Profile>(victim).map_or(0, |p| p.level);
        if level >= my_level {
            continue;
        }
        transfer_to_staff_room(world, player, victim, dest);
    }
    send_to(world, player, "Ok.\r\n");
}

pub(crate) fn cmd_transfer(world: &mut World, player: Entity, args: &str) {
    let Some(first_arg) = args.split_whitespace().next() else {
        send_to(world, player, "Whom do you wish to transfer?\r\n");
        return;
    };
    if first_arg.eq_ignore_ascii_case("all") {
        cmd_transfer_all(world, player);
        return;
    }
    let Some(target) = find_actor_anywhere(world, player, first_arg) else {
        send_to(world, player, NO_SUCH_ACTOR);
        return;
    };
    if target == player {
        send_to(world, player, "That doesn't make much sense, does it?\r\n");
        return;
    }
    if outranks_for_move(world, player, target, false) {
        send_to(world, player, "Go transfer someone your own size.\r\n");
        return;
    }
    let Some(dest_loc) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere — can't transfer here.\r\n");
        return;
    };
    let Some(src_loc) = world.get::<Located>(target).copied() else {
        send_to(
            world,
            player,
            "They're nowhere; nothing to transfer from.\r\n",
        );
        return;
    };
    if src_loc.0 == dest_loc.0 {
        send_to(world, player, "They're already in your room.\r\n");
        return;
    }
    transfer_to_staff_room(world, player, target, dest_loc.0);
}
pub(crate) fn cmd_teleport(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let Some((target_word, dest_words)) = parts.split_first() else {
        send_to(world, player, "Whom do you wish to teleport?\r\n");
        return;
    };
    let Some(target) = find_actor_anywhere(world, player, target_word) else {
        send_to(world, player, NO_SUCH_ACTOR);
        return;
    };
    if target == player {
        send_to(world, player, "Use 'goto' to teleport yourself.\r\n");
        return;
    }
    if outranks_for_move(world, player, target, true) {
        send_to(world, player, "Maybe you shouldn't do that.\r\n");
        return;
    }
    let dest = match resolve_teleport_destination(world, player, dest_words) {
        Ok(d) => d,
        Err(msg) => {
            send_to(world, player, msg);
            return;
        }
    };
    let (zone, room_id) = world
        .get::<WorldKey>(dest)
        .map_or((-1, -1), |k| (k.zone, k.id));
    // NoTeleportRoom gate — `Room.allows_teleport = false` on the
    // *destination* refuses the teleport (legacy gates the target,
    // not the origin). Admin staff still honor it: builders clear
    // the flag explicitly when staging an admin destination.
    if world.get::<mud_world::NoTeleportRoom>(dest).is_some() {
        send_to(
            world,
            player,
            format!("Room ({zone}, {room_id}) refuses inbound teleports.\r\n"),
        );
        return;
    }
    let admin_name = name_of(world, player);
    let target_name = name_of(world, target);
    let Some(src_loc) = world.get::<Located>(target).copied() else {
        send_to(world, player, "Target is nowhere.\r\n");
        return;
    };
    if src_loc.0 == dest {
        send_to(world, player, "They're already there.\r\n");
        return;
    }
    let mount = world.get::<mud_world::Mounted>(target).map(|m| m.0);
    let target_capped = cap_sentence_start(&target_name);

    for b in players_in_room_except(world, src_loc.0, &[target]) {
        send_rendered(
            world,
            b,
            &format!("{target_capped} vanishes in a puff of smoke.\r\n"),
        );
    }

    crate::combat::relocate(world, target, dest);
    if let Some(mount) = mount {
        crate::combat::relocate(world, mount, dest);
    }

    for b in players_in_room_except(world, dest, &[target]) {
        send_rendered(
            world,
            b,
            &format!("{target_capped} arrives in a swirl of light.\r\n"),
        );
    }

    send_rendered(
        world,
        player,
        &format!("You teleport {target_name} to ({zone}, {room_id}).\r\n"),
    );
    send_rendered(
        world,
        target,
        &format!("{admin_name} teleports you elsewhere.\r\n"),
    );
    if world.get::<Player>(target).is_some() {
        cmd_look(world, target, "");
    }
}
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_goto(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let target: Option<Entity> = match parts.as_slice() {
        [] => {
            send_to(
                world,
                player,
                "Usage: goto <id> | goto <zone> <id> | goto <name> | goto home\r\n",
            );
            return;
        }
        [arg] if arg.eq_ignore_ascii_case("home") => {
            // Legacy do_goto: "home" is the staff member's own home room
            // (here the bound recall point, else the race start room).
            let Some(home) = crate::commands::recall::recall_room(world, player) else {
                send_to(world, player, "Your home room is invalid.\r\n");
                return;
            };
            Some(home)
        }
        [a, b] if a.parse::<i32>().is_ok() && b.parse::<i32>().is_ok() => {
            // `goto <zone> <id>` — composite key.
            let zone: i32 = a.parse().unwrap();
            let room_id: i32 = b.parse().unwrap();
            let entity = world
                .resource::<WorldKeyIndex>()
                .rooms
                .get(&(zone, room_id))
                .copied();
            if entity.is_none() {
                send_to(world, player, format!("No room ({zone}, {room_id}).\r\n"));
                return;
            }
            entity
        }
        [a] if a.parse::<i32>().is_ok() => {
            // `goto <id>` — room id in the player's current zone.
            // Falls through to a name lookup if the current zone
            // doesn't have a matching room (rare, but a name
            // collision like "999" deserves a useful error).
            let room_id: i32 = a.parse().unwrap();
            let here_zone = world
                .get::<Located>(player)
                .and_then(|l| world.get::<WorldKey>(l.0).map(|k| k.zone));
            let Some(zone) = here_zone else {
                send_to(world, player, "Can't resolve current zone.\r\n");
                return;
            };
            let entity = world
                .resource::<WorldKeyIndex>()
                .rooms
                .get(&(zone, room_id))
                .copied();
            if entity.is_none() {
                send_to(
                    world,
                    player,
                    format!("No room {room_id} in zone {zone}.\r\n"),
                );
                return;
            }
            entity
        }
        _ => {
            // Anything else: treat as a player or mob name. Players
            // first (online + named match), then any mob with a
            // matching Named/Keywords. Resolves to that actor's
            // current room.
            let needle = parts.join(" ");
            let needle_lc = needle.to_ascii_lowercase();
            let player_target: Option<Entity> = {
                let mut q = world.query_filtered::<
                    (Entity, &Named),
                    (With<mud_world::Player>, With<mud_world::Online>),
                >();
                q.iter(world)
                    .find(|(_, n)| n.name.eq_ignore_ascii_case(&needle))
                    .map(|(e, _)| e)
            };
            let mob_target: Option<Entity> = if player_target.is_some() {
                None
            } else {
                let mut q = world.query_filtered::<
                    (Entity, &Named, Option<&mud_world::Keywords>),
                    With<mud_world::Mob>,
                >();
                q.iter(world)
                    .find(|(_, n, kw)| {
                        n.name.to_ascii_lowercase().contains(&needle_lc)
                            || kw.is_some_and(|k| {
                                k.0.iter().any(|w| w.eq_ignore_ascii_case(&needle))
                            })
                    })
                    .map(|(e, _, _)| e)
            };
            let Some(actor) = player_target.or(mob_target) else {
                send_to(
                    world,
                    player,
                    format!("No one named '{needle}' here or anywhere.\r\n"),
                );
                return;
            };
            world.get::<Located>(actor).map(|l| l.0)
        }
    };

    let Some(target) = target else {
        send_to(world, player, "Couldn't resolve a destination.\r\n");
        return;
    };
    // Legacy do_goto has no no-teleport gate: it is a staff command and
    // `ROOM_NOTELEPORT` only constrains the teleport spells.
    // Legacy do_goto: below the god ranks a restricted (GODROOM) room is
    // off limits. Immortal+ bypass inside `entry_allowed`.
    if !crate::room_access::entry_allowed(world, player, target) {
        send_to(
            world,
            player,
            "You are not godly enough to use that room!\r\n",
        );
        return;
    }
    let origin = world.get::<Located>(player).map(|l| l.0);
    let (poof_in, poof_out) = world
        .get::<mud_world::Poofs>(player)
        .map(|p| (p.poof_in.clone(), p.poof_out.clone()))
        .unwrap_or_default();
    let name = name_of(world, player);
    let poof_line = |custom: Option<String>, default: &str| {
        custom
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| default.to_string())
            .replace("$n", &name)
    };
    if let Some(origin) = origin {
        let line = poof_line(poof_out, "$n disappears in a puff of smoke.");
        crate::commands::broadcast_room_visible(
            world,
            origin,
            player,
            &[player],
            &format!("{line}\r\n"),
        );
        mud_world::movement::move_to_room(world, player, target);
    }
    // Bring the mount along on goto / recall — otherwise the mount
    // is orphaned in the old room with a stale RiddenBy link.
    crate::combat::carry_mount(world, player, target);
    // Legacy do_goto also brings the staff member's pets (servant followers).
    let pets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &mud_world::Follower), With<mud_world::Mob>>();
        q.iter(world)
            .filter(|(_, f)| f.0 == player)
            .map(|(e, _)| e)
            .collect()
    };
    for pet in pets {
        if world.get::<Located>(pet).is_some() && crate::commands::attack_ok::is_servant(world, pet)
        {
            crate::combat::relocate(world, pet, target);
        }
    }
    if origin.is_some() {
        let line = poof_line(poof_in, "$n appears with an ear-splitting bang.");
        crate::commands::broadcast_room_visible(
            world,
            target,
            player,
            &[player],
            &format!("{line}\r\n"),
        );
    }
    cmd_look(world, player, "");
}

#[cfg(test)]
mod purge_tests {
    use super::*;
    use crate::commands::test_support::{drain, player_in};
    use mud_world::{Corpse, PlayerCorpseId, Room};

    fn corpse_with_item(world: &mut World, room: Entity, id: Option<i32>) -> (Entity, Entity) {
        let corpse = world
            .spawn((
                Item,
                Corpse,
                PlayerCorpse,
                Named {
                    name: "the corpse of Bob".into(),
                },
                Keywords(vec!["corpse".into(), "bob".into()]),
                Located(room),
            ))
            .id();
        if let Some(id) = id {
            world.entity_mut(corpse).insert(PlayerCorpseId(id));
        }
        let held = world
            .spawn((
                Item,
                Named {
                    name: "a sword".into(),
                },
                Keywords(vec!["sword".into()]),
                Located(corpse),
            ))
            .id();
        (corpse, held)
    }

    #[test]
    fn room_purge_leaves_player_corpses_alone_and_says_so() {
        let mut world = World::new();
        let room = world.spawn(Room).id();
        let (admin, mut rx) = player_in(&mut world, room);
        let (corpse, held) = corpse_with_item(&mut world, room, Some(3));
        let junk = world
            .spawn((
                Item,
                Named {
                    name: "a rock".into(),
                },
                Located(room),
            ))
            .id();

        cmd_purge(&mut world, admin, "");
        assert!(world.get_entity(corpse).is_ok());
        assert!(world.get_entity(held).is_ok());
        assert!(world.get_entity(junk).is_err(), "ordinary items still go");
        let out = drain(&mut rx);
        assert!(out.contains("1 player corpse(s) left alone"), "{out}");
    }

    #[test]
    fn explicit_purge_removes_a_settled_corpse_with_its_contents() {
        let mut world = World::new();
        let room = world.spawn(Room).id();
        let (admin, mut rx) = player_in(&mut world, room);
        let (corpse, held) = corpse_with_item(&mut world, room, Some(3));

        cmd_purge(&mut world, admin, "corpse");
        assert!(world.get_entity(corpse).is_err());
        assert!(world.get_entity(held).is_err(), "contents go with it");
        let out = drain(&mut rx);
        assert!(out.contains("You purge the corpse of Bob"), "{out}");
    }

    #[test]
    fn explicit_purge_refuses_an_unsettled_corpse() {
        let mut world = World::new();
        let room = world.spawn(Room).id();
        let (admin, mut rx) = player_in(&mut world, room);
        let (corpse, held) = corpse_with_item(&mut world, room, None);

        cmd_purge(&mut world, admin, "corpse");
        assert!(world.get_entity(corpse).is_ok());
        assert!(world.get_entity(held).is_ok());
        let out = drain(&mut rx);
        assert!(out.contains("still settling"), "{out}");
    }
}
