//! Fear: making frightened actors actually run.
//!
//! Legacy sources ported here (`fierymud_legacy/src`):
//!
//! * `spells.cpp:3089` `inflict_fear` (FEAR, and HYSTERIA room-wide). Its
//!   flee branch stops the victim's fight, clears its wait state and runs
//!   `flee` for mobs and players alike, then makes a mob remember the
//!   caster. The other branches (frozen in terror, drop weapon, falter) are
//!   represented by the data-driven `feared` status effect and its WILL
//!   save, so a victim that fails the save always panics.
//! * `spells.cpp:1215` `chant_ivory_symphony` (the area flee chant): awake,
//!   non-group targets flee unless they save (skipped in dark rooms),
//!   sentinel mobs save a second time, then a `skill`% roll gates the flee.
//! * `act.offensive.cpp:262` `do_roar` / `howl`: per target, a PARA save,
//!   a second one for sentinels, `Aware` / `NoSummon` mobs and alignment
//!   protection are immune; then a sleeper may wake (50%), an awake target
//!   trips (`dex - 15 < d100`) or flees. Roar leaves no lasting effect.
//!
//! Legacy fear has no `MOB_NOFEAR` flag. Immunity here is data: a mob
//! proto whose `resistances` lists `"fear": 0` (the same shape the importer
//! writes for `NOCHARM`) cannot be frightened.
//!
//! While a `feared` status effect lasts (the [`Feared`] marker), a mob
//! neither starts nor resumes a fight and tries to run each combat round;
//! one with no open exit is cornered and fights on. Players stay in
//! control: a feared player panics once when the effect lands (legacy).

use std::collections::HashSet;

use bevy_ecs::prelude::*;
use mud_db::enums::MobBehavior;
use mud_world::{
    AppliedTo, CombatStats, CoreStats, EffectInstance, Feared, Fighting, Frozen, Ghost, Located,
    Mob, MobBehaviors, MobPrototypes, Player, Posture, PostureKind, SavingThrows, Stunned,
    WorldKey,
};

use crate::combat::{mob_flee, remember_attacker};
use crate::commands::{
    Prevent, broadcast_room_except_rendered, cmd_flee, effect_prevents, has_effect_named, name_of,
    remove_effect_named, room_is_dark, send_to, try_insert, try_remove,
};

/// Is `flag` (a status effect's `flag` param) the fear flag?
pub(crate) fn is_fear_flag(flag: &str) -> bool {
    flag.eq_ignore_ascii_case("feared") || flag.eq_ignore_ascii_case("fear")
}

/// Is the actor under a fear effect right now?
pub(crate) fn is_feared(world: &World, actor: Entity) -> bool {
    world.get::<Feared>(actor).is_some()
}

/// A mob proto listing `fear: 0` in its resistances is immune to fear.
/// Players are never immune.
pub(crate) fn is_fear_immune(world: &World, actor: Entity) -> bool {
    if world.get::<Mob>(actor).is_none() {
        return false;
    }
    let Some(key) = world.get::<WorldKey>(actor) else {
        return false;
    };
    let Some(protos) = world.get_resource::<MobPrototypes>() else {
        return false;
    };
    protos
        .by_key
        .get(&(key.zone, key.id))
        .and_then(|p| p.resistances.as_object())
        .is_some_and(|m| {
            m.iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("fear") && v.as_i64() == Some(0))
        })
}

/// What a panic attempt came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Panic {
    /// Left the room through an exit.
    Fled,
    /// Wanted to run but every way out is shut.
    Cornered,
    /// Couldn't run at all (asleep, paralysed, webbed, stunned, berserk) or
    /// spent the turn getting to its feet.
    Unable,
}

/// Make `victim` panic: stop its fight and cast, then run through the same
/// flee primitive the wimpy path (`mob_flee`) and the `flee` command
/// (`cmd_flee`) use, so movement rules and messages stay consistent.
/// Mirrors the guards at the top of legacy `do_flee`.
pub(crate) fn panic_flee(world: &mut World, victim: Entity, source: Option<Entity>) -> Panic {
    let Some(room) = world.get::<Located>(victim).map(|l| l.0) else {
        return Panic::Unable;
    };
    if world.get::<Ghost>(victim).is_some()
        || world.get::<Frozen>(victim).is_some()
        || world.get::<Stunned>(victim).is_some()
    {
        return Panic::Unable;
    }
    let is_player = world.get::<Player>(victim).is_some();
    match world.get::<Posture>(victim).map(|p| p.0) {
        None | Some(PostureKind::Standing) => {}
        Some(PostureKind::Sleeping) => {
            send_to(world, victim, "You dream of fleeing!\r\n");
            return Panic::Unable;
        }
        Some(_) => {
            // Legacy `do_flee`: a panicked sitter scrambles to its feet
            // and that is the whole turn.
            crate::casting::cancel_own_cast(world, victim);
            try_insert(world, victim, Posture(PostureKind::Standing));
            let name = crate::commands::cap_sentence_start(&name_of(world, victim));
            send_to(world, victim, "You scramble madly to your feet!\r\n");
            broadcast_room_except_rendered(
                world,
                room,
                &[victim],
                &format!("Looking panicked, {name} scrambles madly to their feet!\r\n"),
            );
            return Panic::Unable;
        }
    }
    if effect_prevents(world, victim, Prevent::Movement) {
        send_to(world, victim, "You can't move!\r\n");
        return Panic::Unable;
    }
    if world.get::<Fighting>(victim).is_some() && has_effect_named(world, victim, "berserk") {
        send_to(world, victim, "You're too angry to leave this fight!\r\n");
        return Panic::Unable;
    }

    crate::casting::cancel_own_cast(world, victim);
    try_remove::<Fighting>(world, victim);
    if let Some(source) = source
        && !is_player
    {
        remember_attacker(world, victim, source);
    }
    let moved = if is_player {
        cmd_flee(world, victim, "");
        world.get::<Located>(victim).map(|l| l.0) != Some(room)
    } else {
        mob_flee(world, victim, room)
    };
    if moved { Panic::Fled } else { Panic::Cornered }
}

