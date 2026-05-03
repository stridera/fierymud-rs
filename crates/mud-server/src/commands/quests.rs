//! Quest commands: `quests`, `qaccept`, `qload`, `qgive`,
//! `qcomplete`, `abandon`, `questinfo`, `innate`. Both the
//! Command records and the async handler bodies live here.

#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::wildcard_imports)]

use bevy_ecs::prelude::{Entity, World};
use mud_db::enums::UserRole;
use mud_world::*;

use crate::commands::*;

inventory::submit! {
    Command {
        names: &["quests", "qstat", "qlist"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "quests",
            summary: "List your accepted quests.",
            long: "In-progress quests appear first with their short \
                   description; completed/abandoned quests follow with \
                   their final status. Each in-progress quest also \
                   shows its phases + objective progress (▶ for \
                   current, ✓ for done, [N/M] for showProgress).",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["abandon"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "abandon <#>",
            summary: "Drop an in-progress quest.",
            long: "Argument is the slot number from the in-progress \
                   section of `quests`. Marks the row ABANDONED \
                   rather than deleting it, preserving the audit \
                   trail and the (char, zone, id) unique key.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["innate"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "innate",
            summary: "List your race's innate abilities.",
            long: "Reads the `RaceAbilities` rows for your character's \
                   race and prints each ability's name, category \
                   (PRIMARY / SECONDARY / ...), starting bonus, and \
                   proficiency cap.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["questinfo"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "questinfo <zone> <id>",
            summary: "Show details for a single quest definition.",
            long: "Reads the `Quest` row for `(zone, id)` and prints \
                   name, level range, flags (repeatable / shareable / \
                   hidden / auto-accept), short description, and full \
                   description.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["qaccept"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "qaccept <zone> <quest-id>",
            summary: "Accept a quest by composite id.",
            long: "Self-service quest acceptance. Validates level \
                   range, checks every required prerequisite, and \
                   refuses if you already have it in progress (or \
                   completed it once already on a non-repeatable \
                   quest).",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["qload"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "qload <zone> <quest-id>",
            summary: "Admin: assign a quest to your character (testing).",
            long: "Inserts a CharacterQuest row with status \
                   IN_PROGRESS for the caller's character. No-op if \
                   that quest is already assigned to you.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["qgive"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "qgive <player> <zone> <quest-id>",
            summary: "Admin: assign a quest to another online player.",
            long: "Same as `qload` but targets a named online player \
                   instead of the caller. Target must be currently \
                   online (offline-character assignment is left for a \
                   future iteration).",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["qcomplete"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "qcomplete <#>",
            summary: "Admin: force-complete an in-progress quest.",
            long: "Slot number from the `quests` in-progress section. \
                   Flips IN_PROGRESS → COMPLETED, stamps completed_at, \
                   bumps completion_count.",
        },
        run: cmd_mail_stub,
    }
}

/// `quests` / `qstat` / `qlist`: list quests the current character has
/// accepted, in-progress first then recently completed. Active rows
/// show the quest name + short description; completed rows show
/// completion count. Empty inbox = "no quests accepted."
pub(crate) async fn cmd_quests(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
) {
    let character_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let Some(character_id) = character_id else {
        send_to(world, player, "No account info; can't fetch quests.\r\n");
        return;
    };
    let rows = match mud_db::quests::list_for_character(pool, &character_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Quest fetch failed: {e}\r\n"));
            return;
        }
    };
    if rows.is_empty() {
        send_to(world, player, "\r\nYou have no active or completed quests.\r\n");
        return;
    }
    let active: Vec<&mud_db::quests::CharacterQuestRow> = rows
        .iter()
        .filter(|r| r.status == "IN_PROGRESS")
        .collect();
    let other: Vec<&mud_db::quests::CharacterQuestRow> = rows
        .iter()
        .filter(|r| r.status != "IN_PROGRESS")
        .collect();
    let mut out = String::from("\r\n");
    if !active.is_empty() {
        out.push_str(&format!("In progress ({}):\r\n", active.len()));
        for q in &active {
            out.push_str(&format!(
                "  ({}, {})  {}\r\n",
                q.quest_zone_id, q.quest_id, q.quest_name
            ));
            if let Some(desc) = &q.short_description
                && !desc.trim().is_empty()
            {
                out.push_str(&format!("        {}\r\n", desc.trim()));
            }
            // Per-quest objective listing. Group by phase; mark
            // the current phase with a "▶" so the player sees
            // which step they're on. Past phases show "✓".
            match mud_db::quest_objectives::list_for_quest(pool, &q.id).await {
                Ok(rows) => {
                    let mut last_phase: Option<i32> = None;
                    let current_phase_id = rows.first().and_then(|r| r.current_phase_id);
                    for r in &rows {
                        if last_phase != Some(r.phase_id) {
                            let marker = if Some(r.phase_id) == current_phase_id {
                                "▶"
                            } else if rows
                                .iter()
                                .filter(|x| x.phase_id == r.phase_id)
                                .all(|x| x.completed)
                            {
                                "✓"
                            } else {
                                " "
                            };
                            out.push_str(&format!(
                                "      {marker} Phase {}: {}\r\n",
                                r.phase_order, r.phase_name
                            ));
                            last_phase = Some(r.phase_id);
                        }
                        let status = if r.completed {
                            "[done]".to_string()
                        } else if r.show_progress {
                            format!("[{}/{}]", r.current_count, r.required_count)
                        } else {
                            "[ ]".to_string()
                        };
                        out.push_str(&format!(
                            "          {status} {}\r\n",
                            r.player_description
                        ));
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "objective listing failed");
                }
            }
        }
        out.push_str("\r\n");
    }
    if !other.is_empty() {
        out.push_str(&format!("Other ({}):\r\n", other.len()));
        for q in &other {
            out.push_str(&format!(
                "  [{}] ({}, {})  {}",
                q.status, q.quest_zone_id, q.quest_id, q.quest_name,
            ));
            if q.completion_count > 1 {
                out.push_str(&format!(" ×{}", q.completion_count));
            }
            out.push_str("\r\n");
        }
    }
    send_to(world, player, out);
}

/// `qload <zone> <id>`: admin command — assign a quest to the caller's
/// character with status `IN_PROGRESS`. Skips if the row already
/// exists. Useful for testing the quest listing/abandon loop without
/// the full trigger-acceptance flow.
/// `qaccept <zone> <id>` — player-driven quest acceptance.
/// Validates level / hidden / prereqs / no-duplicate via
/// `quests::accept_for_player` and prints a clear refusal line
/// per outcome.
pub(crate) async fn cmd_qaccept(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let (Some(zone_raw), Some(id_raw)) = (parts.next(), parts.next()) else {
        send_to(world, player, "Usage: qaccept <zone> <quest-id>\r\n");
        return;
    };
    let (Ok(zone), Ok(id)) = (zone_raw.parse::<i32>(), id_raw.parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
        return;
    };
    let character_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let level = world.get::<Profile>(player).map_or(1, |p| p.level);
    let Some(character_id) = character_id else {
        send_to(world, player, "No account info.\r\n");
        return;
    };
    let outcome = match mud_db::quests::accept_for_player(
        pool,
        &character_id,
        level,
        zone,
        id,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            send_to(world, player, format!("DB error: {e}\r\n"));
            return;
        }
    };
    let line = match outcome {
        mud_db::quests::AcceptOutcome::Accepted => {
            format!("Quest ({zone}, {id}) accepted.\r\n")
        }
        mud_db::quests::AcceptOutcome::NotFound => {
            format!("No quest at ({zone}, {id}).\r\n")
        }
        mud_db::quests::AcceptOutcome::Hidden => {
            "That quest can't be accepted directly.\r\n".to_string()
        }
        mud_db::quests::AcceptOutcome::LevelTooLow(min) => {
            format!("You must be at least level {min} to accept this quest.\r\n")
        }
        mud_db::quests::AcceptOutcome::LevelTooHigh(max) => {
            format!("This quest is for characters up to level {max}.\r\n")
        }
        mud_db::quests::AcceptOutcome::AlreadyInProgress => {
            "You're already on that quest.\r\n".to_string()
        }
        mud_db::quests::AcceptOutcome::AlreadyCompletedNonRepeatable => {
            "You've already finished that quest and it can't be repeated.\r\n"
                .to_string()
        }
        mud_db::quests::AcceptOutcome::PrerequisiteIncomplete { zone, id } => {
            format!("You need to finish quest ({zone}, {id}) first.\r\n")
        }
    };
    send_to(world, player, line);
}

pub(crate) async fn cmd_qload(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let zone_raw = parts.next();
    let id_raw = parts.next();
    let (Some(zone_raw), Some(id_raw)) = (zone_raw, id_raw) else {
        send_to(world, player, "Usage: qload <zone> <quest-id>\r\n");
        return;
    };
    let Ok(zone) = zone_raw.parse::<i32>() else {
        send_to(world, player, "Zone must be an integer.\r\n");
        return;
    };
    let Ok(id) = id_raw.parse::<i32>() else {
        send_to(world, player, "Quest id must be an integer.\r\n");
        return;
    };
    let exists = match mud_db::quests::quest_exists(pool, zone, id).await {
        Ok(b) => b,
        Err(e) => {
            send_to(world, player, format!("Quest lookup failed: {e}\r\n"));
            return;
        }
    };
    if !exists {
        send_to(
            world,
            player,
            format!("No Quest defined at ({zone}, {id}).\r\n"),
        );
        return;
    }
    let character_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let Some(character_id) = character_id else {
        send_to(world, player, "No account info; can't assign.\r\n");
        return;
    };
    match mud_db::quests::admin_assign(pool, &character_id, zone, id).await {
        Ok(Some(_)) => send_to(
            world,
            player,
            format!("Assigned Quest ({zone}, {id}) to your character.\r\n"),
        ),
        Ok(None) => send_to(
            world,
            player,
            format!("Already have Quest ({zone}, {id}).\r\n"),
        ),
        Err(e) => send_to(world, player, format!("Assign failed: {e}\r\n")),
    }
}

/// `innate`: list the caller race's innate abilities (`RaceAbilities`).
pub(crate) async fn cmd_innate(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
) {
    let race = world.get::<Profile>(player).map(|p| p.race.clone());
    let Some(race) = race else {
        send_to(world, player, "You have no race assigned.\r\n");
        return;
    };
    let rows = match mud_db::race_abilities::list_for_race(pool, &race).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Innate fetch failed: {e}\r\n"));
            return;
        }
    };
    if rows.is_empty() {
        send_to(
            world,
            player,
            format!("\r\nThe {race} race has no innate abilities.\r\n"),
        );
        return;
    }
    let mut out = format!("\r\nInnate abilities for {race} ({}):\r\n", rows.len());
    for r in &rows {
        out.push_str(&format!(
            "  {name:<24} {cat:<10} bonus +{bonus:<3} cap {cap}\r\n",
            name = r.ability_name,
            cat = r.category,
            bonus = r.bonus,
            cap = r.proficiency_cap,
        ));
    }
    send_to(world, player, out);
}

