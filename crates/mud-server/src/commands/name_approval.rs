//! Character name-approval admin commands.
//!
//! When the live `social.name_approval_required` GameConfig flag is
//! ON, fresh characters are created with `Characters.name_approved =
//! false` and spawn carrying the `NameApprovalPending` marker. The
//! player can move / look / fight normally; every social command
//! (`tell`, `say`, `gossip`, clan / group chat, etc.) refuses with a
//! "your name is awaiting staff approval" line until the marker is
//! removed.
//!
//! Staff resolves the gate one of two ways:
//! * `approve_name <character>` — keep the chosen name. Flips
//!   `name_approved = true`, drops the marker on the live entity if
//!   the player is online, and DMs them.
//! * `reject_name <character> <new-name>` — force-rename. Refuses
//!   while the target is online (renames need the in-memory Named
//!   cache to rebuild via reconnect). The new staff-chosen name is
//!   self-approving, so the same flow renames + flips
//!   `name_approved` back to `true`.
//!
//! Players can run `name_status` to see their own gate state.
//!
//! `approve_name` / `reject_name` are Immortal+; `name_status` is
//! Player-level so anyone with a pending name can self-diagnose.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{NameApprovalPending, Named, Online, Player};

use crate::commands::{
    AsyncCommand, Category, Command, Help, cmd_mail_stub, name_of, record_admin_action,
    send_rendered, send_to, try_remove,
};

inventory::submit! {
    Command {
        names: &["approve_name", "approvename"],
        min_role: UserRole::Immortal,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "approve_name <character>",
            summary: "Approve a pending character name (keep the name).",
            long: "Immortal+. Flips Characters.name_approved to true \
                   for the named character. Removes the \
                   NameApprovalPending marker on the live entity if \
                   they're online and DMs them. No-op against an \
                   already-approved character. Pair with `reject_name \
                   <char> <new>` if the name needs to change.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["reject_name", "rejectname"],
        min_role: UserRole::Immortal,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "reject_name <character> <new-name>",
            summary: "Force-rename a pending character and approve.",
            long: "Immortal+. Renames the named character to the \
                   staff-chosen replacement and flips name_approved \
                   to true (the new name is by definition staff-\
                   approved). Refuses while the target is online — \
                   the in-memory Named cache needs a fresh login to \
                   pick up the rename cleanly. A duplicate new-name \
                   surfaces as the Postgres unique-index error.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["name_status", "namestatus"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "name_status",
            summary: "Check whether your character name is staff-approved.",
            long: "Reports the live name-approval gate state. Useful \
                   when chat channels refuse and you want to confirm \
                   that the gate (not an effect / freeze / ignore-\
                   list) is the cause. No-op besides the readout.",
        },
        run: cmd_name_status,
    }
}

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, args| match head {
            "approve_name" | "approvename" => {
                Some(Box::pin(cmd_approve_name_async(world, player, pool, args)))
            }
            "reject_name" | "rejectname" => {
                Some(Box::pin(cmd_reject_name_async(world, player, pool, args)))
            }
            _ => None,
        },
    }
}

/// `name_status` is sync-only — reads the marker on `self`.
fn cmd_name_status(world: &mut World, player: Entity, _args: &str) {
    if world.get::<NameApprovalPending>(player).is_some() {
        send_to(
            world,
            player,
            "Your character name is <b:yellow>awaiting staff approval</>. \
             Social channels (tell / say / gossip / group / clan) are \
             silenced until staff runs `approve_name` (keep the name) \
             or `reject_name` (rename you). You can still move, look, \
             and fight in the meantime.\r\n",
        );
    } else {
        send_to(
            world,
            player,
            "Your character name is <b:green>approved</>. No name-approval \
             gate is active.\r\n",
        );
    }
}

