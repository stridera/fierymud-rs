//! Quest commands: `quests`, `qaccept`, `qload`, `qgive`,
//! `qcomplete`, `abandon`, `questinfo`, `innate`. Both the
//! Command records and the async handler bodies live here.

#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::wildcard_imports)]

use bevy_ecs::prelude::{Entity, World};
use mud_db::enums::UserRole;
use mud_world::*;

use crate::commands::off_tick;
use crate::commands::*;

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, args| match head {
            "quests" | "qstat" | "qlist" | "questlog" => Some(Box::pin(cmd_quests(world, player, pool))),
            "abandon" => Some(Box::pin(cmd_abandon(world, player, pool, args))),
            "questinfo" => Some(Box::pin(cmd_questinfo(world, player, pool, args))),
            "innate" => Some(Box::pin(cmd_innate(world, player, pool, args))),
            "qload" => Some(Box::pin(cmd_qload(world, player, pool, args))),
            "qaccept" => Some(Box::pin(cmd_qaccept(world, player, pool, args))),
            "qgive" => Some(Box::pin(cmd_qgive(world, player, pool, args))),
            "qcomplete" => Some(Box::pin(cmd_qcomplete(world, player, pool, args))),
            "qreset" => Some(Box::pin(cmd_qreset(world, player, pool, args))),
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
        names: &["quests", "qstat", "qlist", "questlog"],
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
                   section of 'quests'. Marks the row ABANDONED \
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
            usage: "innate [<ability> [target]]",
            summary: "List or use your race's innate abilities.",
            long: "Without arguments, reads the 'RaceAbilities' rows for \
                   your character's race and prints each ability's name, \
                   category (PRIMARY / SECONDARY / ...), starting bonus, \
                   and proficiency cap. 'innate <ability> [target]' uses \
                   one of the active ones, exactly as if you had typed \
                   its own command (cast, breathe, ...); only abilities \
                   your race grants can be used this way.",
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
            long: "Reads the 'Quest' row for '(zone, id)' and prints \
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
            long: "Same as 'qload' but targets a named online player \
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
            summary: "Admin: force-complete an in-progress quest (coder+ pays rewards).",
            long: "Slot number from the 'quests' in-progress section. \
                   Flips IN_PROGRESS -> COMPLETED, stamps completed_at \
                   and bumps completion_count. Coder rank and above \
                   also get the quest's unconditional rewards exactly \
                   as a real completion pays them (conditional and \
                   choice rewards stay behind 'qreward'); lower ranks \
                   complete it without rewards. Objectives and phases \
                   are not run. Audited.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["qreset"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "qreset <player> <zone> <quest-id>",
            summary: "Admin: wipe a player's record of a quest.",
            long: "Deletes the character's whole record of the quest: \
                   status, objective progress and quest variables, so \
                   it can be given or accepted from scratch ('qload', \
                   'qgive' and 'qaccept' refuse while any record \
                   exists). Builders may reset their own character \
                   only; coder rank and above may reset anyone, \
                   including offline characters. Rewards already paid \
                   are not taken back. 'zone:id' is accepted in place \
                   of '<zone> <quest-id>'. Audited.",
        },
        run: cmd_mail_stub,
    }
}

/// `quests` / `qstat` / `qlist`: list quests the current character has
/// accepted, in-progress first then recently completed. Active rows
/// show the quest name + short description; completed rows show
/// completion count. Empty inbox = "no quests accepted."
pub(crate) async fn cmd_quests(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    let character_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    let Some(character_id) = character_id else {
        send_to(world, player, "No account info; can't fetch quests.\r\n");
        return;
    };
    let viewer_role = world
        .get::<Account>(player)
        .map_or(UserRole::Player, |a| a.role);
    let pool = pool.clone();
    // Off the tick, and ordered: whatever the player types next waits for
    // the listing instead of overtaking it.
    off_tick::run(
        world,
        player,
        async move { quests_text(&pool, &character_id, viewer_role).await },
        |world, player, text| send_to(world, player, text),
    )
    .await;
}

/// The `quests` listing: one query for the quests plus one per active
/// quest for its objectives, so it runs off the world loop.
async fn quests_text(
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
    viewer_role: UserRole,
) -> String {
    let rows = match mud_db::quests::list_for_character(pool, character_id).await {
        Ok(r) => r,
        Err(e) => return format!("Quest fetch failed: {e}\r\n"),
    };
    if rows.is_empty() {
        return "\r\nYou have no active or completed quests.\r\n".to_string();
    }
    let active: Vec<&mud_db::quests::CharacterQuestRow> =
        rows.iter().filter(|r| r.status == "IN_PROGRESS").collect();
    let other: Vec<&mud_db::quests::CharacterQuestRow> =
        rows.iter().filter(|r| r.status != "IN_PROGRESS").collect();
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
                        out.push_str(&format!("          {status} {}\r\n", r.player_description));
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
    out
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
    // Two stages, both off the tick, with the Lua gate on it between them:
    //  1. read the quest's availability requirement;
    //  2. (the gate passed) accept it. `accept_for_player` refuses a quest
    //     already held, so a stage that is cut off by the watchdog after it
    //     committed cannot accept twice.
    let chain = off_tick::Chain::default();
    let (read_pool, accept_pool) = (pool.clone(), pool.clone());
    let read_gate = async move {
        // Availability Lua gate (Wave 4.4): if the quest's
        // `availability_requirement` is non-null, evaluate it against
        // this player before consulting the DB. The DB layer can't do
        // this because it needs the live world. A failed read is left for
        // `accept_for_player` to report.
        mud_db::quests::get_quest(&read_pool, zone, id)
            .await
            .ok()
            .flatten()
            .and_then(|q| q.availability_requirement)
            .filter(|expr| !expr.trim().is_empty())
    };
    let next = chain.clone();
    off_tick::run(world, player, read_gate, move |world, player, gate| {
        if let Some(expr) = gate
            && !eval_quest_availability(
                world,
                player,
                &expr,
                &format!("quest ({zone}, {id}) availability requirement"),
            )
        {
            send_to(
                world,
                player,
                "You can't take that quest — you don't meet the requirements.\r\n",
            );
            return;
        }
        let accept = {
            let character_id = character_id.clone();
            async move {
                mud_db::quests::accept_for_player(&accept_pool, &character_id, level, zone, id)
                    .await
                    .map_err(|e| e.to_string())
            }
        };
        next.next(world, player, accept, move |world, player, outcome| {
            finish_accept(world, player, &character_id, zone, id, outcome);
        });
    })
    .await;
    chain.settle(world).await;
}

