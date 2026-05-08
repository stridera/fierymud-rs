//! Clan-scoped commands: `ctell` (clan chat) and `clan` (info /
//! roster / motd / kick). Player-facing — admin clan management
//! (`cclan`) stays elsewhere.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, ClanMembership, Online, Player};

use crate::commands::{
    Category, Command, Connection, DbPool, Help, Prevent, effect_prevents, name_of,
    send_to,
};

inventory::submit! {
    Command {
        names: &["ctell", "ct"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "ctell <message>",
            summary: "Chat on your clan's private channel.",
            long: "Broadcasts to every online clan member sharing \
                   your `clan_id`. Refused for players not in a clan. \
                   Each line is rendered with the clan abbreviation \
                   prefix so cross-clan staff can tell the channels \
                   apart in `wiznet` cross-traffic.",
        },
        run: cmd_ctell,
    }
}

inventory::submit! {
    Command {
        names: &["clan"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "clan",
            summary: "Show your clan's info, MOTD, and roster.",
            long: "Prints clan name + abbreviation + MOTD plus the \
                   roster (sorted leader→officer→member, then by \
                   level desc, then name). Online members are \
                   marked with a `*`. Refused for players not in a \
                   clan. Subcommands: `clan motd <text>` (LEADER / \
                   OFFICER), `clan kick <player>` (LEADER).",
        },
        run: cmd_clan,
    }
}

fn cmd_ctell(world: &mut World, player: Entity, args: &str) {
    let message = args.trim();
    if message.is_empty() {
        send_to(world, player, "Clan-tell what?\r\n");
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let Some((clan_id, abbrev)) = world
        .get::<ClanMembership>(player)
        .map(|c| (c.clan_id, c.clan_abbrev.clone()))
    else {
        send_to(world, player, "You aren't in a clan.\r\n");
        return;
    };
    let player_name = name_of(world, player);
    let targets: Vec<Entity> = {
        let mut q = world
            .query_filtered::<(Entity, &ClanMembership), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(_, c)| c.clan_id == clan_id)
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        // Clan-tell channel tag in bold yellow (matches the
        // clan-abbreviation tag color already used on `who` and
        // the score sheet's clan line). Speaker name highlighted
        // bright yellow against the dimmer message body.
        let line = if t == player {
            format!(
                "<b:yellow>[{abbrev}]</> <b:white>You:</> {message}\r\n"
            )
        } else {
            format!(
                "<b:yellow>[{abbrev}]</> <b:white>{player_name}:</> {message}\r\n"
            )
        };
        send_to(world, t, line);
    }
}

#[allow(clippy::too_many_lines)]
fn cmd_clan(world: &mut World, player: Entity, args: &str) {
    let Some((clan_id, name, abbrev, rank)) = world
        .get::<ClanMembership>(player)
        .map(|c| {
            (
                c.clan_id,
                c.clan_name.clone(),
                c.clan_abbrev.clone(),
                c.rank.clone(),
            )
        })
    else {
        send_to(world, player, "You aren't in a clan.\r\n");
        return;
    };
    let trimmed = args.trim();
    if let Some(rest) = trimmed.strip_prefix("kick") {
        if rank.as_str() != "LEADER" {
            send_to(world, player, "Only the clan leader can kick.\r\n");
            return;
        }
        let target_name = rest.trim().to_string();
        if target_name.is_empty() {
            send_to(world, player, "Usage: clan kick <player>\r\n");
            return;
        }
        let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
            send_to(world, player, "Database unavailable.\r\n");
            return;
        };
        let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
        let Some(out) = outbound else { return };
        tokio::spawn(async move {
            let Ok(Some(target)) =
                mud_db::characters::find_by_name(&pool, &target_name).await
            else {
                let _ = out
                    .try_send(format!("No character named '{target_name}'.\r\n").into_bytes());
                return;
            };
            let in_clan = mud_db::clans::membership_for(&pool, &target.id)
                .await
                .ok()
                .flatten()
                .is_some_and(|m| m.clan_id == clan_id);
            if !in_clan {
                let _ = out
                    .try_send(format!("{} isn't in your clan.\r\n", target.name).into_bytes());
                return;
            }
            match mud_db::clans::remove_member(&pool, &target.id).await {
                Ok(_) => {
                    let _ = out.try_send(
                        format!("{} kicked from {}.\r\n", target.name, abbrev)
                            .into_bytes(),
                    );
                }
                Err(e) => {
                    let _ = out.try_send(format!("Kick failed: {e}\r\n").into_bytes());
                }
            }
        });
        return;
    }
    if let Some(rest) = trimmed.strip_prefix("motd") {
        if !matches!(rank.as_str(), "LEADER" | "OFFICER") {
            send_to(
                world,
                player,
                "Only clan leaders / officers can set the MOTD.\r\n",
            );
            return;
        }
        let body = rest.trim().to_string();
        let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
            send_to(world, player, "Database unavailable.\r\n");
            return;
        };
        let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
        let Some(out) = outbound else { return };
        let abbrev_for_msg = abbrev.clone();
        tokio::spawn(async move {
            let new_motd = if body.is_empty() {
                None
            } else {
                Some(body.as_str())
            };
            match mud_db::clans::set_motd(&pool, clan_id, new_motd).await {
                Ok(_) => {
                    let _ = out.try_send(
                        format!("MOTD updated on {abbrev_for_msg}.\r\n").into_bytes(),
                    );
                }
                Err(e) => {
                    let _ = out.try_send(format!("MOTD set failed: {e}\r\n").into_bytes());
                }
            }
        });
        return;
    }
    let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
    let pool = world.get_resource::<DbPool>().map(|p| p.0.clone());
    let online_cids: std::collections::HashSet<String> = {
        let mut q = world
            .query_filtered::<(&Account, &ClanMembership), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(_, c)| c.clan_id == clan_id)
            .map(|(a, _)| a.character_id.clone())
            .collect()
    };
    let (Some(out), Some(pool)) = (outbound, pool) else {
        return;
    };
    tokio::spawn(async move {
        let Ok(Some(clan_row)) = mud_db::clans::get_clan(&pool, clan_id).await else {
            let _ = out.try_send(b"Couldn't load clan info.\r\n".to_vec());
            return;
        };
        let roster = mud_db::clans::members_of(&pool, clan_id)
            .await
            .unwrap_or_default();
        let mut buf = format!("\r\n=== {name} [{abbrev}] ===\r\n");
        if let Some(motd) = clan_row.motd.as_deref()
            && !motd.trim().is_empty()
        {
            buf.push_str(&format!("MOTD: {}\r\n", motd.trim()));
        }
        buf.push_str(&format!(
            "Members ({}, {} online):\r\n",
            roster.len(),
            online_cids.len()
        ));
        for r in &roster {
            let online = if online_cids.contains(&r.character_id) {
                "*"
            } else {
                " "
            };
            buf.push_str(&format!(
                "  {online} [{:>7}] L{:>3} {}\r\n",
                r.rank, r.level, r.name
            ));
        }
        let _ = out.try_send(buf.into_bytes());
    });
}
