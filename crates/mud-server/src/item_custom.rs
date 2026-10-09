//! Per-instance item text customization: a player-given label on an item
//! (`nameitem`, issue #68) and the staff `iedit` overrides (issue #67).
//!
//! The overrides live in [`ItemCustomization`]; the live `Named`,
//! `Description` and `Keywords` components are always derived from the
//! prototype plus those overrides by [`apply`], so clearing an override
//! restores the prototype's text. Persistence is the `CharacterItems`
//! columns `custom_name` / `custom_examine_description` and the `keywords`
//! key of `custom_values` (see `mud_db::character_items`).

use bevy_ecs::prelude::*;
use mud_world::{Description, ItemCustomization, Keywords, Named, ObjectPrototypes, WorldKey};

use crate::commands::{ColorMode, render_color_tags};

/// Shortest and longest player-given item name, in characters.
pub(crate) const MIN_PLAYER_NAME_LEN: usize = 3;
pub(crate) const MAX_PLAYER_NAME_LEN: usize = 40;
/// Staff `iedit` limits.
pub(crate) const MAX_STAFF_NAME_LEN: usize = 80;
pub(crate) const MAX_EXAMINE_LEN: usize = 1000;
pub(crate) const MAX_KEYWORDS: usize = 12;
pub(crate) const MAX_KEYWORD_LEN: usize = 24;

/// Reduce player-typed text to a plain item name: colour tags and every
/// character outside letters, digits, space and a little punctuation are
/// dropped (parentheses included, so a name cannot fake a status tag such
/// as `(edited)`), whitespace is collapsed. Fails when the result is out of
/// bounds or imitates a corpse label; the message is ready to send.
pub(crate) fn sanitize_player_name(raw: &str) -> Result<String, String> {
    let stripped = render_color_tags(&strip_ansi(raw), ColorMode::Strip);
    let mut out = String::with_capacity(stripped.len());
    let mut last_space = true;
    for c in stripped.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '\'' | '-' | ',' | '.' | '!' | '?') {
            out.push(c);
            last_space = false;
        } else if c.is_whitespace() && !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    let out = out.trim().to_string();
    let len = out.chars().count();
    if len < MIN_PLAYER_NAME_LEN {
        return Err(format!(
            "That name is too short (at least {MIN_PLAYER_NAME_LEN} letters or digits).\r\n"
        ));
    }
    if len > MAX_PLAYER_NAME_LEN {
        return Err(format!(
            "That name is too long ({len} characters; the limit is {MAX_PLAYER_NAME_LEN}).\r\n"
        ));
    }
    if mimics_corpse_label(&out) {
        return Err("That name looks like a corpse label; pick another.\r\n".to_string());
    }
    Ok(out)
}

/// Corpse labels ("the corpse of Bob") gate looting and dragging by name,
/// so a player-given name must not read like one, with or without a
/// leading article.
fn mimics_corpse_label(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let mut words = lower.split_whitespace().peekable();
    if words
        .peek()
        .is_some_and(|w| matches!(*w, "the" | "a" | "an" | "some"))
    {
        words.next();
    }
    words.next() == Some("corpse") && words.next() == Some("of")
}

/// Words the targeting parser or commands give a meaning of their own;
/// a custom name must not make an item answer to them.
fn is_reserved_target_word(w: &str) -> bool {
    if matches!(w, "all" | "self" | "me" | "everyone" | "someone" | "here") {
        return true;
    }
    let digits = w.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && (digits.len() == w.len() || matches!(&w[digits.len()..], "st" | "nd" | "rd" | "th"))
}

/// Drop ANSI CSI escape sequences (`ESC [ ... letter`) so a pasted colour
/// code leaves no `31m` residue behind. Lone control characters are
/// handled by the callers' character filters.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Bidirectional-override and zero-width characters: invisible, yet able to
/// reorder or disguise the text around them.
fn is_invisible_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200D}' | '\u{FEFF}'
    )
}

/// Staff text: colour tags are allowed; control characters (including ESC
/// and line breaks, since `iedit` takes typed single lines), bidi controls
/// and zero-width characters are removed.
pub(crate) fn sanitize_staff_text(raw: &str, max: usize) -> Result<String, String> {
    let cleaned: String = strip_ansi(raw)
        .chars()
        .filter(|c| !c.is_control() && !is_invisible_format_char(*c))
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return Err("That text is empty.\r\n".to_string());
    }
    let len = cleaned.chars().count();
    if len > max {
        return Err(format!(
            "Too long ({len} characters; the limit is {max}).\r\n"
        ));
    }
    Ok(cleaned)
}

