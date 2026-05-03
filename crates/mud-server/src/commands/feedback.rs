//! Player feedback channels — `bug` / `idea` / `typo` and
//! `petition`. Each writes a `Reports` row (or broadcasts to
//! immortals, in petition's case) so staff has a queryable
//! audit trail beyond the live tracing logs.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, Located, Online, Player, WorldKey};
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
            usage: "petition <message>",
            summary: "Send a message to all online immortals.",
            long: "Quick way to ask a staff member for help. Reaches \
                   every online player whose role is Immortal+; the \
                   sender gets a confirmation echo. Mortals never see \
                   anyone else's petitions.",
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
    let message = args.trim();
    if message.is_empty() {
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
    let immortals: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Account), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(_, a)| a.role.at_least(UserRole::Immortal))
            .map(|(e, _)| e)
            .collect()
    };
    let line = format!("[PETITION] {player_name}: {message}\r\n");
    for t in immortals {
        send_to(world, t, line.clone());
    }
    send_to(
        world,
        player,
        "Your petition has been sent to the immortals.\r\n",
    );
}
