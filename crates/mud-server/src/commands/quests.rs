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
    AsyncCommand {
        dispatch: |world, player, pool, head, args| match head {
            "quests" | "qstat" | "qlist" => Some(Box::pin(cmd_quests(world, player, pool))),
            "abandon" => Some(Box::pin(cmd_abandon(world, player, pool, args))),
            "questinfo" => Some(Box::pin(cmd_questinfo(world, player, pool, args))),
            "innate" => Some(Box::pin(cmd_innate(world, player, pool))),
            "qload" => Some(Box::pin(cmd_qload(world, player, pool, args))),
            "qaccept" => Some(Box::pin(cmd_qaccept(world, player, pool, args))),
            "qgive" => Some(Box::pin(cmd_qgive(world, player, pool, args))),
            "qcomplete" => Some(Box::pin(cmd_qcomplete(world, player, pool, args))),
            "qreward" => Some(Box::pin(cmd_qreward(world, player, pool, args))),
            _ => None,
        },
    }
}

inventory::submit! {
    Command {
        names: &["qreward"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Quest,
        help: Help {
            usage: "qreward [<zone> <quest-id> [<reward-id>]]",
            summary: "List or claim a choice-group reward.",
            long: "With no arguments, lists every choice-reward group \
                   from your completed quests that you haven't claimed \
                   yet. With <zone> <quest-id>, lists choices for that \
                   quest. With all three args, claims the named reward \
                   (id from the listing). One pick per choice_group.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["quests", "qstat", "qlist"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Quest,
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
        category: Category::Quest,
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
        category: Category::Magic,
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
        category: Category::Quest,
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
        category: Category::Quest,
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
            //
            // Wave 4.8: `phase_description` (one-line blurb)
            // renders below the phase header when present.
            // Wave 4.7: `internal_note` renders next to its
            // objective for Builder+ viewers only.
            let viewer_role = world
                .get::<Account>(player)
                .map(|a| a.role)
                .unwrap_or(UserRole::Player);
            let show_internal_notes = viewer_role.at_least(UserRole::Builder);
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
                            if let Some(pd) = &r.phase_description
                                && !pd.trim().is_empty()
                            {
                                out.push_str(&format!("          {}\r\n", pd.trim()));
                            }
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
                        if show_internal_notes
                            && let Some(note) = &r.internal_note
                            && !note.trim().is_empty()
                        {
                            out.push_str(&format!(
                                "              (builder note: {})\r\n",
                                note.trim()
                            ));
                        }
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
    // Availability Lua gate (Wave 4.4): if the quest's
    // `availability_requirement` is non-null, evaluate it against
    // this player before consulting the DB. The DB layer can't do
    // this because it needs the live world. Quest row is re-read
    // there anyway, so the double-fetch cost is negligible.
    if let Ok(Some(qrow)) = mud_db::quests::get_quest(pool, zone, id).await
        && let Some(expr) = qrow.availability_requirement.as_ref().filter(|s| !s.trim().is_empty())
    {
        if !eval_quest_availability(world, player, expr) {
            send_to(
                world,
                player,
                "You can't take that quest — you don't meet the requirements.\r\n",
            );
            return;
        }
    }
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
        mud_db::quests::AcceptOutcome::ExclusiveGroupConflict {
            group,
            held_zone,
            held_id,
        } => format!(
            "You can't take this — you already hold quest ({held_zone}, {held_id}) \
             in exclusive group '{group}'. Abandon it first.\r\n"
        ),
        mud_db::quests::AcceptOutcome::Cooldown { remaining_secs } => {
            let mins = remaining_secs / 60;
            let secs = remaining_secs % 60;
            format!("That quest is on cooldown for another {mins}m{secs:02}s.\r\n")
        }
        mud_db::quests::AcceptOutcome::RequirementNotMet { .. } => {
            // Stripped of the raw expression for players. Builders
            // see the expression in syslog via `eval_quest_availability`.
            "You can't take that quest — you don't meet the requirements.\r\n"
                .to_string()
        }
    };
    send_to(world, player, line);
}

/// Evaluate `availability_requirement` Lua for `player` (Wave 4.4).
/// Returns true when the expression returns truthy or when
/// evaluation fails (fail-open — a syntax error shouldn't lock the
/// player out of an entire quest line; the error lands in syslog).
/// The expression is wrapped in `return (...)` so builders can author
/// raw boolean expressions like `character.class == 'PALADIN'`.
pub(crate) fn eval_quest_availability(
    world: &mut World,
    player: Entity,
    expr: &str,
) -> bool {
    let body = format!("return ({expr})");
    let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
        host.exec_for_event_with_value(world, player, player, None, &body, &[])
    });
    match result {
        Ok((_out, Some(b))) => b,
        Ok((_out, None)) => {
            // Non-boolean return — treat as "yes" so builders who
            // author side-effecting checks don't accidentally
            // brick the quest. Fail-open.
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, expr = %expr, "quest availability Lua failed");
            true
        }
    }
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
    // Wave 4.2-4.5: builder-visible trigger / gate info. Players
    // don't need to see exclusive groups or availability Lua, but
    // staff do for debugging and content review.
    let viewer_role = world
        .get::<Account>(player)
        .map(|a| a.role)
        .unwrap_or(UserRole::Player);
    if viewer_role.at_least(UserRole::Builder) {
        out.push_str("\r\nBuilder info:\r\n");
        if let Some(t) = row.time_limit_minutes {
            out.push_str(&format!("  Time limit: {t} minute(s)\r\n"));
        }
        if let Some(c) = row.cooldown_minutes {
            out.push_str(&format!("  Cooldown: {c} minute(s)\r\n"));
        }
        if let Some(g) = &row.exclusive_group {
            out.push_str(&format!("  Exclusive group: {g}\r\n"));
        }
        if let Some(a) = &row.availability_requirement {
            out.push_str(&format!("  Availability Lua: {a}\r\n"));
        }
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

/// `qreward [<zone> <id> [<reward-id>]]` — Wave 4.9 choice-reward
/// selection. With no args, lists every unclaimed choice group on
/// the caller's completed quests. With `<zone> <id>`, lists the
/// choices for that quest. With all three, grants the chosen
/// reward and records the claim in `CharacterQuest.variables.
/// claimed_rewards` so the choice can only be picked once.
///
/// Tracking lives in the per-quest JSON rather than a dedicated
/// table because the existing `CharacterQuest.variables` already
/// rides through the standard accept/abandon/complete path.
pub(crate) async fn cmd_qreward(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let character_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let Some(character_id) = character_id else {
        send_to(world, player, "No account info.\r\n");
        return;
    };
    match (parts.next(), parts.next(), parts.next()) {
        (None, _, _) => qreward_list_all(world, player, pool, &character_id).await,
        (Some(z), Some(i), None) => {
            let (Ok(zone), Ok(id)) = (z.parse::<i32>(), i.parse::<i32>()) else {
                send_to(world, player, "Zone and id must be integers.\r\n");
                return;
            };
            qreward_list_one(world, player, pool, &character_id, zone, id).await;
        }
        (Some(z), Some(i), Some(r)) => {
            let (Ok(zone), Ok(id), Ok(reward_id)) =
                (z.parse::<i32>(), i.parse::<i32>(), r.parse::<i32>())
            else {
                send_to(world, player, "Zone, id, and reward-id must be integers.\r\n");
                return;
            };
            qreward_claim(world, player, pool, &character_id, zone, id, reward_id).await;
        }
        _ => {
            send_to(
                world,
                player,
                "Usage: qreward [<zone> <quest-id> [<reward-id>]]\r\n",
            );
        }
    }
}

/// Pull the `claimed_rewards: [i32]` array out of the per-quest
/// variables JSON. Missing key → empty Vec. Permissive parse:
/// non-array or non-integer entries are silently dropped.
fn claimed_rewards_from(vars: &serde_json::Value) -> Vec<i32> {
    vars.get("claimed_rewards")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_i64().map(|n| n as i32))
                .collect()
        })
        .unwrap_or_default()
}

