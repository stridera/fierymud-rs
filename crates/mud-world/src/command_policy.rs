//! Builder-editable per-command policy flags.
//!
//! Which commands run mid-cast, while paralyzed, without revealing a hider, or are held by
//! cast lag used to be four hard-coded name lists (legacy `CMD_*` flags). The flags now live
//! in `"Command"` (editable in Muditor by a coder; seeded by
//! `fierylib/data/sql/2026-10-12-command-policy-flags.sql`). The lists stay in the server as
//! the fallback for a command with no row, or a database that predates the columns, so the
//! answers are identical with or without the patch.
//!
//! A column nobody has seeded (every row `false`, which is what the columns default to when
//! they were added but `command-policy-flags.sql` never ran) is treated as absent for that
//! flag: every registry command has a row, so reading it literally would turn each list into
//! "nothing allowed". Such a flag falls back to its list; a column with at least one `true`
//! is authoritative.

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

impl PolicyFlag {
    pub const ALL: [PolicyFlag; 4] = [
        PolicyFlag::AllowedWhileCasting,
        PolicyFlag::AllowedWhileParalyzed,
        PolicyFlag::KeepsHidden,
        PolicyFlag::HeldByCastLag,
    ];

    /// The `"Command"` column this flag is stored in.
    #[must_use]
    pub fn column(self) -> &'static str {
        match self {
            PolicyFlag::AllowedWhileCasting => "allowed_while_casting",
            PolicyFlag::AllowedWhileParalyzed => "allowed_while_paralyzed",
            PolicyFlag::KeepsHidden => "keeps_hidden",
            PolicyFlag::HeldByCastLag => "held_by_cast_lag",
        }
    }
}

#[allow(clippy::struct_excessive_bools)] // one column per flag
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PolicyFlags {
    pub allowed_while_casting: bool,
    pub allowed_while_paralyzed: bool,
    pub keeps_hidden: bool,
    pub held_by_cast_lag: bool,
}

impl PolicyFlags {
    fn get(self, flag: PolicyFlag) -> bool {
        match flag {
            PolicyFlag::AllowedWhileCasting => self.allowed_while_casting,
            PolicyFlag::AllowedWhileParalyzed => self.allowed_while_paralyzed,
            PolicyFlag::KeepsHidden => self.keeps_hidden,
            PolicyFlag::HeldByCastLag => self.held_by_cast_lag,
        }
    }
}

#[derive(Resource, Debug, Default, Clone)]
pub struct CommandPolicy {
    /// Canonical command name -> flags.
    by_name: HashMap<String, PolicyFlags>,
    /// Per flag (indexed like [`PolicyFlag::ALL`]): does any row have it set? `false` means
    /// the column was never seeded and the flag answers `None` for every command.
    seeded: [bool; 4],
}

impl CommandPolicy {
    pub fn from_rows(rows: impl IntoIterator<Item = (String, PolicyFlags)>) -> Self {
        let by_name: HashMap<String, PolicyFlags> = rows.into_iter().collect();
        let seeded = PolicyFlag::ALL.map(|flag| by_name.values().any(|f| f.get(flag)));
        Self { by_name, seeded }
    }

    /// Flags whose column has no `true` in any row (never seeded), so they use the built-in
    /// list. Empty for an empty policy: that case is reported by the loader separately.
    #[must_use]
    pub fn unseeded(&self) -> Vec<PolicyFlag> {
        if self.by_name.is_empty() {
            return Vec::new();
        }
        PolicyFlag::ALL
            .into_iter()
            .zip(self.seeded)
            .filter_map(|(flag, seeded)| (!seeded).then_some(flag))
            .collect()
    }

    /// The flag for the command, or `None` when it has no row or the flag's column was never
    /// seeded (the caller then uses its built-in list).
    #[must_use]
    pub fn flag(&self, name: &str, flag: PolicyFlag) -> Option<bool> {
        if !self.seeded[flag as usize] {
            return None;
        }
        Some(self.by_name.get(name)?.get(flag))
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
