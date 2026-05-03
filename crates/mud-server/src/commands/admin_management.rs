//! Admin commands for player / clan / housing / staff-note
//! management. Both the Command records AND the handler bodies
//! live here — `inventory::submit!` registers them and the
//! handlers reach into commands.rs only for shared helpers.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, Located, WorldKey};

use crate::commands::{
    AsyncCommand, Category, Command, Connection, DbPool, Help, cmd_mail_stub, name_of,
    record_admin_action, send_to, try_remove,
};

inventory::submit! {
    Command {
        names: &["ban"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "ban <player> <reason>",
            summary: "Ban a player's account.",
            long: "Implementor-only. Looks up the player by name, \
                   resolves the owning Users row, inserts a \
                   BanRecords row. The login flow refuses any of \
                   that account's characters until `unban` lifts.",
        },
        run: cmd_ban,
    }
}

inventory::submit! {
    Command {
        names: &["cclan"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "cclan create <name> <abbrev>\n       cclan assign <player> <abbrev> [rank]\n       cclan kick <player>\n       cclan motd <abbrev> <text>\n       cclan disband <abbrev>",
            summary: "Manage clans (create / assign / kick / MOTD).",
            long: "Implementor-only.\n\
                   - `create` opens a new clan; name + abbrev must \
                     both be unique.\n\
                   - `assign` puts a character into a clan, defaulting \
                     to MEMBER. Rank can be LEADER / OFFICER / MEMBER \
                     / APPLICANT.\n\
                   - `kick` removes their clan_member row entirely.\n\
                   - `motd` sets a clan's message-of-the-day; empty \
                     text clears it.",
        },
        run: cmd_cclan,
    }
}

inventory::submit! {
    Command {
        names: &["pnote", "playernote"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "pnote <player> [<text> | clear]",
            summary: "Read / append / clear staff notes on a character.",
            long: "Builder+. Without args after the name, prints the \
                   current staff notes. With text, appends a new line \
                   prefixed with the timestamp and your character name. \
                   `clear` is Implementor-only and wipes the entire log.",
        },
        run: cmd_pnote,
    }
}

inventory::submit! {
    Command {
        names: &["hinfo"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "hinfo <player>",
            summary: "Inspect any player's house from anywhere.",
            long: "Builder+. Loads the named player's PlayerHouse row \
                   plus rooms / item count / guest list and prints the \
                   summary. Doesn't require the target to be online.",
        },
        run: cmd_hinfo,
    }
}

inventory::submit! {
    Command {
        names: &["hgrant"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "hgrant <player>",
            summary: "Assign a fresh house to a player.",
            long: "Builder+. Creates a `PlayerHouse` row for the named \
                   character with the entrance set to the room you're \
                   currently standing in. Seeds one foyer room \
                   (`local_index = 0`). Fails if the character already \
                   owns a house — use `hrevoke` first.",
        },
        run: cmd_hgrant,
    }
}

inventory::submit! {
    Command {
        names: &["hrevoke"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "hrevoke <player>",
            summary: "Delete a player's house and all its rooms / items.",
            long: "Implementor-only. Deletes the named character's \
                   PlayerHouse row; FK cascades remove all rooms, \
                   exits, placed items, and guest entries. The owner's \
                   in-memory `HouseSummary` component remains until \
                   they reconnect — bounce them if they're online.",
        },
        run: cmd_hrevoke,
    }
}

inventory::submit! {
    Command {
        names: &["hgoto"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "hgoto <player>",
            summary: "Teleport into the named player's house foyer.",
            long: "Builder+. Looks up the target's PlayerHouse, \
                   synthesizes the ECS Room entities for it (if not \
                   yet cached in HousingIndex), then warps you to the \
                   foyer (`local_index = 0`). Works for offline \
                   owners — reads the snapshot straight from the DB. \
                   Routed through the async dispatcher; the sync stub \
                   only fires on dispatcher misconfig.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["treload"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "treload",
            summary: "Reload the trigger catalog from the database.",
            long: "Builder+. Re-queries the Triggers + attachment \
                   tables, atomically swaps the live `TriggerCatalog` \
                   resource, and re-stamps every Room's `AttachedTriggers`. \
                   Mob and object instances keep their current bindings — \
                   next respawn picks up catalog edits naturally. \
                   Mirrors the `/api/admin/triggers/reload` endpoint so \
                   builders can iterate without round-tripping through MCP. \
                   Routed through the async dispatcher; the sync stub \
                   only fires on dispatcher misconfig.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, args| match head {
            "hgoto" => Some(Box::pin(cmd_hgoto(world, player, pool, args))),
            "treload" => Some(Box::pin(cmd_treload(world, player, pool, args))),
            _ => None,
        },
    }
}