/// List every unclaimed choice group across all of `character_id`'s
/// completed quests (no `<zone> <id>` filter). Also lists any
/// conditional (Wave 4.10) rewards that the async grant path
/// couldn't evaluate inline.
async fn qreward_list_all(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
) {
    let quests = match mud_db::quests::list_for_character(pool, character_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Quest fetch failed: {e}\r\n"));
            return;
        }
    };
    let mut buf = String::from("\r\nUnclaimed rewards:\r\n");
    let mut printed_any = false;
    for q in &quests {
        if q.status != "COMPLETED" {
            continue;
        }
        let choices = mud_db::quest_objectives::list_choice_rewards(
            pool,
            q.quest_zone_id,
            q.quest_id,
        )
        .await
        .unwrap_or_default();
        let conditionals = mud_db::quest_objectives::list_conditional_rewards(
            pool,
            q.quest_zone_id,
            q.quest_id,
        )
        .await
        .unwrap_or_default();
        if choices.is_empty() && conditionals.is_empty() {
            continue;
        }
        let claimed = claimed_rewards_from(&q.variables);
        // Choice-group section.
        let mut groups: std::collections::BTreeMap<i32, Vec<&mud_db::quest_objectives::QuestRewardRow>> = std::collections::BTreeMap::new();
        for r in &choices {
            if let Some(g) = r.choice_group {
                groups.entry(g).or_default().push(r);
            }
        }
        for (g, rs) in &groups {
            if rs.iter().any(|r| claimed.contains(&r.id)) {
                continue;
            }
            printed_any = true;
            buf.push_str(&format!(
                "  Quest ({}, {}) {} — group {}:\r\n",
                q.quest_zone_id, q.quest_id, q.quest_name, g
            ));
            for r in rs {
                buf.push_str(&format!(
                    "    [{}] {}\r\n",
                    r.id,
                    describe_reward(r)
                ));
            }
            buf.push_str(&format!(
                "    (pick one: qreward {} {} <id>)\r\n",
                q.quest_zone_id, q.quest_id
            ));
        }
        // Conditional rewards section — same UI shape, no group label.
        let unclaimed: Vec<&mud_db::quest_objectives::QuestRewardRow> = conditionals
            .iter()
            .filter(|r| !claimed.contains(&r.id))
            .collect();
        if !unclaimed.is_empty() {
            printed_any = true;
            buf.push_str(&format!(
                "  Quest ({}, {}) {} — conditional:\r\n",
                q.quest_zone_id, q.quest_id, q.quest_name
            ));
            for r in &unclaimed {
                buf.push_str(&format!(
                    "    [{}] {}\r\n",
                    r.id,
                    describe_reward(r)
                ));
            }
            buf.push_str(&format!(
                "    (claim: qreward {} {} <id>)\r\n",
                q.quest_zone_id, q.quest_id
            ));
        }
    }
    if !printed_any {
        // Distinguish "no completed quests" from "all rewards already
        // claimed" — the second case is the common one once a player
        // has played through a chunk of content; surfacing both
        // makes the gap obvious without leaving the player guessing
        // why `qreward` looks empty.
        let any_completed = quests.iter().any(|q| q.status == "COMPLETED");
        let msg = if any_completed {
            "\r\nNo unclaimed rewards. All conditional and choice rewards \
             from your completed quests have been claimed.\r\n"
        } else {
            "\r\nNo unclaimed rewards. Finish a quest with conditional or \
             choice-group rewards and they'll show up here.\r\n"
        };
        send_to(world, player, msg);
        return;
    }
    send_to(world, player, buf);
}

