//! Mail commands: `mail`, `mailbox`, `readmail`, `delmail`, plus
//! the async handler bodies, the composer step, and the
//! `cmd_mail_stub` sentinel for the sync registry.

#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::wildcard_imports)]

use bevy_ecs::prelude::{Entity, World};
use mud_db::enums::UserRole;

use crate::commands::off_tick;
use crate::commands::*;

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, args| match head {
            "mail" => Some(Box::pin(cmd_mail(world, player, pool, args))),
            "mailbox" | "mailboxes" | "check" => Some(Box::pin(cmd_mailbox(world, player, pool))),
            "readmail" | "receive" | "getmail" => Some(Box::pin(cmd_readmail(world, player, pool, args))),
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
        category: Category::Mail,
        help: Help {
            usage: "mail <character>",
            summary: "Open a mail composition session to one player.",
            long: "Opens a multi-line composition session: first \
                   non-blank line is the subject, subsequent lines \
                   accumulate as body. Control verbs: '.send' \
                   ships the draft, '.abort' discards it, '.preview' \
                   shows what's queued, '.clear' wipes the draft.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["mailbox", "check"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Mail,
        help: Help {
            usage: "mailbox",
            summary: "List inbound mail in your account inbox.",
            long: "Newest first; unread messages have a '*' prefix. \
                   Use 'readmail <#>' to read by slot — that also \
                   marks the row read.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["readmail", "receive", "getmail"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Mail,
        help: Help {
            usage: "readmail <#>",
            summary: "Read a single mail message by slot.",
            long: "Slot number is the '#' from 'mailbox'. Marks the \
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
        category: Category::Mail,
        help: Help {
            usage: "delmail <#>",
            summary: "Delete a mail message by slot.",
            long: "Hard-deletes the row. Slot number is the '#' from \
                   'mailbox'.",
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
        let Some(sender) = require_linked_account(world, player, "mail") else {
            return;
        };
        let sender_user_id = sender.user_id;
        let pool = pool.clone();
        let send = {
            let (recipient, subject) = (draft.recipient_user_id.clone(), subject.clone());
            async move {
                mud_db::mail::send(&pool, &sender_user_id, &recipient, &subject, &body)
                    .await
                    .map_err(|e| e.to_string())
            }
        };
        // The draft leaves the player now, not when the insert is confirmed:
        // if the answer never comes back (a stuck database, a dropped link)
        // the mail may or may not have been stored, and a draft left behind
        // would be sent a second time. A failed send hands it back.
        try_remove::<MailDraft>(world, player);
        off_tick::run(world, player, send, move |world, player, sent| match sent {
            Ok(_id) => {
                send_to(
                    world,
                    player,
                    format!("Mail sent to {}.\r\n", draft.recipient_label),
                );
                notify_new_mail(world, &draft.recipient_user_id);
            }
            Err(e) => {
                try_insert(world, player, draft);
                send_to(world, player, format!("Send failed: {e}\r\n"));
            }
        })
        .await;
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
        // Body lines: silent acceptance — the player sees their own typing
        // already; an echo on every line would feel chatty.
        ComposeStep::BodyAdded => {}
    }
}

/// Tell every online character on `recipient_user_id`'s account that new
/// mail arrived.
pub(crate) fn notify_new_mail(world: &mut World, recipient_user_id: &str) {
    let recipients: Vec<Entity> = world
        .query_filtered::<(Entity, &Account), bevy_ecs::prelude::With<mud_world::Online>>()
        .iter(world)
        .filter(|(_, a)| a.user_id == recipient_user_id)
        .map(|(e, _)| e)
        .collect();
    for r in recipients {
        send_to(
            world,
            r,
            "\r\nYou have new mail waiting at the post office.\r\n",
        );
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
    if require_linked_account(world, player, "mail").is_none() {
        return;
    }
    let (pool, arg) = (pool.clone(), arg.to_string());
    let lookup = {
        let arg = arg.clone();
        async move {
            mud_db::mail::user_for_character_name(&pool, &arg)
                .await
                .map_err(|e| e.to_string())
        }
    };
    off_tick::run(world, player, lookup, move |world, player, resolved| {
        let (user_id, name) = match resolved {
            Ok(Some(found)) => found,
            Ok(None) => {
                send_to(
                    world,
                    player,
                    format!("No character named '{arg}' on this realm.\r\n"),
                );
                return;
            }
            Err(e) => {
                send_to(world, player, format!("Lookup failed: {e}\r\n"));
                return;
            }
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
                 '.send' ships it; '.abort' cancels; '.preview' shows the draft; \
                 '.clear' wipes and starts over.\r\n"
            ),
        );
    })
    .await;
}

/// Stub registered as the sync `run` of every async-only command (mail,
/// `save`, `visit`, ...): the async pre-dispatch hook claims them first. It
/// only runs when a sync-only path (Lua `command()`, `force`) reaches one.
pub(crate) fn cmd_mail_stub(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        "That command can't run here (async-only: it needs the database \
         dispatcher, which this path doesn't have).\r\n",
    );
}

/// `mailbox`: list inbound non-deleted mail for the player's account,
/// newest first. Each line shows `# unread? sender — subject`.
/// `readmail <#>` reads the body and marks the row read.
pub(crate) async fn cmd_mailbox(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    let Some(account) = require_linked_account(world, player, "mail") else {
        return;
    };
    let user_id = account.user_id;
    let pool = pool.clone();
    off_tick::run(
        world,
        player,
        async move { mailbox_text(&pool, &user_id).await },
        |world, player, text| send_to(world, player, text),
    )
    .await;
}

async fn mailbox_text(pool: &mud_db::sqlx::PgPool, user_id: &str) -> String {
    let rows = match mud_db::mail::inbox_for(pool, user_id).await {
        Ok(r) => r,
        Err(e) => return format!("Mail fetch failed: {e}\r\n"),
    };
    if rows.is_empty() {
        return "\r\nYour mailbox is empty.\r\n".to_string();
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
    out.push_str("\r\n* = unread.   Use 'readmail <#>' to read, 'delmail <#>' to delete.\r\n");
    out
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
            "Read which mail? Pick a number from 'mailbox'.\r\n",
        );
        return;
    };
    if slot == 0 {
        send_to(world, player, "Mail slots are 1-based.\r\n");
        return;
    }
    let Some(account) = require_linked_account(world, player, "mail") else {
        return;
    };
    let user_id = account.user_id;
    let pool = pool.clone();
    let read = async move {
        let rows = mud_db::mail::inbox_for(&pool, &user_id)
            .await
            .map_err(|e| format!("Mail fetch failed: {e}\r\n"))?;
        let Some(row) = rows.get(slot - 1) else {
            return Err(format!("No mail at slot {slot}.\r\n"));
        };
        let mut out = String::from("\r\n");
        out.push_str(&format!("From:    {}\r\n", row.sender_display_name));
        out.push_str(&format!("Subject: {}\r\n", row.subject));
        out.push_str(&format!(
            "Sent:    {}\r\n",
            row.sent_at.format("%Y-%m-%d %H:%M")
        ));
        out.push_str("---\r\n");
        out.push_str(row.body.trim_end());
        out.push_str("\r\n---\r\n");
        if let Err(e) = mud_db::mail::mark_read(&pool, row.id).await {
            tracing::warn!(error = %e, mail_id = row.id, "mark_read failed");
        }
        Ok(out)
    };
    off_tick::run(world, player, read, |world, player, text| match text {
        Ok(out) => crate::commands::send_prose(world, player, out),
        Err(line) => send_to(world, player, line),
    })
    .await;
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
            "Delete which mail? Pick a number from 'mailbox'.\r\n",
        );
        return;
    };
    if slot == 0 {
        send_to(world, player, "Mail slots are 1-based.\r\n");
        return;
    }
    let Some(account) = require_linked_account(world, player, "mail") else {
        return;
    };
    let user_id = account.user_id;
    let pool = pool.clone();
    let delete = async move {
        let rows = mud_db::mail::inbox_for(&pool, &user_id)
            .await
            .map_err(|e| format!("Mail fetch failed: {e}\r\n"))?;
        let Some(row) = rows.get(slot - 1) else {
            return Err(format!("No mail at slot {slot}.\r\n"));
        };
        mud_db::mail::soft_delete(&pool, row.id)
            .await
            .map_err(|e| format!("Delete failed: {e}\r\n"))?;
        Ok(format!("Deleted mail #{slot}: \"{}\".\r\n", row.subject))
    };
    off_tick::run(world, player, delete, |world, player, text| {
        send_to(world, player, text.unwrap_or_else(|line| line));
    })
    .await;
}