// ---- handler bodies ----

/// to find the owning `Users.id`, then inserts a `BanRecords` row.
/// Permanent ban (no duration today; a `[Nh|Nd]` argument can
/// land later).
pub(crate) fn cmd_ban(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "ban", args);
    let mut parts = args.splitn(2, char::is_whitespace);
    let target_name = parts.next().unwrap_or("").trim();
    let reason = parts.next().unwrap_or("").trim();
    if target_name.is_empty() || reason.is_empty() {
        send_to(world, player, "Usage: ban <player> <reason>\r\n");
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        send_to(world, player, "Database unavailable.\r\n");
        return;
    };
    let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
    let admin_uid = world
        .get::<Account>(player)
        .map(|a| a.user_id.clone());
    let target_name = target_name.to_string();
    let reason = reason.to_string();
    tokio::spawn(async move {
        let Some(out) = outbound else { return };
        let Some(admin_uid) = admin_uid else { return };
        let Ok(Some(target)) =
            mud_db::characters::find_by_name(&pool, &target_name).await
        else {
            let _ = out
                .send(format!("No character named '{target_name}'.\r\n").into_bytes());
            return;
        };
        let Some(uid) = target.user_id else {
            let _ = out.send(
                format!("{} has no associated user account.\r\n", target.name)
                    .into_bytes(),
            );
            return;
        };
        match mud_db::bans::ban(&pool, &uid, &admin_uid, &reason, None).await {
            Ok(id) => {
                let _ = out.send(
                    format!(
                        "Banned {} (user {uid}). Ban id: {id}\r\nReason: {reason}\r\n",
                        target.name
                    )
                    .into_bytes(),
                );
            }
            Err(e) => {
                let _ = out.send(format!("Ban write failed: {e}\r\n").into_bytes());
            }
        }
    });
}

// `unban` body lives in commands/unban.rs (inventory-distributed).