/// The tick-side half of `qaccept`: say how it went and, when the quest
/// was taken, tell the objectives tracker.
fn finish_accept(
    world: &mut World,
    player: Entity,
    character_id: &str,
    zone: i32,
    id: i32,
    outcome: Result<mud_db::quests::AcceptOutcome, String>,
) {
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            send_to(world, player, format!("DB error: {e}\r\n"));
            return;
        }
    };
    let accepted = outcome == mud_db::quests::AcceptOutcome::Accepted;
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
            "You've already finished that quest and it can't be repeated.\r\n".to_string()
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
            "You can't take that quest — you don't meet the requirements.\r\n".to_string()
        }
    };
    send_to(world, player, line);
    if accepted {
        // The row's variables were reset; items already in the pack
        // count towards a first-phase COLLECT objective.
        crate::quest_progress::on_quest_accepted(world, player, character_id, zone, id);
    }
}

/// Evaluate a quest gate expression (`availability_requirement` or a
/// reward `condition`) for `player`. The expression is wrapped in
/// `return (...)` so builders can author raw boolean expressions like
/// `actor.class == 'paladin'`.
///
/// Fails CLOSED: a script error or a non-boolean result denies, so a
/// typo can never hand a gated quest or reward to everyone. The
/// failure is logged with `label` (the quest, and the reward when
/// there is one) so a builder can find the broken row in syslog.
pub(crate) fn eval_quest_availability(
    world: &mut World,
    player: Entity,
    expr: &str,
    label: &str,
) -> bool {
    let body = format!("return ({expr})");
    // Asked from inside a running script: no host to evaluate with, so the
    // gate stays closed like any other failed evaluation.
    if crate::deferred_triggers::lua_busy(world) {
        tracing::warn!(gate = %label, "quest expression asked while a script runs; denying");
        return false;
    }
    let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
        host.exec_for_event_with_value(world, player, player, None, &body, &[])
    });
    match result {
        Ok((_out, Some(b))) => b,
        Ok((_out, None)) => {
            tracing::warn!(
                gate = %label,
                expr = %expr,
                "quest expression returned a non-boolean; denying"
            );
            false
        }
        Err(e) => {
            tracing::warn!(
                gate = %label,
                error = %e,
                expr = %expr,
                "quest expression failed; denying"
            );
            false
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
    let me = name_of(world, player);
    record_admin_action(world, player, "qload", &format!("{me} {zone}:{id}"));
    match mud_db::quests::admin_assign(pool, &character_id, zone, id).await {
        Ok(Some(_)) => {
            send_to(
                world,
                player,
                format!("Assigned Quest ({zone}, {id}) to your character.\r\n"),
            );
            crate::quest_progress::recheck_collect_objectives(world, player);
        }
        Ok(None) => send_to(
            world,
            player,
            format!("Already have Quest ({zone}, {id}).\r\n"),
        ),
        Err(e) => send_to(world, player, format!("Assign failed: {e}\r\n")),
    }
}

/// `innate`: list the caller race's innate abilities (`RaceAbilities`);
/// `innate <ability> [target]` uses one (see `commands::innate`).
pub(crate) async fn cmd_innate(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    if !args.trim().is_empty() {
        crate::commands::innate::use_innate(world, player, args);
        return;
    }
    if mud_world::bypasses_restrictions(world, player) {
        let actives = crate::commands::innate::racial_actives(world, player);
        let mut out = format!(
            "\r\nInnate abilities of every race ({}):\r\n",
            actives.len()
        );
        let names: Vec<String> = actives.iter().map(|d| d.name.clone()).collect();
        let width = crate::layout::wrap_width(world, player);
        let column_width = names.iter().map(String::len).max().unwrap_or(0) + 2;
        for row in crate::layout::format_grid(&names, column_width, 2, width) {
            out.push_str(&row);
            out.push_str("\r\n");
        }
        send_to(world, player, out);
        return;
    }
    let race = world.get::<Profile>(player).map(|p| p.race.clone());
    let Some(race) = race else {
        send_to(world, player, "You have no race assigned.\r\n");
        return;
    };
    let pool = pool.clone();
    off_tick::run(
        world,
        player,
        async move { innate_text(&pool, &race).await },
        |world, player, text| send_to(world, player, text),
    )
    .await;
}

async fn innate_text(pool: &mud_db::sqlx::PgPool, race: &str) -> String {
    let rows = match mud_db::race_abilities::list_for_race(pool, race).await {
        Ok(r) => r,
        Err(e) => return format!("Innate fetch failed: {e}\r\n"),
    };
    if rows.is_empty() {
        return format!("\r\nThe {race} race has no innate abilities.\r\n");
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
    out
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
    let viewer_role = world
        .get::<Account>(player)
        .map_or(UserRole::Player, |a| a.role);
    let pool = pool.clone();
    off_tick::run(
        world,
        player,
        async move { questinfo_text(&pool, zone, id, viewer_role).await },
        |world, player, text| send_to(world, player, text),
    )
    .await;
}

async fn questinfo_text(
    pool: &mud_db::sqlx::PgPool,
    zone: i32,
    id: i32,
    viewer_role: UserRole,
) -> String {
    let row = match mud_db::quests::get_quest(pool, zone, id).await {
        Ok(r) => r,
        Err(e) => return format!("Quest fetch failed: {e}\r\n"),
    };
    let Some(row) = row else {
        return format!("No Quest defined at ({zone}, {id}).\r\n");
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
    out
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
    record_admin_action(
        world,
        player,
        "qgive",
        &format!("{target_name} {zone}:{id}"),
    );
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
                    "An immortal grants you a quest: ({zone}, {id}). Type 'quests' to view.\r\n"
                ),
            );
            crate::quest_progress::recheck_collect_objectives(world, target);
        }
        Ok(None) => send_msg(
            world,
            player,
            format!("{target_name} already has Quest ({zone}, {id}).\r\n"),
        ),
        Err(e) => send_to(world, player, format!("Assign failed: {e}\r\n")),
    }
}

