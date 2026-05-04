//! `balance` / `bal` — readout of the player's bank balance.
//!
//! First command migrated to the distributed-registration pattern.
//! Proof of concept that `inventory::submit!` plus `all_commands()`
//! lets a command live next to its handler without editing the
//! central `COMMANDS` array.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{BankWealth, Wealth};

use crate::commands::{
    Category, Command, Help, format_wealth, send_to,
};

inventory::submit! {
    Command {
        names: &["balance", "bal"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
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
    let bank = world.get::<BankWealth>(player).map_or(0, |b| b.0);
    let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);
    let bank_line = format_wealth(bank).map_or_else(
        || "Your bank balance is empty.".to_string(),
        |parts| format!("Your bank balance is {parts}."),
    );
    // Surface on-hand alongside the bank so the player gets a
    // complete coin picture. `wealth` shows the same on-hand
    // value standalone; this is "what's in the bank vs. on me?"
    // without round-tripping through both.
    let on_hand_line = format_wealth(on_hand).map_or_else(
        || "On hand: nothing.".to_string(),
        |parts| format!("On hand:  {parts}."),
    );
    send_to(
        world,
        player,
        format!("\r\n{bank_line}\r\n{on_hand_line}\r\n"),
    );
}