/// List choice groups for one specific quest. Refuses if the
/// quest isn't COMPLETED for the caller.
async fn qreward_list_one(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
    zone: i32,
    id: i32,
) {
    let Some((_cqid, status, vars)) =
        find_character_quest_with_vars(pool, character_id, zone, id).await
    else {
        send_to(
            world,
            player,
            format!("You haven't completed quest ({zone}, {id}).\r\n"),
        );
        return;
    };
    if status != "COMPLETED" {
        send_to(
            world,
            player,
            "That quest isn't complete yet.\r\n",
        );
        return;
    }
    let choices = mud_db::quest_objectives::list_choice_rewards(pool, zone, id)
        .await
        .unwrap_or_default();
    let conditionals =
        mud_db::quest_objectives::list_conditional_rewards(pool, zone, id)
            .await
            .unwrap_or_default();
    if choices.is_empty() && conditionals.is_empty() {
        send_to(
            world,
            player,
            format!("Quest ({zone}, {id}) has no claimable rewards.\r\n"),
        );
        return;
    }
    let claimed = claimed_rewards_from(&vars);
    let mut buf = format!("\r\nRewards for quest ({zone}, {id}):\r\n");
    let mut groups: std::collections::BTreeMap<i32, Vec<&mud_db::quest_objectives::QuestRewardRow>> = std::collections::BTreeMap::new();
    for r in &choices {
        if let Some(g) = r.choice_group {
            groups.entry(g).or_default().push(r);
        }
    }
    for (g, rs) in &groups {
        let group_done = rs.iter().any(|r| claimed.contains(&r.id));
        buf.push_str(&format!(
            "  Group {g}{}:\r\n",
            if group_done { " (claimed)" } else { "" }
        ));
        for r in rs {
            let marker = if claimed.contains(&r.id) { "✓" } else { " " };
            buf.push_str(&format!(
                "    {marker} [{}] {}\r\n",
                r.id,
                describe_reward(r)
            ));
        }
    }
    if !conditionals.is_empty() {
        buf.push_str("  Conditional:\r\n");
        for r in &conditionals {
            let marker = if claimed.contains(&r.id) { "✓" } else { " " };
            buf.push_str(&format!(
                "    {marker} [{}] {}\r\n",
                r.id,
                describe_reward(r)
            ));
        }
    }
    send_to(world, player, buf);
}