/// `qcomplete <#>`: admin command — force-complete an in-progress
/// quest the caller's character has accepted and pay its rewards.
/// Slot is 1-based against the `quests` in-progress section, same as
/// `abandon`. Useful for verifying the reward / completion flow
/// end-to-end without the full objective-resolution pipeline.
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
            "Complete which quest? Pick a number from 'quests'.\r\n",
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
    let pays = has_quest_admin_rank(world, player);
    let me = name_of(world, player);
    // Two stages: read the quest at the slot (off the tick), then on the
    // tick audit it and start the completion (see `start_completion`).
    let chain = off_tick::Chain::default();
    let read_pool = pool.clone();
    let write_pool = pool.clone();
    let read_cid = character_id.clone();
    let find = async move { in_progress_at(&read_pool, &read_cid, slot).await };
    let next = chain.clone();
    off_tick::run(world, player, find, move |world, player, found| {
        let target = match found {
            Ok(t) => t,
            Err(line) => {
                send_to(world, player, line);
                return;
            }
        };
        record_admin_action(
            world,
            player,
            "qcomplete",
            &format!(
                "{me} {}:{} rewards={}",
                target.quest_zone_id,
                target.quest_id,
                if pays { "paid" } else { "withheld" }
            ),
        );
        let done = start_completion(
            world,
            Completion {
                pool: write_pool,
                character_id,
                row_id: target.id,
                zone: target.quest_zone_id,
                quest: target.quest_id,
                name: target.quest_name,
                pays,
            },
        );
        next.next(world, player, done, |world, player, text| {
            send_to(world, player, text);
        });
    })
    .await;
    chain.settle(world).await;
}

/// The `slot`th in-progress quest of `character_id`, numbered as the
/// `quests` listing numbers them; the error is the line to show.
async fn in_progress_at(
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
    slot: usize,
) -> Result<mud_db::quests::CharacterQuestRow, String> {
    let rows = mud_db::quests::list_for_character(pool, character_id)
        .await
        .map_err(|e| format!("Quest fetch failed: {e}\r\n"))?;
    rows.into_iter()
        .filter(|r| r.status == "IN_PROGRESS")
        .nth(slot - 1)
        .ok_or_else(|| format!("No in-progress quest at slot {slot}.\r\n"))
}

/// Everything a forced completion needs, owned.
struct Completion {
    pool: mud_db::sqlx::PgPool,
    character_id: String,
    row_id: String,
    zone: i32,
    quest: i32,
    name: String,
    pays: bool,
}

