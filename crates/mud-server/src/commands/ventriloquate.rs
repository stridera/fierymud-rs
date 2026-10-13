//! Ventriloquate (legacy `MAG_MANUAL` `spell_ventriloquate`, `spells.cpp`):
//! `cast 'ventriloquate' <target> <message>` makes the room hear the target
//! say `<message>`. It applies nothing to the caster, who sees nothing at all
//! on success.
//!
//! The spell is the `speak_as` mode of its `alter_object` effect row (the same
//! mode switch Preserve and Curse use for their manual handlers). A cast with
//! that row is also silent: legacy sends no "starts chanting", "completes
//! their spell" or "utters the words" line to anyone (only to staff of at least
//! the caster's level, who hear the start), see `casting.rs`.
//!
//! Every other player in the room rolls against the caster's skill:
//!
//! * `1..=8*(skill+24) < level + wis/2` (always, when the target is a player)
//!   sees through it: "Someone tried to make it sound like Bob said, '...'!";
//! * failing that, `1..=8*(skill+24) < (level + wis)/4` also spots the
//!   caster: "Cara tries to make it sound like Bob says, '...'!";
//! * otherwise they hear exactly what a real `say` from Bob would show them,
//!   coloured line and `say` Comm.Channel GMCP frame included.

use bevy_ecs::prelude::*;
use mud_world::{AbilityDef, CoreStats, EffectCatalog, Player, Posture, PostureKind};

use super::{AbilityCatalog, Pronouns, name_of, seen_name, send_msg};

/// The `mode` of the `alter_object` row that marks the spell as a
/// ventriloquism.
const MODE: &str = "speak_as";

/// True when `def` carries the `speak_as` row.
pub(crate) fn is_speak_as(world: &World, def: &AbilityDef) -> bool {
    let (Some(catalog), Some(effects)) = (
        world.get_resource::<AbilityCatalog>(),
        world.get_resource::<EffectCatalog>(),
    ) else {
        return false;
    };
    catalog.effects_for.get(&def.id).is_some_and(|rows| {
        rows.iter().any(|(id, override_params)| {
            effects.by_id.get(id).is_some_and(|e| {
                e.effect_type == "alter_object"
                    && override_params
                        .as_ref()
                        .and_then(|p| p.get("mode"))
                        .or_else(|| e.default_params.get("mode"))
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|m| m.eq_ignore_ascii_case(MODE))
            })
        })
    })
}

/// Split the text after the spell name into the target's name (the first
/// word) and the message (the rest, `None` when empty).
pub(crate) fn split_tail(tail: Option<&str>) -> (Option<&str>, Option<&str>) {
    let Some(tail) = tail.map(str::trim).filter(|t| !t.is_empty()) else {
        return (None, None);
    };
    match tail.split_once(char::is_whitespace) {
        Some((target, rest)) => (Some(target), Some(rest.trim()).filter(|m| !m.is_empty())),
        None => (Some(tail), None),
    }
}

/// Highest roll that sees through the trick: legacy `level + wis / 2`.
fn see_through_limit(level: i32, wis: i32) -> i32 {
    level + wis / 2
}

/// Highest roll that spots the caster behind it: legacy `(level + wis) / 4`.
fn spot_caster_limit(level: i32, wis: i32) -> i32 {
    (level + wis) / 4
}

/// The roll's ceiling for a caster with `skill` (0..=100): `8 * (skill + 24)`.
fn roll_span(skill: i32) -> i32 {
    8 * (skill.max(0) + 24)
}

/// What a listener hears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heard {
    /// Saw through the trick, caster unknown.
    SeenThrough,
    /// Saw through the trick and spotted the caster.
    CasterSpotted,
    /// Believed it.
    Fooled,
}

/// Roll for one listener. `roll(max)` returns a value in `1..=max`.
fn hear(
    victim_is_player: bool,
    skill: i32,
    level: i32,
    wis: i32,
    roll: &mut dyn FnMut(i32) -> i32,
) -> Heard {
    let span = roll_span(skill);
    if victim_is_player || roll(span) < see_through_limit(level, wis) {
        Heard::SeenThrough
    } else if roll(span) < spot_caster_limit(level, wis) {
        Heard::CasterSpotted
    } else {
        Heard::Fooled
    }
}

/// Run the spell: `caster` makes `victim` say `message`. `skill` is the
/// caster's skill (0..=100). `roll(max)` returns a value in `1..=max`.
pub(crate) fn speak_as(
    world: &mut World,
    caster: Entity,
    victim: Entity,
    skill: i32,
    message: Option<&str>,
    roll: &mut dyn FnMut(i32) -> i32,
) {
    let Some(msg) = message.map(str::trim).filter(|m| !m.is_empty()) else {
        let him = Pronouns::of(world, victim).objective;
        send_msg(
            world,
            caster,
            format!("You seem to have forgotten what you wanted {him} to say!\r\n"),
        );
        return;
    };
    let Some(room) = world.get::<mud_world::Located>(caster).map(|l| l.0) else {
        return;
    };
    let victim_is_player = world.get::<Player>(victim).is_some();
    let victim_name = name_of(world, victim);
    let caster_name = name_of(world, caster);
    for listener in crate::room_index::player_listeners_in(world, room) {
        if listener == caster
            || world
                .get::<Posture>(listener)
                .is_some_and(|p| p.0 == PostureKind::Sleeping)
        {
            continue;
        }
        let level = mud_world::effective_level(world, listener);
        let wis = world.get::<CoreStats>(listener).map_or(0, |s| s.wisdom);
        let is_victim = listener == victim;
        let victim_seen = seen_name(world, listener, victim, &victim_name);
        let caster_seen = seen_name(world, listener, caster, &caster_name);
        // A player victim is always seen through, so the victim's own copy
        // of the other two outcomes (legacy has them) can never be reached.
        let line = match hear(victim_is_player, skill, level, wis, roll) {
            Heard::SeenThrough if is_victim => {
                format!("Someone tried to make it sound like you said '{msg}'!\r\n")
            }
            Heard::SeenThrough => {
                format!("Someone tried to make it sound like {victim_seen} said, '{msg}'!\r\n")
            }
            Heard::CasterSpotted => format!(
                "{caster_seen} tries to make it sound like {victim_seen} says, '{msg}'!\r\n"
            ),
            Heard::Fooled => {
                // Exactly what a real say from the victim looks like, GMCP included.
                super::room_chat::hear_say(world, listener, victim, &victim_name, msg);
                continue;
            }
        };
        send_msg(world, listener, line);
    }
}