/// Legacy `get_base_saves` default for `SAVING_PARA` (no class table here),
/// improved by one point per two levels, plus the actor's own modifier.
fn para_save_number(world: &World, actor: Entity) -> i32 {
    let level = mud_world::effective_level(world, actor);
    let modifier = world.get::<SavingThrows>(actor).map_or(0, |s| s.para);
    105 - level / 2 + modifier
}

/// Legacy `mag_savingthrow(.., SAVING_PARA)`: `max(1, save) < d(0..99)`.
/// `roll` is that d100 so tests can pin it.
pub(crate) fn para_save(world: &World, actor: Entity, roll: i32) -> bool {
    para_save_number(world, actor).max(1) < roll
}

fn has_behavior(world: &World, mob: Entity, flag: MobBehavior) -> bool {
    world.get::<MobBehaviors>(mob).is_some_and(|b| b.has(flag))
}

/// The dice of the area-chant gates (`chant_ivory_symphony`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChantRolls {
    /// d(0..99) for the PARA save.
    pub save: i32,
    /// d(0..99) for the sentinel's second save.
    pub sentinel_save: i32,
    /// d(0..100) against the caster's skill.
    pub chance: i32,
}

impl ChantRolls {
    pub(crate) fn random() -> Self {
        Self {
            save: rand::random_range(0..=99),
            sentinel_save: rand::random_range(0..=99),
            chance: rand::random_range(0..=100),
        }
    }
}

/// The extra gates an area fear chant puts in front of the flee. False
/// means the victim shrugs it off.
pub(crate) fn chant_gates_pass(
    world: &World,
    victim: Entity,
    skill: i32,
    rolls: ChantRolls,
) -> bool {
    let dark = world
        .get::<Located>(victim)
        .is_some_and(|l| room_is_dark(world, l.0));
    if !dark && para_save(world, victim, rolls.save) {
        return false;
    }
    if has_behavior(world, victim, MobBehavior::Sentinel)
        && para_save(world, victim, rolls.sentinel_save)
    {
        return false;
    }
    rolls.chance <= skill
}

/// A `feared` status effect just landed on `victim`: mark it, then make it
/// panic. `area_skill` is `Some(skill)` for an area ability, which keeps
/// the chant's extra gates; a victim that passes them keeps neither the
/// flee nor the effect.
pub(crate) fn on_fear_applied(
    world: &mut World,
    caster: Entity,
    victim: Entity,
    area_skill: Option<i32>,
) {
    try_insert(world, victim, Feared);
    if let Some(skill) = area_skill
        && !chant_gates_pass(world, victim, skill, ChantRolls::random())
    {
        remove_effect_named(world, victim, "feared");
        try_remove::<Feared>(world, victim);
        return;
    }
    panic_flee(world, victim, Some(caster));
}

/// Drop the [`Feared`] marker from anyone with no `feared` effect left
/// (expired, dispelled, cleansed). Cheap: only marked actors are visited.
pub(crate) fn sync_markers(world: &mut World) {
    let marked: Vec<Entity> = world
        .query_filtered::<Entity, With<Feared>>()
        .iter(world)
        .collect();
    if marked.is_empty() {
        return;
    }
    let backed: HashSet<Entity> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(eff, _)| is_fear_flag(&eff.name))
            .map(|(_, applied)| applied.0)
            .collect()
    };
    for e in marked {
        if !backed.contains(&e) {
            try_remove::<Feared>(world, e);
        }
    }
}

