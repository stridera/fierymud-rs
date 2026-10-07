//! Shared, prefix-anchored name matching for every target resolver.
//!
//! Legacy `CircleMUD` semantics (`isname` / `is_abbrev`): a typed word
//! matches a keyword only when it is a case-insensitive *prefix* of that
//! keyword (`og` matches "ogre"; `gre` does not). A multi-word needle
//! (`cure l`) matches when every word is a prefix of some word of the
//! candidate. Ordinal (`2.sword`) and `all.` parsing stay with the callers;
//! this module only decides whether one entity's names match a needle.

/// True when `word` is a non-empty, case-insensitive prefix of `candidate`.
#[must_use]
pub fn is_abbrev(word: &str, candidate: &str) -> bool {
    !word.is_empty()
        && candidate.len() >= word.len()
        && candidate.as_bytes()[..word.len()].eq_ignore_ascii_case(word.as_bytes())
}

/// True when every whitespace-separated word of `needle` is a prefix of at
/// least one whitespace-separated word of some name in `names`. An empty
/// needle never matches.
pub fn names_match<'a, I>(needle: &str, names: I) -> bool
where
    I: IntoIterator<Item = &'a str>,
{
    let names: Vec<&str> = names.into_iter().collect();
    let mut any_word = false;
    for word in needle.split_whitespace() {
        any_word = true;
        let found = names
            .iter()
            .any(|name| name.split_whitespace().any(|w| is_abbrev(word, w)));
        if !found {
            return false;
        }
    }
    any_word
}

/// Match an entity by its keyword list; when it has no keywords, fall back
/// to the words of its display name (players carry only a name).
#[must_use]
pub fn entity_matches(needle: &str, name: &str, keywords: Option<&[String]>) -> bool {
    match keywords {
        Some(kw) if !kw.is_empty() => names_match(needle, kw.iter().map(String::as_str)),
        _ => names_match(needle, std::iter::once(name)),
    }
}

/// Resolve `needle` against a set of ability names (already
/// underscore/lowercase normalised or not; both sides are normalised here).
/// An exact (normalised) match wins; otherwise the first prefix match in
/// alphabetical order, so the result never depends on map iteration order.
#[must_use]
pub fn best_name_match<'a, I>(needle: &str, names: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let norm = |s: &str| {
        s.split(|c: char| c == '_' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("_")
            .to_ascii_lowercase()
    };
    let needle = norm(needle);
    if needle.is_empty() {
        return None;
    }
    let mut best: Option<(&'a str, String)> = None;
    for name in names {
        let n = norm(name);
        if n == needle {
            return Some(name);
        }
        if is_abbrev(&needle, &n) && best.as_ref().is_none_or(|(_, b)| n < *b) {
            best = Some((name, n));
        }
    }
    best.map(|(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kw(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn prefix_matches_substring_does_not() {
        assert!(is_abbrev("og", "ogre"));
        assert!(is_abbrev("OGRE", "ogre"));
        assert!(!is_abbrev("gre", "ogre"));
        assert!(!is_abbrev("", "ogre"));
        assert!(!is_abbrev("ogres", "ogre"));
    }

    #[test]
    fn entity_matches_keywords_by_prefix_only() {
        let k = kw(&["ogre", "large", "brute"]);
        assert!(entity_matches("og", "a large ogre", Some(&k)));
        assert!(entity_matches("br", "a large ogre", Some(&k)));
        assert!(!entity_matches("gre", "a large ogre", Some(&k)));
        assert!(!entity_matches("", "a large ogre", Some(&k)));
    }

    #[test]
    fn sign_does_not_match_assignment() {
        let k = kw(&["assignment", "banshee"]);
        assert!(!entity_matches(
            "sign",
            "an assignment for the Banshee",
            Some(&k)
        ));
        assert!(entity_matches(
            "assign",
            "an assignment for the Banshee",
            Some(&k)
        ));
    }

    #[test]
    fn name_words_used_only_without_keywords() {
        assert!(entity_matches("str", "Strider", None));
        assert!(!entity_matches("rider", "Strider", None));
        assert!(entity_matches("str", "Strider", Some(&[])));
        let k = kw(&["sword"]);
        assert!(!entity_matches("rusty", "a rusty sword", Some(&k)));
    }

    #[test]
    fn multi_word_needle_needs_every_word() {
        let k = kw(&["cure", "light"]);
        assert!(names_match("cure l", k.iter().map(String::as_str)));
        assert!(!names_match("cure x", k.iter().map(String::as_str)));
    }

    #[test]
    fn exact_ability_name_beats_prefix() {
        let names = [
            "invisibility",
            "mass_invisibility",
            "invis",
            "invisible_stalker",
        ];
        assert_eq!(
            best_name_match("invis", names.iter().copied()),
            Some("invis")
        );
        assert_eq!(
            best_name_match(
                "invis",
                ["mass_invisibility", "invisibility"].iter().copied()
            ),
            Some("invisibility")
        );
        assert_eq!(
            best_name_match("mass inv", names.iter().copied()),
            Some("mass_invisibility")
        );
        assert_eq!(best_name_match("sibility", names.iter().copied()), None);
        assert_eq!(
            best_name_match("cure l", ["cure_light", "cure_critic"].iter().copied()),
            Some("cure_light")
        );
    }
}
