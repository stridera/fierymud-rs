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

/// True when the typed `word` matches one of `names`' words.
///
/// A plain word matches when it is a prefix of a name word (legacy
/// `isname`). A hyphenated word (`short-sword`) matches either as a whole
/// (a keyword that itself contains a hyphen, `half-elf`) or, legacy
/// style, when *every* hyphen-separated part is a prefix of some name
/// word: `x-y-z` targets the thing whose keywords include `x`, `y` and `z`.
/// Builders no longer hyphenate keywords by hand, so this is how aliases
/// written against `short-sword` keep working.
fn word_matches(word: &str, names: &[&str]) -> bool {
    let direct = |w: &str| {
        names
            .iter()
            .any(|name| name.split_whitespace().any(|n| is_abbrev(w, n)))
    };
    if direct(word) {
        return true;
    }
    if !word.contains('-') {
        return false;
    }
    let mut any_part = false;
    for part in word.split('-').filter(|p| !p.is_empty()) {
        any_part = true;
        if !direct(part) {
            return false;
        }
    }
    any_part
}

/// True when every whitespace-separated word of `needle` matches (see
/// [`word_matches`]: prefix of a name word, or all parts of a hyphenated
/// word). An empty needle never matches.
pub fn names_match<'a, I>(needle: &str, names: I) -> bool
where
    I: IntoIterator<Item = &'a str>,
{
    let names: Vec<&str> = names.into_iter().collect();
    let mut any_word = false;
    for word in needle.split_whitespace() {
        any_word = true;
        if !word_matches(word, &names) {
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

/// How a typed ability name relates to a candidate name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameRank {
    /// Same words, case-insensitively (`_` and spaces are equivalent).
    Exact,
    /// Each typed word is a prefix of the corresponding candidate word, in
    /// order (`c l` -> `cure_light`, `fire` -> `fire_breath`).
    Prefix,
    None,
}

/// Rank `needle` against one ability name, legacy word-by-word style.
#[must_use]
pub fn rank_ability_name(needle: &str, name: &str) -> NameRank {
    let split = |s: &'_ str| -> Vec<String> {
        s.split(|c: char| c == '_' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .map(str::to_ascii_lowercase)
            .collect()
    };
    let typed = split(needle);
    let words = split(name);
    if typed.is_empty() || typed.len() > words.len() {
        return NameRank::None;
    }
    if !typed.iter().zip(&words).all(|(t, w)| is_abbrev(t, w)) {
        return NameRank::None;
    }
    if typed == words {
        NameRank::Exact
    } else {
        NameRank::Prefix
    }
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
    fn hyphenated_needle_requires_every_part() {
        let k = kw(&["sword", "short", "steel"]);
        assert!(entity_matches("short-sword", "a short sword", Some(&k)));
        assert!(entity_matches("sh-sw", "a short sword", Some(&k)));
        assert!(entity_matches(
            "steel-short-sword",
            "a short sword",
            Some(&k)
        ));
        assert!(!entity_matches("long-sword", "a short sword", Some(&k)));
        assert!(!entity_matches("short-axe", "a short sword", Some(&k)));
        // Stray or trailing hyphens degrade to their real parts; all-hyphen
        // input matches nothing.
        assert!(entity_matches("short-", "a short sword", Some(&k)));
        assert!(!entity_matches("-", "a short sword", Some(&k)));
        assert!(!entity_matches("--", "a short sword", Some(&k)));
    }

    #[test]
    fn hyphenated_keyword_still_matches_whole() {
        let k = kw(&["half-elf", "guard"]);
        assert!(entity_matches("half-elf", "a guard", Some(&k)));
        assert!(entity_matches("half-e", "a guard", Some(&k)));
        assert!(entity_matches("half-guard", "a guard", Some(&k)));
    }

    #[test]
    fn hyphenated_needle_falls_back_to_name_words() {
        assert!(entity_matches("big-str", "Big Strider", None));
        assert!(!entity_matches("big-rider", "Big Strider", None));
    }

    #[test]
    fn ability_rank_is_word_by_word() {
        assert_eq!(rank_ability_name("c l", "cure_light"), NameRank::Prefix);
        assert_eq!(rank_ability_name("cure l", "CURE_LIGHT"), NameRank::Prefix);
        assert_eq!(
            rank_ability_name("cure_light", "cure_light"),
            NameRank::Exact
        );
        assert_eq!(rank_ability_name("fire", "fire"), NameRank::Exact);
        assert_eq!(rank_ability_name("fire", "fire_breath"), NameRank::Prefix);
        assert_eq!(
            rank_ability_name("invis", "mass_invisibility"),
            NameRank::None
        );
        assert_eq!(
            rank_ability_name("sibility", "invisibility"),
            NameRank::None
        );
        assert_eq!(rank_ability_name("l c", "cure_light"), NameRank::None);
        assert_eq!(rank_ability_name("a b c", "a_b"), NameRank::None);
        assert_eq!(rank_ability_name("", "a"), NameRank::None);
    }
}