/// `cclan` admin dispatch: create / assign / kick / motd.
/// Implementor-only — gated upstream by `min_role`.
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_cclan(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "cclan", args);
    let mut parts = args.splitn(2, char::is_whitespace);
    let action = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest = parts.next().unwrap_or("").trim();
    if action.is_empty() {
        send_to(
            world,
            player,
            "Usage: cclan <create|assign|kick|motd|disband> ...\r\n",
        );
        return;
    }
    // `disband` walks online players in this clan and clears their
    // ClanMembership component before kicking off the async DB
    // delete — has to happen before the spawn since &mut World
    // can't cross the await.
    if action == "disband" {
        let abbrev_arg = rest.trim().to_string();
        if abbrev_arg.is_empty() {
            send_to(world, player, "Usage: cclan disband <abbrev>\r\n");
            return;
        }
        // Snapshot online entities matching the abbrev (case-
        // insensitive). The query borrows the world; collect the
        // entity list first, then mutate.
        let needle = abbrev_arg.to_ascii_lowercase();
        let mut q = world.query_filtered::<(Entity, &mud_world::ClanMembership), With<mud_world::Player>>();
        let online_in_clan: Vec<Entity> = q
            .iter(world)
            .filter(|(_, c)| c.clan_abbrev.eq_ignore_ascii_case(&needle))
            .map(|(e, _)| e)
            .collect();
        for e in online_in_clan {
            try_remove::<mud_world::ClanMembership>(world, e);
        }
        let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
            send_to(world, player, "Database unavailable.\r\n");
            return;
        };
        let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
        tokio::spawn(async move {
            let Some(out) = outbound else { return };
            let Ok(Some(clan)) = mud_db::clans::get_by_abbrev(&pool, &abbrev_arg).await else {
                let _ = out.send(
                    format!("No clan with abbrev '{abbrev_arg}'.\r\n").into_bytes(),
                );
                return;
            };
            match mud_db::clans::delete_clan(&pool, clan.id).await {
                Ok(removed) => {
                    let _ = out.send(
                        format!(
                            "Disbanded {} [{}] ({} member rows cleared).\r\n",
                            clan.name, clan.abbrev, removed,
                        )
                        .into_bytes(),
                    );
                }
                Err(e) => {
                    let _ = out.send(format!("Disband failed: {e}\r\n").into_bytes());
                }
            }
        });
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        send_to(world, player, "Database unavailable.\r\n");
        return;
    };
    let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
    let rest = rest.to_string();
    tokio::spawn(async move {
        let Some(out) = outbound else { return };
        match action.as_str() {
            "create" => {
                let mut tokens = rest.split_whitespace();
                let abbrev = tokens.next_back();
                let name: String = tokens.collect::<Vec<_>>().join(" ");
                let (Some(abbrev), false) = (abbrev, name.is_empty()) else {
                    let _ = out.send(b"Usage: cclan create <name> <abbrev>\r\n".to_vec());
                    return;
                };
                match mud_db::clans::create_clan(&pool, &name, abbrev).await {
                    Ok(id) => {
                        let _ = out.send(
                            format!("Clan #{id} '{name}' [{abbrev}] created.\r\n")
                                .into_bytes(),
                        );
                    }
                    Err(e) => {
                        let _ = out
                            .send(format!("Couldn't create clan: {e}\r\n").into_bytes());
                    }
                }
            }
            "assign" => {
                let mut tokens = rest.split_whitespace();
                let player_name = tokens.next();
                let abbrev = tokens.next();
                let rank = tokens.next().unwrap_or("MEMBER").to_ascii_uppercase();
                let (Some(player_name), Some(abbrev)) = (player_name, abbrev) else {
                    let _ = out.send(
                        b"Usage: cclan assign <player> <abbrev> [rank]\r\n".to_vec(),
                    );
                    return;
                };
                if !matches!(rank.as_str(), "LEADER" | "OFFICER" | "MEMBER" | "APPLICANT")
                {
                    let _ = out.send(
                        b"Rank must be LEADER, OFFICER, MEMBER, or APPLICANT.\r\n".to_vec(),
                    );
                    return;
                }
                let Ok(Some(target)) =
                    mud_db::characters::find_by_name(&pool, player_name).await
                else {
                    let _ = out
                        .send(format!("No character named '{player_name}'.\r\n").into_bytes());
                    return;
                };
                let Ok(Some(clan)) = mud_db::clans::get_by_abbrev(&pool, abbrev).await else {
                    let _ = out
                        .send(format!("No clan with abbrev '{abbrev}'.\r\n").into_bytes());
                    return;
                };
                if let Err(e) =
                    mud_db::clans::assign_member(&pool, &target.id, clan.id, &rank).await
                {
                    let _ = out.send(format!("Assign failed: {e}\r\n").into_bytes());
                } else {
                    let _ = out.send(
                        format!("{} → {} as {rank}.\r\n", target.name, clan.abbrev)
                            .into_bytes(),
                    );
                }
            }
            "kick" => {
                let player_name = rest.trim();
                if player_name.is_empty() {
                    let _ = out.send(b"Usage: cclan kick <player>\r\n".to_vec());
                    return;
                }
                let Ok(Some(target)) =
                    mud_db::characters::find_by_name(&pool, player_name).await
                else {
                    let _ = out
                        .send(format!("No character named '{player_name}'.\r\n").into_bytes());
                    return;
                };
                match mud_db::clans::remove_member(&pool, &target.id).await {
                    Ok(0) => {
                        let _ = out
                            .send(format!("{} isn't in any clan.\r\n", target.name).into_bytes());
                    }
                    Ok(_) => {
                        let _ = out
                            .send(format!("{} kicked from clan.\r\n", target.name).into_bytes());
                    }
                    Err(e) => {
                        let _ = out.send(format!("Kick failed: {e}\r\n").into_bytes());
                    }
                }
            }
            "motd" => {
                let mut tokens = rest.splitn(2, char::is_whitespace);
                let abbrev = tokens.next();
                let body = tokens.next().unwrap_or("").trim();
                let Some(abbrev) = abbrev else {
                    let _ = out
                        .send(b"Usage: cclan motd <abbrev> <text>\r\n".to_vec());
                    return;
                };
                let Ok(Some(clan)) = mud_db::clans::get_by_abbrev(&pool, abbrev).await else {
                    let _ = out
                        .send(format!("No clan with abbrev '{abbrev}'.\r\n").into_bytes());
                    return;
                };
                let new_motd = if body.is_empty() { None } else { Some(body) };
                if let Err(e) = mud_db::clans::set_motd(&pool, clan.id, new_motd).await {
                    let _ = out.send(format!("MOTD set failed: {e}\r\n").into_bytes());
                } else {
                    let _ = out.send(
                        format!("MOTD updated on {}.\r\n", clan.abbrev).into_bytes(),
                    );
                }
            }
            other => {
                let _ = out.send(
                    format!("Unknown cclan action '{other}'.\r\n").into_bytes(),
                );
            }
        }
    });
}

