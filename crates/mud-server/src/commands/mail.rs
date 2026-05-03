//! Mail commands: `mail`, `mailbox`, `readmail`, `delmail`, plus
//! the async handler bodies, the composer step, and the
//! `cmd_mail_stub` sentinel for the sync registry.

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
            "mail" => Some(Box::pin(cmd_mail(world, player, pool, args))),
            "mailbox" | "mailboxes" => Some(Box::pin(cmd_mailbox(world, player, pool))),
            "readmail" => Some(Box::pin(cmd_readmail(world, player, pool, args))),
            "delmail" => Some(Box::pin(cmd_delmail(world, player, pool, args))),
            _ => None,
        },
    }
}

inventory::submit! {
    Command {
        names: &["mail"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "mail <character>",
            summary: "Open a mail composition session to one player.",
            long: "Opens a multi-line composition session: first \
                   non-blank line is the subject, subsequent lines \
                   accumulate as body. Control verbs: `.send` \
                   ships the draft, `.abort` discards it, `.preview` \
                   shows what's queued, `.clear` wipes the draft.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["mailbox"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "mailbox",
            summary: "List inbound mail in your account inbox.",
            long: "Newest first; unread messages have a `*` prefix. \
                   Use `readmail <#>` to read by slot — that also \
                   marks the row read.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["readmail"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "readmail <#>",
            summary: "Read a single mail message by slot.",
            long: "Slot number is the `#` from `mailbox`. Marks the \
                   row read on first read; subsequent re-reads are \
                   silent.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["delmail"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "delmail <#>",
            summary: "Delete a mail message by slot.",
            long: "Hard-deletes the row. Slot number is the `#` from \
                   `mailbox`.",
        },
        run: cmd_mail_stub,
    }
}

/// Process one line of input from a player who has an active
/// `MailDraft`. Recognized control verbs:
///   `.send`    — finalize and persist the mail
///   `.abort`   — discard the draft
///   `.preview` — show the current draft so far
/// Anything else is a content line: first non-blank line becomes
/// the subject, subsequent lines append to the body.
#[allow(clippy::too_many_lines)]
pub(crate) async fn compose_mail_step(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    line: &str,
) {
    let trimmed = line.trim();
    if trimmed.eq_ignore_ascii_case(".abort") {
        try_remove::<MailDraft>(world, player);
        send_to(world, player, "Mail composition aborted.\r\n");
        return;
    }
    if trimmed.eq_ignore_ascii_case(".clear") {
        if let Some(mut draft) = world.get_mut::<MailDraft>(player) {
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
        let Some(draft) = world.get::<MailDraft>(player).cloned() else {
            return;
        };
        let mut out = String::from("\r\n--- DRAFT ---\r\n");
        out.push_str(&format!("To:      {}\r\n", draft.recipient_label));
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
        let Some(draft) = world.get::<MailDraft>(player).cloned() else {
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
        let sender_user_id = world
            .get::<Account>(player)
            .map(|a| a.user_id.clone());
        let Some(sender_user_id) = sender_user_id else {
            send_to(world, player, "No account info; can't send.\r\n");
            return;
        };
        match mud_db::mail::send(
            pool,
            &sender_user_id,
            &draft.recipient_user_id,
            &subject,
            &body,
        )
        .await
        {
            Ok(_id) => {
                try_remove::<MailDraft>(world, player);
                send_to(
                    world,
                    player,
                    format!("Mail sent to {}.\r\n", draft.recipient_label),
                );
            }
            Err(e) => {
                send_to(world, player, format!("Send failed: {e}\r\n"));
            }
        }
        return;
    }
    // Plain content line: first non-blank line is the subject; rest
    // accumulate as body. Compute what to do under the mutable
    // borrow, then release before sending feedback.
    let step = if let Some(mut draft) = world.get_mut::<MailDraft>(player) {
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
        // Body lines: silent acceptance — the player sees their own typing
        // already; an echo on every line would feel chatty.
        ComposeStep::BodyAdded => {}
    }
}

/// `mail <character>`: open a mail-composition draft addressed to
/// the named character's account. Resolves the recipient via DB
/// (case-insensitive name match), attaches a `MailDraft` component,
/// and prompts the player for a subject. Subsequent input is routed
/// to `compose_mail_step` until `.send` / `.abort` clears the draft.
pub(crate) async fn cmd_mail(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Usage: mail <character>\r\n");
        return;
    }
    if !require_profession_in_room(
        world,
        player,
        mud_db::enums::MobProfession::Postmaster,
        "postmaster",
    ) {
        return;
    }
    let resolved = match mud_db::mail::user_for_character_name(pool, arg).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Lookup failed: {e}\r\n"));
            return;
        }
    };
    let Some((user_id, name)) = resolved else {
        send_to(
            world,
            player,
            format!("No character named '{arg}' on this realm.\r\n"),
        );
        return;
    };
    try_insert(
        world,
        player,
        MailDraft {
            recipient_user_id: user_id,
            recipient_label: name.clone(),
            subject: None,
            body: Vec::new(),
        },
    );
    send_to(
        world,
        player,
        format!(
            "Composing mail to {name}.\r\n\
             First line is the subject. Then type the body, one line at a time.\r\n\
             `.send` ships it; `.abort` cancels; `.preview` shows the draft; \
             `.clear` wipes and starts over.\r\n"
        ),
    );
}