/// Combat-tick pass: every feared mob in a fight tries to run instead of
/// swinging. A cornered one keeps its fight and swings as usual.
pub(crate) fn feared_mobs_flee(world: &mut World) {
    let scared: Vec<(Entity, Entity)> = {
        let mut q = world.query_filtered::<(Entity, &Located), (
            With<Mob>,
            With<Feared>,
            With<Fighting>,
            Without<Stunned>,
            Without<Frozen>,
            Without<Ghost>,
        )>();
        q.iter(world).map(|(e, l)| (e, l.0)).collect()
    };
    for (mob, room) in scared {
        let standing = world
            .get::<Posture>(mob)
            .is_none_or(|p| p.0 == PostureKind::Standing);
        if standing && !effect_prevents(world, mob, Prevent::Movement) {
            mob_flee(world, mob, room);
        }
    }
}

/// The dice of one roar victim.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RoarRolls {
    /// d(0..99) PARA save.
    pub save: i32,
    /// d(0..99) sentinel second save.
    pub sentinel_save: i32,
    /// A sleeper wakes up (`random_number(0, 1)`).
    pub wake: bool,
    /// d(0..100) against `dex - 15` for the panicked trip.
    pub trip: i32,
}

impl RoarRolls {
    pub(crate) fn random() -> Self {
        Self {
            save: rand::random_range(0..=99),
            sentinel_save: rand::random_range(0..=99),
            wake: rand::random_bool(0.5),
            trip: rand::random_range(0..=100),
        }
    }
}

/// What a roar did to one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoarOutcome {
    /// Saved, immune, or already out cold: nothing happened.
    Resisted,
    /// Woke a sleeper.
    Woke,
    /// Slept on.
    SleptOn,
    /// Tripped over its own feet.
    Tripped,
    /// Panicked; the result of the flee attempt.
    Panicked(Panic),
}

/// Legacy `do_roar` / `howl` for one victim the caller already vetted with
/// `attack_ok` and group exclusion.
pub(crate) fn roar_target(
    world: &mut World,
    caster: Entity,
    victim: Entity,
    rolls: RoarRolls,
) -> RoarOutcome {
    if world.get::<Ghost>(victim).is_some() || is_fear_immune(world, victim) {
        return RoarOutcome::Resisted;
    }
    if para_save(world, victim, rolls.save) {
        return RoarOutcome::Resisted;
    }
    // Twice as hard to roar at a sentinel mob.
    if has_behavior(world, victim, MobBehavior::Sentinel)
        && para_save(world, victim, rolls.sentinel_save)
    {
        return RoarOutcome::Resisted;
    }
    if has_behavior(world, victim, MobBehavior::Aware)
        || has_behavior(world, victim, MobBehavior::NoSummon)
    {
        return RoarOutcome::Resisted;
    }
    let alignment = world.get::<CombatStats>(caster).map_or(0, |c| c.alignment);
    if world.get::<mud_world::ProtectFromEvil>(victim).is_some() && alignment <= -500 {
        let caster_name = name_of(world, caster);
        send_to(
            world,
            victim,
            format!(
                "Your holy protection strengthens your resolve against \
                 {caster_name}'s roar!\r\n"
            ),
        );
        return RoarOutcome::Resisted;
    }
    if world.get::<mud_world::ProtectFromGood>(victim).is_some() && alignment <= 500 {
        let caster_name = name_of(world, caster);
        send_to(
            world,
            victim,
            format!(
                "Your unholy protection strengthens your resolve against \
                 {caster_name}'s roar!\r\n"
            ),
        );
        return RoarOutcome::Resisted;
    }

    let room = world.get::<Located>(victim).map(|l| l.0);
    let posture = world.get::<Posture>(victim).map(|p| p.0);
    if posture == Some(PostureKind::Sleeping) {
        if !rolls.wake {
            return RoarOutcome::SleptOn;
        }
        try_insert(world, victim, Posture(PostureKind::Sitting));
        send_to(
            world,
            victim,
            "A loud ROAAARRRRRR jolts you from your slumber!\r\n",
        );
        if let Some(room) = room {
            let name = crate::commands::cap_sentence_start(&name_of(world, victim));
            broadcast_room_except_rendered(
                world,
                room,
                &[victim],
                &format!("{name} jumps up dazedly, awakened by the noise!\r\n"),
            );
        }
        return RoarOutcome::Woke;
    }
    let dex = world.get::<CoreStats>(victim).map_or(50, |s| s.dexterity);
    let standing = matches!(posture, None | Some(PostureKind::Standing));
    if standing && dex - 15 < rolls.trip {
        try_insert(world, victim, Posture(PostureKind::Sitting));
        send_to(
            world,
            victim,
            "In your panicked rush to flee, you trip!\r\n",
        );
        if let Some(room) = room {
            let name = crate::commands::cap_sentence_start(&name_of(world, victim));
            broadcast_room_except_rendered(
                world,
                room,
                &[victim],
                &format!("In a panicked rush to flee, {name} trips!\r\n"),
            );
        }
        return RoarOutcome::Tripped;
    }
    RoarOutcome::Panicked(panic_flee(world, victim, Some(caster)))
}

#[cfg(test)]
mod tests;