#[cfg(test)]
mod notify_tests {
    use super::*;
    use crate::commands::test_support::{drain, player_in};

    #[test]
    fn online_recipient_account_is_told_about_new_mail() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let account = |user: &str| Account {
            user_id: user.into(),
            character_id: "c".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        };
        let (target, mut trx) = player_in(&mut world, room);
        world
            .entity_mut(target)
            .insert((account("u-target"), mud_world::Online));
        let (other, mut orx) = player_in(&mut world, room);
        world
            .entity_mut(other)
            .insert((account("u-other"), mud_world::Online));
        let (offline, mut frx) = player_in(&mut world, room);
        world.entity_mut(offline).insert(account("u-target"));

        notify_new_mail(&mut world, "u-target");

        assert!(
            drain(&mut trx).contains("You have new mail waiting at the post office."),
            "recipient told"
        );
        assert!(drain(&mut orx).is_empty(), "other accounts are not told");
        assert!(
            drain(&mut frx).is_empty(),
            "offline characters are not told"
        );
    }

    /// The draft leaves the player when `.send` starts, so a send whose
    /// answer never comes back cannot be sent twice; a failed one returns it.
    #[tokio::test(flavor = "current_thread")]
    async fn send_takes_the_draft_and_a_failure_gives_it_back() {
        let mut world = World::new();
        world.init_resource::<crate::commands::off_tick::CommandJobs>();
        let room = world.spawn_empty().id();
        let (player, mut rx) = player_in(&mut world, room);
        world.entity_mut(player).insert((
            Account {
                user_id: "u-sender".into(),
                character_id: "c".into(),
                role: UserRole::Player,
                account_role: UserRole::Player,
                perms: vec![],
            },
            MailDraft {
                recipient_user_id: "u-rcpt".into(),
                recipient_label: "Rcpt".into(),
                subject: Some("hi".into()),
                body: vec!["hello".into()],
            },
        ));
        let pool = mud_db::sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(100))
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap();
        compose_mail_step(&mut world, player, &pool, ".send").await;
        assert!(
            world.get::<MailDraft>(player).is_none(),
            "a draft left behind could be sent twice"
        );
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        crate::commands::off_tick::drain_command_jobs(&mut world);
        assert!(
            world.get::<MailDraft>(player).is_some(),
            "failed send keeps the draft"
        );
        assert!(drain(&mut rx).contains("Send failed"));
    }
}
