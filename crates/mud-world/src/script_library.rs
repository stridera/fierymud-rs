//! Importable Lua libraries: the `name -> source` table behind the script `require(name)`.
//!
//! Loaded from `"ScriptLibrary"` at boot and again by the trigger reload (admin endpoint and the
//! `treload` command). The script host keeps a per-Lua-state cache of evaluated modules and
//! drops it whenever [`ScriptLibraries::generation`] changes, so a reload makes the next
//! `require` re-evaluate the new source.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use bevy_ecs::prelude::Resource;
use mud_db::script_libraries::ScriptLibraryRow;

/// Source of fresh generations; 0 is reserved for "never loaded" (the `Default`).
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Resource, Debug, Clone, Default)]
pub struct ScriptLibraries {
    sources: HashMap<String, String>,
    generation: u64,
}

impl ScriptLibraries {
    /// Build from DB rows. Every call gets a new generation.
    #[must_use]
    pub fn from_rows(rows: impl IntoIterator<Item = ScriptLibraryRow>) -> Self {
        Self::from_sources(rows.into_iter().map(|r| (r.name, r.code)))
    }

    /// Build from `(name, source)` pairs (tests, tools). Every call gets a new generation.
    #[must_use]
    pub fn from_sources(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            sources: pairs.into_iter().collect(),
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Lua source of the library called `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.sources.get(name).map(String::as_str)
    }

    /// Identifies this load. Differs between any two loads (reloads included).
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_by_name() {
        let libs = ScriptLibraries::from_sources([("util".to_string(), "return 1".to_string())]);
        assert_eq!(libs.get("util"), Some("return 1"));
        assert_eq!(libs.get("nope"), None);
        assert_eq!(libs.len(), 1);
    }

    #[test]
    fn every_load_gets_a_new_generation() {
        let a = ScriptLibraries::from_sources([]);
        let b = ScriptLibraries::from_sources([]);
        assert_ne!(a.generation(), b.generation());
        assert_ne!(ScriptLibraries::default().generation(), a.generation());
    }
}
