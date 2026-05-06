//! Admin commands for world manipulation: movement (goto /
//! transfer / teleport / summon / where), state mutation
//! (freeze / slay / restore / apply / purge / force), and
//! prototype loading (load / loadobj / dumpworld). Command
//! records and bodies both live here.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use tracing::info;
use mud_world::{
    Account, AppliedTo, CombatStats, Description, EffectCatalog, EffectInstance,
    EffectSource, Fighting, Frozen, Health, Item, Keywords, Located, Mob, MobPrototypes,
    Named, ObjectPrototypes, Online, Player, Posture, PostureKind, Profile, Stamina, Wealth,
    WearableIn, WorldKey, WorldKeyIndex,
};

use crate::TickCount;
use crate::commands::{
    self, Category, Command, Help, broadcast_room_except_players_rendered, cmd_look,
    find_actor_in_room, matches, name_of, name_or, pad_visible, record_admin_action,
    send_rendered, send_to, try_insert, try_remove,
};

inventory::submit! {
    Command {
        names: &["where"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "where [<player> | all]",
            summary: "Show your location, a named player's location, or list all online.",
            long: "With no argument, prints your own current room \
                   (name, zone, id). With a player name, prints that \
                   online player's current room. `where all` (Builder+ \
                   only) lists every online player and where they are.",
        },
        run: cmd_where,
    }
}