/// Force-complete the quest and pay its rewards, exactly once, from a task
/// of its own that holds the character's write turn
/// ([`SaveCoordinator::spawn_ordered`]). The returned future resolves to the
/// text for the player; it is only the command's wait, not the work, so the
/// watchdog cutting the command off cannot stop the work between the
/// completion and the reward.
///
/// Exactly once:
/// * the status flip (`IN_PROGRESS` -> `COMPLETED`) is guarded, so a repeat
///   command finds nothing to complete and pays nothing;
/// * each numeric reward is a relative SQL update (`wealth = wealth + n`),
///   so it adds to whatever else changed the row, instead of writing an
///   absolute balance that a snapshot could have made stale;
/// * the matching in-memory deltas are queued while the turn is still held
///   (after `supersede_earlier_snapshots`), so no snapshot can write the old
///   balance over the reward and the next autosave carries it as ordinary
///   state, never a second time;
/// * items are spawned by the tick from the same queue and saved by the
///   normal save.
fn start_completion(
    world: &World,
    c: Completion,
) -> impl std::future::Future<Output = String> + Send + 'static + use<> {
    let (done, wait) = tokio::sync::oneshot::channel::<String>();
    let coordinator = world
        .get_resource::<crate::autosave::SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    let update_tx = world.get_resource::<PlayerUpdateTx>().map(|t| t.0.clone());
    let character_id = c.character_id.clone();
    coordinator.spawn_ordered(&character_id, move |mut ordered| async move {
        let text = match mud_db::quests::admin_complete(&c.pool, &c.row_id).await {
            Ok(0) => "That quest isn't in-progress; nothing to complete.\r\n".to_string(),
            Err(e) => format!("Complete failed: {e}\r\n"),
            Ok(_) => {
                let mut text = format!("Force-completed quest: {}.\r\n", c.name);
                if c.pays {
                    // Same payout as finishing the last objective - for ranks
                    // trusted with it.
                    let payout = crate::quest_progress::pay_completion_rewards(
                        &c.pool,
                        &c.character_id,
                        c.zone,
                        c.quest,
                    )
                    .await;
                    text.push_str(&payout.text());
                    if !payout.updates.is_empty()
                        && let Some(tx) = &update_tx
                    {
                        ordered.supersede_earlier_snapshots();
                        crate::quest_progress::deliver_under_turn(ordered, tx, payout.updates)
                            .await;
                    }
                } else {
                    text.push_str(
                        "Rewards withheld: paying quest rewards by hand needs coder rank.\r\n",
                    );
                }
                text
            }
        };
        report(done, update_tx, c.character_id, text).await;
    });
    async move {
        wait.await
            .unwrap_or_else(|_| "The completion was interrupted.\r\n".to_string())
    }
}

/// Hand the outcome text to the waiting command. If it gave up (the
/// watchdog released the player), print it through the update channel
/// instead, so the player still learns what happened.
pub(crate) async fn report(
    done: tokio::sync::oneshot::Sender<String>,
    update_tx: Option<tokio::sync::mpsc::Sender<PendingPlayerUpdate>>,
    character_id: String,
    text: String,
) {
    if let Err(text) = done.send(text)
        && let Some(tx) = update_tx
    {
        let _ = tx
            .send(PendingPlayerUpdate::Reply { character_id, text })
            .await;
    }
}

/// Rank needed to pay quest rewards by hand (`qcomplete`) or to touch
/// another player's quest record (`qreset`). Builders get the testing
/// tools on their own character only, so they cannot mint XP, gold or
/// items for themselves or anyone else.
const QUEST_ADMIN_RANK: UserRole = UserRole::Coder;

fn has_quest_admin_rank(world: &World, player: Entity) -> bool {
    world
        .get::<Account>(player)
        .is_some_and(|a| a.role.at_least(QUEST_ADMIN_RANK))
}

/// Parse a quest reference given as `<zone> <id>` or `<zone>:<id>`.
fn parse_quest_ref<'a>(mut parts: impl Iterator<Item = &'a str>) -> Option<(i32, i32)> {
    let first = parts.next()?;
    if let Some((z, i)) = first.split_once(':') {
        return Some((z.parse().ok()?, i.parse().ok()?));
    }
    Some((first.parse().ok()?, parts.next()?.parse().ok()?))
}