/// Claim a single reward from a choice group. Validates the reward
/// is part of the quest, the player has completed the quest, the
/// group hasn't already been claimed, and the reward's `condition`
/// Lua (if any) is truthy. On success: grants the reward and
/// stamps `claimed_rewards` in the per-quest variables JSON.
async fn qreward_claim(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
    zone: i32,
    id: i32,
    reward_id: i32,
) {
    let Some((cqid, status, vars)) =
        find_character_quest_with_vars(pool, character_id, zone, id).await
    else {
        send_to(
            world,
            player,
            format!("You haven't completed quest ({zone}, {id}).\r\n"),
        );
        return;
    };
    if status != "COMPLETED" {
        send_to(world, player, "That quest isn't complete yet.\r\n");
        return;
    }
    let reward = match mud_db::quest_objectives::get_reward(pool, reward_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            send_to(world, player, format!("No reward with id {reward_id}.\r\n"));
            return;
        }
        Err(e) => {
            send_to(world, player, format!("Reward fetch failed: {e}\r\n"));
            return;
        }
    };
    // Make sure the reward is from THIS quest (defense against
    // typo'd reward ids that happen to exist on some other quest).
    // Conditional non-choice rewards live alongside choice-group
    // rewards under the qreward umbrella.
    let all_choices = mud_db::quest_objectives::list_choice_rewards(pool, zone, id)
        .await
        .unwrap_or_default();
    let all_conditionals =
        mud_db::quest_objectives::list_conditional_rewards(pool, zone, id)
            .await
            .unwrap_or_default();
    let is_choice = all_choices.iter().any(|r| r.id == reward.id);
    let is_conditional = all_conditionals.iter().any(|r| r.id == reward.id);
    if !is_choice && !is_conditional {
        send_to(
            world,
            player,
            format!(
                "Reward {reward_id} isn't a claimable reward of quest ({zone}, {id}).\r\n"
            ),
        );
        return;
    }
    let claimed = claimed_rewards_from(&vars);
    // Already-claimed gate. For choice groups, "claimed" means any
    // row in the same group is in the list. For conditional rewards,
    // it means that exact id is in the list.
    if let Some(group) = reward.choice_group {
        if all_choices
            .iter()
            .filter(|r| r.choice_group == Some(group))
            .any(|r| claimed.contains(&r.id))
        {
            send_to(
                world,
                player,
                format!("You've already claimed a reward from group {group}.\r\n"),
            );
            return;
        }
    } else if claimed.contains(&reward.id) {
        send_to(
            world,
            player,
            "You've already claimed that reward.\r\n",
        );
        return;
    }
    // Condition gate (Wave 4.10). Treat empty / missing as truthy.
    // Echo the actual condition expression on failure so the player
    // (and any staffer they ask) can see *why* — the legacy "you
    // don't meet that reward's conditions" was opaque when a player
    // re-classed and a class-locked reward silently refused.
    if let Some(expr) = reward.condition.as_deref()
        && !expr.trim().is_empty()
        && !eval_quest_availability(world, player, expr)
    {
        send_to(
            world,
            player,
            format!(
                "You don't currently meet that reward's condition: {expr}\r\n"
            ),
        );
        return;
    }
    // Grant the simple reward via the existing path (ITEM/HOUSING
    // are announced rather than spawned). The single-row slice
    // keeps the DB code shared with the auto-grant flow.
    let single = vec![reward.clone()];
    if let Err(e) =
        mud_db::quest_objectives::grant_simple_rewards(pool, character_id, &single).await
    {
        send_to(world, player, format!("Reward grant failed: {e}\r\n"));
        return;
    }
    // Mirror live ECS state for the running player. SpawnItem only
    // works for ITEM; the rest update Profile / Wealth / etc.
    let update_tx = world.get_resource::<PlayerUpdateTx>().map(|t| t.0.clone());
    if let Some(tx) = update_tx {
        let upd = match reward.reward_type.as_str() {
            "EXPERIENCE" => reward.amount.map(|a| PendingPlayerUpdate::ExperienceDelta {
                character_id: character_id.to_string(),
                amount: a,
            }),
            "GOLD" => reward.amount.map(|a| PendingPlayerUpdate::WealthDelta {
                character_id: character_id.to_string(),
                amount: i64::from(a),
            }),
            "SKILL_POINTS" => reward.amount.map(|a| PendingPlayerUpdate::SkillPointsDelta {
                character_id: character_id.to_string(),
                amount: a,
            }),
            "ABILITY" => reward.ability_id.map(|aid| PendingPlayerUpdate::AbilityKnown {
                character_id: character_id.to_string(),
                ability_id: aid,
            }),
            "ITEM" => match (reward.object_zone_id, reward.object_id) {
                (Some(oz), Some(oid)) => Some(PendingPlayerUpdate::SpawnItem {
                    character_id: character_id.to_string(),
                    object_zone: oz,
                    object_id: oid,
                    quantity: reward.quantity,
                }),
                _ => None,
            },
            _ => None,
        };
        if let Some(u) = upd {
            let _ = tx.send(u).await;
        }
    }
    // Persist the claim. Append reward.id to claimed_rewards.
    let mut new_claimed = claimed;
    new_claimed.push(reward.id);
    let array =
        serde_json::Value::Array(new_claimed.iter().map(|n| serde_json::json!(n)).collect());
    if let Err(e) =
        mud_db::quests::set_quest_variable(pool, &cqid, "claimed_rewards", &array).await
    {
        tracing::warn!(error = %e, "qreward: persist claim failed");
    }
    // Remaining-claims tally across all of this player's completed
    // quests: unfinished choice groups + unclaimed conditionals.
    // Cheap-ish (one DB pass per completed quest) but bounded by
    // content size, which is small relative to a single combat round.
    let remaining = count_pending_claims(pool, character_id, &new_claimed).await;
    let mut msg =
        format!("You claim your reward: {}.\r\n", describe_reward(&reward));
    msg.push_str(&match remaining {
        0 => "All your conditional / choice rewards are now claimed.\r\n".to_string(),
        1 => "1 reward still pending — type `qreward` to view.\r\n".to_string(),
        n => format!("{n} rewards still pending — type `qreward` to view.\r\n"),
    });
    send_to(world, player, msg);
}

