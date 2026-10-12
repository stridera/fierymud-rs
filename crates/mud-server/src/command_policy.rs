//! Lookup of the builder-editable per-command policy flags (`mud_world::CommandPolicy`).
//!
//! `None` means "no row for this command (or no catalog at all)": each caller then falls back
//! to its built-in list, so the answers do not change when the table, its columns, or a
//! command's row is missing.

use bevy_ecs::prelude::World;
use mud_world::{CommandPolicy, PolicyFlag};

/// The `"Command"` flag for the command's canonical name, if the loaded catalog has a row.
#[must_use]
pub(crate) fn flag(world: &World, command: &str, flag: PolicyFlag) -> Option<bool> {
    world.get_resource::<CommandPolicy>()?.flag(command, flag)
}
