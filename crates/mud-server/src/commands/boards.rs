//! Message boards: `boards`, `board`, `post`, `delpost`, `editpost`,
//! plus the async handler bodies and composer step.

#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::wildcard_imports)]

use bevy_ecs::prelude::{Entity, With, World};
use mud_db::enums::UserRole;
use mud_world::*;

use crate::commands::off_tick;
use crate::commands::*;

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, args| match head {
            "boards" => Some(Box::pin(cmd_boards(world, player, pool))),
            "board" => Some(Box::pin(cmd_board(world, player, pool, args))),
            "post" => Some(Box::pin(cmd_post(world, player, pool, args))),
            "editpost" => Some(Box::pin(cmd_editpost(world, player, pool, args))),
            "delpost" => Some(Box::pin(cmd_delpost(world, player, pool, args))),
            // Numeric `read <#>` while standing near a board → render
            // that message body. Non-numeric `read` falls through to
            // the sync handler.
            "read" if args.trim().parse::<usize>().is_ok() => {
                let target_room = world.get::<Located>(player).map(|l| l.0);
                let board_id = target_room.and_then(|room| {
                    let mut q = world.query_filtered::<(&Located, &BoardLink), With<Item>>();
                    q.iter(world).find(|(l, _)| l.0 == room).map(|(_, b)| b.0)
                })?;
                Some(Box::pin(cmd_read_board_msg(world, player, pool, board_id, args)))
            }
            // `look`/`examine <keyword>` resolving to a BOARD-tagged
            // item in the room → render the board listing inline.
            // Falls through when no match so plain look/examine still
            // works on non-board targets.
            "look" | "examine" | "exa" => {
                let target_room = world.get::<Located>(player).map(|l| l.0);
                let needle = args.trim().to_ascii_lowercase();
                if needle.is_empty() {
                    return None;
                }
                let board_id = target_room.and_then(|room| {
                    let mut q = world.query_filtered::<
                        (&Located, &Named, Option<&Keywords>, &BoardLink),
                        With<Item>,
                    >();
                    q.iter(world)
                        .find(|(l, n, kw, _)| l.0 == room && matches(&needle, n, *kw))
                        .map(|(_, _, _, b)| b.0)
                })?;
                Some(Box::pin(cmd_look_board(world, player, pool, board_id)))
            }
            _ => None,
        },
    }
}