/// Parse a keyword list: lowercase, deduplicated, bounded.
pub(crate) fn sanitize_keywords(raw: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for word in raw.split_whitespace() {
        let w: String = word
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '\'' | '-'))
            .collect::<String>()
            .to_ascii_lowercase();
        if w.is_empty() {
            continue;
        }
        if w.len() > MAX_KEYWORD_LEN {
            return Err(format!(
                "Keyword '{w}' is too long (limit {MAX_KEYWORD_LEN}).\r\n"
            ));
        }
        if !out.contains(&w) {
            out.push(w);
        }
    }
    if out.is_empty() {
        return Err("Give at least one keyword.\r\n".to_string());
    }
    if out.len() > MAX_KEYWORDS {
        return Err(format!("Too many keywords (limit {MAX_KEYWORDS}).\r\n"));
    }
    Ok(out)
}

const LABEL_OPEN: &str = " (labeled '";
const LABEL_CLOSE: &str = "')";

/// A player label is shown after the item's own short description, e.g.
/// `a cloth sack (labeled 'gems')`, so the item stays recognisable.
pub(crate) fn with_label(base: &str, label: &str) -> String {
    format!("{base}{LABEL_OPEN}{label}{LABEL_CLOSE}")
}

/// Split a name made by [`with_label`] into `(base, label)`. Player labels
/// cannot contain parentheses, so the last opener is the real one.
pub(crate) fn split_label(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_suffix(LABEL_CLOSE)?;
    let at = rest.rfind(LABEL_OPEN)?;
    Some((&rest[..at], &rest[at + LABEL_OPEN.len()..]))
}

/// Keywords a customized item answers to: the base list (the prototype's,
/// or the staff override) plus every word of its custom name, so `get
/// daedela` finds "Daedela's cloth sack". For a labeled name only the
/// label's words are added. Single-character words and reserved targeting
/// words (`all`, `self`, numbers, `2nd`) are skipped.
pub(crate) fn effective_keywords(base: &[String], custom_name: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = base.to_vec();
    if let Some(name) = custom_name {
        let name = split_label(name).map_or(name, |(_, label)| label);
        for word in name.split_whitespace() {
            let w: String = word
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '\'' | '-'))
                .collect::<String>()
                .to_ascii_lowercase();
            if w.len() >= 2 && !is_reserved_target_word(&w) && !out.contains(&w) {
                out.push(w);
            }
        }
    }
    out
}

/// Recompute the live `Named` / `Description` / `Keywords` of `item` from
/// its prototype and its [`ItemCustomization`]. An item whose prototype is
/// not loaded keeps its current text for anything not overridden.
pub(crate) fn apply(world: &mut World, item: Entity) {
    let Some(custom) = world.get::<ItemCustomization>(item).cloned() else {
        return;
    };
    let proto = world.get::<WorldKey>(item).copied().and_then(|k| {
        world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(k.zone, k.id))
            .cloned()
    });
    let Ok(mut em) = world.get_entity_mut(item) else {
        return;
    };
    let name = custom
        .name
        .clone()
        .or_else(|| proto.as_ref().map(|p| p.name.clone()));
    if let Some(name) = name {
        em.insert(Named { name });
    }
    let examine = custom
        .examine
        .clone()
        .or_else(|| proto.as_ref().and_then(|p| p.examine_description.clone()));
    match examine {
        Some(text) => {
            em.insert(Description(text));
        }
        None if proto.is_some() => {
            em.remove::<Description>();
        }
        None => {}
    }
    let base = custom
        .keywords
        .clone()
        .or_else(|| proto.as_ref().map(|p| p.keywords.clone()));
    if let Some(base) = base {
        em.insert(Keywords(effective_keywords(&base, custom.name.as_deref())));
    }
}

/// Attach `custom` to `item` and refresh its derived text.
pub(crate) fn install(world: &mut World, item: Entity, custom: ItemCustomization) {
    crate::commands::try_insert(world, item, custom);
    apply(world, item);
}

/// Edit the customization of `item` in place (creating it when absent),
/// mark it dirty so the next save writes it, and refresh the live text.
pub(crate) fn edit(world: &mut World, item: Entity, change: impl FnOnce(&mut ItemCustomization)) {
    let mut custom = world
        .get::<ItemCustomization>(item)
        .cloned()
        .unwrap_or_default();
    change(&mut custom);
    custom.dirty = true;
    install(world, item, custom);
}

/// The player whose save persists `item`: the nearest `Player` up the
/// `Located` chain (a bag inside a bag still belongs to its carrier).
pub(crate) fn holder_of(world: &World, item: Entity) -> Option<Entity> {
    let mut cur = item;
    for _ in 0..16 {
        let parent = world.get::<mud_world::Located>(cur)?.0;
        if world.get::<mud_world::Player>(parent).is_some() {
            return Some(parent);
        }
        cur = parent;
    }
    None
}
