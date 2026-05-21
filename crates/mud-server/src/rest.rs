//! Rest / repose: runtime hot paths (R4 + R5 + R6).
//!
//! Three responsibilities split across this module:
//!
//! 1. **Wake consumer** (`consume_rest_source_on_xp`): runs at the
//!    first XP gain after a `restSource` was acquired. Spawns the
//!    Refreshed Effect, applies Wake Effect attachments per source
//!    kind, then clears the source (per ADR 0001 §1, consumption is
//!    on-XP-gain, not on-login).
//!
//! 2. **Repose math** (`apply_repose_on_xp`): multiplies the base XP
//!    against the player's Repose pool, drawing from the pool up to
//!    the bonus amount. Returns the final XP to award.
//!
//! 3. **Refreshed regen** (`apply_refreshed_regen`): R6. Computes the
//!    flat `RegenBonus` delta proportional to attachment strength and
//!    stamps it on the wearer; pairs with a `RefreshedBonus`
//!    companion component so the on-remove unwind subtracts the
//!    same amount when the Effect fades.
//!
//! The XP-award helper at the bottom (`award_experience`) is the
//! single chokepoint every gain site calls — combat kill rewards
//! and the `PendingPlayerUpdate::ExperienceDelta` drain both route
//! through it. Admin paths (`advance`, `set xp`) intentionally
//! bypass — they're explicit floor / set commands, not gameplay
//! gains, and should not consume the rest source.

use bevy_ecs::prelude::*;
use mud_db::enums::RestSource;
use mud_world::{
    AppliedTo, EffectCatalog, EffectInstance, EffectSource, PendingWakeAttachments, Profile,
    RefreshedBonus, RegenBonus, RestState, WakeRow, WorldKey,
};
use tracing::warn;

/// XP-gain Repose multiplier. Each gain consumes `base * (M - 1)` XP
/// from the Repose pool when available; the player banks the bonus
/// on top of the base. **TUNABLE** — design doc default 2.0.
const REPOSE_MULTIPLIER: f64 = 2.0;

/// Refreshed Effect on-attach duration, in seconds. **TUNABLE** —
/// design doc default 1800 (30 real minutes).
const REFRESHED_DURATION_SECS: i32 = 1800;

/// Catalog name of the universal Refreshed Effect row. Looked up
/// at wake-consume time so we resolve the live `Effect.id` rather
/// than baking a magic number; fierylib seeds the row, runtime
/// just spawns by name.
const REFRESHED_EFFECT_NAME: &str = "Refreshed";

/// Strength-to-regen scaling for the Refreshed Effect. Per the
/// design doc §"Refreshed Effect": `base_regen * 0.25 * strength`
/// HP and stamina per tick. We bake the math here because the live
/// regen rates vary by posture — for the v1 implementation we set
/// a flat per-strength bonus rather than capturing the current
/// posture-derived rate; revisit if playtest finds it too generous
/// at high-posture (sleeping) or too stingy at low (standing).
/// **TUNABLE**.
const REFRESHED_HP_PER_STRENGTH: i32 = 1;
const REFRESHED_STAMINA_PER_STRENGTH: i32 = 2;

/// Single chokepoint for every gameplay XP gain. Routes through:
/// 1. **Wake consumer** when `restSource != NONE` (R4).
/// 2. **Repose math** to multiply the gain against the pool (R5).
/// 3. The actual `Profile.experience += amount` write.
///
/// `base_xp` is the pre-Repose award. Returns the actual XP that
/// landed on the character (`base_xp + drawn_bonus`) so the caller
/// can render the right number in the "you gain X" line.
///
/// Admin paths (`advance`, `xp <value>`) intentionally don't call
/// this — they're explicit set/floor commands, not gameplay gains.
pub fn award_experience(world: &mut World, entity: Entity, base_xp: i32) -> i32 {
    if base_xp <= 0 {
        return 0;
    }
    // Wake consumer runs FIRST so the Refreshed Effect lands before
    // the multiplied gain, in case any wake attachment grants a
    // spell-power buff (etc.) that the kill-XP narration would want
    // to reflect on the next swing.
    consume_rest_source_on_xp(world, entity);
    let bonus = apply_repose_on_xp(world, entity, base_xp);
    let total = base_xp.saturating_add(bonus);
    if let Some(mut p) = world.get_mut::<Profile>(entity) {
        p.experience = p.experience.saturating_add(total);
    }
    total
}

