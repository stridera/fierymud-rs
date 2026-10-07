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
    RefreshedBonus, RegenBonus, RestState, WakeEffectCatalog, WakeRow, WorldKey,
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
    // Staff-level characters gain nothing (`Profile::grant_experience`), so
    // they must not burn their rest source or Repose pool either.
    if world
        .get::<Profile>(entity)
        .is_some_and(|p| mud_db::enums::is_staff_level(p.level))
    {
        return 0;
    }
    // Wake consumer runs FIRST so the Refreshed Effect lands before
    // the multiplied gain, in case any wake attachment grants a
    // spell-power buff (etc.) that the kill-XP narration would want
    // to reflect on the next swing.
    consume_rest_source_on_xp(world, entity);
    let bonus = apply_repose_on_xp(world, entity, base_xp);
    let total = base_xp.saturating_add(bonus);
    // Staff-level characters gain nothing (`Profile::grant_experience`).
    let applied = world
        .get_mut::<Profile>(entity)
        .is_some_and(|mut p| p.grant_experience(total));
    if applied { total } else { 0 }
}

/// R5: spend Repose to multiply the gain. Returns the bonus XP
/// drawn from the pool (0 when the pool is empty). Pure mutation of
/// `RestState.repose`; doesn't touch `Profile.experience`.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
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
    // Apply source-keyed Wake Effect attachments from the boot-loaded
    // `WakeEffectCatalog` (no DB access on this path).
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
    let strength = rest_tier.clamp(1, 3);
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

/// Spawn one `EffectInstance` per `WakeRow`, attached to the waking
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

/// INN wake attachments: look up `RoomWakeEffects` for the player's
/// current room in the boot-loaded [`WakeEffectCatalog`], filtered by
/// tier. Spec says the room where the player logged off — for v1 we
/// use `Located` (the current room), which is the same room since
/// `pick_rest_starting_room` lands INN-sourced players back where
/// they rented. Pure in-memory read: this runs inside ECS systems on
/// a current-thread runtime and must never block on the DB.
fn apply_inn_wake_attachments(world: &mut World, entity: Entity, rest_tier: i32) {
    let room_key = world
        .get::<mud_world::Located>(entity)
        .and_then(|l| world.get::<WorldKey>(l.0).copied());
    let Some(wk) = room_key else { return };
    let Some(catalog) = world.get_resource::<WakeEffectCatalog>() else {
        return;
    };
    let rows = catalog.room_rows(wk.zone, wk.id, rest_tier);
    spawn_wake_rows(world, entity, rows);
}

/// CAMP wake attachments: read the `PendingWakeAttachments`
/// transient component populated at camp completion, look up the kit's
/// `ObjectWakeEffects` rows in the [`WakeEffectCatalog`], spawn each,
/// then drop the component.
fn apply_camp_wake_attachments(world: &mut World, entity: Entity) {
    let Some(pending) = world.get::<PendingWakeAttachments>(entity).copied() else {
        return;
    };
    let rows = world
        .get_resource::<WakeEffectCatalog>()
        .map(|c| c.object_rows(pending.kit_zone, pending.kit_id))
        .unwrap_or_default();
    spawn_wake_rows(world, entity, rows);
    if let Ok(mut em) = world.get_entity_mut(entity) {
        em.remove::<PendingWakeAttachments>();
    }
}