/// `pnote <player> [<text>|clear]` — staff annotations on a
/// character. Single shared blob on `Characters.staff_notes`;
/// appends prefix each new note with timestamp + author name.
pub(crate) fn cmd_pnote(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "pnote", args);
    let mut parts = args.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    let body = parts.next().unwrap_or("").trim();
    if name.is_empty() {
        send_to(world, player, "Usage: pnote <player> [<text> | clear]\r\n");
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        send_to(world, player, "Database unavailable.\r\n");
        return;
    };
    let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
    let actor_role = world
        .get::<Account>(player)
        .map_or(UserRole::Player, |a| a.role);
    let actor_name = name_of(world, player);
    let name = name.to_string();
    let body = body.to_string();
    tokio::spawn(async move {
        let Some(out) = outbound else { return };
        let target = match mud_db::characters::find_by_name(&pool, &name).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                let _ = out.send(format!("No character named '{name}'.\r\n").into_bytes());
                return;
            }
            Err(e) => {
                let _ = out.send(format!("DB error: {e}\r\n").into_bytes());
                return;
            }
        };
        if body.is_empty() {
            // Read mode.
            match mud_db::characters::load_staff_notes(&pool, &target.id).await {
                Ok(Some(notes)) if !notes.is_empty() => {
                    let _ = out.send(
                        format!(
                            "\r\n=== Staff notes for {} ===\r\n{notes}\r\n",
                            target.name
                        )
                        .into_bytes(),
                    );
                }
                Ok(_) => {
                    let _ = out.send(
                        format!("No staff notes on {}.\r\n", target.name).into_bytes(),
                    );
                }
                Err(e) => {
                    let _ = out.send(format!("DB error: {e}\r\n").into_bytes());
                }
            }
            return;
        }
        if body.eq_ignore_ascii_case("clear") {
            if actor_role.rank() < UserRole::Implementor.rank() {
                let _ = out.send(b"`pnote ... clear` is Implementor-only.\r\n".to_vec());
                return;
            }
            match mud_db::characters::save_staff_notes(&pool, &target.id, "").await {
                Ok(()) => {
                    let _ = out.send(
                        format!("Cleared staff notes on {}.\r\n", target.name)
                            .into_bytes(),
                    );
                }
                Err(e) => {
                    let _ = out.send(format!("DB error: {e}\r\n").into_bytes());
                }
            }
            return;
        }
        // Append mode.
        let existing = mud_db::characters::load_staff_notes(&pool, &target.id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M");
        let line = format!("[{now}] {actor_name}: {body}");
        let new_blob = if existing.trim().is_empty() {
            line
        } else {
            format!("{existing}\n{line}")
        };
        match mud_db::characters::save_staff_notes(&pool, &target.id, &new_blob).await {
            Ok(()) => {
                let _ = out.send(
                    format!("Note added to {}.\r\n", target.name).into_bytes(),
                );
            }
            Err(e) => {
                let _ = out.send(format!("DB error: {e}\r\n").into_bytes());
            }
        }
    });
}

