//! All 12 directional movement commands (n/s/e/w/ne/nw/se/sw/u/d
//! plus `in` / `out`). Each is a thin shim that calls
//! `commands::cmd_move(world, player, Direction::X)` — the macro
//! below generates them all from a single declaration so adding
//! a new direction is a one-line schema change.
//!
//! Step 2 of the inventory-distributed migration: this is the
//! first whole-category migration. Movement was the smallest
//! category at 15 entries; the 12 directional commands plus
//! `recall` / `release` / `enter` complete it.

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, UserRole};

use crate::commands::{Category, Command, MOVE_HELP, cmd_move};

macro_rules! mv {
    ($name:ident, $dir:ident, $($alias:literal),+) => {
        fn $name(world: &mut World, player: Entity, _args: &str) {
            cmd_move(world, player, Direction::$dir);
        }
        inventory::submit! {
            Command {
                names: &[$($alias),+],
                min_role: UserRole::Player,
                required_perm: None,
                category: Category::Movement,
                help: MOVE_HELP,
                run: $name,
            }
        }
    };
}

mv!(cmd_north, North, "north", "n");
mv!(cmd_south, South, "south", "s");
mv!(cmd_east, East, "east", "e");
mv!(cmd_west, West, "west", "w");
mv!(cmd_up, Up, "up", "u");
mv!(cmd_down, Down, "down", "d");
mv!(cmd_northeast, Northeast, "northeast", "ne");
mv!(cmd_northwest, Northwest, "northwest", "nw");
mv!(cmd_southeast, Southeast, "southeast", "se");
mv!(cmd_southwest, Southwest, "southwest", "sw");
mv!(cmd_in_dir, In, "in");
mv!(cmd_out_dir, Out, "out");