/// R6: companion to effects.rs's on-remove path. When a Refreshed
/// `EffectInstance` fades, look up its `RefreshedBonus` companion and
/// subtract the same `RegenBonus` delta that the wake spawned. Called
/// from `effects_tick`'s `on_remove` arm.
pub fn unwind_refreshed_bonus(world: &mut World, effect_entity: Entity, target: Entity) {
    let Some(bonus) = world.get::<RefreshedBonus>(effect_entity).copied() else {
        return;
    };
    if let Some(mut r) = world.get_mut::<RegenBonus>(target) {
        r.hp = r.hp.saturating_sub(bonus.hp);
        r.stamina = r.stamina.saturating_sub(bonus.stamina);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::{EffectDef, Located, PendingWakeAttachments, RoomWakeRow, WakeEffectCatalog};

    fn effect_def(id: i32, name: &str) -> EffectDef {
        EffectDef {
            id,
            name: name.to_string(),
            description: None,
            effect_type: "status".to_string(),
            tags: Vec::new(),
            presence_override: None,
            default_params: serde_json::Value::Null,
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        }
    }

    fn wake_row(effect_id: i32) -> WakeRow {
        WakeRow {
            effect_id,
            modifier_data: serde_json::Value::Null,
            duration: 600,
        }
    }

    /// Minimal world: one room, one player resting in it, a catalog with
    /// the universal Refreshed effect (id 1) and one wake-attachment
    /// effect (id 2), and a wake catalog authored for the room + a kit.
    fn setup(source: RestSource, tier: i32) -> (World, Entity) {
        let mut world = World::new();
        let mut effects = EffectCatalog::default();
        effects
            .by_id
            .insert(1, effect_def(1, REFRESHED_EFFECT_NAME));
        effects.by_id.insert(2, effect_def(2, "Cozy"));
        world.insert_resource(effects);
        let mut wake = WakeEffectCatalog::default();
        wake.by_room.insert(
            (30, 1),
            vec![
                RoomWakeRow {
                    min_tier: None,
                    row: wake_row(2),
                },
                RoomWakeRow {
                    min_tier: Some(3),
                    row: wake_row(2),
                },
            ],
        );
        wake.by_object.insert((40, 7), vec![wake_row(2)]);
        world.insert_resource(wake);
        let room = world.spawn(WorldKey { zone: 30, id: 1 }).id();
        let player = world
            .spawn((
                Located(room),
                Profile {
                    level: 5,
                    class_id: None,
                    race: "Human".to_string(),
                    experience: 0,
                    gender: "neutral".to_string(),
                },
                RestState {
                    repose: 0,
                    source,
                    tier,
                },
            ))
            .id();
        (world, player)
    }

    fn effect_count(world: &mut World, kind: i32) -> usize {
        world
            .query::<&EffectInstance>()
            .iter(world)
            .filter(|e| e.kind == kind)
            .count()
    }

    // current_thread flavor mirrors `#[tokio::main(flavor = "current_thread")]`
    // in main.rs, where tokio blocking-in-place helpers would panic.
    #[tokio::test(flavor = "current_thread")]
    async fn inn_wake_consumed_without_blocking_runtime() {
        let (mut world, player) = setup(RestSource::Inn, 1);
        let gained = award_experience(&mut world, player, 100);
        assert_eq!(gained, 100);
        let rest = *world.get::<RestState>(player).unwrap();
        assert!(matches!(rest.source, RestSource::None));
        assert_eq!(rest.tier, 0);
        assert_eq!(world.get::<Profile>(player).unwrap().experience, 100);
        assert_eq!(effect_count(&mut world, 1), 1, "Refreshed spawned");
        // tier 1 passes only the un-gated row; the min_tier=3 row is filtered.
        assert_eq!(effect_count(&mut world, 2), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inn_wake_high_tier_includes_gated_rows() {
        let (mut world, player) = setup(RestSource::Inn, 3);
        award_experience(&mut world, player, 10);
        assert_eq!(effect_count(&mut world, 2), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn camp_wake_consumed_without_blocking_runtime() {
        let (mut world, player) = setup(RestSource::Camp, 1);
        world.entity_mut(player).insert(PendingWakeAttachments {
            kit_zone: 40,
            kit_id: 7,
        });
        award_experience(&mut world, player, 50);
        let rest = *world.get::<RestState>(player).unwrap();
        assert!(matches!(rest.source, RestSource::None));
        assert!(world.get::<PendingWakeAttachments>(player).is_none());
        assert_eq!(effect_count(&mut world, 1), 1);
        assert_eq!(effect_count(&mut world, 2), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_wake_catalog_is_not_fatal() {
        let (mut world, player) = setup(RestSource::Inn, 1);
        world.remove_resource::<WakeEffectCatalog>();
        award_experience(&mut world, player, 10);
        let rest = *world.get::<RestState>(player).unwrap();
        assert!(matches!(rest.source, RestSource::None));
        assert_eq!(effect_count(&mut world, 2), 0);
    }
}