/// `hgrant <player>` — admin command. Creates a fresh `PlayerHouse`
/// for the named character, with the entrance set to the admin's
/// current room. Refuses (via DB unique constraint) if the
/// character already owns one.
pub(crate) fn cmd_hgrant(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "hgrant", args);
    let name = args.trim();
    if name.is_empty() {
        send_to(world, player, "Usage: hgrant <player>\r\n");
        return;
    }
    let entrance = world
        .get::<Located>(player)
        .and_then(|l| world.get::<WorldKey>(l.0).copied());
    let Some(entrance) = entrance else {
        send_to(
            world,
            player,
            "Stand in the room you want to use as the house entrance.\r\n",
        );
        return;
    };
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        send_to(world, player, "Database unavailable.\r\n");
        return;
    };
    let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
    let name = name.to_string();
    tokio::spawn(async move {
        let Some(out) = outbound else { return };
        let target = match mud_db::characters::find_by_name(&pool, &name).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                let _ = out.send(format!("No character named '{name}'.\r\n").into_bytes());
                return;
            }
            Err(e) => {
                let _ = out.send(format!("DB error: {e}\r\n").into_bytes());
                return;
            }
        };
        match mud_db::housing::create_house(&pool, &target.id, entrance.zone, entrance.id).await {
            Ok((house_id, foyer_id)) => {
                let _ = out.send(
                    format!(
                        "House #{house_id} created for {} (foyer room id {foyer_id}, entrance ({}, {})).\r\n",
                        target.name, entrance.zone, entrance.id
                    )
                    .into_bytes(),
                );
            }
            Err(e) => {
                let _ = out.send(
                    format!("Couldn't create house: {e}\r\n").into_bytes(),
                );
            }
        }
    });
}

/// `hrevoke <player>` — admin command. Deletes the named
/// character's `PlayerHouse` row; FK cascades clean up every
/// dependent table.
pub(crate) fn cmd_hrevoke(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "hrevoke", args);
    let name = args.trim();
    if name.is_empty() {
        send_to(world, player, "Usage: hrevoke <player>\r\n");
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        send_to(world, player, "Database unavailable.\r\n");
        return;
    };
    let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
    let name = name.to_string();
    tokio::spawn(async move {
        let Some(out) = outbound else { return };
        let target = match mud_db::characters::find_by_name(&pool, &name).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                let _ = out.send(format!("No character named '{name}'.\r\n").into_bytes());
                return;
            }
            Err(e) => {
                let _ = out.send(format!("DB error: {e}\r\n").into_bytes());
                return;
            }
        };
        let house = match mud_db::housing::for_character(&pool, &target.id).await {
            Ok(Some(h)) => h,
            Ok(None) => {
                let _ = out.send(
                    format!("{} doesn't own a house — nothing to revoke.\r\n", target.name)
                        .into_bytes(),
                );
                return;
            }
            Err(e) => {
                let _ = out.send(format!("DB error: {e}\r\n").into_bytes());
                return;
            }
        };
        match mud_db::housing::delete_house(&pool, house.id).await {
            Ok(0) => {
                let _ = out.send(b"House row vanished mid-call.\r\n".to_vec());
            }
            Ok(_) => {
                let _ = out.send(
                    format!(
                        "House #{} ({}'s) deleted; cascade cleared rooms / items / guests.\r\n",
                        house.id, target.name
                    )
                    .into_bytes(),
                );
            }
            Err(e) => {
                let _ = out.send(format!("Delete failed: {e}\r\n").into_bytes());
            }
        }
    });
}