inventory::submit! {
    Command {
        names: &["goto"],
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
                   \r\n\
                   Bypasses exits, doors, and movement gates.",
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
            usage: "transfer <player>",
            summary: "Pull an online player to your current room.",
            long: "Builder+ command. Looks up an online player by exact \
                   name (case-insensitive) and moves them to wherever \
                   you are.",
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
            usage: "teleport <player> <zone> <room>",
            summary: "Send an online player to a specific room.",
            long: "Builder+. Inverse of `transfer` (which pulls them \
                   to you) and `goto` (which moves you).",
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
                   and for running RP-as-NPC. `return` ends the \
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
            summary: "End a `switch` session and return to your own body.",
            long: "Builder+. Inverse of `switch`. Always types as \
                   the puppeteer (not the mob) — the dispatcher \
                   keeps `return` and `switch` as escape hatches \
                   so a stuck switch can always be undone.",
        },
        run: cmd_return,
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
                   `MobResets` / `ObjectResets` row in the named \
                   zone. The next respawn tick (~6s) refills the \
                   gaps. Admin-summoned / loadobj'd entities are \
                   preserved (no `FromMobReset` / `FromObjectReset` \
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
                   level decreases (those need a separate `delevel` \
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
            long: "Builder+. Adds or removes a `Muted` marker on the \
                   target. Muted players can't use gossip / shout / \
                   music / clan / quest channels — `say` and `tell` \
                   are unaffected so they can still play. Re-running \
                   `mute <name>` clears it.",
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
                   prints the last `last_login` timestamp, level, \
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
                   level. `wizinvis <level>` sets to that exact \
                   level (capped at your own). `wizinvis off` clears \
                   it. Players whose level is below yours (or \
                   below the explicit level) won't see you in \
                   `who` / `look` / `scan` listings.",
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
                   you with a dim `%` prefix). With no arg, stops \
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
            long: "Builder+. Removes the `Fighting` component from \
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
            long: "Builder+. Despawns every `EffectInstance` whose \
                   `AppliedTo` is the target. Reverses any modifier \
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
                   `wizlock on` blocks non-staff (UserRole < Builder) \
                   from completing login; `wizlock off` clears the \
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
                   `count` (default 1) instances Located on your room.",
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
            long: "Builder+. Same as `summon` for count=1, kept as a \
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
                   from `ObjectPrototypes` Located on your current \
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
            let mut q = world
                .query_filtered::<(&Named, &Located), (With<Player>, With<Online>)>();
            q.iter(world)
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

    // `where <name>` — locate one online player. Match is case-
    // insensitive against `Characters.name`. Offline characters
    // intentionally fall through to "isn't online" rather than
    // disclosing their last-known room.
    let needle = arg.to_ascii_lowercase();
    let target = {
        let mut q = world
            .query_filtered::<(Entity, &Named, &Located), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n, _)| n.name.eq_ignore_ascii_case(&needle))
            .map(|(e, _, l)| (e, l.0))
    };
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
        &format!("{target_name} is in: {room_name}  [{zone}:{id}]\r\n"),
    );
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
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if world.get::<Player>(target).is_some() {
        send_to(
            world,
            player,
            "Slaying players is not allowed. Use `restore` if they're in trouble.\r\n",
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
    send_rendered(world, player, &format!("{target_name} crumbles to dust at your gesture.\r\n"),
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
        let mut q = world.query_filtered::<
            (
                &Named,
                &Account,
                Option<&Profile>,
                &Located,
                Option<&Health>,
                Option<&Stamina>,
                Option<&Wealth>,
            ),
            (With<Player>, With<Online>),
        >();
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
        format!(
            "World dumped to {path} ({bytes} bytes, {player_count} player(s)).\r\n"
        ),
    );
    info!(path = %path, bytes, "dumpworld checkpoint written");
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
        // Cascade-despawn: anything Located on the target (mob's gear /
        // container contents) goes too.
        let nested: Vec<Entity> = {
            let mut q = world.query::<(Entity, &Located)>();
            q.iter(world).filter(|(_, l)| l.0 == target).map(|(e, _)| e).collect()
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
        q.iter(world).filter(|(_, l)| l.0 == room).map(|(e, _)| e).collect()
    };
    let items: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
        q.iter(world).filter(|(_, l)| l.0 == room).map(|(e, _)| e).collect()
    };
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
    for e in nested_of_mobs.into_iter().chain(mobs.into_iter()).chain(items.into_iter()) {
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
    send_to(
        world,
        player,
        format!(
            "Purged {mob_count} mob(s), {item_count} item(s), and {nested_count} nested.\r\n"
        ),
    );
}
pub(crate) fn cmd_restore(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let target = if arg.is_empty() || arg.eq_ignore_ascii_case("me")
        || arg.eq_ignore_ascii_case("self")
    {
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
    send_rendered(world, target, &format!("{admin_name} restores you. You feel completely refreshed.\r\n"),
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
    let target = if target_word.eq_ignore_ascii_case("me")
        || target_word.eq_ignore_ascii_case("self")
    {
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
        send_rendered(world, player, &format!("No '{target_word}' here.\r\n"),
        );
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
        send_to(
            world,
            player,
            "Usage: load <obj|mob> <zone> <id>\r\n",
        );
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
    if parts.len() != 2 {
        send_to(world, player, "Usage: loadobj <zone_id> <obj_id>\r\n");
        return;
    }
    let Ok(zone) = parts[0].parse::<i32>() else {
        send_to(world, player, "Invalid zone id.\r\n");
        return;
    };
    let Ok(obj_id) = parts[1].parse::<i32>() else {
        send_to(world, player, "Invalid object id.\r\n");
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
        Named { name: proto_name.clone() },
        Keywords(proto_keywords),
        WorldKey { zone: proto.zone_id, id: proto.id },
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
    let item = bundle.id();
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
        &format!(
            "Loaded {proto_name} (entity {item:?}) into your inventory.\r\n"
        ),
    );
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} produces {proto_name} from thin air.\r\n"),
    );
}
pub(crate) fn cmd_summon(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "summon", args);
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: summon <zone_id> <mob_id>\r\n");
        return;
    }
    let Ok(zone) = parts[0].parse::<i32>() else {
        send_to(world, player, "Invalid zone id.\r\n");
        return;
    };
    let Ok(mob_id) = parts[1].parse::<i32>() else {
        send_to(world, player, "Invalid mob id.\r\n");
        return;
    };

    let proto = world
        .resource::<MobPrototypes>()
        .by_key
        .get(&(zone, mob_id))
        .cloned();
    let Some(proto) = proto else {
        send_rendered(world, player, &format!("No mob prototype ({zone}, {mob_id}).\r\n"),
        );
        return;
    };

    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere; can't summon.\r\n");
        return;
    };
    let room = located.0;

    let hp = proto.rolled_hp();
    let dmg = proto.avg_damage();
    let proto_name = proto.name.clone();
    let proto_keywords = proto.keywords.clone();
    let proto_room_desc = proto.room_description.clone();
    let proto_examine_desc = proto.examine_description.clone();
    let proto_alignment = proto.alignment;
    let proto_hit_roll = proto.hit_roll;
    let proto_armor_class = proto.armor_class;
    let proto_ward_percent = proto.ward_percent;

    let mob_entity = world
        .spawn((
            Mob,
            Named { name: proto_name.clone() },
            Keywords(proto_keywords),
            Description(proto_room_desc),
            Located(room),
            Health { hp, max: hp },
            CombatStats {
                hit_roll: proto_hit_roll,
                dmg_roll: dmg,
                ac: proto_armor_class,
                alignment: proto_alignment,
                ward_pct: proto_ward_percent,
            },
            Posture(PostureKind::Standing),
        ))
        .id();
    if !proto_examine_desc.trim().is_empty()
        && let Ok(mut em) = world.get_entity_mut(mob_entity)
    {
        em.insert(mud_world::ExamineText(proto_examine_desc));
    }

    send_rendered(
        world,
        player,
        &format!(
            "Summoned {proto_name} (entity {mob_entity:?}) — HP {hp}, dmg {dmg}.\r\n"
        ),
    );
    let player_name = name_of(world, player);
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} summons {proto_name} from thin air.\r\n"),
    );
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
            "You're already controlling someone. Use `return` first.\r\n",
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
        &format!(
            "<dim>You slip into {mob_name}. Type `return` to come back.</>\r\n"
        ),
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
        let mut q = world
            .query_filtered::<(Entity, &WorldKey, &FromMobReset), With<Mob>>();
        q.iter(world)
            .filter(|(_, k, _)| k.zone == zone)
            .map(|(e, _, _)| e)
            .collect()
    };
    let item_targets: Vec<Entity> = {
        let mut q = world
            .query_filtered::<(Entity, &WorldKey, &FromObjectReset), With<Item>>();
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
    // Look up the XP threshold for the target level. If the level
    // table doesn't have it we refuse rather than silently no-op.
    let threshold = world
        .resource::<mud_world::LevelTable>()
        .clone_rows()
        .into_iter()
        .find(|r| r.level == target_level)
        .map(|r| r.exp_required);
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
    crate::combat::check_level_up(world, target);
    let target_name = name_of(world, target);
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
        if let Some(entry) = known.entries.iter_mut().find(|(id, _, _)| *id == ability_id) {
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
        &format!(
            "Set {target_name}'s {ability_word} proficiency to {prof}.\r\n"
        ),
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
    let roll_3d6 = || {
        rand::random_range(1..=6) + rand::random_range(1..=6) + rand::random_range(1..=6)
    };
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
            &format!("Your stats were rerolled by an admin: STR {} DEX {} CON {} INT {} WIS {} CHA {}.\r\n",
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
    use mud_world::Muted;
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
    let was_muted = world.get::<Muted>(target).is_some();
    if was_muted {
        try_remove::<Muted>(world, target);
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
    } else {
        try_insert(world, target, Muted);
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
            let _ = out.send(format!("No character named '{target_name}'.\r\n").into_bytes());
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
        let _ = out.send(line.into_bytes());
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
        if current.is_some() { Some(0) } else { Some(own_level) }
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
        send_rendered(
            world,
            player,
            "<dim>You fade back into view.</>\r\n",
        );
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
        &format!("You quell the violence — {count} combatant{suffix} disengage{}.\r\n",
            if count == 1 { "s" } else { "" }),
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
            format!("{target_name} has no active effects.\r\n"),
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
            format!(
                "{admin_name} freezes you in place. You cannot act until thawed.\r\n"
            ),
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

    send_rendered(world, player, &format!("You force {target_name} to: {cmd_text}\r\n"),
    );
    send_rendered(world, target, &format!("{admin_name} forces you to: {cmd_text}\r\n"),
    );
    info!(
        admin = %admin_name,
        target = %target_name,
        command = %cmd_text,
        "force"
    );
    commands::dispatch(world, target, cmd_text);
}
pub(crate) fn cmd_transfer(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: transfer <player>\r\n");
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
        send_to(world, player, "You're already with yourself.\r\n");
        return;
    }
    let Some(dest_loc) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere — can't transfer here.\r\n");
        return;
    };
    let Some(src_loc) = world.get::<Located>(target).copied() else {
        send_to(world, player, "They're nowhere; nothing to transfer from.\r\n");
        return;
    };
    if src_loc.0 == dest_loc.0 {
        send_to(world, player, "They're already in your room.\r\n");
        return;
    }

    let admin_name = name_of(world, player);
    let target_name = name_of(world, target);

    // Source-room bystanders (everyone but the target).
    let src_bystanders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != target && l.0 == src_loc.0)
            .map(|(e, _)| e)
            .collect()
    };
    for b in src_bystanders {
        send_rendered(world, b, &format!("{target_name} vanishes in a puff of smoke.\r\n"),
        );
    }

    // Move the target.
    if let Some(mut l) = world.get_mut::<Located>(target) {
        l.0 = dest_loc.0;
    }

    // Destination-room bystanders (everyone but admin and the just-arrived target).
    let dest_bystanders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != player && *e != target && l.0 == dest_loc.0)
            .map(|(e, _)| e)
            .collect()
    };
    for b in dest_bystanders {
        send_rendered(world, b, &format!("{target_name} appears, summoned by {admin_name}.\r\n"),
        );
    }

    send_rendered(world, player, &format!("You summon {target_name}.\r\n"));
    send_rendered(world, target, &format!("{admin_name} summons you.\r\n"),
    );
    cmd_look(world, target, "");
}
pub(crate) fn cmd_teleport(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 3 {
        send_to(
            world,
            player,
            "Usage: teleport <player> <zone> <room>\r\n",
        );
        return;
    }
    let target_word = parts[0];
    let Ok(zone) = parts[1].parse::<i32>() else {
        send_to(world, player, "Zone must be an integer.\r\n");
        return;
    };
    let Ok(room_id) = parts[2].parse::<i32>() else {
        send_to(world, player, "Room id must be an integer.\r\n");
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
    let dest = world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(zone, room_id))
        .copied();
    let Some(dest) = dest else {
        send_to(world, player, format!("No room ({zone}, {room_id}).\r\n"));
        return;
    };
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

    let src_bystanders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != target && l.0 == src_loc.0)
            .map(|(e, _)| e)
            .collect()
    };
    for b in src_bystanders {
        send_rendered(
            world,
            b,
            &format!("{target_name} vanishes in a puff of smoke.\r\n"),
        );
    }

    if let Some(mut l) = world.get_mut::<Located>(target) {
        l.0 = dest;
    }
    if let Some(mount) = mount
        && let Some(mut l) = world.get_mut::<Located>(mount)
    {
        l.0 = dest;
    }

    let dest_bystanders: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != target && l.0 == dest)
            .map(|(e, _)| e)
            .collect()
    };
    let target_capped = crate::commands::cap_sentence_start(&target_name);
    for b in dest_bystanders {
        send_rendered(world, b, &format!("{target_capped} arrives in a swirl of light.\r\n"));
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
    cmd_look(world, target, "");
}
pub(crate) fn cmd_goto(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let target: Option<Entity> = match parts.as_slice() {
        [] => {
            send_to(
                world,
                player,
                "Usage: goto <id> | goto <zone> <id> | goto <name>\r\n",
            );
            return;
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
                            || kw.is_some_and(|k| k.0.iter().any(|w| w.eq_ignore_ascii_case(&needle)))
                    })
                    .map(|(e, _, _)| e)
            };
            let Some(actor) = player_target.or(mob_target) else {
                send_to(world, player, format!("No one named '{needle}' here or anywhere.\r\n"));
                return;
            };
            world.get::<Located>(actor).map(|l| l.0)
        }
    };

    let Some(target) = target else {
        send_to(world, player, "Couldn't resolve a destination.\r\n");
        return;
    };
    let mount = world.get::<mud_world::Mounted>(player).map(|m| m.0);
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    }
    // Bring the mount along on goto / recall — otherwise the mount
    // is orphaned in the old room with a stale RiddenBy link.
    if let Some(mount) = mount
        && let Some(mut l) = world.get_mut::<Located>(mount)
    {
        l.0 = target;
    }
    cmd_look(world, player, "");
}
