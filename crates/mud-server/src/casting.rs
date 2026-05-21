//! Cast queue + per-tick wind-up resolution.
//!
//! `invoke_ability_with` is the resolution path. The `start_cast`
//! entry inspects `Ability.cast_time_rounds`; when > 0 it installs a
//! `Casting` component instead of running the cast immediately. Once
//! per `casting_tick`, every active `Casting` component decrements
//! `ticks_remaining`; on reaching 0 the cast is resolved through
//! `invoke_ability_with` with `skip_queue = true`.
//!
//! Interruption:
//! - Posture drop / sleep / stun → cancel (effect-prevents-casting
//!   gate inside `invoke_ability_with` would refuse anyway).
//! - Damage taken during cast → Concentration check (damage as % of
//!   max HP, > 30% breaks).
//! - `cancel`, `flee`, `abort` → explicit interrupt.
//! - Movement → cancel (combat-lock already blocks movement while
//!   Fighting; this catches the non-combat case).
//!
//! `cast_time_rounds = 0` is treated as instant — call site routes
//! straight into `invoke_ability_with`. Item-driven casts (scrolls /
//! wands / potions) also skip the queue: the item itself is the
//! delay-bearer, the resulting cast lands instantly.

use bevy_ecs::prelude::*;
use mud_world::{Casting, Fighting, Health, Located};

use crate::commands::send_to;

/// One combat round in ticks. Combat round = 4s (per
/// `GameConfig.combat.round_seconds`), TICK_HZ = 10, so 40 ticks.
/// Cast wind-up = `cast_time_rounds * COMBAT_ROUND_TICKS`.
///
/// Kept in code as the conversion is a runtime invariant — the
/// GameConfig row tunes the *seconds* per round, not the tick rate.
pub(crate) const COMBAT_ROUND_TICKS: i32 = 40;

/// Decrement `Casting.ticks_remaining` for every active wind-up,
/// resolve casts that hit 0 by re-invoking `invoke_ability_with`
/// with `skip_queue = true`. Runs once per server tick.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn casting_tick(world: &mut World) {
    // Snapshot the ready set under an immutable query, then drain
    // outside the borrow so we can `&mut World` through resolution.
    let mut q = world.query::<(Entity, &mut Casting)>();
    let mut ready: Vec<Entity> = Vec::new();
    let mut still_winding: Vec<Entity> = Vec::new();
    for (e, mut c) in q.iter_mut(world) {
        c.ticks_remaining -= 1;
        if c.ticks_remaining <= 0 {
            ready.push(e);
        } else {
            still_winding.push(e);
        }
    }
    for caster in ready {
        // Grab cast config before despawning the Casting component
        // so resolution can re-route through the normal entry point.
        let snapshot = world.get::<Casting>(caster).cloned();
        if let Ok(mut em) = world.get_entity_mut(caster) {
            em.remove::<Casting>();
        }
        let Some(snap) = snapshot else { continue };
        // Final pre-resolution check: caster still alive + in a
        // valid room. The full effect-prevents / no-magic /
        // peaceful gates re-run inside `invoke_ability_with`, so
        // there's no need to duplicate them here.
        if world.get::<Health>(caster).is_none_or(|h| h.hp <= 0) {
            continue;
        }
        crate::commands::resolve_queued_cast(
            world,
            caster,
            &snap.args,
            mud_db::abilities::AbilityKind::from_label(
                &snap.kind_label.to_ascii_uppercase(),
            ),
            &snap.verb,
        );
    }
    let _ = still_winding;
}

/// Notify the caster every tick that their cast is winding up?
/// Today we surface a single "you begin casting X" message at the
/// start and leave the prompt to render the progress bar. The
/// hook below is reserved for a future per-tick atmospheric
/// emit (e.g. flickering glow as the wind-up nears completion).
pub(crate) fn interrupt_cast(world: &mut World, caster: Entity, reason: &str) -> bool {
    let Some(snap) = world.get::<Casting>(caster).cloned() else {
        return false;
    };
    if let Ok(mut em) = world.get_entity_mut(caster) {
        em.remove::<Casting>();
    }
    send_to(
        world,
        caster,
        format!(
            "Your concentration on {} shatters — {reason}.\r\n",
            snap.ability_name,
        ),
    );
    true
}

/// Concentration break on incoming damage. Damage > 30% of caster
/// max HP forces a save; for now any spike that big simply
/// interrupts. Lower-impact hits leave the cast intact.
pub(crate) fn check_concentration_on_damage(
    world: &mut World,
    caster: Entity,
    damage_taken: i32,
) {
    if world.get::<Casting>(caster).is_none() {
        return;
    }
    let max_hp = world.get::<Health>(caster).map_or(1, |h| h.max).max(1);
    let frac = (damage_taken * 100) / max_hp;
    if frac >= 30 {
        interrupt_cast(world, caster, "the blow rattles you");
    }
}

/// Stamp every actor in `room` who's currently casting with an
/// interruption. Used by movement / posture-drop sites: the cast
/// shatters when the caster suddenly stops focusing. Returns the
/// number interrupted.
#[allow(dead_code)]
pub(crate) fn interrupt_casters_in_room(world: &mut World, room: Entity, reason: &str) -> usize {
    let casters: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located, &Casting)>();
        q.iter(world)
            .filter(|(_, l, _)| l.0 == room)
            .map(|(e, _, _)| e)
            .collect()
    };
    let count = casters.len();
    for c in casters {
        interrupt_cast(world, c, reason);
    }
    count
}

/// Used by `cmd_cancel` / `cmd_abort` / `cmd_flee` — explicit
/// player-initiated interruption with a friendlier message.
pub(crate) fn cancel_own_cast(world: &mut World, caster: Entity) -> bool {
    interrupt_cast(world, caster, "you stop chanting")
}

// `_` reads to keep clippy happy when interrupt is feature-gated
// to a code path we may add later.
#[allow(dead_code)]
fn _unused() {
    let _ = Fighting;
    let _ = COMBAT_ROUND_TICKS;
}
