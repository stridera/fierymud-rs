//! `balance` / `bal` — readout of the player's bank balance.
//!
//! First command migrated to the distributed-registration pattern.
//! Proof of concept that `inventory::submit!` plus `all_commands()`
//! lets a command live next to its handler without editing the
//! central `COMMANDS` array.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::BankWealth;

use crate::commands::{
    Category, Command, Help, format_wealth, send_to,
};

inventory::submit! {
    Command {
        names: &["balance", "bal"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "balance",
            summary: "Show your bank-stored coin.",
            long: "Read-only display of the `bank_wealth` column from \
                   your character row. Pair with `deposit` / \
                   `withdraw` to move coin to / from the bank.",
        },
        run: cmd_balance,
    }
}

fn cmd_balance(world: &mut World, player: Entity, _args: &str) {
    let total = world.get::<BankWealth>(player).map_or(0, |b| b.0);
    let msg = if let Some(parts) = format_wealth(total) {
        format!("\r\nYour bank balance is {parts}.\r\n")
    } else {
        "\r\nYour bank balance is empty.\r\n".to_string()
    };
    send_to(world, player, msg);
}
