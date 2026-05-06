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