/// `questinfo <zone> <id>`: read-only catalog view of one quest.
/// Reads `Quest` directly (not `CharacterQuest`), so the row doesn't
/// have to be assigned to anyone.
pub(crate) async fn cmd_questinfo(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let (Some(zone_raw), Some(id_raw)) = (parts.next(), parts.next()) else {
        send_to(world, player, "Usage: questinfo <zone> <id>\r\n");
        return;
    };
    let Ok(zone) = zone_raw.parse::<i32>() else {
        send_to(world, player, "Zone must be an integer.\r\n");
        return;
    };
    let Ok(id) = id_raw.parse::<i32>() else {
        send_to(world, player, "Id must be an integer.\r\n");
        return;
    };
    let row = match mud_db::quests::get_quest(pool, zone, id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Quest fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(row) = row else {
        send_to(
            world,
            player,
            format!("No Quest defined at ({zone}, {id}).\r\n"),
        );
        return;
    };
    let mut out = format!("\r\nQuest ({}, {}) — {}\r\n", row.zone_id, row.id, row.name);
    out.push_str(&format!(
        "Level range: {} to {}\r\n",
        row.min_level, row.max_level
    ));
    let mut flags: Vec<&'static str> = Vec::new();
    if row.repeatable {
        flags.push("repeatable");
    }
    if row.shareable {
        flags.push("shareable");
    }
    if row.hidden {
        flags.push("hidden");
    }
    if row.auto_accept {
        flags.push("auto-accept");
    }
    out.push_str(&format!(
        "Flags: {}\r\n",
        if flags.is_empty() {
            "none".to_string()
        } else {
            flags.join(", ")
        }
    ));
    if let Some(short) = row.short_description.as_deref()
        && !short.trim().is_empty()
    {
        out.push_str(&format!("\r\n{}\r\n", short.trim()));
    }
    if let Some(desc) = row.description.as_deref()
        && !desc.trim().is_empty()
    {
        out.push_str(&format!("\r\n{}\r\n", desc.trim()));
    }
    send_to(world, player, out);
}

/// `qgive <player> <zone> <quest-id>`: admin command — assign a
/// quest to another online player's character. Refuses if target
/// isn't online; offline assignment isn't wired today.
pub(crate) async fn cmd_qgive(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let target_word = parts.next();
    let zone_raw = parts.next();
    let id_raw = parts.next();
    let (Some(target_word), Some(zone_raw), Some(id_raw)) = (target_word, zone_raw, id_raw) else {
        send_to(world, player, "Usage: qgive <player> <zone> <quest-id>\r\n");
        return;
    };
    let Ok(zone) = zone_raw.parse::<i32>() else {
        send_to(world, player, "Zone must be an integer.\r\n");
        return;
    };
    let Ok(id) = id_raw.parse::<i32>() else {
        send_to(world, player, "Quest id must be an integer.\r\n");
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
    let exists = match mud_db::quests::quest_exists(pool, zone, id).await {
        Ok(b) => b,
        Err(e) => {
            send_to(world, player, format!("Quest lookup failed: {e}\r\n"));
            return;
        }
    };
    if !exists {
        send_to(
            world,
            player,
            format!("No Quest defined at ({zone}, {id}).\r\n"),
        );
        return;
    }
    let target_char_id = world.get::<Account>(target).map(|a| a.character_id.clone());
    let Some(target_char_id) = target_char_id else {
        send_to(world, player, "Target has no account info.\r\n");
        return;
    };
    let target_name = name_of(world, target);
    match mud_db::quests::admin_assign(pool, &target_char_id, zone, id).await {
        Ok(Some(_)) => {
            send_to(
                world,
                player,
                format!("Assigned Quest ({zone}, {id}) to {target_name}.\r\n"),
            );
            send_to(
                world,
                target,
                format!(
                    "An immortal grants you a quest: ({zone}, {id}). Type `quests` to view.\r\n"
                ),
            );
        }
        Ok(None) => send_to(
            world,
            player,
            format!("{target_name} already has Quest ({zone}, {id}).\r\n"),
        ),
        Err(e) => send_to(world, player, format!("Assign failed: {e}\r\n")),
    }
}

/// `qcomplete <#>`: admin command — force-complete an in-progress
/// quest the caller's character has accepted. Slot is 1-based
/// against the `quests` in-progress section, same as `abandon`.
/// Useful for verifying reward / completion flow end-to-end without
/// the full objective-resolution pipeline.
pub(crate) async fn cmd_qcomplete(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let arg = args.trim();
    let Ok(slot) = arg.parse::<usize>() else {
        send_to(
            world,
            player,
            "Complete which quest? Pick a number from `quests`.\r\n",
        );
        return;
    };
    if slot == 0 {
        send_to(world, player, "Quest slots are 1-based.\r\n");
        return;
    }
    let character_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let Some(character_id) = character_id else {
        send_to(world, player, "No account info; can't fetch quests.\r\n");
        return;
    };
    let rows = match mud_db::quests::list_for_character(pool, &character_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Quest fetch failed: {e}\r\n"));
            return;
        }
    };
    let active: Vec<&mud_db::quests::CharacterQuestRow> = rows
        .iter()
        .filter(|r| r.status == "IN_PROGRESS")
        .collect();
    let Some(target) = active.get(slot - 1) else {
        send_to(
            world,
            player,
            format!("No in-progress quest at slot {slot}.\r\n"),
        );
        return;
    };
    match mud_db::quests::admin_complete(pool, &target.id).await {
        Ok(0) => {
            send_to(
                world,
                player,
                "That quest isn't in-progress; nothing to complete.\r\n",
            );
        }
        Ok(_) => {
            send_to(
                world,
                player,
                format!("Force-completed quest: {}.\r\n", target.quest_name),
            );
        }
        Err(e) => {
            send_to(world, player, format!("Complete failed: {e}\r\n"));
        }
    }
}

/// `abandon <#>`: drop an in-progress quest. Slot is 1-based against
/// the in-progress section of the `quests` listing. Marks the row
/// `ABANDONED` rather than deleting it, so the audit trail and
/// `(char, zone, id)` unique key are preserved.
pub(crate) async fn cmd_abandon(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let arg = args.trim();
    let Ok(slot) = arg.parse::<usize>() else {
        send_to(
            world,
            player,
            "Abandon which quest? Pick a number from `quests`.\r\n",
        );
        return;
    };
    if slot == 0 {
        send_to(world, player, "Quest slots are 1-based.\r\n");
        return;
    }
    let character_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let Some(character_id) = character_id else {
        send_to(world, player, "No account info; can't fetch quests.\r\n");
        return;
    };
    let rows = match mud_db::quests::list_for_character(pool, &character_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Quest fetch failed: {e}\r\n"));
            return;
        }
    };
    let active: Vec<&mud_db::quests::CharacterQuestRow> = rows
        .iter()
        .filter(|r| r.status == "IN_PROGRESS")
        .collect();
    let Some(target) = active.get(slot - 1) else {
        send_to(
            world,
            player,
            format!("No in-progress quest at slot {slot}.\r\n"),
        );
        return;
    };
    match mud_db::quests::abandon(pool, &target.id).await {
        Ok(0) => {
            send_to(
                world,
                player,
                "That quest isn't in-progress; nothing to abandon.\r\n",
            );
        }
        Ok(_) => {
            send_to(
                world,
                player,
                format!("Abandoned quest: {}.\r\n", target.quest_name),
            );
        }
        Err(e) => {
            send_to(world, player, format!("Abandon failed: {e}\r\n"));
        }
    }
}
