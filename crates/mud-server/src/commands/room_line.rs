//! What a room listing says an actor is doing (issues #105, #107, #108,
//! #110), after legacy `print_char_to_char`.
//!
//! A mob that is in its default posture, idle and free shows its long
//! description. Any other mob, and every player, shows a status line built
//! from its name: held in place ("is standing here, completely
//! motionless."), fighting ("is here, fighting you!"), or its posture ("is
//! sleeping here."). The text `look` and the GMCP `Room.Mobs` /
//! `Room.Players` frames share these rules, so a client panel and the room
//! text never disagree.

use bevy_ecs::prelude::*;
use mud_world::{
    AppliedTo, EffectInstance, EffectSource, Fighting, Flying, Located, Mob, MobPrototypes, Named,
    Player, Posture, PostureKind, Stunned, Title, WorldKey,
};

use crate::commands::{cap_sentence_start, seen_name};

/// Why an actor is held where it stands. Paralysis, mesmerize and stun all
/// install the [`Stunned`] marker; the backing effect says which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hold {
    Paralyzed,
    Mesmerized,
    Stunned,
}

impl Hold {
    /// The `status` string of the GMCP frames.
    pub(crate) fn status(self) -> &'static str {
        match self {
            Self::Paralyzed => "paralyzed",
            Self::Mesmerized => "mesmerized",
            Self::Stunned => "stunned",
        }
    }
}

/// Is `actor` held (the [`Stunned`] marker), and by what.
pub(crate) fn hold_of(world: &mut World, actor: Entity) -> Option<Hold> {
    world.get::<Stunned>(actor)?;
    let mut hold = Hold::Stunned;
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    for (inst, applied) in q.iter(world) {
        if applied.0 != actor {
            continue;
        }
        if inst.name.eq_ignore_ascii_case("paralyzed") {
            return Some(Hold::Paralyzed);
        }
        if inst.name.eq_ignore_ascii_case("mesmerized") {
            hold = Hold::Mesmerized;
        }
    }
    Some(hold)
}

/// The actor's posture; no [`Posture`] component means standing.
fn posture_of(world: &World, actor: Entity) -> PostureKind {
    world
        .get::<Posture>(actor)
        .map_or(PostureKind::Standing, |p| p.0)
}

/// The posture a mob rests in when nothing disturbs it: its prototype's
/// default position (standing when it has none).
fn default_posture(world: &World, mob: Entity) -> PostureKind {
    world
        .get::<WorldKey>(mob)
        .and_then(|k| {
            world
                .get_resource::<MobPrototypes>()
                .and_then(|p| p.by_key.get(&(k.zone, k.id)))
        })
        .map_or(PostureKind::Standing, |p| {
            Posture::from_default_position(p.default_position)
        })
}

/// True when `actor` flies and that is not just how it lives: a mob whose
/// flight comes from its prototype's default effects or its race flies by
/// default, so a "flying" long description is already right for it.
fn flying_off_default(world: &mut World, actor: Entity) -> bool {
    if world.get::<Flying>(actor).is_none() {
        return false;
    }
    if world.get::<Mob>(actor).is_none() {
        return true;
    }
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    !q.iter(world).any(|(inst, applied)| {
        applied.0 == actor
            && inst.name.eq_ignore_ascii_case("fly")
            && (mud_world::mob_effects::is_innate_effect(&inst.source)
                || matches!(&inst.source, EffectSource::Other(s) if s == "mob_default"))
    })
}

/// The posture word GMCP clients get: the posture's label, or `"flying"`
/// for an upright flier.
pub(crate) fn posture_word(world: &World, actor: Entity) -> &'static str {
    let posture = posture_of(world, actor);
    if posture == PostureKind::Standing && world.get::<Flying>(actor).is_some() {
        "flying"
    } else {
        posture.label()
    }
}

/// The text after the actor's name, with its leading space: held, fighting
/// or its posture.
fn phrase(world: &mut World, viewer: Entity, actor: Entity) -> String {
    match hold_of(world, actor) {
        Some(Hold::Paralyzed) => return " is standing here, completely motionless.".to_string(),
        Some(Hold::Mesmerized) => {
            return format!(
                " is here, gazing carefully at a point in front of {} nose.",
                crate::flight::possessive(world, actor)
            );
        }
        Some(Hold::Stunned) => return " is here, stunned.".to_string(),
        None => {}
    }
    if let Some(Fighting(target)) = world.get::<Fighting>(actor).copied() {
        let who = if target == viewer {
            "YOU".to_string()
        } else if world.get::<Located>(target).map(|l| l.0)
            == world.get::<Located>(actor).map(|l| l.0)
        {
            let name = world
                .get::<Named>(target)
                .map_or_else(|| "someone".to_string(), |n| n.name.clone());
            seen_name(world, viewer, target, &name)
        } else {
            "someone who has already left".to_string()
        };
        return format!(" is here, fighting {who}!");
    }
    let doing =
        if posture_of(world, actor) == PostureKind::Standing && flying_off_default(world, actor) {
            "flying"
        } else {
            posture_word(world, actor)
        };
    format!(" is {doing} here.")
}

/// True when a mob should show its long description: free, idle, and in
/// the posture (and flight) its prototype gives it.
fn shows_long_description(world: &mut World, mob: Entity) -> bool {
    if world.get::<Stunned>(mob).is_some() || world.get::<Fighting>(mob).is_some() {
        return false;
    }
    if posture_of(world, mob) != default_posture(world, mob) {
        return false;
    }
    posture_of(world, mob) != PostureKind::Standing || !flying_off_default(world, mob)
}

/// The room-listing line for `mob` as `viewer` sees it, before colouring:
/// the mob's long description when it is in its default state, otherwise
/// its capitalised short name and what it is doing. A mob in its default
/// state with an empty `long_description` is listed by its bare name.
pub(crate) fn mob_line(
    world: &mut World,
    viewer: Entity,
    mob: Entity,
    long_description: &str,
) -> String {
    let name = world
        .get::<Named>(mob)
        .map(|n| n.name.clone())
        .unwrap_or_default();
    if shows_long_description(world, mob) {
        // A mob with no long description is listed by its bare name.
        let long = long_description.trim_end();
        return if long.trim().is_empty() {
            name
        } else {
            long.to_string()
        };
    }
    format!(
        "{}{}",
        cap_sentence_start(&name),
        phrase(world, viewer, mob)
    )
}

/// The room-listing line for another `player`: name and title, then what
/// they are doing.
pub(crate) fn player_line(world: &mut World, viewer: Entity, player: Entity) -> String {
    debug_assert!(world.get::<Player>(player).is_some());
    let name = world
        .get::<Named>(player)
        .map(|n| n.name.clone())
        .unwrap_or_default();
    let title = world
        .get::<Title>(player)
        .map(|t| t.0.trim().to_string())
        .filter(|t| !t.is_empty());
    let who = match title {
        Some(t) => format!("{name} {t}"),
        None => name,
    };
    format!("{who}{}", phrase(world, viewer, player))
}