async fn cmd_approve_name_async(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    record_admin_action(world, player, "approve_name", args);
    let target_name = args.trim();
    if target_name.is_empty() {
        send_to(world, player, "Usage: approve_name <character>\r\n");
        return;
    }
    // Offline-safe lookup — we don't require the player to be online
    // for `approve_name` (the column flip is what matters; the
    // marker drop is best-effort).
    let row = match mud_db::characters::find_by_name(pool, target_name).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            send_to(
                world,
                player,
                format!("No character named '{target_name}'.\r\n"),
            );
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, target = %target_name, "approve_name lookup failed");
            send_to(world, player, format!("DB error: {e}\r\n"));
            return;
        }
    };
    if row.name_approved {
        send_to(
            world,
            player,
            format!("'{}' is already approved — no change.\r\n", row.name),
        );
        return;
    }
    if let Err(e) = mud_db::characters::set_name_approved(pool, &row.id, true).await {
        tracing::warn!(error = %e, target = %target_name, "approve_name update failed");
        send_to(world, player, format!("DB error: {e}\r\n"));
        return;
    }
    // Drop the marker on the live entity if the character is online
    // — clears the gate without forcing the player to relog.
    let online_entity = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        q.iter(world)
            .find(|(_, n)| n.name.eq_ignore_ascii_case(&row.name))
            .map(|(e, _)| e)
    };
    let admin_name = name_of(world, player);
    if let Some(target_entity) = online_entity {
        try_remove::<NameApprovalPending>(world, target_entity);
        send_rendered(
            world,
            target_entity,
            &format!(
                "\r\n<b:green>Your character name has been approved by {admin_name}.</> \
                 Social channels are open again.\r\n"
            ),
        );
    }
    send_rendered(
        world,
        player,
        &format!(
            "Approved <b:cyan>{}</> — name_approved flipped to true.\r\n",
            row.name
        ),
    );
    tracing::info!(admin = %admin_name, target = %row.name, "name approved");
}

async fn cmd_reject_name_async(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    record_admin_action(world, player, "reject_name", args);
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(
            world,
            player,
            "Usage: reject_name <character> <new-name>\r\n",
        );
        return;
    }
    let old_name = parts[0];
    let new_name = parts[1];
    // Refuse while target is online — rename needs the Named cache
    // and ConnRouter mappings rebuilt via reconnect (same constraint
    // as `cmd_rename`).
    let online = {
        let mut q = world.query_filtered::<&Named, (With<Player>, With<Online>)>();
        q.iter(world)
            .any(|n| n.name.eq_ignore_ascii_case(old_name))
    };
    if online {
        send_to(
            world,
            player,
            format!("'{old_name}' is online — have them log out first.\r\n"),
        );
        return;
    }
    let row = match mud_db::characters::find_by_name(pool, old_name).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            send_to(
                world,
                player,
                format!("No character named '{old_name}'.\r\n"),
            );
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, target = %old_name, "reject_name lookup failed");
            send_to(world, player, format!("DB error: {e}\r\n"));
            return;
        }
    };
    // Rename first; if the new name is taken we don't want to half-
    // commit a flag flip against a still-pending character.
    match mud_db::characters::rename(pool, &row.id, new_name).await {
        Ok(0) => {
            send_to(
                world,
                player,
                format!("Character row '{old_name}' vanished mid-rename.\r\n"),
            );
            return;
        }
        Ok(_) => {}
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("unique") || msg.contains("duplicate key") {
                send_to(
                    world,
                    player,
                    format!("Name '{new_name}' is already taken.\r\n"),
                );
            } else {
                send_to(world, player, format!("Rename failed: {e}\r\n"));
            }
            return;
        }
    }
    // Flip the gate. A failure here leaves the character renamed-
    // but-still-pending — staff can recover with `approve_name
    // <new-name>` and the player is silenced in the meantime, so
    // this isn't catastrophic. Log loudly so the operator notices.
    if let Err(e) = mud_db::characters::set_name_approved(pool, &row.id, true).await {
        tracing::warn!(
            error = %e,
            target = %new_name,
            "reject_name: rename succeeded but name_approved flip failed",
        );
        send_to(
            world,
            player,
            format!(
                "Renamed to '{new_name}' but failed to clear the gate: {e}. \
                 Recover with `approve_name {new_name}`.\r\n",
            ),
        );
        return;
    }
    let admin_name = name_of(world, player);
    send_rendered(
        world,
        player,
        &format!(
            "Force-renamed <b:cyan>{old_name}</> → <b:cyan>{new_name}</> and approved.\r\n"
        ),
    );
    tracing::info!(
        admin = %admin_name,
        old_name,
        new_name,
        "name rejected; force-renamed",
    );
}