/// R5: spend Repose to multiply the gain. Returns the bonus XP
/// drawn from the pool (0 when the pool is empty). Pure mutation of
/// `RestState.repose`; doesn't touch `Profile.experience`.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn apply_repose_on_xp(world: &mut World, entity: Entity, base_xp: i32) -> i32 {
    let pool = world.get::<RestState>(entity).map_or(0, |r| r.repose);
    if pool <= 0 || base_xp <= 0 {
        return 0;
    }
    let bonus_target = ((f64::from(base_xp)) * (REPOSE_MULTIPLIER - 1.0)).max(0.0) as i32;
    let drawn = bonus_target.min(pool);
    if drawn <= 0 {
        return 0;
    }
    if let Some(mut r) = world.get_mut::<RestState>(entity) {
        r.repose = r.repose.saturating_sub(drawn);
    }
    drawn
}

/// R4: spawn Refreshed + apply Wake Effect attachments + clear the
/// queued source. No-op when source is NONE (idempotent — subsequent
/// XP gains hit this path but find nothing to do). QUIT clears the
/// source without spawning Refreshed (per design doc §"First XP gain
/// after login": "skip if QUIT").
pub fn consume_rest_source_on_xp(world: &mut World, entity: Entity) {
    let Some(rest) = world.get::<RestState>(entity).copied() else {
        return;
    };
    if matches!(rest.source, RestSource::None) {
        return;
    }
    // QUIT: clear source but don't spawn Refreshed or apply
    // attachments. The "log off, log back in, kill a mob" path
    // should not grant resting benefits.
    if matches!(rest.source, RestSource::Quit) {
        if let Some(mut r) = world.get_mut::<RestState>(entity) {
            r.source = RestSource::None;
            r.tier = 0;
        }
        return;
    }
    // Spawn the universal Refreshed Effect.
    spawn_refreshed_effect(world, entity, rest.tier);
    // Apply source-keyed Wake Effect attachments. The DB load
    // happens via a stashed `Pool` and the helper is sync; we keep
    // the per-attachment EffectInstance spawn inline because the
    // queue is tiny (~1-3 rows typical) and the alternative is a
    // full async path through the dispatcher.
    match rest.source {
        RestSource::Inn => apply_inn_wake_attachments(world, entity, rest.tier),
        RestSource::Camp => apply_camp_wake_attachments(world, entity),
        RestSource::House => {
            // TODO(housing): housing schema hasn't landed; when it
            // does, query `ObjectWakeEffects` for the bed Object
            // present in the player's room and apply each row.
        }
        RestSource::None | RestSource::Quit => unreachable!("filtered above"),
    }
    // Clear the source so subsequent XP gains don't re-fire.
    if let Some(mut r) = world.get_mut::<RestState>(entity) {
        r.source = RestSource::None;
        r.tier = 0;
    }
}

