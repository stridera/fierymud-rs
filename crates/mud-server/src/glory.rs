//! Glory (`EFF_GLORY`): a glorion's divine presence keeps aggressive mobs
//! from picking fights, with a small chance that a mob sees through it and
//! attacks the glorion instead.
//!
//! Ported from the legacy "Glory rework" (stridera/fierymud#405). The old
//! check re-rolled the 2% attack on every aggro pulse, so every mob in the
//! room eventually "saw through" the glory and the spell made things worse.
//! The reworked rules, kept here:
//!
//! * The attack roll happens once, when a mob is first exposed to the glory:
//!   when Glory lands or a glorion enters a room (every mob there rolls), and
//!   when a mob enters a room that holds a glorion (the mob rolls, against
//!   one glorion picked at random). Both run one second after the move so the
//!   arrival text comes first.
//! * Afterwards the aggro check is simply cancelled: a mob that can see any
//!   glorion in its room starts no fight on its own (see [`blocks_aggro`]).
//!
//! Entering a room is detected in `mud_world::movement::move_to_room` (the
//! one place every room change goes through), which fills the
//! [`GloryWitnessQueue`]; [`glory_tick`] drains it.

use bevy_ecs::prelude::*;
use mud_db::enums::{MobBehavior, UserRole};
use mud_world::movement::GloryWitnessQueue;
use mud_world::{
    Account, Fighting, Frozen, Ghost, Glory, Located, Mob, MobBehaviors, PeacefulRoom, Player,
    Posture, PostureKind, Stunned,
};

use crate::TICK_HZ;
use crate::commands::{
    Pronouns, broadcast_room_anonymised, can_see_player, cap_sentence_start, engage_combat_quiet,
    mob_will_start_fight, name_of, send_msg,
};

/// Ticks between a move and its glory check: legacy `1 RL_SEC`.
#[allow(clippy::cast_possible_truncation)]
const WITNESS_DELAY_TICKS: u32 = TICK_HZ as u32;

/// A see-through roll below this (of 1..=100, so 2%) breaks the glory for a mob: legacy
/// `random_number(1, 100) < 3`.
const SEE_THROUGH_ROLL_BELOW: i32 = 3;

/// One in this many failed attempts shows the awe message.
const AWE_ONE_IN: i32 = 8;

