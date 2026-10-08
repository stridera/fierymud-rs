//! Per-instance item text customization: a player-given name for a bag
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

/// Reduce player-typed text to a plain item name: colour tags and every
/// character outside letters, digits, space and a little punctuation are
/// dropped, whitespace is collapsed. Fails when the result is out of
/// bounds; the message is ready to send.
pub(crate) fn sanitize_player_name(raw: &str) -> Result<String, String> {
    let stripped = render_color_tags(&strip_ansi(raw), ColorMode::Strip);
    let mut out = String::with_capacity(stripped.len());
    let mut last_space = true;
    for c in stripped.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '\'' | '-' | ',' | '.' | '(' | ')' | '!' | '?')
        {
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
    Ok(out)
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

/// Keywords a customized item answers to: the base list (the prototype's,
/// or the staff override) plus every word of its custom name, so `get
/// daedela` finds "Daedela's cloth sack".
pub(crate) fn effective_keywords(base: &[String], custom_name: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = base.to_vec();
    if let Some(name) = custom_name {
        for word in name.split_whitespace() {
            let w: String = word
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '\'' | '-'))
                .collect::<String>()
                .to_ascii_lowercase();
            if !w.is_empty() && !out.contains(&w) {
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