/// Count this character's outstanding qreward claims across every
/// completed quest, treating the just-updated `latest_claimed_for_this_quest`
/// vec as already persisted (`set_quest_variable` may not have visibly
/// returned by the time we re-read). Used for the post-claim tally.
async fn count_pending_claims(
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
    _latest_claimed_for_this_quest: &[i32],
) -> usize {
    let quests = match mud_db::quests::list_for_character(pool, character_id).await {
        Ok(r) => r,
        Err(_) => return 0,
    };
    let mut total = 0usize;
    for q in &quests {
        if q.status != "COMPLETED" {
            continue;
        }
        let choices = mud_db::quest_objectives::list_choice_rewards(
            pool,
            q.quest_zone_id,
            q.quest_id,
        )
        .await
        .unwrap_or_default();
        let conditionals = mud_db::quest_objectives::list_conditional_rewards(
            pool,
            q.quest_zone_id,
            q.quest_id,
        )
        .await
        .unwrap_or_default();
        if choices.is_empty() && conditionals.is_empty() {
            continue;
        }
        let claimed = claimed_rewards_from(&q.variables);
        // Unfinished choice groups: a group is "unfinished" while no
        // member id is in `claimed`.
        let mut groups: std::collections::BTreeMap<i32, Vec<i32>> =
            std::collections::BTreeMap::new();
        for r in &choices {
            if let Some(g) = r.choice_group {
                groups.entry(g).or_default().push(r.id);
            }
        }
        for (_g, ids) in &groups {
            if !ids.iter().any(|id| claimed.contains(id)) {
                total += 1;
            }
        }
        // Each unclaimed conditional is its own pending claim.
        for r in &conditionals {
            if !claimed.contains(&r.id) {
                total += 1;
            }
        }
    }
    total
}