#[cfg(test)]
thread_local! {
    /// Pins the 1..=100 see-through roll (per test thread).
    static FORCED_SEE_THROUGH_ROLL: std::cell::Cell<Option<i32>> =
        const { std::cell::Cell::new(None) };
    /// Pins the 1..=8 awe-message roll (per test thread).
    static FORCED_AWE_ROLL: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

fn see_through_roll() -> i32 {
    #[cfg(test)]
    if let Some(r) = FORCED_SEE_THROUGH_ROLL.with(std::cell::Cell::get) {
        return r;
    }
    rand::random_range(1..=100)
}

fn awe_roll() -> i32 {
    #[cfg(test)]
    if let Some(r) = FORCED_AWE_ROLL.with(std::cell::Cell::get) {
        return r;
    }
    rand::random_range(1..=AWE_ONE_IN)
}

/// Legacy `PLAYERALLY`: a player, or a mob a player commands.
fn is_player_ally(world: &World, e: Entity) -> bool {
    world.get::<Player>(e).is_some()
        || crate::commands::servant_master(world, e)
            .is_some_and(|m| world.get::<Player>(m).is_some())
}

/// Legacy `find_glorions`: the glorions `mob` can see in its room, `mob`
/// itself excluded. Empty for anything that is not a mob.
pub(crate) fn find_glorions(world: &World, mob: Entity) -> Vec<Entity> {
    if world.get::<Mob>(mob).is_none() {
        return Vec::new();
    }
    let Some(room) = world.get::<Located>(mob).map(|l| l.0) else {
        return Vec::new();
    };
    crate::room_index::contents_of(world, room)
        .filter(|&e| {
            e != mob
                && world.get::<Glory>(e).is_some()
                && is_player_ally(world, e)
                && can_see_player(world, mob, e)
        })
        .collect()
}

/// True when `mob` sits in a room with a glorion it can see: its aggro check
/// is cancelled (legacy `find_aggr_target` returning no target).
pub(crate) fn blocks_aggro(world: &World, mob: Entity) -> bool {
    !find_glorions(world, mob).is_empty()
}

/// Legacy `able_to_fight` without the player aggro-level checks: the actor's
/// room allows fighting and the actor is awake and not held.
fn able_to_fight(world: &World, e: Entity) -> bool {
    let Some(room) = world.get::<Located>(e).map(|l| l.0) else {
        return false;
    };
    if world.get::<PeacefulRoom>(room).is_some() {
        return false;
    }
    if world.get::<Mob>(e).is_some() {
        return crate::mob_ai::mob_can_act(world, e);
    }
    world
        .get::<Posture>(e)
        .is_none_or(|p| p.0 != PostureKind::Sleeping)
        && world.get::<Stunned>(e).is_none()
        && world.get::<Frozen>(e).is_none()
        && world.get::<Ghost>(e).is_none()
}

/// Legacy `PRF_NOHASSLE`: staff are never picked on.
fn is_hassle_proof(world: &World, e: Entity) -> bool {
    world
        .get::<Account>(e)
        .is_some_and(|a| a.role.rank() > UserRole::Player.rank())
}

/// Mobs whose job is to keep the peace never see through a glorion.
fn is_exempt_from_distraction(world: &World, mob: Entity) -> bool {
    world.get::<MobBehaviors>(mob).is_some_and(|b| {
        b.has(MobBehavior::Peacekeeper)
            || b.has(MobBehavior::Helper)
            || b.has(MobBehavior::Peaceful)
            || b.has(MobBehavior::Protector)
    })
}

/// Legacy `glorion_distraction`: `mob` meets `glorion`'s glory. A 2% roll
/// breaks the spell for this mob, which attacks; otherwise the glory holds,
/// and now and then the mob is seen gawking.
fn glorion_distraction(world: &mut World, mob: Entity, glorion: Entity) {
    if mob == glorion {
        return;
    }
    let Some(room) = world.get::<Located>(mob).map(|l| l.0) else {
        return;
    };
    if world.get::<Located>(glorion).map(|l| l.0) != Some(room) {
        return;
    }
    let attacks = see_through_roll() < SEE_THROUGH_ROLL_BELOW
        && !is_hassle_proof(world, glorion)
        && !is_exempt_from_distraction(world, mob)
        && world.get::<Fighting>(mob).is_none()
        && mob_will_start_fight(world, mob, glorion);
    let mob_name = name_of(world, mob);
    let glorion_name = name_of(world, glorion);
    let mob_p = Pronouns::of(world, mob);
    if attacks {
        broadcast_room_anonymised(
            world,
            room,
            &[mob, glorion],
            &[(mob, &mob_name), (glorion, &glorion_name)],
            &format!(
                "{} forgets {} appreciation of {glorion_name}'s glorious appearance, and attacks!\r\n",
                cap_sentence_start(&mob_name),
                mob_p.possessive,
            ),
        );
        send_msg(
            world,
            glorion,
            format!(
                "The look of awe in {mob_name}'s eyes falters, and {} attacks!\r\n",
                mob_p.subjective,
            ),
        );
        engage_combat_quiet(world, mob, glorion, room);
        if world.get::<Fighting>(mob).is_some_and(|f| f.0 == glorion) {
            crate::combat::engage_swing_now(world, mob, glorion);
        }
    } else if awe_roll() == 1 {
        broadcast_room_anonymised(
            world,
            room,
            &[mob, glorion],
            &[(mob, &mob_name), (glorion, &glorion_name)],
            &format!(
                "{} looks upon {glorion_name} with awe in {} eyes.\r\n",
                cap_sentence_start(&mob_name),
                mob_p.possessive,
            ),
        );
        send_msg(
            world,
            glorion,
            format!("{mob_name} gazes at you in wonder.\r\n"),
        );
    }
}

/// Legacy `check_glory`: `mob` is exposed to glory. With a `caster` the
/// exposure is to that glorion; without, to one of the glorions the mob can
/// see, picked at random.
fn check_glory(world: &mut World, mob: Entity, caster: Option<Entity>) {
    if world.get::<Mob>(mob).is_none() || !able_to_fight(world, mob) {
        return;
    }
    let glorion = if let Some(c) = caster {
        if !can_see_player(world, mob, c) {
            return;
        }
        c
    } else {
        let glorions = find_glorions(world, mob);
        if glorions.is_empty() {
            return;
        }
        glorions[rand::random_range(0..glorions.len())]
    };
    glorion_distraction(world, mob, glorion);
}

/// Legacy `check_glory_around`: a glorion has just arrived (or just cast
/// Glory), so every mob in the room is exposed to it.
fn check_glory_around(world: &mut World, glorion: Entity) {
    if !able_to_fight(world, glorion) {
        return;
    }
    let Some(room) = world.get::<Located>(glorion).map(|l| l.0) else {
        return;
    };
    let mobs: Vec<Entity> = crate::room_index::contents_of(world, room)
        .filter(|&e| e != glorion && world.get::<Mob>(e).is_some())
        .collect();
    for mob in mobs {
        if world.get_entity(mob).is_err() || world.get_entity(glorion).is_err() {
            continue;
        }
        check_glory(world, mob, Some(glorion));
    }
}

/// Legacy `witness_glory_event`: the delayed check for a mover.
fn witness_glory(world: &mut World, who: Entity) {
    if world.get_entity(who).is_err() {
        return;
    }
    if world.get::<Glory>(who).is_some() {
        check_glory_around(world, who);
    } else {
        check_glory(world, who, None);
    }
}

/// Legacy `queue_glory_event`: have `who`'s glory checked in a second. Used
/// when Glory lands; room changes queue themselves.
pub(crate) fn queue_witness(world: &mut World, who: Entity) {
    if let Some(mut queue) = world.get_resource_mut::<GloryWitnessQueue>() {
        queue.queue(who);
    }
}

/// Legacy `aggro_lose_spells`: attacking, or casting something hostile,
/// shatters the attacker's own glory.
pub(crate) fn drop_on_aggression(world: &mut World, who: Entity) {
    if world.get::<Glory>(who).is_some() {
        crate::commands::remove_effects_for_condition(world, who, "glory");
    }
}

/// Per-tick drain of the [`GloryWitnessQueue`]: movers whose second is up get
/// their glory check.
pub(crate) fn glory_tick(world: &mut World) {
    let due: Vec<Entity> = {
        let Some(mut queue) = world.get_resource_mut::<GloryWitnessQueue>() else {
            return;
        };
        if queue.entries.is_empty() {
            return;
        }
        for entry in &mut queue.entries {
            entry.1 += 1;
        }
        let due = queue
            .entries
            .iter()
            .filter(|(_, waited)| *waited >= WITNESS_DELAY_TICKS)
            .map(|(e, _)| *e)
            .collect();
        queue
            .entries
            .retain(|(_, waited)| *waited < WITNESS_DELAY_TICKS);
        due
    };
    for who in due {
        witness_glory(world, who);
    }
}

#[cfg(test)]
mod tests;