/// `qreset <player> <zone> <quest-id>`: staff wipe of one character's
/// record of a quest. Works on offline characters (resolved by name).
pub(crate) async fn cmd_qreset(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let Some(target_word) = parts.next() else {
        send_to(
            world,
            player,
            "Usage: qreset <player> <zone> <quest-id>\r\n",
        );
        return;
    };
    let Some((zone, id)) = parse_quest_ref(parts) else {
        send_to(
            world,
            player,
            "Usage: qreset <player> <zone> <quest-id>\r\n",
        );
        return;
    };
    let online = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(target_word))
            .map(|(e, _)| e)
    };
    let character_id = match online {
        Some(e) => world.get::<Account>(e).map(|a| a.character_id.clone()),
        None => match mud_db::characters::find_by_name(pool, target_word).await {
            Ok(row) => row.map(|r| r.id),
            Err(e) => {
                send_to(world, player, format!("Character lookup failed: {e}\r\n"));
                return;
            }
        },
    };
    let Some(character_id) = character_id else {
        send_to(
            world,
            player,
            format!("No character named '{target_word}'.\r\n"),
        );
        return;
    };
    let own_id = world.get::<Account>(player).map(|a| a.character_id.clone());
    if own_id.as_deref() != Some(character_id.as_str()) && !has_quest_admin_rank(world, player) {
        send_to(
            world,
            player,
            "You can only reset your own quests; resetting another player's needs coder rank.\r\n",
        );
        return;
    }
    let me = name_of(world, player);
    record_admin_action(
        world,
        player,
        "qreset",
        &format!("{me} -> {target_word} {zone}:{id}"),
    );
    match mud_db::quests::admin_reset(pool, &character_id, zone, id).await {
        Ok(0) => send_msg(
            world,
            player,
            format!("{target_word} has no record of Quest ({zone}, {id}).\r\n"),
        ),
        Ok(_) => {
            // Drop the cached script variables too, so a fresh run
            // does not inherit them (or flush them back).
            if let Some(mut cache) = world.get_resource_mut::<mud_world::QuestVariableCache>() {
                cache.reset_quest(&character_id, zone, id);
            }
            send_to(
                world,
                player,
                format!("Reset Quest ({zone}, {id}) for {target_word}.\r\n"),
            );
            if let Some(e) = online.filter(|&e| e != player) {
                send_to(
                    world,
                    e,
                    format!("An immortal resets your progress on quest ({zone}, {id}).\r\n"),
                );
            }
        }
        Err(e) => send_to(world, player, format!("Reset failed: {e}\r\n")),
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
            "Abandon which quest? Pick a number from 'quests'.\r\n",
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
    // The flip is guarded on the row still being in progress, so a repeat
    // after a watchdog cut-off finds nothing to abandon.
    let pool = pool.clone();
    let work = async move {
        let target = match in_progress_at(&pool, &character_id, slot).await {
            Ok(t) => t,
            Err(line) => return line,
        };
        match mud_db::quests::abandon(&pool, &target.id).await {
            Ok(0) => "That quest isn't in-progress; nothing to abandon.\r\n".to_string(),
            Ok(_) => format!("Abandoned quest: {}.\r\n", target.quest_name),
            Err(e) => format!("Abandon failed: {e}\r\n"),
        }
    };
    off_tick::run(world, player, work, |world, player, text| {
        send_to(world, player, text);
    })
    .await;
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
        (None, _, _) => {
            let pool = pool.clone();
            off_tick::run(
                world,
                player,
                async move { qreward_list_all(&pool, &character_id).await },
                |world, player, text| send_to(world, player, text),
            )
            .await;
        }
        (Some(z), Some(i), None) => {
            let (Ok(zone), Ok(id)) = (z.parse::<i32>(), i.parse::<i32>()) else {
                send_to(world, player, "Zone and id must be integers.\r\n");
                return;
            };
            let pool = pool.clone();
            off_tick::run(
                world,
                player,
                async move { qreward_list_one(&pool, &character_id, zone, id).await },
                |world, player, text| send_to(world, player, text),
            )
            .await;
        }
        (Some(z), Some(i), Some(r)) => {
            let (Ok(zone), Ok(id), Ok(reward_id)) =
                (z.parse::<i32>(), i.parse::<i32>(), r.parse::<i32>())
            else {
                send_to(
                    world,
                    player,
                    "Zone, id, and reward-id must be integers.\r\n",
                );
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
#[allow(clippy::cast_possible_truncation)]
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
async fn qreward_list_all(pool: &mud_db::sqlx::PgPool, character_id: &str) -> String {
    let quests = match mud_db::quests::list_for_character(pool, character_id).await {
        Ok(r) => r,
        Err(e) => return format!("Quest fetch failed: {e}\r\n"),
    };
    let mut buf = String::from("\r\nUnclaimed rewards:\r\n");
    let mut printed_any = false;
    for q in &quests {
        if q.status != "COMPLETED" {
            continue;
        }
        let choices =
            mud_db::quest_objectives::list_choice_rewards(pool, q.quest_zone_id, q.quest_id)
                .await
                .unwrap_or_default();
        let conditionals =
            mud_db::quest_objectives::list_conditional_rewards(pool, q.quest_zone_id, q.quest_id)
                .await
                .unwrap_or_default();
        if choices.is_empty() && conditionals.is_empty() {
            continue;
        }
        let claimed = claimed_rewards_from(&q.variables);
        // Choice-group section.
        let mut groups: std::collections::BTreeMap<
            i32,
            Vec<&mud_db::quest_objectives::QuestRewardRow>,
        > = std::collections::BTreeMap::new();
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
                buf.push_str(&format!("    [{}] {}\r\n", r.id, describe_reward(r)));
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
                buf.push_str(&format!("    [{}] {}\r\n", r.id, describe_reward(r)));
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
        return msg.to_string();
    }
    buf
}

/// List choice groups for one specific quest. Refuses if the
/// quest isn't COMPLETED for the caller.
async fn qreward_list_one(
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
    zone: i32,
    id: i32,
) -> String {
    let Some((_cqid, status, vars)) =
        find_character_quest_with_vars(pool, character_id, zone, id).await
    else {
        return format!("You haven't completed quest ({zone}, {id}).\r\n");
    };
    if status != "COMPLETED" {
        return "That quest isn't complete yet.\r\n".to_string();
    }
    let choices = mud_db::quest_objectives::list_choice_rewards(pool, zone, id)
        .await
        .unwrap_or_default();
    let conditionals = mud_db::quest_objectives::list_conditional_rewards(pool, zone, id)
        .await
        .unwrap_or_default();
    if choices.is_empty() && conditionals.is_empty() {
        return format!("Quest ({zone}, {id}) has no claimable rewards.\r\n");
    }
    let claimed = claimed_rewards_from(&vars);
    let mut buf = format!("\r\nRewards for quest ({zone}, {id}):\r\n");
    let mut groups: std::collections::BTreeMap<
        i32,
        Vec<&mud_db::quest_objectives::QuestRewardRow>,
    > = std::collections::BTreeMap::new();
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
    buf
}

/// Claim a single reward from a choice group. Validates the reward
/// is part of the quest, the player has completed the quest, the
/// group hasn't already been claimed, and the reward's `condition`
/// Lua (if any) is truthy. On success: grants the reward and
/// stamps `claimed_rewards` in the per-quest variables JSON.
///
/// Two stages, both off the tick, with the Lua condition on it between
/// them: read and check everything the database knows, then claim (see
/// `start_claim`).
async fn qreward_claim(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
    zone: i32,
    id: i32,
    reward_id: i32,
) {
    let chain = off_tick::Chain::default();
    let (read_pool, claim_pool) = (pool.clone(), pool.clone());
    let read_cid = character_id.to_string();
    let plan = async move { plan_claim(&read_pool, &read_cid, zone, id, reward_id).await };
    let next = chain.clone();
    let character_id = character_id.to_string();
    off_tick::run(world, player, plan, move |world, player, planned| {
        let plan = match planned {
            Ok(p) => p,
            Err(line) => {
                send_to(world, player, line);
                return;
            }
        };
        // Condition gate (Wave 4.10). Treat empty / missing as truthy.
        // Echo the actual condition expression on failure so the player
        // (and any staffer they ask) can see *why* — the legacy "you
        // don't meet that reward's conditions" was opaque when a player
        // re-classed and a class-locked reward silently refused.
        if let Some(expr) = plan.reward.condition.as_deref()
            && !expr.trim().is_empty()
            && !eval_quest_availability(
                world,
                player,
                expr,
                &format!("quest ({zone}, {id}) reward {reward_id} condition"),
            )
        {
            send_to(
                world,
                player,
                format!("You don't currently meet that reward's condition: {expr}\r\n"),
            );
            return;
        }
        let done = start_claim(world, claim_pool, character_id, zone, id, plan);
        next.next(world, player, done, |world, player, text| {
            send_to(world, player, text);
        });
    })
    .await;
    chain.settle(world).await;
}

/// What the read stage of a `qreward` claim found: the quest row to lock,
/// the reward, and the rewards whose prior claim blocks this one.
pub(crate) struct ClaimPlan {
    cqid: String,
    reward: mud_db::quest_objectives::QuestRewardRow,
    blocking_ids: Vec<i32>,
    claimed: Vec<i32>,
}

/// Everything about a claim that needs no world: `Err` is the line to show.
pub(crate) async fn plan_claim(
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
    zone: i32,
    id: i32,
    reward_id: i32,
) -> Result<ClaimPlan, String> {
    let Some((cqid, status, vars)) =
        find_character_quest_with_vars(pool, character_id, zone, id).await
    else {
        return Err(format!("You haven't completed quest ({zone}, {id}).\r\n"));
    };
    if status != "COMPLETED" {
        return Err("That quest isn't complete yet.\r\n".to_string());
    }
    let reward = match mud_db::quest_objectives::get_reward(pool, reward_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(format!("No reward with id {reward_id}.\r\n")),
        Err(e) => return Err(format!("Reward fetch failed: {e}\r\n")),
    };
    // Make sure the reward is from THIS quest (defense against
    // typo'd reward ids that happen to exist on some other quest).
    // Conditional non-choice rewards live alongside choice-group
    // rewards under the qreward umbrella.
    let all_choices = mud_db::quest_objectives::list_choice_rewards(pool, zone, id)
        .await
        .unwrap_or_default();
    let all_conditionals = mud_db::quest_objectives::list_conditional_rewards(pool, zone, id)
        .await
        .unwrap_or_default();
    let is_choice = all_choices.iter().any(|r| r.id == reward.id);
    let is_conditional = all_conditionals.iter().any(|r| r.id == reward.id);
    if !is_choice && !is_conditional {
        return Err(format!(
            "Reward {reward_id} isn't a claimable reward of quest ({zone}, {id}).\r\n"
        ));
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
            return Err(format!(
                "You've already claimed a reward from group {group}.\r\n"
            ));
        }
    } else if claimed.contains(&reward.id) {
        return Err("You've already claimed that reward.\r\n".to_string());
    }
    let blocking_ids: Vec<i32> = match reward.choice_group {
        Some(group) => all_choices
            .iter()
            .filter(|r| r.choice_group == Some(group))
            .map(|r| r.id)
            .collect(),
        None => vec![reward.id],
    };
    Ok(ClaimPlan {
        cqid,
        reward,
        blocking_ids,
        claimed,
    })
}

/// Grant the reward and record the claim in ONE transaction, then mirror it
/// to the live character, exactly once, from a task of its own that holds
/// the character's write turn (see [`start_completion`] for the design; the
/// claim is `claim_reward`, which locks the quest row, so two simultaneous
/// claims serialize and only the first wins, and a failed write of the claim
/// record rolls the grant back). ITEM rewards are spawned by the tick from
/// the update queue and saved by the normal save.
pub(crate) fn start_claim(
    world: &World,
    pool: mud_db::sqlx::PgPool,
    character_id: String,
    zone: i32,
    id: i32,
    plan: ClaimPlan,
) -> impl std::future::Future<Output = String> + Send + 'static + use<> {
    let (done, wait) = tokio::sync::oneshot::channel::<String>();
    let coordinator = world
        .get_resource::<crate::autosave::SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    let update_tx = world.get_resource::<PlayerUpdateTx>().map(|t| t.0.clone());
    let turn_for = character_id.clone();
    coordinator.spawn_ordered(&turn_for, move |mut ordered| async move {
        let ClaimPlan {
            cqid,
            reward,
            blocking_ids,
            claimed,
        } = plan;
        let outcome = match mud_db::quest_objectives::claim_reward(
            &pool,
            &character_id,
            &cqid,
            &reward,
            &blocking_ids,
        )
        .await
        {
            Ok(mud_db::quest_objectives::ClaimOutcome::Claimed(o)) => o,
            other => {
                drop(ordered);
                let text = match other {
                    Ok(mud_db::quest_objectives::ClaimOutcome::AlreadyClaimed) => {
                        "You've already claimed that reward.\r\n".to_string()
                    }
                    Ok(_) => format!("You haven't completed quest ({zone}, {id}).\r\n"),
                    Err(e) => {
                        tracing::warn!(error = %e, "qreward: claim failed");
                        format!("Reward grant failed: {e}\r\n")
                    }
                };
                report(done, update_tx, character_id, text).await;
                return;
            }
        };
        // Mirror live ECS state for the running player. SpawnItem only
        // works for ITEM; the rest update Profile / Wealth / etc.
        let mut updates = Vec::new();
        if outcome.house_created
            && let Some(granted) =
                crate::quest_progress::house_granted_update(&pool, &character_id).await
        {
            updates.push(granted);
        }
        updates.extend(crate::quest_progress::reward_update(&character_id, &reward));
        match &update_tx {
            Some(tx) if !updates.is_empty() => {
                ordered.supersede_earlier_snapshots();
                crate::quest_progress::deliver_under_turn(ordered, tx, updates).await;
            }
            _ => drop(ordered),
        }
        // The claim was recorded by `claim_reward`; mirror it for the tally.
        let mut new_claimed = claimed;
        new_claimed.push(reward.id);
        // Remaining-claims tally across all of this player's completed
        // quests: unfinished choice groups + unclaimed conditionals.
        // Cheap-ish (one DB pass per completed quest) but bounded by
        // content size, which is small relative to a single combat round.
        let remaining = count_pending_claims(&pool, &character_id, &new_claimed).await;
        let mut msg = format!("You claim your reward: {}.\r\n", describe_reward(&reward));
        if reward.reward_type == "HOUSING" && !outcome.house_created {
            msg.push_str("You already own a house, so nothing more is added.\r\n");
        }
        msg.push_str(&match remaining {
            0 => "All your conditional / choice rewards are now claimed.\r\n".to_string(),
            1 => "1 reward still pending — type 'qreward' to view.\r\n".to_string(),
            n => format!("{n} rewards still pending — type 'qreward' to view.\r\n"),
        });
        report(done, update_tx, character_id, msg).await;
    });
    async move {
        wait.await
            .unwrap_or_else(|_| "The claim was interrupted.\r\n".to_string())
    }
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
    let Ok(quests) = mud_db::quests::list_for_character(pool, character_id).await else {
        return 0;
    };
    let mut total = 0usize;
    for q in &quests {
        if q.status != "COMPLETED" {
            continue;
        }
        let choices =
            mud_db::quest_objectives::list_choice_rewards(pool, q.quest_zone_id, q.quest_id)
                .await
                .unwrap_or_default();
        let conditionals =
            mud_db::quest_objectives::list_conditional_rewards(pool, q.quest_zone_id, q.quest_id)
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
        for ids in groups.values() {
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
    let rows = mud_db::quests::list_for_character(pool, character_id)
        .await
        .ok()?;
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
        ("HOUSING", _, _) => "a house of your own".to_string(),
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
        assert_eq!(describe_reward(&r), "a house of your own");
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
    // Both the `qaccept` availability check and the `qreward` condition
    // gate route through `eval_quest_availability`. A full e2e test
    // would need a quest row + completed CharacterQuest +
    // Lua-evaluable `actor` userdata backed by Profile/Class/Wealth
    // components, so test the gate directly with literal expressions:
    // true grants, false denies, and anything else (script error,
    // non-boolean) DENIES.

    fn gate(expr: &str) -> bool {
        let mut world = World::new();
        world.insert_resource(mud_script::LuaHost::default());
        let entity = world.spawn(()).id();
        eval_quest_availability(&mut world, entity, expr, "quest (30, 1) test gate")
    }

    #[test]
    fn eval_quest_availability_literal_true_returns_true() {
        assert!(gate("true"));
    }

    #[test]
    fn eval_quest_availability_literal_false_returns_false() {
        assert!(!gate("false"));
    }

    #[test]
    fn eval_quest_availability_arithmetic_predicate() {
        assert!(gate("1 + 1 == 2"));
        assert!(!gate("1 + 1 == 3"));
    }

    #[test]
    fn eval_quest_availability_compile_error_denies() {
        // Author-error: unbalanced paren. Wrapped in `return (...)`,
        // so the body is `return ((` — a compile error. The gate
        // must deny rather than let everyone through.
        assert!(!gate("((unbalanced"));
    }

    #[test]
    fn eval_quest_availability_runtime_error_denies() {
        // `character` is not a variable (the player is `actor`), so
        // indexing it raises at runtime.
        assert!(!gate("character.class == 'paladin'"));
        assert!(!gate("error('boom')"));
    }

    #[test]
    fn eval_quest_availability_non_boolean_denies() {
        assert!(!gate("1"));
        assert!(!gate("'yes'"));
        assert!(!gate("nil"));
    }
}

#[cfg(test)]
mod innate_listing_tests {
    use super::*;
    use crate::commands::test_support::{ability_def, drain, player_in};
    use mud_db::abilities::AbilityKind;
    use mud_db::enums::UserRole;
    use mud_world::{AbilityCatalog, Account, RaceAbilitiesData};

    /// The god's every-race `innate` listing prints display names, not the
    /// raw `plain_name` ids such as `INN_SYLL`.
    #[tokio::test]
    async fn god_innate_listing_shows_display_names() {
        let mut world = World::new();
        let mut catalog = AbilityCatalog::default();
        let mut def = ability_def(7, "Innate Sylvan", AbilityKind::Spell);
        def.plain_name = "INN_SYLL".to_string();
        def.passive = false;
        catalog.by_name.insert("inn_syll".to_string(), def);
        world.insert_resource(catalog);
        let mut data = RaceAbilitiesData::default();
        data.insert("ELF", 7, 100);
        world.insert_resource(data);
        let room = world.spawn_empty().id();
        let (god, mut rx) = player_in(&mut world, room);
        world.entity_mut(god).insert(Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: UserRole::Immortal,
            account_role: UserRole::Immortal,
            perms: vec![],
        });
        // Never touched: the god listing is answered from the catalog.
        let pool = mud_db::sqlx::PgPool::connect_lazy("postgres://nobody@localhost/none")
            .expect("lazy pool");
        cmd_innate(&mut world, god, &pool, "").await;
        let out = drain(&mut rx);
        assert!(out.contains("Innate Sylvan"), "{out}");
        assert!(!out.contains("INN_SYLL"), "{out}");
    }
}

#[cfg(test)]
mod off_tick_tests {
    use super::*;
    use crate::commands::off_tick::{CommandJobs, drain_command_jobs, is_busy};
    use crate::commands::test_support::{drain, player_in};

    /// `quests` hits the database off the tick and the player's next line
    /// waits for the listing. The unreachable pool fails the lookup after
    /// 200 ms, which is the slow database here.
    #[tokio::test(flavor = "current_thread")]
    async fn quests_answers_off_the_tick_and_keeps_the_next_line_behind_it() {
        let mut world = World::new();
        world.init_resource::<CommandJobs>();
        world.init_resource::<mud_world::SocialRegistry>();
        let room = world
            .spawn((
                Room,
                Named {
                    name: "A quiet hall".into(),
                },
            ))
            .id();
        let (player, mut rx) = player_in(&mut world, room);
        world.entity_mut(player).insert(Account {
            user_id: "u".into(),
            character_id: "c-quests".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        });
        let pool = mud_db::sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(200))
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap();
        let started = std::time::Instant::now();
        crate::commands::dispatch_with_async(&mut world, player, &pool, "questlog").await;
        crate::commands::dispatch_with_async(&mut world, player, &pool, "look").await;
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        assert_eq!(
            drain(&mut rx),
            "",
            "nothing yet, and look has not overtaken it"
        );
        assert!(is_busy(&world, player));
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        drain_command_jobs(&mut world);
        assert!(drain(&mut rx).contains("Quest fetch failed"));
        assert!(crate::commands::run_queued_input(&mut world, player, &pool).await);
        assert!(drain(&mut rx).contains("A quiet hall"));
    }

    /// Every listing and quest write that waits on the database answers
    /// before the line typed after it runs, however many stages it has
    /// (the two-stage ones hand over on a later tick). The unreachable pool
    /// fails each stage after 100 ms, which is the slow database here.
    #[tokio::test(flavor = "current_thread")]
    async fn listings_and_quest_writes_keep_command_order() {
        let mut world = World::new();
        world.init_resource::<CommandJobs>();
        world.init_resource::<mud_world::SocialRegistry>();
        let room = world
            .spawn((
                Room,
                Named {
                    name: "A quiet hall".into(),
                },
            ))
            .id();
        let (player, mut rx) = player_in(&mut world, room);
        world.entity_mut(player).insert((
            Account {
                user_id: "u".into(),
                character_id: "c-order".into(),
                role: UserRole::Builder,
                account_role: UserRole::Builder,
                perms: vec![],
            },
            Profile {
                level: 10,
                class_id: None,
                race: "ELF".into(),
                experience: 0,
                gender: "neutral".into(),
            },
        ));
        let pool = mud_db::sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(100))
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap();
        for cmd in [
            "wizlist",
            "questinfo 1 1",
            "innate",
            "abandon 1",
            "qaccept 1 1",
            "qcomplete 1",
            "qreward",
            "qreward 1 1",
            "qreward 1 1 5",
        ] {
            // `innate` lists from the catalog for staff; a mortal's list is
            // read from the database.
            let role = if cmd == "innate" {
                UserRole::Player
            } else {
                UserRole::Builder
            };
            if let Some(mut account) = world.get_mut::<Account>(player) {
                account.role = role;
                account.account_role = role;
            }
            crate::commands::dispatch_with_async(&mut world, player, &pool, cmd).await;
            crate::commands::dispatch_with_async(&mut world, player, &pool, "look").await;
            assert!(is_busy(&world, player), "{cmd}: {}", drain(&mut rx));
            assert_eq!(drain(&mut rx), "", "{cmd}: look overtook it");
            let mut out = String::new();
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                drain_command_jobs(&mut world);
                out.push_str(&drain(&mut rx));
                if !is_busy(&world, player) {
                    break;
                }
            }
            assert!(!is_busy(&world, player), "{cmd}: never finished");
            assert!(!out.is_empty(), "{cmd}: said nothing");
            assert!(!out.contains("A quiet hall"), "{cmd}: {out}");
            assert!(crate::commands::run_queued_input(&mut world, player, &pool).await);
            assert!(drain(&mut rx).contains("A quiet hall"), "{cmd}");
        }
    }
}
