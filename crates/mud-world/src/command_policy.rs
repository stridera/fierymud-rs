//! Builder-editable per-command policy flags.
//!
//! Which commands run mid-cast, while paralyzed, without revealing a hider, or are held by
//! cast lag used to be four hard-coded name lists (legacy `CMD_*` flags). The flags now live
//! in `"Command"` (editable in Muditor by a coder; seeded by
//! `fierylib/data/sql/2026-10-12-command-policy-flags.sql`). The lists stay in the server as
//! the fallback for a command with no row, or a database that predates the columns, so the
//! answers are identical with or without the patch.

use std::collections::HashMap;

use bevy_ecs::prelude::Resource;

/// One flag column of the `Command` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyFlag {
    AllowedWhileCasting,
    AllowedWhileParalyzed,
    KeepsHidden,
    HeldByCastLag,
}

#[allow(clippy::struct_excessive_bools)] // one column per flag
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PolicyFlags {
    pub allowed_while_casting: bool,
    pub allowed_while_paralyzed: bool,
    pub keeps_hidden: bool,
    pub held_by_cast_lag: bool,
}

#[derive(Resource, Debug, Default, Clone)]
pub struct CommandPolicy {
    /// Canonical command name -> flags.
    by_name: HashMap<String, PolicyFlags>,
}

impl CommandPolicy {
    pub fn from_rows(rows: impl IntoIterator<Item = (String, PolicyFlags)>) -> Self {
        Self {
            by_name: rows.into_iter().collect(),
        }
    }

    /// The flag for the command, or `None` when it has no row (the caller then uses its
    /// built-in list).
    #[must_use]
    pub fn flag(&self, name: &str, flag: PolicyFlag) -> Option<bool> {
        let f = self.by_name.get(name)?;
        Some(match flag {
            PolicyFlag::AllowedWhileCasting => f.allowed_while_casting,
            PolicyFlag::AllowedWhileParalyzed => f.allowed_while_paralyzed,
            PolicyFlag::KeepsHidden => f.keeps_hidden,
            PolicyFlag::HeldByCastLag => f.held_by_cast_lag,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}
