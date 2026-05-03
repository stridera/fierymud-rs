//! Message boards: `boards`, `board`, `post`, `delpost`, `editpost`,
//! plus the async handler bodies and composer step.

#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::wildcard_imports)]

use bevy_ecs::prelude::{Entity, World};
use mud_db::enums::UserRole;
use mud_world::*;

use crate::commands::*;

inventory::submit! {
    Command {
        names: &["boards"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "boards",
            summary: "List every public message board.",
            long: "Each board has an alias (`mortal`, `god`, `quest`, \
                   etc.) and a title. Use `board <alias>` to list \
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
        category: Category::Communication,
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
        category: Category::Communication,
        help: Help {
            usage: "post <board-alias>",
            summary: "Compose a new message on a board.",
            long: "Opens a multi-line composition session: first \
                   non-blank line is the subject, subsequent lines \
                   accumulate as body. `.send` ships, `.abort` \
                   cancels, `.preview` shows the draft. Locked \
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
        category: Category::Communication,
        help: Help {
            usage: "delpost <board-alias> <#>",
            summary: "Delete a board message (yours, or any if Builder+).",
            long: "Hard-deletes the row at the given slot. Players \
                   can only delete posts they made (case-insensitive \
                   poster-name match); Builder-and-above can delete \
                   anyone's. Edit history (`BoardMessageEdit`) cascades.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["editpost"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "editpost <board-alias> <#>",
            summary: "Re-open a board post for editing.",
            long: "Pre-loads the existing subject and body into a \
                   composition session. Add lines to append, or \
                   `.clear` to wipe before writing the new body. \
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
        out.push_str(&format!("Board:   {} ({})\r\n", draft.board_title, draft.board_alias));
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
        let Some(subject) = draft.subject else {
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
        let poster = name_of(world, player);
        let level = world.get::<Profile>(player).map_or(1, |p| p.level);
        let result = if let Some(edit_id) = draft.edit_message_id {
            mud_db::boards::update_message(pool, edit_id, &subject, &body, &poster)
                .await
                .map(|_| edit_id)
        } else {
            mud_db::boards::post_message(
                pool,
                draft.board_id,
                &poster,
                level,
                &subject,
                &body,
            )
            .await
        };
        match result {
            Ok(_id) => {
                try_remove::<BoardDraft>(world, player);
                let verb = if draft.edit_message_id.is_some() { "Updated" } else { "Posted" };
                send_to(
                    world,
                    player,
                    format!(
                        "{verb} on {} ({}).\r\n",
                        draft.board_title, draft.board_alias
                    ),
                );
            }
            Err(e) => {
                send_to(world, player, format!("Save failed: {e}\r\n"));
            }
        }
        return;
    }
    let step = if let Some(mut draft) = world.get_mut::<BoardDraft>(player) {
        if draft.subject.is_none() {
            if trimmed.is_empty() {
                ComposeStep::Nudge
            } else {
                draft.subject = Some(trimmed.to_string());
                ComposeStep::SubjectSet
            }
        } else {
            draft.body.push(line.to_string());
            ComposeStep::BodyAdded
        }
    } else {
        return;
    };
    match step {
        ComposeStep::Nudge => send_to(
            world,
            player,
            "Type a subject line, then the body. `.send` to ship; `.abort` to cancel.\r\n",
        ),
        ComposeStep::SubjectSet => send_to(
            world,
            player,
            "Subject set. Type the body, one line at a time. \
             `.send` to ship, `.abort` to cancel, `.preview` to review.\r\n",
        ),
        ComposeStep::BodyAdded => {}
    }
}

/// `delpost <board> <#>`: delete one of your own posts on a board.
/// Builders+ can delete anyone's posts (matches the legacy "moderator
/// can edit/remove any" privilege; refining via `Board.privileges`
/// JSON is a follow-up). The `poster` column is a string compare
/// against the caller's current character `Named`.
pub(crate) async fn cmd_delpost(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let Some(alias) = parts.next() else {
        send_to(world, player, "Usage: delpost <board-alias> <#>\r\n");
        return;
    };
    let Some(slot_raw) = parts.next() else {
        send_to(world, player, "Usage: delpost <board-alias> <#>\r\n");
        return;
    };
    let Ok(slot) = slot_raw.parse::<usize>() else {
        send_to(world, player, "Slot number must be a positive integer.\r\n");
        return;
    };
    if slot == 0 {
        send_to(world, player, "Slots are 1-based.\r\n");
        return;
    }
    let board = match mud_db::boards::find_board_by_alias(pool, alias).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            send_to(world, player, format!("No board called '{alias}'.\r\n"));
            return;
        }
        Err(e) => {
            send_to(world, player, format!("Board lookup failed: {e}\r\n"));
            return;
        }
    };
    let messages = match mud_db::boards::messages_for_board(pool, board.id).await {
        Ok(m) => m,
        Err(e) => {
            send_to(world, player, format!("Message fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(msg) = messages.get(slot - 1) else {
        send_to(
            world,
            player,
            format!("No message at slot {slot} on '{alias}'.\r\n"),
        );
        return;
    };
    let caller_name = name_of(world, player);
    let is_builder = world
        .get::<Account>(player)
        .is_some_and(|a| a.role.at_least(UserRole::Builder));
    let is_owner = msg.poster.eq_ignore_ascii_case(&caller_name);
    if !is_owner && !is_builder {
        send_to(
            world,
            player,
            "You can only delete your own posts (builders+ can delete any).\r\n",
        );
        return;
    }
    let preview_subject = msg.subject.clone();
    let preview_poster = msg.poster.clone();
    match mud_db::boards::delete_message(pool, msg.id).await {
        Ok(0) => {
            send_to(
                world,
                player,
                "Message was already gone — nothing deleted.\r\n",
            );
        }
        Ok(_) => {
            send_to(
                world,
                player,
                format!(
                    "Deleted '{preview_subject}' by {preview_poster} from {}.\r\n",
                    board.title,
                ),
            );
        }
        Err(e) => {
            send_to(world, player, format!("Delete failed: {e}\r\n"));
        }
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
    let alias = args.trim();
    if alias.is_empty() {
        send_to(world, player, "Usage: post <board-alias>\r\n");
        return;
    }
    let board = match mud_db::boards::find_board_by_alias(pool, alias).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            send_to(world, player, format!("No board called '{alias}'.\r\n"));
            return;
        }
        Err(e) => {
            send_to(world, player, format!("Board lookup failed: {e}\r\n"));
            return;
        }
    };
    if board.locked {
        send_to(
            world,
            player,
            format!("'{}' is locked; no posts accepted.\r\n", board.title),
        );
        return;
    }
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
             `.send` ships it; `.abort` cancels; `.preview` shows the draft.\r\n",
            board.title, board.alias,
        ),
    );
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
    let mut parts = args.split_whitespace();
    let Some(alias) = parts.next() else {
        send_to(world, player, "Usage: editpost <board-alias> <#>\r\n");
        return;
    };
    let Some(slot_raw) = parts.next() else {
        send_to(world, player, "Usage: editpost <board-alias> <#>\r\n");
        return;
    };
    let Ok(slot) = slot_raw.parse::<usize>() else {
        send_to(world, player, "Slot number must be a positive integer.\r\n");
        return;
    };
    if slot == 0 {
        send_to(world, player, "Slots are 1-based.\r\n");
        return;
    }
    let board = match mud_db::boards::find_board_by_alias(pool, alias).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            send_to(world, player, format!("No board called '{alias}'.\r\n"));
            return;
        }
        Err(e) => {
            send_to(world, player, format!("Board lookup failed: {e}\r\n"));
            return;
        }
    };
    if board.locked {
        send_to(
            world,
            player,
            format!("'{}' is locked; no edits accepted.\r\n", board.title),
        );
        return;
    }
    let messages = match mud_db::boards::messages_for_board(pool, board.id).await {
        Ok(m) => m,
        Err(e) => {
            send_to(world, player, format!("Message fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(msg) = messages.get(slot - 1).cloned() else {
        send_to(
            world,
            player,
            format!("No message at slot {slot} on '{alias}'.\r\n"),
        );
        return;
    };
    let caller_name = name_of(world, player);
    let is_builder = world
        .get::<Account>(player)
        .is_some_and(|a| a.role.at_least(UserRole::Builder));
    let is_owner = msg.poster.eq_ignore_ascii_case(&caller_name);
    if !is_owner && !is_builder {
        send_to(
            world,
            player,
            "You can only edit your own posts (builders+ can edit any).\r\n",
        );
        return;
    }
    // Seed the draft with the existing body, line-split. Subject is
    // pre-set so the first input line goes straight to the body.
    let body_lines: Vec<String> = msg
        .content
        .split('\n')
        .map(str::to_string)
        .collect();
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
             (or `.abort` to bail without saving). Use `.preview` to see \
             the current state, `.send` to commit (records an audit row).\r\n",
            board.title, board.alias,
        ),
    );
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
    let messages = match mud_db::boards::messages_for_board(pool, board_id).await {
        Ok(m) => m,
        Err(e) => {
            send_to(world, player, format!("Message fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(msg) = messages.get(slot - 1) else {
        send_to(
            world,
            player,
            format!(
                "No message at slot {slot} on {} (it has {} message{}).\r\n",
                summary.title,
                messages.len(),
                if messages.len() == 1 { "" } else { "s" },
            ),
        );
        return;
    };
    let mut out = format!(
        "\r\n[{}] message {}/{}\r\n",
        summary.title, slot, messages.len()
    );
    out.push_str(&format!("From:    {} (level {})\r\n", msg.poster, msg.poster_level));
    out.push_str(&format!("Subject: {}\r\n", msg.subject));
    out.push_str(&format!("Posted:  {}\r\n", msg.posted_at.format("%Y-%m-%d %H:%M")));
    if msg.sticky {
        out.push_str("(sticky)\r\n");
    }
    out.push_str("---\r\n");
    out.push_str(msg.content.trim_end());
    out.push_str("\r\n---\r\n");
    send_to(world, player, out);
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
    let messages = match mud_db::boards::messages_for_board(pool, board_id).await {
        Ok(m) => m,
        Err(e) => {
            send_to(world, player, format!("Message fetch failed: {e}\r\n"));
            return;
        }
    };
    if messages.is_empty() {
        send_to(
            world,
            player,
            format!("\r\n{} has no messages.\r\n", summary.title),
        );
        return;
    }
    let mut out = format!(
        "\r\n{} ({} message{}):\r\n",
        summary.title,
        messages.len(),
        if messages.len() == 1 { "" } else { "s" },
    );
    for (i, msg) in messages.iter().enumerate() {
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
    out.push_str("\r\nUse `read <#>` to read a message, or `post` to add one.\r\n");
    send_to(world, player, out);
}

/// `boards`: list every available board with its alias and title.
/// Lock state is shown — locked boards refuse posts.
pub(crate) async fn cmd_boards(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    let rows = match mud_db::boards::list_boards(pool).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Board fetch failed: {e}\r\n"));
            return;
        }
    };
    if rows.is_empty() {
        send_to(world, player, "\r\nNo boards exist.\r\n");
        return;
    }
    let mut out = format!("\r\nBoards ({}):\r\n", rows.len());
    for b in &rows {
        let lock = if b.locked { "[locked]" } else { "        " };
        out.push_str(&format!("  {:<10} {} {}\r\n", b.alias, lock, b.title));
    }
    out.push_str("\r\nUse `board <alias>` to list messages, `board <alias> <#>` to read one.\r\n");
    send_to(world, player, out);
}

/// `board <alias> [#]`: list messages on a board, or read a specific
/// one if a slot number is appended. Sticky messages float to the top
/// of the listing and are flagged.
pub(crate) async fn cmd_board(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let mut parts = args.split_whitespace();
    let Some(alias) = parts.next() else {
        send_to(world, player, "Usage: board <alias> [#]\r\n");
        return;
    };
    let slot = parts.next().and_then(|s| s.parse::<usize>().ok());
    let board = match mud_db::boards::find_board_by_alias(pool, alias).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            send_to(world, player, format!("No board called '{alias}'.\r\n"));
            return;
        }
        Err(e) => {
            send_to(world, player, format!("Board fetch failed: {e}\r\n"));
            return;
        }
    };
    let messages = match mud_db::boards::messages_for_board(pool, board.id).await {
        Ok(m) => m,
        Err(e) => {
            send_to(world, player, format!("Message fetch failed: {e}\r\n"));
            return;
        }
    };
    if let Some(slot) = slot {
        if slot == 0 {
            send_to(world, player, "Message slots are 1-based.\r\n");
            return;
        }
        let Some(msg) = messages.get(slot - 1) else {
            send_to(
                world,
                player,
                format!("No message at slot {slot} on '{alias}'.\r\n"),
            );
            return;
        };
        let mut out = format!("\r\n[{}] message {}/{}\r\n", board.title, slot, messages.len());
        out.push_str(&format!("From:    {} (level {})\r\n", msg.poster, msg.poster_level));
        out.push_str(&format!("Subject: {}\r\n", msg.subject));
        out.push_str(&format!("Posted:  {}\r\n", msg.posted_at.format("%Y-%m-%d %H:%M")));
        if msg.sticky {
            out.push_str("(sticky)\r\n");
        }
        out.push_str("---\r\n");
        out.push_str(msg.content.trim_end());
        out.push_str("\r\n---\r\n");
        send_to(world, player, out);
        return;
    }
    if messages.is_empty() {
        send_to(
            world,
            player,
            format!("\r\n{} has no messages.\r\n", board.title),
        );
        return;
    }
    let mut out = format!(
        "\r\n{} ({} message{}):\r\n",
        board.title,
        messages.len(),
        if messages.len() == 1 { "" } else { "s" },
    );
    for (i, msg) in messages.iter().enumerate() {
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
    out.push_str(&format!("\r\nUse `board {alias} <#>` to read a message.\r\n"));
    send_to(world, player, out);
}