/// `hinfo <player>` — admin command. Loads the named player's
/// `PlayerHouse` (plus rooms / items / guests) from the DB and
/// renders a summary. Doesn't require the target to be online.
pub(crate) fn cmd_hinfo(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "hinfo", args);
    let name = args.trim();
    if name.is_empty() {
        send_to(world, player, "Usage: hinfo <player>\r\n");
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        send_to(world, player, "Database unavailable.\r\n");
        return;
    };
    let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
    let name = name.to_string();
    tokio::spawn(async move {
        let Some(out) = outbound else { return };
        let target = match mud_db::characters::find_by_name(&pool, &name).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                let _ = out.send(format!("No character named '{name}'.\r\n").into_bytes());
                return;
            }
            Err(e) => {
                let _ = out.send(format!("DB error: {e}\r\n").into_bytes());
                return;
            }
        };
        let house = match mud_db::housing::for_character(&pool, &target.id).await {
            Ok(Some(h)) => h,
            Ok(None) => {
                let _ = out.send(
                    format!("{} doesn't own a house.\r\n", target.name).into_bytes(),
                );
                return;
            }
            Err(e) => {
                let _ = out.send(format!("DB error: {e}\r\n").into_bytes());
                return;
            }
        };
        let rooms = mud_db::housing::rooms_for_house(&pool, house.id).await.unwrap_or_default();
        let items = mud_db::housing::items_for_house(&pool, house.id).await.unwrap_or_default();
        let guests = mud_db::housing::guests_for_house(&pool, house.id).await.unwrap_or_default();
        let mut buf = String::new();
        buf.push_str(&format!("\r\n=== House #{} ({}) ===\r\n", house.id, target.name));
        buf.push_str(&format!(
            "Entrance: zone {} room {}\r\n",
            house.entrance_room_zone_id, house.entrance_room_id
        ));
        if let (Some(rz), Some(ri)) = (house.return_room_zone_id, house.return_room_id) {
            buf.push_str(&format!("Return-on-exit: zone {rz} room {ri}\r\n"));
        }
        buf.push_str(&format!("Rooms: {}\r\n", rooms.len()));
        for r in &rooms {
            let item_count = items.iter().filter(|i| i.room_id == r.id).count();
            buf.push_str(&format!(
                "  [{:>2}] {} ({} item(s), capacity {}{})\r\n",
                r.local_index,
                r.name,
                item_count,
                r.capacity,
                if r.is_peaceful { ", peaceful" } else { "" },
            ));
        }
        buf.push_str(&format!("Items placed: {}\r\n", items.len()));
        buf.push_str(&format!("Guests: {}\r\n", guests.len()));
        for g in &guests {
            buf.push_str(&format!(
                "  {} ({})\r\n",
                g.character_id,
                if g.can_place { "can place items" } else { "visit only" },
            ));
        }
        let _ = out.send(buf.into_bytes());
    });
}

