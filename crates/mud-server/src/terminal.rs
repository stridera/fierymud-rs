//! Per-player terminal capability settings: `color` and `charset`.
//!
//! The telnet layer learns what each client can display (colour depth
//! from TTYPE/MTTS, UTF-8 from MTTS/CHARSET/NEW-ENVIRON) and applies it
//! in the connection's writer, which is the single place every outbound
//! frame is encoded (see `mud_net::output`). This module layers the
//! player's own choices on top as *overrides* on the connection's
//! [`mud_net::OutputHandle`]:
//!
//! * colour: `COLOR_BLIND` flag means off, else `pref.color`
//!   (`16`/`256`/`truecolor`), else follow the client;
//! * charset: `pref.charset` (`ascii`/`utf8`), else follow the client.
//!
//! The preferences live in `ScriptVars`, so they save with the
//! character exactly like `columns`.

use bevy_ecs::prelude::{Component, Entity, World};
use mud_db::enums::PlayerFlag;
use mud_net::{Charset, ColorDepth, OutputHandle};
use mud_world::{PREF_CHARSET_KEY, PREF_COLOR_KEY, PlayerFlags, ScriptVars};

/// The connection's output capabilities, attached to a player entity
/// next to its `Connection`. Absent for mobs and in unit tests, where
/// output is treated as full-colour UTF-8 (no downgrade).
#[derive(Component, Clone)]
pub struct ClientOutput(pub OutputHandle);

fn pref<'a>(world: &'a World, player: Entity, key: &str) -> Option<&'a str> {
    world
        .get::<ScriptVars>(player)?
        .0
        .get(key)
        .map(String::as_str)
}

/// The player's explicit colour-depth preference (not counting `off`).
pub(crate) fn pref_color(world: &World, player: Entity) -> Option<ColorDepth> {
    match pref(world, player, PREF_COLOR_KEY)? {
        "16" => Some(ColorDepth::Ansi16),
        "256" => Some(ColorDepth::Ansi256),
        "truecolor" => Some(ColorDepth::TrueColor),
        _ => None,
    }
}

/// The player's explicit charset preference.
pub(crate) fn pref_charset(world: &World, player: Entity) -> Option<Charset> {
    match pref(world, player, PREF_CHARSET_KEY)? {
        "ascii" => Some(Charset::Ascii),
        "utf8" => Some(Charset::Utf8),
        _ => None,
    }
}

/// Whether the player turned colour off (`color off` / `toggle color`).
pub(crate) fn color_is_off(world: &World, player: Entity) -> bool {
    world
        .get::<PlayerFlags>(player)
        .is_some_and(|f| f.has(PlayerFlag::ColorBlind))
}

/// The colour override to impose on the connection: off wins, then the
/// explicit depth, else `None` (follow the client).
pub(crate) fn color_override(world: &World, player: Entity) -> Option<ColorDepth> {
    if color_is_off(world, player) {
        Some(ColorDepth::None)
    } else {
        pref_color(world, player)
    }
}

/// Push the player's current settings onto their connection's output
/// handle. Call after changing a setting and whenever a connection is
/// attached to a (re)spawned player. No-op without a `ClientOutput`.
pub(crate) fn sync_output(world: &World, player: Entity) {
    let Some(ClientOutput(handle)) = world.get::<ClientOutput>(player) else {
        return;
    };
    handle.set_color_override(color_override(world, player));
    handle.set_charset_override(pref_charset(world, player));
}

/// Effective colour depth for a player's output; full colour when no
/// connection is attached.
pub(crate) fn effective_color(world: &World, player: Entity) -> ColorDepth {
    match world.get::<ClientOutput>(player) {
        Some(ClientOutput(h)) => h.color(),
        None if color_is_off(world, player) => ColorDepth::None,
        None => ColorDepth::TrueColor,
    }
}

/// Effective charset for a player's output; UTF-8 when no connection
/// is attached (nothing to downgrade for).
pub(crate) fn effective_charset(world: &World, player: Entity) -> Charset {
    match world.get::<ClientOutput>(player) {
        Some(ClientOutput(h)) => h.charset(),
        None => Charset::Utf8,
    }
}

/// Human label for a colour depth.
pub(crate) fn depth_label(d: ColorDepth) -> &'static str {
    match d {
        ColorDepth::None => "off",
        ColorDepth::Ansi16 => "16 colours",
        ColorDepth::Ansi256 => "256 colours",
        ColorDepth::TrueColor => "truecolor",
    }
}

/// Human label for a charset.
pub(crate) fn charset_label(c: Charset) -> &'static str {
    match c {
        Charset::Ascii => "ASCII",
        Charset::Utf8 => "UTF-8",
    }
}

/// Set (or clear, with `None`) a string preference in `ScriptVars`.
pub(crate) fn set_pref(world: &mut World, player: Entity, key: &str, value: Option<&str>) {
    match value {
        Some(v) => {
            if world.get::<ScriptVars>(player).is_none() {
                crate::commands::try_insert(world, player, ScriptVars::default());
            }
            if let Some(mut vars) = world.get_mut::<ScriptVars>(player) {
                vars.0.insert(key.to_string(), v.to_string());
            }
        }
        None => {
            if let Some(mut vars) = world.get_mut::<ScriptVars>(player) {
                vars.0.remove(key);
            }
        }
    }
}

/// Turn the `COLOR_BLIND` flag on or off.
pub(crate) fn set_color_off(world: &mut World, player: Entity, off: bool) {
    if let Some(mut flags) = world.get_mut::<PlayerFlags>(player)
        && flags.has(PlayerFlag::ColorBlind) != off
    {
        flags.toggle(PlayerFlag::ColorBlind);
    }
}
