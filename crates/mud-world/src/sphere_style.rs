//! Per-sphere display colours.
//!
//! The spells / skills listings (`<red>(fire)</>`) and the `<fire>`-style tag
//! aliases both colour by an ability's sphere. The colour is data: the
//! `"SphereStyle"` table (`color` = XML-Lite tag body without brackets, e.g.
//! `red`, `b:cyan`), editable in Muditor and seeded by
//! `fierylib/data/sql/2026-10-12-sphere-colors.sql`. A missing row, a NULL
//! colour, or a database that predates the table all fall back to
//! [`DEFAULT_PALETTE`], so behaviour is identical with or without the patch.
//!
//! Render code (`render_color_tags`, the ability listings) has no `World`
//! handle, so the loaded styles are published once at boot with [`install`]
//! and read through [`color`].

use std::collections::HashMap;
use std::sync::RwLock;

/// Built-in palette: the fallback for any sphere the database has no colour
/// for. `generic` is deliberately absent (listings render it dim).
pub const DEFAULT_PALETTE: [(&str, &str); 10] = [
    ("fire", "red"),
    ("water", "cyan"),
    ("air", "b:cyan"),
    ("earth", "yellow"),
    ("healing", "green"),
    ("death", "b:black"),
    ("protection", "b:white"),
    ("enchantment", "magenta"),
    ("summoning", "b:magenta"),
    ("divination", "b:yellow"),
];

/// Colours loaded from `"SphereStyle"`, keyed by lowercase sphere label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SphereStyles {
    colors: HashMap<String, String>,
}

impl SphereStyles {
    /// Build from `(sphere, color)` rows. Blank colours (NULL / empty) and
    /// colours with characters no tag body can contain are skipped, so those
    /// spheres keep the built-in default.
    #[must_use]
    pub fn from_rows<I>(rows: I) -> Self
    where
        I: IntoIterator<Item = (String, Option<String>)>,
    {
        let mut colors = HashMap::new();
        for (sphere, color) in rows {
            let Some(color) = color else { continue };
            let color = color.trim().to_ascii_lowercase();
            if color.is_empty()
                || !color
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'#' | b'_' | b'-'))
            {
                continue;
            }
            colors.insert(sphere.trim().to_ascii_lowercase(), color);
        }
        Self { colors }
    }

    /// Colour tag body for `sphere` (lowercase label): the database value,
    /// else the built-in default, else `None` (unmapped / generic).
    #[must_use]
    pub fn color(&self, sphere: &str) -> Option<&str> {
        self.colors
            .get(sphere)
            .map(String::as_str)
            .or_else(|| default_color(sphere))
    }

    /// Number of spheres with a database colour.
    #[must_use]
    pub fn len(&self) -> usize {
        self.colors.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.colors.is_empty()
    }
}

/// The built-in colour for `sphere`, ignoring the database.
#[must_use]
pub fn default_color(sphere: &str) -> Option<&'static str> {
    DEFAULT_PALETTE
        .iter()
        .find(|(s, _)| *s == sphere)
        .map(|(_, c)| *c)
}

static ACTIVE: RwLock<Option<SphereStyles>> = RwLock::new(None);

/// Publish the loaded styles for [`color`]. Called by the world loader.
pub fn install(styles: SphereStyles) {
    *ACTIVE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(styles);
}

/// Colour tag body for `sphere` (lowercase label) from the installed styles,
/// else the built-in default.
#[must_use]
pub fn color(sphere: &str) -> Option<String> {
    let guard = ACTIVE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.as_ref() {
        Some(styles) => styles.color(sphere).map(str::to_owned),
        None => default_color(sphere).map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(items: &[(&str, Option<&str>)]) -> SphereStyles {
        SphereStyles::from_rows(
            items
                .iter()
                .map(|(s, c)| ((*s).to_string(), c.map(str::to_string))),
        )
    }

    #[test]
    fn empty_styles_use_the_builtin_palette() {
        let none = SphereStyles::default();
        for (sphere, color) in DEFAULT_PALETTE {
            assert_eq!(none.color(sphere), Some(color), "{sphere}");
        }
        assert_eq!(none.color("generic"), None);
        assert_eq!(none.color("FIRE"), None);
        assert_eq!(none.color(""), None);
    }

    #[test]
    fn seeded_rows_are_identical_to_the_builtin_palette() {
        // What data/sql/2026-10-12-sphere-colors.sql inserts.
        let seeded = rows(&[
            ("fire", Some("red")),
            ("water", Some("cyan")),
            ("air", Some("b:cyan")),
            ("earth", Some("yellow")),
            ("healing", Some("green")),
            ("death", Some("b:black")),
            ("protection", Some("b:white")),
            ("enchantment", Some("magenta")),
            ("summoning", Some("b:magenta")),
            ("divination", Some("b:yellow")),
            ("generic", None),
        ]);
        let none = SphereStyles::default();
        for sphere in [
            "fire",
            "water",
            "air",
            "earth",
            "healing",
            "death",
            "protection",
            "enchantment",
            "summoning",
            "divination",
            "generic",
        ] {
            assert_eq!(seeded.color(sphere), none.color(sphere), "{sphere}");
        }
        assert_eq!(seeded.len(), 10);
    }

    #[test]
    fn database_colour_overrides_and_null_falls_back() {
        let s = rows(&[
            ("fire", Some(" B:Red ")),
            ("water", None),
            ("air", Some("")),
            ("earth", Some("<yellow>")),
            ("generic", Some("white")),
        ]);
        assert_eq!(s.color("fire"), Some("b:red"));
        assert_eq!(s.color("water"), Some("cyan"));
        assert_eq!(s.color("air"), Some("b:cyan"));
        assert_eq!(s.color("earth"), Some("yellow"), "bad tag body ignored");
        assert_eq!(s.color("generic"), Some("white"));
    }

    #[test]
    fn global_lookup_falls_back_until_installed_values_override() {
        // Only this test touches the process-wide styles in this crate.
        assert_eq!(color("fire").as_deref(), Some("red"));
        install(rows(&[("fire", Some("magenta"))]));
        assert_eq!(color("fire").as_deref(), Some("magenta"));
        assert_eq!(color("water").as_deref(), Some("cyan"));
        install(SphereStyles::default());
        assert_eq!(color("fire").as_deref(), Some("red"));
    }
}