/// `hgoto <player>` — teleport the admin into the named player's
/// house foyer (`local_index = 0`). Loads the house snapshot
/// straight from the DB so it works for offline owners; reuses
/// the same `HousingIndex` cache + `synthesize_house_rooms` path
/// `cmd_home` does, so subsequent admin / owner enters land in
/// the same Room entities. Async to keep the multi-table fetch
/// off the world tick; mutations apply between awaits via the
/// shared `&mut World`.
#[allow(clippy::too_many_lines)]
pub(crate) async fn cmd_hgoto(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    record_admin_action(world, player, "hgoto", args);
    let name = args.trim();
    if name.is_empty() {
        send_to(world, player, "Usage: hgoto <player>\r\n");
        return;
    }
    let target = match mud_db::characters::find_by_name(pool, name).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            send_to(world, player, format!("No character named '{name}'.\r\n"));
            return;
        }
        Err(e) => {
            send_to(world, player, format!("DB error: {e}\r\n"));
            return;
        }
    };
    let house = match mud_db::housing::for_character(pool, &target.id).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            send_to(
                world,
                player,
                format!("{} doesn't own a house.\r\n", target.name),
            );
            return;
        }
        Err(e) => {
            send_to(world, player, format!("DB error: {e}\r\n"));
            return;
        }
    };
    let rooms = mud_db::housing::rooms_for_house(pool, house.id)
        .await
        .unwrap_or_default();
    if rooms.is_empty() {
        send_to(
            world,
            player,
            format!("{}'s house has no rooms.\r\n", target.name),
        );
        return;
    }
    let exits = mud_db::housing::exits_for_house(pool, house.id)
        .await
        .unwrap_or_default();
    let items = mud_db::housing::items_for_house(pool, house.id)
        .await
        .unwrap_or_default();
    let guests = mud_db::housing::guests_for_house(pool, house.id)
        .await
        .unwrap_or_default();
    let summary = mud_world::HouseSummary {
        house_id: house.id,
        entrance_room: mud_world::WorldKey {
            zone: house.entrance_room_zone_id,
            id: house.entrance_room_id,
        },
        return_room: match (house.return_room_zone_id, house.return_room_id) {
            (Some(z), Some(i)) => Some(mud_world::WorldKey { zone: z, id: i }),
            _ => None,
        },
        rooms: rooms
            .into_iter()
            .map(|r| mud_world::HouseRoomEntry {
                id: r.id,
                local_index: r.local_index,
                name: r.name,
                description: r.description,
                is_peaceful: r.is_peaceful,
                capacity: r.capacity,
            })
            .collect(),
        exits: exits
            .into_iter()
            .map(|e| mud_world::HouseExitEntry {
                from_room_id: e.from_room_id,
                to_room_id: e.to_room_id,
                direction: e.direction,
            })
            .collect(),
        items: items
            .into_iter()
            .map(|i| mud_world::HouseItemEntry {
                id: i.id,
                room_id: i.room_id,
                object_zone_id: i.object_zone_id,
                object_id: i.object_id,
            })
            .collect(),
        guests: guests
            .into_iter()
            .map(|g| mud_world::HouseGuestEntry {
                character_id: g.character_id,
                can_place: g.can_place,
            })
            .collect(),
    };
    // Synthesize rooms once per house, gated by the index so a
    // repeat hgoto on the same house doesn't double-spawn.
    let foyer_local = summary
        .rooms
        .iter()
        .find(|r| r.local_index == 0)
        .map_or(summary.rooms[0].local_index, |r| r.local_index);
    let already_spawned = world
        .resource::<mud_world::HousingIndex>()
        .by_key
        .contains_key(&(summary.house_id, foyer_local));
    if !already_spawned {
        crate::commands::synthesize_house_rooms(world, &summary);
    }
    let foyer = world
        .resource::<mud_world::HousingIndex>()
        .by_key
        .get(&(summary.house_id, foyer_local))
        .copied();
    let Some(foyer_entity) = foyer else {
        send_to(world, player, "Couldn't resolve the house foyer.\r\n");
        return;
    };
    if let Some(mut l) = world.get_mut::<mud_world::Located>(player) {
        l.0 = foyer_entity;
    }
    send_to(
        world,
        player,
        format!("You step into {}'s house.\r\n", target.name),
    );
    crate::commands::cmd_look(world, player, "");
}

/// `treload` — Re-pull the trigger catalog from the database and
/// hot-swap it into the live `TriggerCatalog` resource. Async so
/// the DB round-trip doesn't stall the world tick. Reuses the
/// shared `triggers::apply_reloaded_catalog` helper that the
/// admin HTTP endpoint also calls.
pub(crate) async fn cmd_treload(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    record_admin_action(world, player, "treload", args);
    let new = match mud_world::load_trigger_catalog(pool).await {
        Ok(c) => c,
        Err(e) => {
            send_to(world, player, format!("Trigger reload failed: {e}\r\n"));
            return;
        }
    };
    let stats = crate::triggers::apply_reloaded_catalog(world, new);
    send_to(
        world,
        player,
        format!(
            "Trigger catalog reloaded: {} rows ({} mob, {} object, {} room \
             attachment groups; {} rooms now carry triggers).\r\n\
             Mob/object instance attachments unchanged — next respawn \
             picks up catalog edits.\r\n",
            stats.total,
            stats.mob_links,
            stats.object_links,
            stats.room_links,
            stats.rooms_with_triggers,
        ),
    );
}