/// Spawn the Refreshed `EffectInstance` plus the matching
/// `RegenBonus` adjustment. Strength = `rest_tier`; duration =
/// [`REFRESHED_DURATION_SECS`]. The companion `RefreshedBonus`
/// component records the delta so `on_remove` unwinding (in
/// effects.rs) subtracts the same amount.
fn spawn_refreshed_effect(world: &mut World, entity: Entity, rest_tier: i32) {
    let effect_id = world
        .get_resource::<EffectCatalog>()
        .and_then(|c| c.find_by_name(REFRESHED_EFFECT_NAME).map(|d| d.id));
    let Some(effect_id) = effect_id else {
        warn!(
            target = ?entity,
            "Refreshed Effect row not in catalog; skipping wake spawn",
        );
        return;
    };
    let strength = rest_tier.max(1).min(3);
    let hp_bonus = REFRESHED_HP_PER_STRENGTH.saturating_mul(strength);
    let stamina_bonus = REFRESHED_STAMINA_PER_STRENGTH.saturating_mul(strength);
    // Apply the regen delta inline (R6). The on-remove unwind in
    // effects.rs reads `RefreshedBonus` and subtracts the same.
    if world.get::<RegenBonus>(entity).is_none()
        && let Ok(mut em) = world.get_entity_mut(entity)
    {
        em.insert(RegenBonus::default());
    }
    if let Some(mut r) = world.get_mut::<RegenBonus>(entity) {
        r.hp = r.hp.saturating_add(hp_bonus);
        r.stamina = r.stamina.saturating_add(stamina_bonus);
    }
    world.spawn((
        EffectInstance {
            kind: effect_id,
            name: REFRESHED_EFFECT_NAME.to_string(),
            strength,
            remaining_secs: REFRESHED_DURATION_SECS,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(entity),
        RefreshedBonus {
            hp: hp_bonus,
            stamina: stamina_bonus,
        },
    ));
}

/// Spawn one `EffectInstance` per WakeRow, attached to the waking
/// character. The catalog lookup keys on `effect_id`; rows whose
/// effect isn't in the live catalog log a warn and skip (a missing
/// row is a content bug, not a runtime crash).
fn spawn_wake_rows(world: &mut World, entity: Entity, rows: Vec<WakeRow>) {
    for row in rows {
        let effect_name = world
            .get_resource::<EffectCatalog>()
            .and_then(|c| c.by_id.get(&row.effect_id).map(|d| d.name.clone()));
        let Some(name) = effect_name else {
            warn!(
                effect_id = row.effect_id,
                "WakeRow references missing Effect; skipping",
            );
            continue;
        };
        world.spawn((
            EffectInstance {
                kind: row.effect_id,
                name,
                strength: 1,
                remaining_secs: row.duration,
                source: EffectSource::Spell,
                ability_id: None,
            },
            AppliedTo(entity),
        ));
        // Note: modifier_data is captured at the DB load layer
        // (WakeRow.modifier_data) but the runtime currently has no
        // per-attachment override channel for status-typed effects.
        // The data is preserved on the WakeRow for a future
        // pass where wake-effect EffectInstances grow a
        // modifier_data slot.
        let _ = row.modifier_data;
    }
}

/// INN wake attachments: query `RoomWakeEffects` for the player's
/// current room, filtered by tier. Spec says the room where the
/// player logged off — for v1 we use `Located` (the current room),
/// which is the same room since `pick_rest_starting_room` lands
/// INN-sourced players back where they rented.
fn apply_inn_wake_attachments(world: &mut World, entity: Entity, rest_tier: i32) {
    // We need the room's (zone, id) and a DB pool. Mirror the
    // pattern in commands.rs: `DbPool` resource carries a clone of
    // the pool we can block_on against. Wake-attachment loading is
    // synchronous-from-the-call-site but the underlying DB op is
    // async; we use `tokio::runtime::Handle::block_on` so we don't
    // need to thread async through every gain site.
    let room_key = world
        .get::<mud_world::Located>(entity)
        .and_then(|l| world.get::<WorldKey>(l.0).copied());
    let Some(wk) = room_key else { return };
    let Some(pool) = world.get_resource::<crate::commands::DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let rows = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            mud_world::room_wake_effects(&pool, wk.zone, wk.id, rest_tier).await
        })
    });
    match rows {
        Ok(rows) => spawn_wake_rows(world, entity, rows),
        Err(e) => warn!(error = %e, "room wake-effect load failed"),
    }
}

/// CAMP wake attachments: read the `PendingWakeAttachments`
/// transient component populated at camp completion, query
/// `ObjectWakeEffects` for the kit, spawn each, then drop the
/// component.
fn apply_camp_wake_attachments(world: &mut World, entity: Entity) {
    let Some(pending) = world.get::<PendingWakeAttachments>(entity).copied() else {
        return;
    };
    let Some(pool) = world.get_resource::<crate::commands::DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let rows = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            mud_world::object_wake_effects(&pool, pending.kit_zone, pending.kit_id).await
        })
    });
    match rows {
        Ok(rows) => spawn_wake_rows(world, entity, rows),
        Err(e) => warn!(error = %e, "object wake-effect load failed"),
    }
    if let Ok(mut em) = world.get_entity_mut(entity) {
        em.remove::<PendingWakeAttachments>();
    }
}

/// R6: companion to effects.rs's on-remove path. When a Refreshed
/// EffectInstance fades, look up its `RefreshedBonus` companion and
/// subtract the same `RegenBonus` delta that the wake spawned. Called
/// from `effects_tick`'s on_remove arm.
pub fn unwind_refreshed_bonus(world: &mut World, effect_entity: Entity, target: Entity) {
    let Some(bonus) = world.get::<RefreshedBonus>(effect_entity).copied() else {
        return;
    };
    if let Some(mut r) = world.get_mut::<RegenBonus>(target) {
        r.hp = r.hp.saturating_sub(bonus.hp);
        r.stamina = r.stamina.saturating_sub(bonus.stamina);
    }
}