/// Stub for the help/registry path — mail commands are intercepted by
/// the async pre-dispatch hook before this ever runs. If somehow it
/// does (a future refactor moves dispatch order around), bail loudly.
pub(crate) fn cmd_mail_stub(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        "Mail subsystem error: sync dispatch reached an async-only \
         command. Please report.\r\n",
    );
}

/// `mailbox`: list inbound non-deleted mail for the player's account,
/// newest first. Each line shows `# unread? sender — subject`.
/// `readmail <#>` reads the body and marks the row read.
pub(crate) async fn cmd_mailbox(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    let user_id = world.get::<Account>(player).map(|a| a.user_id.clone());
    let Some(user_id) = user_id else {
        send_to(world, player, "No account info; can't fetch mail.\r\n");
        return;
    };
    let rows = match mud_db::mail::inbox_for(pool, &user_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Mail fetch failed: {e}\r\n"));
            return;
        }
    };
    if rows.is_empty() {
        send_to(world, player, "\r\nYour mailbox is empty.\r\n");
        return;
    }
    let mut out = format!("\r\nMailbox ({} message(s)):\r\n", rows.len());
    for (i, row) in rows.iter().enumerate() {
        let unread = if row.read_at.is_some() { " " } else { "*" };
        let when = row.sent_at.format("%Y-%m-%d %H:%M");
        out.push_str(&format!(
            "  {:<3} {unread} {when}  {:<24} {}\r\n",
            i + 1,
            row.sender_display_name,
            row.subject,
        ));
    }
    out.push_str("\r\n* = unread.   Use `readmail <#>` to read, `delmail <#>` to delete.\r\n");
    send_to(world, player, out);
}

/// `readmail <#>`: print the body of the slot-numbered mail (1-based,
/// matching the `mailbox` listing). Marks the row read on success.
pub(crate) async fn cmd_readmail(
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
            "Read which mail? Pick a number from `mailbox`.\r\n",
        );
        return;
    };
    if slot == 0 {
        send_to(world, player, "Mail slots are 1-based.\r\n");
        return;
    }
    let user_id = world.get::<Account>(player).map(|a| a.user_id.clone());
    let Some(user_id) = user_id else {
        send_to(world, player, "No account info; can't fetch mail.\r\n");
        return;
    };
    let rows = match mud_db::mail::inbox_for(pool, &user_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Mail fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(row) = rows.get(slot - 1) else {
        send_to(world, player, format!("No mail at slot {slot}.\r\n"));
        return;
    };
    let mut out = String::from("\r\n");
    out.push_str(&format!("From:    {}\r\n", row.sender_display_name));
    out.push_str(&format!("Subject: {}\r\n", row.subject));
    out.push_str(&format!("Sent:    {}\r\n", row.sent_at.format("%Y-%m-%d %H:%M")));
    out.push_str("---\r\n");
    out.push_str(row.body.trim_end());
    out.push_str("\r\n---\r\n");
    send_to(world, player, out);
    if let Err(e) = mud_db::mail::mark_read(pool, row.id).await {
        tracing::warn!(error = %e, mail_id = row.id, "mark_read failed");
    }
}

/// `delmail <#>`: soft-delete the slot-numbered mail.
pub(crate) async fn cmd_delmail(
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
            "Delete which mail? Pick a number from `mailbox`.\r\n",
        );
        return;
    };
    if slot == 0 {
        send_to(world, player, "Mail slots are 1-based.\r\n");
        return;
    }
    let user_id = world.get::<Account>(player).map(|a| a.user_id.clone());
    let Some(user_id) = user_id else {
        send_to(world, player, "No account info; can't fetch mail.\r\n");
        return;
    };
    let rows = match mud_db::mail::inbox_for(pool, &user_id).await {
        Ok(r) => r,
        Err(e) => {
            send_to(world, player, format!("Mail fetch failed: {e}\r\n"));
            return;
        }
    };
    let Some(row) = rows.get(slot - 1) else {
        send_to(world, player, format!("No mail at slot {slot}.\r\n"));
        return;
    };
    if let Err(e) = mud_db::mail::soft_delete(pool, row.id).await {
        send_to(world, player, format!("Delete failed: {e}\r\n"));
        return;
    }
    send_to(
        world,
        player,
        format!("Deleted mail #{slot}: \"{}\".\r\n", row.subject),
    );
}