/// Fetch `(character_quest_id, status, variables_json)` for one
/// quest the caller has accepted. Returns `None` when no row exists.
async fn find_character_quest_with_vars(
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
    zone: i32,
    quest_id: i32,
) -> Option<(String, String, serde_json::Value)> {
    let rows = mud_db::quests::list_for_character(pool, character_id).await.ok()?;
    let row = rows
        .into_iter()
        .find(|r| r.quest_zone_id == zone && r.quest_id == quest_id)?;
    Some((row.id, row.status, row.variables))
}

fn describe_reward(r: &mud_db::quest_objectives::QuestRewardRow) -> String {
    match (r.reward_type.as_str(), r.amount, r.quantity) {
        ("EXPERIENCE", Some(a), _) => format!("+{a} experience"),
        ("GOLD", Some(a), _) => format!("+{a} gold"),
        ("SKILL_POINTS", Some(a), _) => format!("+{a} skill points"),
        ("ABILITY", _, _) => format!(
            "ability ({})",
            r.ability_id.map_or("?".to_string(), |id| id.to_string())
        ),
        ("ITEM", _, q) => match (r.object_zone_id, r.object_id) {
            (Some(z), Some(id)) => format!("{q} × item ({z}, {id})"),
            _ => format!("{q} × item"),
        },
        ("HOUSING", _, _) => "housing access".to_string(),
        (t, _, _) => format!("({t})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reward(id: i32, ty: &str) -> mud_db::quest_objectives::QuestRewardRow {
        mud_db::quest_objectives::QuestRewardRow {
            id,
            reward_type: ty.to_string(),
            amount: None,
            object_zone_id: None,
            object_id: None,
            ability_id: None,
            quantity: 1,
            choice_group: None,
            condition: None,
        }
    }

    #[test]
    fn claimed_rewards_from_empty_object() {
        // Default `variables` is `{}`. No claimed key → empty.
        assert_eq!(claimed_rewards_from(&json!({})), Vec::<i32>::new());
    }

    #[test]
    fn claimed_rewards_from_array() {
        let v = json!({"claimed_rewards": [10, 20, 30]});
        assert_eq!(claimed_rewards_from(&v), vec![10, 20, 30]);
    }

    #[test]
    fn claimed_rewards_from_filters_non_integers() {
        let v = json!({"claimed_rewards": [10, "twenty", 30, null]});
        assert_eq!(claimed_rewards_from(&v), vec![10, 30]);
    }

    #[test]
    fn claimed_rewards_from_non_array_yields_empty() {
        let v = json!({"claimed_rewards": "not-an-array"});
        assert_eq!(claimed_rewards_from(&v), Vec::<i32>::new());
    }

    #[test]
    fn describe_reward_experience_with_amount() {
        let mut r = reward(1, "EXPERIENCE");
        r.amount = Some(500);
        assert_eq!(describe_reward(&r), "+500 experience");
    }

    #[test]
    fn describe_reward_gold() {
        let mut r = reward(2, "GOLD");
        r.amount = Some(100);
        assert_eq!(describe_reward(&r), "+100 gold");
    }

    #[test]
    fn describe_reward_skill_points() {
        let mut r = reward(3, "SKILL_POINTS");
        r.amount = Some(3);
        assert_eq!(describe_reward(&r), "+3 skill points");
    }

    #[test]
    fn describe_reward_ability() {
        let mut r = reward(4, "ABILITY");
        r.ability_id = Some(42);
        assert_eq!(describe_reward(&r), "ability (42)");
    }

    #[test]
    fn describe_reward_ability_missing_id() {
        let r = reward(5, "ABILITY");
        assert_eq!(describe_reward(&r), "ability (?)");
    }

    #[test]
    fn describe_reward_item_with_proto() {
        let mut r = reward(6, "ITEM");
        r.object_zone_id = Some(30);
        r.object_id = Some(45);
        r.quantity = 2;
        assert_eq!(describe_reward(&r), "2 × item (30, 45)");
    }

    #[test]
    fn describe_reward_item_missing_proto() {
        let mut r = reward(7, "ITEM");
        r.quantity = 5;
        assert_eq!(describe_reward(&r), "5 × item");
    }

    #[test]
    fn describe_reward_housing() {
        let r = reward(8, "HOUSING");
        assert_eq!(describe_reward(&r), "housing access");
    }

    #[test]
    fn describe_reward_unknown_type() {
        let r = reward(9, "MYSTERY");
        assert_eq!(describe_reward(&r), "(MYSTERY)");
    }

    // ----------------------------------------------------------------
    // Condition gate (Wave 4.10 — qreward refuses on unmet condition)
    // ----------------------------------------------------------------
    //
    // The qreward_claim path's condition gate routes through
    // `eval_quest_availability`. A full e2e test would need a quest
    // row + completed CharacterQuest + Lua-evaluable `actor` userdata
    // backed by Profile/Class/Wealth components — too heavy for this
    // file. Instead, test the gate directly with literal expressions
    // (no entity-field access) so the contract "false → don't grant,
    // true → grant, malformed → fail-open" is locked in. Anyone
    // refactoring the eval helper now has a unit-level guard.

    #[test]
    fn eval_quest_availability_literal_true_returns_true() {
        let mut world = World::new();
        world.insert_resource(mud_script::LuaHost::default());
        let entity = world.spawn(()).id();
        assert!(eval_quest_availability(&mut world, entity, "true"));
    }

    #[test]
    fn eval_quest_availability_literal_false_returns_false() {
        let mut world = World::new();
        world.insert_resource(mud_script::LuaHost::default());
        let entity = world.spawn(()).id();
        assert!(!eval_quest_availability(&mut world, entity, "false"));
    }

    #[test]
    fn eval_quest_availability_arithmetic_predicate() {
        let mut world = World::new();
        world.insert_resource(mud_script::LuaHost::default());
        let entity = world.spawn(()).id();
        assert!(eval_quest_availability(&mut world, entity, "1 + 1 == 2"));
        assert!(!eval_quest_availability(&mut world, entity, "1 + 1 == 3"));
    }

    #[test]
    fn eval_quest_availability_compile_error_is_fail_open() {
        // Author-error: unbalanced paren. Wrapped in `return (...)`,
        // so the resulting body is `return ((` — a compile error.
        // The helper logs + returns true so the player isn't bricked
        // by a typo in the QuestReward.condition column. The qreward
        // path then grants the reward; staffers see the warning in
        // syslog and can fix the row.
        let mut world = World::new();
        world.insert_resource(mud_script::LuaHost::default());
        let entity = world.spawn(()).id();
        assert!(eval_quest_availability(&mut world, entity, "((unbalanced"));
    }
}
