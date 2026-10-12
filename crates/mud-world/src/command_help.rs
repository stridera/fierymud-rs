//! Builder-editable `help <command>` text.
//!
//! The one-line summary and the long paragraph of each command live in
//! `"Command".help_summary` / `help_text` (editable in Muditor, seeded by
//! `fierylib/data/sql/2026-10-12-command-help-text.sql`). The strings in the
//! Rust command registry stay as the fallback for any command with no row, a
//! NULL or empty value, or a database that predates the columns, so `help`
//! output is identical with or without the patch.

use std::collections::HashMap;

use bevy_ecs::prelude::Resource;

#[derive(Resource, Debug, Default, Clone)]
pub struct CommandHelp {
    /// Canonical command name -> (summary, long); empty strings are dropped.
    by_name: HashMap<String, (Option<String>, Option<String>)>,
}

impl CommandHelp {
    pub fn from_rows(
        rows: impl IntoIterator<Item = (String, Option<String>, Option<String>)>,
    ) -> Self {
        let keep = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
        let by_name = rows
            .into_iter()
            .map(|(name, summary, long)| (name, (keep(summary), keep(long))))
            .filter(|(_, (s, l))| s.is_some() || l.is_some())
            .collect();
        Self { by_name }
    }

    /// DB summary for the command, if one is set.
    #[must_use]
    pub fn summary(&self, name: &str) -> Option<&str> {
        self.by_name.get(name)?.0.as_deref()
    }

    /// DB long paragraph for the command, if one is set.
    #[must_use]
    pub fn long(&self, name: &str) -> Option<&str> {
        self.by_name.get(name)?.1.as_deref()
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