inventory::submit! {
    Command {
        names: &["boards"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Mail,
        help: Help {
            usage: "boards",
            summary: "List every public message board.",
            long: "Each board has an alias ('mortal', 'god', 'quest', \
                   etc.) and a title. Use 'board <alias>' to list \
                   messages on one.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["board"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Mail,
        help: Help {
            usage: "board <alias> [#]",
            summary: "List or read messages on a board.",
            long: "With just an alias, lists messages newest first \
                   (sticky-marked entries float to the top). Append \
                   a slot number to read that message's body.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["post"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Mail,
        help: Help {
            usage: "post <board-alias>",
            summary: "Compose a new message on a board.",
            long: "Opens a multi-line composition session: first \
                   non-blank line is the subject, subsequent lines \
                   accumulate as body. '.send' ships, '.abort' \
                   cancels, '.preview' shows the draft. Locked \
                   boards refuse the open.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["delpost"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Mail,
        help: Help {
            usage: "delpost <board-alias> <#>",
            summary: "Delete a board message (yours, or any if Builder+).",
            long: "Hard-deletes the row at the given slot. Players \
                   can only delete posts they made (case-insensitive \
                   poster-name match); Builder-and-above can delete \
                   anyone's. Edit history ('BoardMessageEdit') cascades.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["editpost"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Mail,
        help: Help {
            usage: "editpost <board-alias> <#>",
            summary: "Re-open a board post for editing.",
            long: "Pre-loads the existing subject and body into a \
                   composition session. Add lines to append, or \
                   '.clear' to wipe before writing the new body. \
                   Players can only edit their own posts; Builder+ \
                   bypasses the gate.",
        },
        run: cmd_mail_stub,
    }
}

/// Process one line of input from a player who has an active
/// `BoardDraft`. Same control verbs as mail (`.send` / `.abort` /
/// `.preview`), same first-line-is-subject rule.
#[allow(clippy::too_many_lines)]
pub(crate) async fn compose_board_step(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    line: &str,
) {
    let trimmed = line.trim();
    if trimmed.eq_ignore_ascii_case(".abort") {
        try_remove::<BoardDraft>(world, player);
        send_to(world, player, "Board post aborted.\r\n");
        return;
    }
    if trimmed.eq_ignore_ascii_case(".clear") {
        if let Some(mut draft) = world.get_mut::<BoardDraft>(player) {
            draft.subject = None;
            draft.body.clear();
        }
        send_to(
            world,
            player,
            "Cleared. Type a new subject, then the body.\r\n",
        );
        return;
    }
    if trimmed.eq_ignore_ascii_case(".preview") {
        let Some(draft) = world.get::<BoardDraft>(player).cloned() else {
            return;
        };
        let mut out = String::from("\r\n--- DRAFT ---\r\n");
        out.push_str(&format!(
            "Board:   {} ({})\r\n",
            draft.board_title, draft.board_alias
        ));
        out.push_str(&format!(
            "Subject: {}\r\n",
            draft.subject.as_deref().unwrap_or("(none yet)"),
        ));
        out.push_str("---\r\n");
        if draft.body.is_empty() {
            out.push_str("(empty body)\r\n");
        } else {
            for ln in &draft.body {
                out.push_str(ln);
                out.push_str("\r\n");
            }
        }
        out.push_str("--- end of draft ---\r\n");
        send_to(world, player, out);
        return;
    }
    if trimmed.eq_ignore_ascii_case(".send") {
        let Some(draft) = world.get::<BoardDraft>(player).cloned() else {
            return;
        };
        let Some(subject) = draft.subject.clone() else {
            send_to(
                world,
                player,
                "No subject set yet — type a subject line first.\r\n",
            );
            return;
        };
        if draft.body.is_empty() {
            send_to(world, player, "Body is empty — type some lines first.\r\n");
            return;
        }
        let body = draft.body.join("\n");
        let caller = Caller::of(world, player);
        let level = world.get::<Profile>(player).map_or(1, |p| p.level);
        let pool = pool.clone();
        let persist = {
            let (board_id, edit_id) = (draft.board_id, draft.edit_message_id);
            async move { persist_draft(&pool, board_id, edit_id, &caller, level, &subject, &body).await }
        };
        send_draft(world, player, draft, persist).await;
        return;
    }
    let step = if let Some(mut draft) = world.get_mut::<BoardDraft>(player) {
        if draft.subject.is_none() {
            if trimmed.is_empty() {
                ComposeStep::Nudge
            } else {
                draft.subject = Some(mud_net::sanitize_text(trimmed, false).into_owned());
                ComposeStep::SubjectSet
            }
        } else {
            draft
                .body
                .push(mud_net::sanitize_text(line, false).into_owned());
            ComposeStep::BodyAdded
        }
    } else {
        return;
    };
    match step {
        ComposeStep::Nudge => send_to(
            world,
            player,
            "Type a subject line, then the body. '.send' to ship; '.abort' to cancel.\r\n",
        ),
        ComposeStep::SubjectSet => send_to(
            world,
            player,
            "Subject set. Type the body, one line at a time. \
             '.send' to ship, '.abort' to cancel, '.preview' to review.\r\n",
        ),
        ComposeStep::BodyAdded => {}
    }
}

/// Shown when the write refuses an edit the draft was opened for.
const EDIT_REFUSED: &str =
    "That post can no longer be edited (the board is locked, it was removed, or it isn't yours).";

/// Store a finished draft: a new post, or the edit of `edit_id`. The edit's
/// permission (poster, board unlocked; staff bypass both) is checked again
/// by the write itself, because the check made when the draft was opened can
/// be stale by now.
async fn persist_draft(
    pool: &mud_db::sqlx::PgPool,
    board_id: i32,
    edit_id: Option<i32>,
    caller: &Caller,
    level: i32,
    subject: &str,
    body: &str,
) -> Result<i32, String> {
    if let Some(edit_id) = edit_id {
        return match mud_db::boards::update_message(
            pool,
            edit_id,
            subject,
            body,
            &caller.name,
            caller.is_builder,
        )
        .await
        {
            Ok(0) => Err(EDIT_REFUSED.to_string()),
            Ok(_) => Ok(edit_id),
            Err(e) => Err(e.to_string()),
        };
    }
    mud_db::boards::post_message(pool, board_id, &caller.name, level, subject, body)
        .await
        .map_err(|e| e.to_string())
}

/// Ship a finished draft: the database write runs off the tick
/// (`off_tick`), and the draft leaves the player *now*, not when the write
/// is confirmed. If the answer never comes back (a stuck database, the
/// watchdog, a dropped link) the post may or may not have been stored, and
/// a draft left behind would be sent a second time. A failed write hands
/// the draft back.
async fn send_draft<Fut>(world: &mut World, player: Entity, draft: BoardDraft, persist: Fut)
where
    Fut: std::future::Future<Output = Result<i32, String>> + Send + 'static,
{
    try_remove::<BoardDraft>(world, player);
    off_tick::run(
        world,
        player,
        persist,
        move |world, player, result| match result {
            Ok(_id) => {
                let verb = if draft.edit_message_id.is_some() {
                    "Updated"
                } else {
                    "Posted"
                };
                send_msg(
                    world,
                    player,
                    format!(
                        "{verb} on {} ({}).\r\n",
                        draft.board_title, draft.board_alias
                    ),
                );
            }
            Err(e) => {
                try_insert(world, player, draft);
                send_to(world, player, format!("Save failed: {e}\r\n"));
            }
        },
    )
    .await;
}

/// What a board command shows: plain text, or message prose that wraps.
enum Shown {
    Plain(String),
    Prose(String),
}

fn show(world: &mut World, player: Entity, shown: Shown) {
    match shown {
        Shown::Plain(text) => send_to(world, player, text),
        Shown::Prose(text) => send_prose(world, player, text),
    }
}

/// Resolve `alias` to a board the caller may address. `allowed` is the
/// mortal scope gate ([`local_board_ids`]); `lookup_failed` heads the line
/// shown when the database errors; `out_of_reach` is shown when a mortal
/// names a board that is not in their room.
async fn resolve_board(
    pool: &mud_db::sqlx::PgPool,
    alias: &str,
    allowed: Option<&std::collections::HashSet<i32>>,
    lookup_failed: &str,
    out_of_reach: &str,
) -> Result<mud_db::boards::Board, String> {
    let board = match mud_db::boards::find_board_by_alias(pool, alias).await {
        Ok(Some(b)) => b,
        Ok(None) => return Err(format!("No board called '{alias}'.\r\n")),
        Err(e) => return Err(format!("{lookup_failed}: {e}\r\n")),
    };
    if let Some(allowed) = allowed
        && !allowed.contains(&board.id)
    {
        return Err(out_of_reach.to_string());
    }
    Ok(board)
}

async fn board_messages(
    pool: &mud_db::sqlx::PgPool,
    board_id: i32,
) -> Result<Vec<mud_db::boards::BoardMessage>, String> {
    mud_db::boards::messages_for_board(pool, board_id)
        .await
        .map_err(|e| format!("Message fetch failed: {e}\r\n"))
}

/// Parse `<board-alias> <#>`; the error is the line to show.
fn alias_and_slot<'a>(args: &'a str, usage: &str) -> Result<(&'a str, usize), String> {
    let mut parts = args.split_whitespace();
    let (Some(alias), Some(slot_raw)) = (parts.next(), parts.next()) else {
        return Err(format!("Usage: {usage}\r\n"));
    };
    let Ok(slot) = slot_raw.parse::<usize>() else {
        return Err("Slot number must be a positive integer.\r\n".to_string());
    };
    if slot == 0 {
        return Err("Slots are 1-based.\r\n".to_string());
    }
    Ok((alias, slot))
}

/// Facts about the caller a board check needs, copied out of the world so
/// the database work can run off the tick.
struct Caller {
    name: String,
    is_builder: bool,
}

impl Caller {
    fn of(world: &World, player: Entity) -> Self {
        Self {
            name: name_of(world, player),
            is_builder: world
                .get::<Account>(player)
                .is_some_and(|a| a.role.at_least(UserRole::Builder)),
        }
    }

    fn may_touch(&self, msg: &mud_db::boards::BoardMessage) -> bool {
        self.is_builder || msg.poster.eq_ignore_ascii_case(&self.name)
    }
}

/// `delpost <board> <#>`: delete one of your own posts on a board.
/// Builders+ can delete anyone's posts (matches the legacy "moderator
/// can edit/remove any" privilege; refining via `Board.privileges`
/// JSON is a follow-up). The `poster` column is a string compare
/// against the caller's current character `Named`.
///
/// The delete is by message id, so it is idempotent: a retry after a
/// watchdog finds the row already gone and says so.
pub(crate) async fn cmd_delpost(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let (alias, slot) = match alias_and_slot(args, "delpost <board-alias> <#>") {
        Ok(parsed) => parsed,
        Err(line) => {
            send_to(world, player, line);
            return;
        }
    };
    let allowed_ids = local_board_ids(world, player);
    let caller = Caller::of(world, player);
    let (pool, alias) = (pool.clone(), alias.to_string());
    let work = async move {
        delete_post(&pool, &alias, slot, allowed_ids.as_ref(), &caller)
            .await
            .unwrap_or_else(|line| line)
    };
    off_tick::run(world, player, work, |world, player, text| {
        send_to(world, player, text);
    })
    .await;
}

async fn delete_post(
    pool: &mud_db::sqlx::PgPool,
    alias: &str,
    slot: usize,
    allowed: Option<&std::collections::HashSet<i32>>,
    caller: &Caller,
) -> Result<String, String> {
    let board = resolve_board(
        pool,
        alias,
        allowed,
        "Board lookup failed",
        "There is no such board here. Stand near the board you \
         want to delete from.\r\n",
    )
    .await?;
    let messages = board_messages(pool, board.id).await?;
    let Some(msg) = messages.get(slot - 1) else {
        return Err(format!("No message at slot {slot} on '{alias}'.\r\n"));
    };
    if !caller.may_touch(msg) {
        return Err("You can only delete your own posts (builders+ can delete any).\r\n".into());
    }
    match mud_db::boards::delete_message(pool, msg.id).await {
        Ok(0) => Ok("Message was already gone — nothing deleted.\r\n".to_string()),
        Ok(_) => Ok(format!(
            "Deleted '{}' by {} from {}.\r\n",
            msg.subject, msg.poster, board.title,
        )),
        Err(e) => Err(format!("Delete failed: {e}\r\n")),
    }
}

/// `post <board>`: open a board-composition draft. Locked boards
/// refuse the open. Resolves the alias, attaches `BoardDraft`, and
/// prompts for a subject. Subsequent input flows through
/// `compose_board_step` until `.send` / `.abort` clears the draft.
pub(crate) async fn cmd_post(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let trimmed = args.trim();
    // No args → default to the in-room board for mortals (and for
    // staff in a board room — same UX). If they're nowhere near a
    // board, refuse with a hint instead of "Usage:".
    let alias_owned = if trimmed.is_empty() {
        if let Some((_, alias)) = in_room_board_alias(world, player) {
            alias
        } else {
            send_to(
                world,
                player,
                "Stand near the board you want to post on, or use \
                 'post <alias>' (Builder+) to post remotely.\r\n",
            );
            return;
        }
    } else {
        trimmed.to_string()
    };
    let allowed_ids = local_board_ids(world, player);
    let pool = pool.clone();
    let open = async move {
        let board = resolve_board(
            &pool,
            &alias_owned,
            allowed_ids.as_ref(),
            "Board lookup failed",
            "There is no such board here. Stand near the board you \
             want to post on.\r\n",
        )
        .await?;
        if board.locked {
            return Err(format!(
                "'{}' is locked; no posts accepted.\r\n",
                board.title
            ));
        }
        Ok(board)
    };
    off_tick::run(world, player, open, |world, player, opened| match opened {
        Ok(board) => {
            try_insert(
                world,
                player,
                BoardDraft {
                    board_id: board.id,
                    board_alias: board.alias.clone(),
                    board_title: board.title.clone(),
                    subject: None,
                    body: Vec::new(),
                    edit_message_id: None,
                },
            );
            send_to(
                world,
                player,
                format!(
                    "Posting to {} ({}).\r\n\
                     First line is the subject. Then type the body, one line at a time.\r\n\
                     '.send' ships it; '.abort' cancels; '.preview' shows the draft.\r\n",
                    board.title, board.alias,
                ),
            );
        }
        Err(line) => send_to(world, player, line),
    })
    .await;
}

/// `editpost <alias> <#>`: re-open one of your posts (or any if
/// Builder+) for editing. Pre-loads the existing subject and body
/// into a `BoardDraft`; `.send` triggers `update_message` (which
/// inserts a `BoardMessageEdit` audit row in the same transaction).
pub(crate) async fn cmd_editpost(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let (alias, slot) = match alias_and_slot(args, "editpost <board-alias> <#>") {
        Ok(parsed) => parsed,
        Err(line) => {
            send_to(world, player, line);
            return;
        }
    };
    let allowed_ids = local_board_ids(world, player);
    let caller = Caller::of(world, player);
    let (pool, alias) = (pool.clone(), alias.to_string());
    let open = async move {
        let board = resolve_board(
            &pool,
            &alias,
            allowed_ids.as_ref(),
            "Board lookup failed",
            "There is no such board here. Stand near the board you \
             want to edit on.\r\n",
        )
        .await?;
        if board.locked {
            return Err(format!(
                "'{}' is locked; no edits accepted.\r\n",
                board.title
            ));
        }
        let messages = board_messages(&pool, board.id).await?;
        let Some(msg) = messages.get(slot - 1).cloned() else {
            return Err(format!("No message at slot {slot} on '{alias}'.\r\n"));
        };
        if !caller.may_touch(&msg) {
            return Err("You can only edit your own posts (builders+ can edit any).\r\n".into());
        }
        Ok((board, msg))
    };
    off_tick::run(
        world,
        player,
        open,
        move |world, player, opened| match opened {
            Ok((board, msg)) => {
                // Seed the draft with the existing body, line-split. Subject is
                // pre-set so the first input line goes straight to the body.
                let body_lines: Vec<String> = msg.content.split('\n').map(str::to_string).collect();
                try_insert(
                    world,
                    player,
                    BoardDraft {
                        board_id: board.id,
                        board_alias: board.alias.clone(),
                        board_title: board.title.clone(),
                        subject: Some(msg.subject.clone()),
                        body: body_lines,
                        edit_message_id: Some(msg.id),
                    },
                );
                send_to(
                    world,
                    player,
                    format!(
                        "Editing message #{slot} on {} ({}).\r\n\
                     Subject and existing body are preserved. Add lines to append \
                     (or '.abort' to bail without saving). Use '.preview' to see \
                     the current state, '.send' to commit (records an audit row).\r\n",
                        board.title, board.alias,
                    ),
                );
            }
            Err(line) => send_to(world, player, line),
        },
    )
    .await;
}

/// `read <#>` while standing near a board: render that board's
/// message body. Routed here from the async pre-dispatch when the
/// argument is a positive integer and the player's room contains a
/// `BoardLink`-tagged item. Out-of-range / fetch errors fall back
/// to friendly messages without re-dispatching.
pub(crate) async fn cmd_read_board_msg(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    board_id: i32,
    args: &str,
) {
    let Ok(slot) = args.trim().parse::<usize>() else {
        return;
    };
    if slot == 0 {
        send_to(world, player, "Slots are 1-based.\r\n");
        return;
    }
    let summary = world
        .get_resource::<BoardCatalog>()
        .and_then(|c| c.by_id.get(&board_id))
        .cloned();
    let Some(summary) = summary else {
        send_to(world, player, "That board's catalog entry is missing.\r\n");
        return;
    };
    let pool = pool.clone();
    let work = async move {
        let messages = match board_messages(&pool, board_id).await {
            Ok(m) => m,
            Err(line) => return Shown::Plain(line),
        };
        let Some(msg) = messages.get(slot - 1) else {
            return Shown::Plain(format!(
                "No message at slot {slot} on {} (it has {} message{}).\r\n",
                summary.title,
                messages.len(),
                if messages.len() == 1 { "" } else { "s" },
            ));
        };
        Shown::Prose(render_message(&summary.title, msg, slot, messages.len()))
    };
    off_tick::run(world, player, work, show).await;
}

/// One message as `read` / `board <alias> <#>` print it.
fn render_message(
    title: &str,
    msg: &mud_db::boards::BoardMessage,
    slot: usize,
    total: usize,
) -> String {
    let mut out = format!("\r\n[{title}] message {slot}/{total}\r\n");
    out.push_str(&format!(
        "From:    {} (level {})\r\n",
        msg.poster, msg.poster_level
    ));
    out.push_str(&format!("Subject: {}\r\n", msg.subject));
    out.push_str(&format!(
        "Posted:  {}\r\n",
        msg.posted_at.format("%Y-%m-%d %H:%M")
    ));
    if msg.sticky {
        out.push_str("(sticky)\r\n");
    }
    out.push_str("---\r\n");
    out.push_str(msg.content.trim_end());
    out.push_str("\r\n---\r\n");
    out
}

/// `look <board>` / `examine <board>`: render the board's message
/// listing inline. Routed here from the async pre-dispatch when the
/// argument matches a BOARD-tagged item in the player's room.
pub(crate) async fn cmd_look_board(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    board_id: i32,
) {
    let summary = world
        .get_resource::<BoardCatalog>()
        .and_then(|c| c.by_id.get(&board_id))
        .cloned();
    let Some(summary) = summary else {
        send_to(world, player, "That board's catalog entry is missing.\r\n");
        return;
    };
    let pool = pool.clone();
    // Off the tick, and ordered: whatever the player types next waits for
    // the listing instead of overtaking it.
    off_tick::run(
        world,
        player,
        async move { board_listing_text(&pool, board_id, &summary.title).await },
        |world, player, text| send_to(world, player, text),
    )
    .await;
}

async fn board_listing_text(pool: &mud_db::sqlx::PgPool, board_id: i32, title: &str) -> String {
    let messages = match mud_db::boards::messages_for_board(pool, board_id).await {
        Ok(m) => m,
        Err(e) => return format!("Message fetch failed: {e}\r\n"),
    };
    render_board_listing(
        title,
        &messages,
        1,
        "Use 'read <#>' to read a message, or 'post' to add one.",
        "board page",
    )
}

/// One page (newest first, [`crate::commands::info::PAGE_ROWS`] rows) of a
/// board's message list. `read_hint` closes the listing; `page_cmd` is the
/// command prefix the footer tells the player to add a page number to.
pub(crate) fn render_board_listing(
    title: &str,
    messages: &[mud_db::boards::BoardMessage],
    page: usize,
    read_hint: &str,
    page_cmd: &str,
) -> String {
    use crate::commands::info::{PAGE_ROWS, page_window};
    if messages.is_empty() {
        return format!("\r\n{title} has no messages.\r\n");
    }
    let (start, end, pages) = match page_window(messages.len(), page, PAGE_ROWS) {
        Ok(w) => w,
        Err(pages) => return format!("{title} only has {pages} page(s).\r\n"),
    };
    let mut out = format!(
        "\r\n{title} ({} message{}):\r\n",
        messages.len(),
        if messages.len() == 1 { "" } else { "s" },
    );
    for (i, msg) in messages.iter().enumerate().take(end).skip(start) {
        let stickymark = if msg.sticky { "*" } else { " " };
        let when = msg.posted_at.format("%Y-%m-%d");
        out.push_str(&format!(
            "  {:<3} {} {when}  {:<20} {}\r\n",
            i + 1,
            stickymark,
            msg.poster,
            msg.subject,
        ));
    }
    if pages > 1 {
        out.push_str(&format!("  (page {page} of {pages}"));
        if page < pages {
            out.push_str(&format!("; '{page_cmd} {}' for more", page + 1));
        }
        out.push_str(")\r\n");
    }
    out.push_str(&format!("\r\n{read_hint}\r\n"));
    out
}

/// `boards`: list every available board with its alias and title.
/// Lock state is shown — locked boards refuse posts.
pub(crate) async fn cmd_boards(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    let pool = pool.clone();
    off_tick::run(
        world,
        player,
        async move { boards_text(&pool).await },
        |world, player, text| send_to(world, player, text),
    )
    .await;
}

async fn boards_text(pool: &mud_db::sqlx::PgPool) -> String {
    let rows = match mud_db::boards::list_boards(pool).await {
        Ok(r) => r,
        Err(e) => return format!("Board fetch failed: {e}\r\n"),
    };
    if rows.is_empty() {
        return "\r\nNo boards exist.\r\n".to_string();
    }
    let mut out = format!("\r\nBoards ({}):\r\n", rows.len());
    for b in &rows {
        let lock = if b.locked { "[locked]" } else { "        " };
        out.push_str(&format!("  {:<10} {} {}\r\n", b.alias, lock, b.title));
    }
    out.push_str("\r\nUse 'board <alias>' to list messages, 'board <alias> <#>' to read one.\r\n");
    out
}

/// `board <alias> [#]`: list messages on a board, or read a specific
/// one if a slot number is appended. Sticky messages float to the top
/// of the listing and are flagged.
/// Mortal scope gate. Builders+ get None (no restriction — they
/// can query any board globally). Mortals get a Some set of board
/// ids visible from their current room (via `BoardLink` items).
/// Empty set means the mortal is in a room with no board at all
/// — every alias-targeted board command refuses for them.
fn local_board_ids(world: &mut World, player: Entity) -> Option<std::collections::HashSet<i32>> {
    let is_staff = world
        .get::<Account>(player)
        .is_some_and(|a| a.role.at_least(mud_db::enums::UserRole::Builder));
    if is_staff {
        return None;
    }
    let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
        return Some(std::collections::HashSet::new());
    };
    let mut q = world.query_filtered::<(&Located, &BoardLink), With<Item>>();
    Some(
        q.iter(world)
            .filter(|(l, _)| l.0 == room)
            .map(|(_, b)| b.0)
            .collect(),
    )
}

/// Return the alias of the (first) board in the player's current
/// room, if any. Used to default `board` / `boards` (no args) to
/// "this room's board" for mortals.
fn in_room_board_alias(world: &mut World, player: Entity) -> Option<(i32, String)> {
    let room = world.get::<Located>(player).map(|l| l.0)?;
    let mut q = world.query_filtered::<(&Located, &BoardLink), With<Item>>();
    let board_id = q.iter(world).find(|(l, _)| l.0 == room).map(|(_, b)| b.0)?;
    let catalog = world.get_resource::<BoardCatalog>()?;
    catalog
        .by_id
        .get(&board_id)
        .map(|b| (board_id, b.alias.clone()))
}

pub(crate) async fn cmd_board(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let (trimmed, page) = crate::commands::info::split_page_arg(args.trim());
    let trimmed = trimmed.as_str();
    // No args / `board list` → default to the in-room board.
    let resolved_args = if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("list") {
        if let Some((_, alias)) = in_room_board_alias(world, player) {
            alias
        } else {
            send_to(
                world,
                player,
                "There is no board here. Stand near a board to read it, \
                 or use 'boards' to see what's available.\r\n",
            );
            return;
        }
    } else {
        trimmed.to_string()
    };
    let mut parts = resolved_args.split_whitespace();
    let alias = parts.next().unwrap_or("").to_string();
    let slot = parts.next().and_then(|s| s.parse::<usize>().ok());

    // Mortal scope gate: builders+ can query any board by alias;
    // mortals can only address boards whose `BoardLink` item is in
    // their current room. Reading a board you can't physically see
    // ("board mortal" from across the world) was the user complaint.
    let allowed_ids = local_board_ids(world, player);
    let pool = pool.clone();
    let work = async move {
        let board = match resolve_board(
            &pool,
            &alias,
            allowed_ids.as_ref(),
            "Board fetch failed",
            // Mortal targeted a board they can't physically see.
            "There is no such board here. Stand near the board you \
             want to read from.\r\n",
        )
        .await
        {
            Ok(b) => b,
            Err(line) => return Shown::Plain(line),
        };
        let messages = match board_messages(&pool, board.id).await {
            Ok(m) => m,
            Err(line) => return Shown::Plain(line),
        };
        if let Some(slot) = slot {
            if slot == 0 {
                return Shown::Plain("Message slots are 1-based.\r\n".to_string());
            }
            let Some(msg) = messages.get(slot - 1) else {
                return Shown::Plain(format!("No message at slot {slot} on '{alias}'.\r\n"));
            };
            return Shown::Prose(render_message(&board.title, msg, slot, messages.len()));
        }
        Shown::Plain(render_board_listing(
            &board.title,
            &messages,
            page,
            &format!("Use 'board {alias} <#>' to read a message."),
            &format!("board {alias} page"),
        ))
    };
    off_tick::run(world, player, work, show).await;
}

#[cfg(test)]
mod off_tick_tests {
    use super::*;
    use crate::commands::off_tick::{CommandJobs, JOB_TIMEOUT, drain_command_jobs, is_busy};
    use crate::commands::test_support::{drain, player_in};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn world_with_draft() -> (World, Entity, crate::commands::test_support::Rx) {
        let mut world = World::new();
        world.init_resource::<CommandJobs>();
        world.init_resource::<mud_world::SocialRegistry>();
        let room = world
            .spawn((
                mud_world::Room,
                Named {
                    name: "A quiet hall".into(),
                },
            ))
            .id();
        let (player, rx) = player_in(&mut world, room);
        world.entity_mut(player).insert((
            Account {
                user_id: "u".into(),
                character_id: "c-board".into(),
                role: UserRole::Player,
                account_role: UserRole::Player,
                perms: vec![],
            },
            BoardDraft {
                board_id: 1,
                board_alias: "mortal".into(),
                board_title: "Mortal Board".into(),
                subject: Some("hello".into()),
                body: vec!["first".into()],
                edit_message_id: None,
            },
        ));
        (world, player, rx)
    }

    /// An unreachable database that gives up after `wait`: the slow write.
    fn slow_pool(wait: Duration) -> mud_db::sqlx::PgPool {
        mud_db::sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(wait)
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap()
    }

    /// `.send` returns at once while the write is still out: the tick is not
    /// held, the draft has left the player, the next line waits behind it,
    /// and a failed write hands the draft back.
    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_post_does_not_block_the_tick_and_keeps_the_next_line_behind_it() {
        let (mut world, player, mut rx) = world_with_draft();
        let pool = slow_pool(Duration::from_millis(200));
        let started = Instant::now();
        crate::commands::dispatch_with_async(&mut world, player, &pool, ".send").await;
        crate::commands::dispatch_with_async(&mut world, player, &pool, "look").await;
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "held the tick"
        );
        assert!(is_busy(&world, player));
        assert!(
            world.get::<BoardDraft>(player).is_none(),
            "a draft left behind could be posted twice"
        );
        assert_eq!(
            drain(&mut rx),
            "",
            "nothing yet, and look has not overtaken it"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        drain_command_jobs(&mut world);
        assert!(drain(&mut rx).contains("Save failed"));
        assert!(
            world.get::<BoardDraft>(player).is_some(),
            "a failed post keeps the draft"
        );
        assert!(!is_busy(&world, player));
    }

    /// The watchdog gives up on a post whose answer never comes. The draft
    /// is gone by then, so nothing can post it again: a second `.send` has
    /// no draft, and the write ran exactly once.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_watchdog_timeout_does_not_double_post() {
        let (mut world, player, mut rx) = world_with_draft();
        let writes = Arc::new(AtomicUsize::new(0));
        let draft = world.get::<BoardDraft>(player).cloned().unwrap();
        let counted = Arc::clone(&writes);
        send_draft(&mut world, player, draft, async move {
            counted.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<Result<i32, String>>().await
        })
        .await;
        assert!(is_busy(&world, player));
        tokio::time::sleep(JOB_TIMEOUT + Duration::from_secs(1)).await;
        drain_command_jobs(&mut world);
        assert!(!is_busy(&world, player), "the watchdog released the player");
        assert!(drain(&mut rx).contains("took too long"));
        assert!(
            world.get::<BoardDraft>(player).is_none(),
            "the draft must not come back after a timeout"
        );

        // Typing `.send` again finds nothing to send.
        let pool = slow_pool(Duration::from_millis(50));
        compose_board_step(&mut world, player, &pool, ".send").await;
        assert!(!is_busy(&world, player));
        assert_eq!(drain(&mut rx), "");
        assert_eq!(writes.load(Ordering::SeqCst), 1, "posted once");
    }

    /// Every board command that waits on the database answers before the
    /// line typed after it runs.
    #[tokio::test(flavor = "current_thread")]
    async fn board_reads_keep_command_order() {
        let (mut world, player, mut rx) = world_with_draft();
        world.entity_mut(player).remove::<BoardDraft>();
        let pool = slow_pool(Duration::from_millis(100));
        for cmd in ["boards", "board mortal", "delpost mortal 1", "post mortal"] {
            crate::commands::dispatch_with_async(&mut world, player, &pool, cmd).await;
            crate::commands::dispatch_with_async(&mut world, player, &pool, "look").await;
            assert_eq!(drain(&mut rx), "", "{cmd}: look overtook it");
            tokio::time::sleep(Duration::from_millis(300)).await;
            drain_command_jobs(&mut world);
            let out = drain(&mut rx);
            assert!(out.contains("failed"), "{cmd}: {out}");
            assert!(!out.contains("A quiet hall"), "{cmd}: {out}");
            assert!(crate::commands::run_queued_input(&mut world, player, &pool).await);
            assert!(drain(&mut rx).contains("A quiet hall"), "{cmd}");
        }
    }
}

/// Live-database check of the write-time permission rule for edits.
#[cfg(test)]
mod edit_permission_tests {
    use super::*;
    use mud_db::sqlx;
    use std::time::Duration;

    fn caller(name: &str, is_builder: bool) -> Caller {
        Caller {
            name: name.to_string(),
            is_builder,
        }
    }

    async fn content_of(pool: &sqlx::PgPool, id: i32) -> Option<String> {
        sqlx::query_scalar("SELECT content FROM \"BoardMessage\" WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .unwrap()
    }

    async fn audits(pool: &sqlx::PgPool, id: i32) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM \"BoardMessageEdit\" WHERE message_id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Opening the draft checked the poster and the lock; the write checks
    /// them again, so a board locked meanwhile, someone else's post and a
    /// post deleted meanwhile are refused (nothing written, no audit row),
    /// while the poster (any letter case) and staff are not.
    #[tokio::test(flavor = "current_thread")]
    async fn an_edit_is_rechecked_when_it_is_written() {
        let _db_lock = crate::commands::test_support::db_test_lock().await;
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
        let Ok(Ok(pool)) = tokio::time::timeout(
            Duration::from_secs(3),
            mud_db::connect_with(&url, crate::commands::test_support::db_test_pool_settings()),
        )
        .await
        else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let alias = format!("zzrh-{}-{nanos}", std::process::id());
        let board: i32 = sqlx::query_scalar(
            "INSERT INTO \"Board\" (alias, title, updated_at) VALUES ($1, 'zz', NOW()) \
             RETURNING id",
        )
        .bind(&alias)
        .fetch_one(&pool)
        .await
        .unwrap();
        let msg = mud_db::boards::post_message(&pool, board, "Alice", 10, "s", "original")
            .await
            .unwrap();
        let edit = |who: Caller, body: &'static str| {
            let pool = pool.clone();
            async move { persist_draft(&pool, board, Some(msg), &who, 10, "s", body).await }
        };

        // Not the poster.
        assert_eq!(
            edit(caller("Bob", false), "bob").await,
            Err(EDIT_REFUSED.into())
        );
        assert_eq!(content_of(&pool, msg).await.as_deref(), Some("original"));
        assert_eq!(
            audits(&pool, msg).await,
            0,
            "a refused edit leaves no audit row"
        );

        // The poster, whatever the letter case of the name.
        assert_eq!(edit(caller("aLiCe", false), "by alice").await, Ok(msg));
        assert_eq!(content_of(&pool, msg).await.as_deref(), Some("by alice"));
        assert_eq!(audits(&pool, msg).await, 1);

        // The board locks after the draft was opened: the poster is refused,
        // staff are not.
        sqlx::query("UPDATE \"Board\" SET locked = true WHERE id = $1")
            .bind(board)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            edit(caller("Alice", false), "late").await,
            Err(EDIT_REFUSED.into())
        );
        assert_eq!(content_of(&pool, msg).await.as_deref(), Some("by alice"));
        assert_eq!(edit(caller("Mod", true), "by staff").await, Ok(msg));
        assert_eq!(content_of(&pool, msg).await.as_deref(), Some("by staff"));
        assert_eq!(audits(&pool, msg).await, 2);

        // The post was deleted meanwhile: nothing to edit, even for staff.
        mud_db::boards::delete_message(&pool, msg).await.unwrap();
        assert_eq!(
            edit(caller("Mod", true), "ghost").await,
            Err(EDIT_REFUSED.into())
        );

        sqlx::query("DELETE FROM \"Board\" WHERE id = $1")
            .bind(board)
            .execute(&pool)
            .await
            .unwrap();
    }
}
