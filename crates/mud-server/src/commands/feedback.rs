//! Player feedback channels — `bug` / `idea` / `typo` and
//! `petition`. Each writes a `Reports` row (or broadcasts to
//! immortals, in petition's case) so staff has a queryable
//! audit trail beyond the live tracing logs.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, Located, Named, Online, Player, WorldKey};
use tracing::info;

use crate::commands::{
    Category, Command, DbPool, Help, Prevent, effect_prevents, name_of, send_to,
};

inventory::submit! {
    Command {
        names: &["bug"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "bug <message>",
            summary: "Report a bug to the staff.",
            long: "Logs a feedback row to the `Reports` table. Body \
                   captures your name + character_id + room + the \
                   text. Empty messages refuse so we don't fill the \
                   table with noise.",
        },
        run: cmd_bug,
    }
}

inventory::submit! {
    Command {
        names: &["idea"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "idea <message>",
            summary: "Suggest a feature / change to the staff.",
            long: "Same as `bug` but tagged Idea. Same `Reports` \
                   row shape; `report list <kind>` filters.",
        },
        run: cmd_idea,
    }
}

inventory::submit! {
    Command {
        names: &["typo"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "typo <message>",
            summary: "Report a typo / wording issue.",
            long: "Same as `bug` but tagged Typo. Useful for builders \
                   when reviewing room descriptions.",
        },
        run: cmd_typo,
    }
}

inventory::submit! {
    Command {
        names: &["petition"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "petition <message>  |  petition <player> <message>",
            summary: "Send to immortals — or, as immortal, reply privately.",
            long: "From a mortal: broadcasts the petition to every \
                   online Immortal+. From an immortal: if the first \
                   arg matches an online player name, reply to that \
                   player privately (the legacy `ptell` form folded \
                   in). Mortals never see anyone else's petitions.",
        },
        run: cmd_petition,
    }
}

fn cmd_bug(world: &mut World, player: Entity, args: &str) {
    submit_feedback(world, player, "bug", args);
}

fn cmd_idea(world: &mut World, player: Entity, args: &str) {
    submit_feedback(world, player, "idea", args);
}

fn cmd_typo(world: &mut World, player: Entity, args: &str) {
    submit_feedback(world, player, "typo", args);
}

/// Log a player feedback report (bug/idea/typo) to the tracing
/// pipeline AND insert into the `Reports` table. The DB write is
/// fire-and-forget via `tokio::spawn` so the sync command handler
/// returns immediately.
fn submit_feedback(world: &mut World, player: Entity, kind: &'static str, args: &str) {
    let body = args.trim();
    if body.is_empty() {
        send_to(world, player, format!("Usage: {kind} <message>\r\n"));
        return;
    }
    let name = name_of(world, player);
    let char_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let (room_zone, room_id) = world
        .get::<Located>(player)
        .and_then(|l| world.get::<WorldKey>(l.0).copied())
        .map_or((None, None), |wk| (Some(wk.zone), Some(wk.id)));
    let room_label = match (room_zone, room_id) {
        (Some(z), Some(i)) => format!("{z}:{i}"),
        _ => "?".to_string(),
    };
    info!(
        kind,
        player = %name,
        character_id = char_id.as_deref().unwrap_or(""),
        room = %room_label,
        body = %body,
        "player feedback"
    );
    let report_kind = match kind {
        "idea" => mud_db::enums::ReportType::Idea,
        "typo" => mud_db::enums::ReportType::Typo,
        _ => mud_db::enums::ReportType::Bug,
    };
    if let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) {
        let body_owned = body.to_string();
        let name_owned = name.clone();
        let char_id_owned = char_id.clone();
        tokio::spawn(async move {
            if let Err(e) = mud_db::reports::submit(
                &pool,
                report_kind,
                &name_owned,
                char_id_owned.as_deref(),
                room_zone,
                room_id,
                &body_owned,
            )
            .await
            {
                tracing::warn!(error = %e, "report submit failed");
            }
        });
    }
    send_to(
        world,
        player,
        format!("Thanks. Your {kind} report has been logged.\r\n"),
    );
}

fn cmd_petition(world: &mut World, player: Entity, args: &str) {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        send_to(
            world,
            player,
            "Petition what? Use this to ask online immortals for help.\r\n",
        );
        return;
    }
    if effect_prevents(world, player, Prevent::Speaking) {
        send_to(world, player, "Your voice is silenced.\r\n");
        return;
    }
    let player_name = name_of(world, player);

    // Staff reply form: if the caller is Immortal+ AND the first
    // token matches an online player's name, treat it as a private
    // reply instead of a broadcast. Folds the legacy `ptell` flow
    // into petition so a god responding to a petition uses the
    // same verb the player did.
    let caller_is_staff = world
        .get::<Account>(player)
        .is_some_and(|a| a.role.at_least(UserRole::Immortal));
    if caller_is_staff
        && let Some((first, rest)) = trimmed.split_once(char::is_whitespace)
        && !rest.trim().is_empty()
    {
        let rest = rest.trim();
        let target = {
            let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
            q.iter(world)
                .find(|(_, n)| n.name.eq_ignore_ascii_case(first))
                .map(|(e, _)| e)
        };
        if let Some(t) = target {
            send_to(
                world,
                t,
                format!("[STAFF REPLY] {player_name}: {rest}\r\n"),
            );
            send_to(
                world,
                player,
                format!("Replied to {}.\r\n", name_of(world, t)),
            );
            return;
        }
        // First word didn't match an online player — fall through
        // to the broadcast form.
    }

    let immortals: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Account), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(_, a)| a.role.at_least(UserRole::Immortal))
            .map(|(e, _)| e)
            .collect()
    };
    let line = format!("[PETITION] {player_name}: {trimmed}\r\n");
    for t in immortals {
        send_to(world, t, line.clone());
    }
    send_to(
        world,
        player,
        "Your petition has been sent to the immortals.\r\n",
    );
}
