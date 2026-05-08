//! `unban <player>` — Implementor admin command. Lifts the most
//! recent active `BanRecords` row on the owning Users account.
//!
//! Second migration of the inventory-distributed pattern. Larger
//! than `balance` — exercises async dispatch, outbound channel
//! plumbing, and `record_admin_action`. Proves the pattern fits
//! non-trivial commands too.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;

use crate::commands::{
    Account, Category, Command, Connection, DbPool, Help, record_admin_action, send_to,
};

inventory::submit! {
    Command {
        names: &["unban", "pardon"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "unban <player>",
            summary: "Lift the active ban on a player's account.",
            long: "Implementor-only. Inverse of `ban`. Doesn't \
                   delete the row — flips active=false and stamps \
                   unbanned_at / unbanned_by for the audit trail.",
        },
        run: cmd_unban,
    }
}

fn cmd_unban(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "unban", args);
    let target_name = args.trim();
    if target_name.is_empty() {
        send_to(world, player, "Usage: unban <player>\r\n");
        return;
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        send_to(world, player, "Database unavailable.\r\n");
        return;
    };
    let outbound = world.get::<Connection>(player).map(|c| c.0.clone());
    let admin_uid = world.get::<Account>(player).map(|a| a.user_id.clone());
    let target_name = target_name.to_string();
    tokio::spawn(async move {
        let Some(out) = outbound else { return };
        let Some(admin_uid) = admin_uid else { return };
        let Ok(Some(target)) =
            mud_db::characters::find_by_name(&pool, &target_name).await
        else {
            let _ = out
                .try_send(format!("No character named '{target_name}'.\r\n").into_bytes());
            return;
        };
        let Some(uid) = target.user_id else {
            let _ = out.try_send(
                format!("{} has no associated user account.\r\n", target.name)
                    .into_bytes(),
            );
            return;
        };
        match mud_db::bans::unban(&pool, &uid, &admin_uid).await {
            Ok(0) => {
                let _ = out.try_send(
                    format!("{} has no active ban.\r\n", target.name).into_bytes(),
                );
            }
            Ok(_) => {
                let _ = out.try_send(format!("Unbanned {}.\r\n", target.name).into_bytes());
            }
            Err(e) => {
                let _ = out.try_send(format!("Unban write failed: {e}\r\n").into_bytes());
            }
        }
    });
}
