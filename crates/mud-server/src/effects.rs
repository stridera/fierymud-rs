use bevy_ecs::prelude::*;
use mud_world::{
    AbilityCatalog, AppliedTo, EffectCatalog, EffectInstance, Item, Located, ModifyDelta,
    Stealth, Stunned,
};
use tracing::{info, warn};

use crate::TickCount;
use crate::commands::{apply_damage, drain_lua_outbox, name_of, name_or, send_rendered, send_to, try_insert, try_remove};

/// Marker added to an `EffectInstance` after its `on_apply` Lua
/// hook has fired. Lets the lifecycle scan distinguish "freshly
/// spawned" effects from ones the loop has already seen, without
/// needing every spawn site to call into Lua synchronously.
#[derive(Component, Debug, Clone, Copy)]
pub struct EffectInstanceApplied;

/// What lifecycle hook to fire. Picked off the `EffectDef` field
/// of the matching name; missing or blank hooks are silently
/// skipped, so content authors only pay for what they use.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::enum_variant_names)]
enum EffectHook {
    OnApply,
    OnTick,
    OnRemove,
}

impl EffectHook {
    fn label(self) -> &'static str {
        match self {
            Self::OnApply => "on_apply",
            Self::OnTick => "on_tick",
            Self::OnRemove => "on_remove",
        }
    }

    fn body(self, def: &mud_world::EffectDef) -> Option<&str> {
        match self {
            Self::OnApply => def.on_apply.as_deref(),
            Self::OnTick => def.on_tick.as_deref(),
            Self::OnRemove => def.on_remove.as_deref(),
        }
    }
}

/// Look up the `EffectDef` for `effect_name` in the catalog and
/// fire the matching lifecycle hook against `target`. `self` binds
/// to the target inside the Lua body. Logs failures via tracing
/// rather than the script error log — these are runtime hooks not
/// dispatched triggers, and the catalog has no (zone, id).
fn run_effect_hook(
    world: &mut World,
    hook: EffectHook,
    target: Entity,
    effect_name: &str,
) {
    let body = {
        let Some(catalog) = world.get_resource::<EffectCatalog>() else {
            return;
        };
        let Some(def) = catalog.find_by_name(effect_name) else {
            return;
        };
        let Some(b) = hook.body(def) else {
            return;
        };
        b.to_string()
    };
    if world.get_entity(target).is_err() {
        return;
    }
    let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
        host.exec_for_actor(world, target, &body)
    });
    drain_lua_outbox(world);
    if let Err(e) = result {
        warn!(
            effect = %effect_name,
            hook = %hook.label(),
            error = %e,
            "effect hook failed",
        );
    }
}

/// One effect tick = one second.
const EFFECT_PERIOD_TICKS: u64 = 10;
/// Damage-per-tick for the `bleed` debuff. 2/s for 30s = 60 total
/// — comparable to one rend hit, kept low so multiple stacked damage-over-time effects
/// stay within combat budgets.
const BLEED_DPS: i32 = 2;

/// Decrement remaining duration on every active effect; despawn ones whose
/// duration hit zero (with a "fades" message to the target if it has a
/// connection); also despawn any effect whose target entity has gone away.
#[allow(clippy::too_many_lines)]
pub fn effects_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(EFFECT_PERIOD_TICKS) {
        return;
    }

    // Pre-pass: fire `on_apply` hooks for any EffectInstance that
    // hasn't been seen yet, then mark it `EffectInstanceApplied`.
    // Spawn sites are too scattered to thread the hook through;
    // a once-per-second sweep with a marker is correct enough and
    // doesn't push a Lua call into every callsite.
    let fresh: Vec<(Entity, Entity, String)> = {
        let mut q = world.query_filtered::<
            (Entity, &EffectInstance, &AppliedTo),
            Without<EffectInstanceApplied>,
        >();
        q.iter(world)
            .map(|(eff, inst, applied)| (eff, applied.0, inst.name.clone()))
            .collect()
    };
    for (eff_entity, target, name) in fresh {
        run_effect_hook(world, EffectHook::OnApply, target, &name);
        try_insert(world, eff_entity, EffectInstanceApplied);
    }

    // Snapshot all active effects: (effect_entity, target_entity, remaining_secs, name, ability_id).
    // Doing this in a scoped block releases the query borrow before we mutate.
    let snapshots: Vec<(Entity, Entity, i32, String, Option<i32>)> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .map(|(eff, inst, applied)| {
                (eff, applied.0, inst.remaining_secs, inst.name.clone(), inst.ability_id)
            })
            .collect()
    };

    let mut expired = 0usize;
    let mut orphaned = 0usize;
    let mut killed_by_bleed = 0usize;
    for (eff_entity, target, ticks, name, ability_id) in snapshots {
        if world.get_entity(target).is_err() {
            // Target gone — orphaned effect.
            if let Ok(e) = world.get_entity_mut(eff_entity) {
                e.despawn();
            }
            orphaned += 1;
            continue;
        }
        // Damage-over-time effects: apply HP loss before the duration
        // tick. Lethal damage despawns the target plus all its effects,
        // so we short-circuit the rest of this iteration.
        if name.eq_ignore_ascii_case("bleed") {
            let (dead, _) = apply_damage(world, target, BLEED_DPS);
            send_to(world, target, format!("You bleed for {BLEED_DPS} damage.\r\n"));
            if dead {
                let target_name = name_or(world, target, "<unknown>");
                let room = world.get::<Located>(target).copied().map(|l| l.0);
                if let Some(r) = room {
                    crate::combat::handle_death(world, target, &target_name, r);
                }
                killed_by_bleed += 1;
                // The bleed effect entity is already despawned by
                // handle_death's cleanup of all child effects (or
                // orphan-pickup on the next tick). Don't touch
                // eff_entity here.
                continue;
            }
        }
        // on_tick: fired once per second for every still-alive
        // effect, including permanent ones. Hook bodies typically
        // do their own internal throttling via `time.stamp` if
        // they need a slower cadence.
        run_effect_hook(world, EffectHook::OnTick, target, &name);
        if world.get_entity(eff_entity).is_err() {
            // Hook may have despawned the effect or its target;
            // bail before we touch a stale entity.
            continue;
        }
        if ticks < 0 {
            // Permanent — leave alone.
            continue;
        }
        let new_ticks = ticks - 1;
        if let Some(mut inst) = world.get_mut::<EffectInstance>(eff_entity) {
            inst.remaining_secs = new_ticks;
        }
        if new_ticks <= 0 {
            // on_remove: fire before any of the despawn or marker
            // cleanup runs, so the hook body can still inspect the
            // effect / target relationship if it wants.
            run_effect_hook(world, EffectHook::OnRemove, target, &name);
            // Look up wearoff_to_target / wearoff_to_room from the
            // AbilityMessages catalog. ability_id is None for hardcoded
            // effects (rend's bleed, gouge's blind, admin tests) — those
            // fall through to the terse default.
            let (wearoff_target, wearoff_room) = ability_id
                .and_then(|aid| {
                    world
                        .resource::<AbilityCatalog>()
                        .messages
                        .get(&aid)
                        .map(|m| (m.wearoff_to_target.clone(), m.wearoff_to_room.clone()))
                })
                .unwrap_or((None, None));
            // Send_to registers the target for end-of-tick prompt refresh
            // via commands::flush_prompts; no per-system tracking here.
            let target_msg = wearoff_target
                .as_deref()
                .map_or_else(|| format!("Your {name} fades.\r\n"), |t| format!("{t}\r\n"));
            send_rendered(world, target, &target_msg);
            if let Some(line) = wearoff_room.as_deref()
                && let Some(located) = world.get::<Located>(target).copied()
            {
                let target_name = name_of(world, target);
                let rendered = line.replace("{target.name}", &target_name);
                crate::commands::broadcast_room_except_rendered(
                    world,
                    located.0,
                    &[target],
                    &format!("{rendered}\r\n"),
                );
            }
            // Reverse a `ModifyDelta` companion before despawning the
            // effect — for `modify` effect-type EffectInstances. The
            // delta records what stat was bumped and by how much; we
            // subtract it back here so stacking buffs from each
            // other's expiries don't double-clear the bonus.
            if let Some(delta) = world.get::<ModifyDelta>(eff_entity).cloned()
                && world.get_entity(target).is_ok()
            {
                crate::commands::reverse_modify_delta(
                    world,
                    target,
                    &delta.target,
                    delta.amount,
                );
            }
            // Rest / repose R6: when the Refreshed Effect fades,
            // subtract the RegenBonus delta the wake path stamped.
            // The companion `RefreshedBonus` component records the
            // exact amount so stacked Refresheds (rare — would
            // require a mid-session re-rent and re-consume) cleanly
            // unwind one at a time.
            crate::rest::unwind_refreshed_bonus(world, eff_entity, target);
            // Reverse a `SpellResistanceDelta` companion the same way
            // ModifyDelta unwinds. Stacked PROT_*/STONE_SKIN cleanly
            // peel back to whatever the underlying item-resistance
            // value was.
            if let Some(delta) =
                world.get::<mud_world::SpellResistanceDelta>(eff_entity).copied()
                && world.get_entity(target).is_ok()
            {
                if let Some(mut r) = world.get_mut::<mud_world::Resistances>(target) {
                    let entry = r.0.entry(delta.element).or_insert(0);
                    *entry = entry.saturating_sub(delta.percent);
                    if *entry == 0 {
                        r.0.remove(&delta.element);
                    }
                }
            }
            if let Ok(e) = world.get_entity_mut(eff_entity) {
                e.despawn();
            }
            expired += 1;
            // Stun marker outlives only as long as at least one
            // backing `stun` EffectInstance is on the target. After
            // despawning *this* one, recheck and clear if none left.
            if name.eq_ignore_ascii_case("stun") {
                let still_stunned = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world)
                        .any(|(eff, applied)| {
                            applied.0 == target && eff.name.eq_ignore_ascii_case("stun")
                        })
                };
                if !still_stunned {
                    try_remove::<Stunned>(world, target);
                }
            }
            // Object-decay: an effect named "decay" applied to an
            // Item entity acts as the object's lifetime gate (used
            // by the `portal` effect-type for spawned gates,
            // moonwells, etc.). When it fades, despawn the object
            // itself so the portal closes.
            if name.eq_ignore_ascii_case("decay")
                && world.get::<Item>(target).is_some()
                && let Ok(e) = world.get_entity_mut(target)
            {
                e.despawn();
            }
            // Stealth marker mirrors the stun pattern: it outlives only
            // as long as at least one `hidden` / `sneak` EffectInstance
            // is on the target. Manually-toggled `hide` / `visible`
            // commands install/remove Stealth directly without a
            // backing effect, so a target with manual stealth + no
            // status effects is unaffected by this branch.
            if name.eq_ignore_ascii_case("hidden") || name.eq_ignore_ascii_case("sneak") {
                let still_hidden = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target
                            && (eff.name.eq_ignore_ascii_case("hidden")
                                || eff.name.eq_ignore_ascii_case("sneak"))
                    })
                };
                if !still_hidden {
                    try_remove::<Stealth>(world, target);
                }
            }
            // Flying marker mirrors the Stealth pattern — alive only
            // while at least one backing effect (FLY, WINGS_OF_*) is
            // on the target. Race-set Flying is proto-attached and
            // not subject to this teardown.
            if name.eq_ignore_ascii_case("fly") {
                let still_flying = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target && eff.name.eq_ignore_ascii_case("fly")
                    })
                };
                if !still_flying {
                    try_remove::<mud_world::Flying>(world, target);
                }
            }
            if name.eq_ignore_ascii_case("bless") {
                let still_blessed = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target && eff.name.eq_ignore_ascii_case("bless")
                    })
                };
                if !still_blessed {
                    try_remove::<mud_world::Bless>(world, target);
                }
            }
            if name.eq_ignore_ascii_case("sanctuary") {
                let still_sanctified = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target && eff.name.eq_ignore_ascii_case("sanctuary")
                    })
                };
                if !still_sanctified {
                    try_remove::<mud_world::Sanctuary>(world, target);
                }
            }
            if name.eq_ignore_ascii_case("haste") {
                let still_hasted = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target && eff.name.eq_ignore_ascii_case("haste")
                    })
                };
                if !still_hasted {
                    try_remove::<mud_world::Haste>(world, target);
                }
            }
            if name.eq_ignore_ascii_case("empowered") {
                let still_empowered = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target && eff.name.eq_ignore_ascii_case("empowered")
                    })
                };
                if !still_empowered {
                    try_remove::<mud_world::Empowered>(world, target);
                }
            }
            // J2 alignment-protect teardown — PROT_FROM_EVIL /
            // PROT_FROM_GOOD spawn instances with name="resistance"
            // (shared with element-resistance flavors) but tag the
            // backing EffectInstance entity with
            // `AlignmentProtectionTag(Evil|Good)`. When *this*
            // expiring entity carries that tag, drop the marker only
            // when no remaining EffectInstance shares the same tag —
            // mirrors the bless/sanctuary refcount pattern.
            if let Some(tag) =
                world.get::<mud_world::AlignmentProtectionTag>(eff_entity).copied()
            {
                let still_protected = {
                    let mut q = world
                        .query::<(Entity, &EffectInstance, &AppliedTo, &mud_world::AlignmentProtectionTag)>();
                    q.iter(world).any(|(e, _, applied, t)| {
                        e != eff_entity
                            && applied.0 == target
                            && matches!(
                                (*t, tag),
                                (
                                    mud_world::AlignmentProtectionTag::Evil,
                                    mud_world::AlignmentProtectionTag::Evil
                                ) | (
                                    mud_world::AlignmentProtectionTag::Good,
                                    mud_world::AlignmentProtectionTag::Good
                                )
                            )
                    })
                };
                if !still_protected {
                    match tag {
                        mud_world::AlignmentProtectionTag::Evil => {
                            try_remove::<mud_world::ProtectFromEvil>(world, target);
                        }
                        mud_world::AlignmentProtectionTag::Good => {
                            try_remove::<mud_world::ProtectFromGood>(world, target);
                        }
                    }
                }
            }
            // L3 summon teardown — conjuration spells spawn an
            // EffectInstance with name="summoned-{mobType}" pointing
            // at the spawned mob via AppliedTo(mob). When the
            // instance expires, the conjured mob vanishes — same
            // semantics as the legacy "follower fades" behavior.
            // Cleanup: drop a final flavor line into the mob's room
            // so observers see the dispel, then despawn the mob and
            // anything it was carrying.
            if name.starts_with("summoned-") {
                if let Some(mob_room) =
                    world.get::<mud_world::Located>(target).map(|l| l.0)
                {
                    let mob_name = world
                        .get::<mud_world::Named>(target)
                        .map_or("the summoned creature".to_string(), |n| n.name.clone());
                    let players: Vec<bevy_ecs::entity::Entity> = {
                        let mut q = world.query_filtered::<(bevy_ecs::entity::Entity, &mud_world::Located), bevy_ecs::prelude::With<mud_world::Player>>();
                        q.iter(world)
                            .filter(|(_, l)| l.0 == mob_room)
                            .map(|(e, _)| e)
                            .collect()
                    };
                    let msg = format!("{mob_name} fades back to where it was summoned from.\r\n");
                    for p in players {
                        crate::commands::send_to(world, p, msg.clone());
                    }
                }
                if let Ok(em) = world.get_entity_mut(target) {
                    em.despawn();
                }
            }
            // Wall teardown — WALL_OF_STONE / WALL_OF_ICE spawn an
            // EffectInstance named "wall-{type}" with
            // AppliedTo(room). The room carries a RoomBlockedExits
            // map keyed on Direction; on expiry, remove whichever
            // entry was backed by THIS expiring entity (the cast
            // arm overwrites stale entries on re-cast, so there's
            // at most one match). Despawn the (now-empty) map when
            // it's the last wall.
            if name.starts_with("wall-") {
                // Capture the (direction, kind_label) of the entry
                // about to drop so the post-cleanup broadcast can
                // name what just faded. Done before the retain so
                // the map still has the entry.
                let expiring: Option<(mud_db::enums::Direction, String)> = world
                    .get::<mud_world::RoomBlockedExits>(target)
                    .and_then(|b| {
                        b.by_direction
                            .iter()
                            .find(|(_, e)| e.backed_by == eff_entity)
                            .map(|(d, e)| (*d, e.kind_label.clone()))
                    });
                if let Some(mut blocked) =
                    world.get_mut::<mud_world::RoomBlockedExits>(target)
                {
                    blocked
                        .by_direction
                        .retain(|_, entry| entry.backed_by != eff_entity);
                }
                let empty = world
                    .get::<mud_world::RoomBlockedExits>(target)
                    .is_some_and(|b| b.by_direction.is_empty());
                if empty {
                    try_remove::<mud_world::RoomBlockedExits>(world, target);
                }
                // Broadcast the fade to everyone in the affected
                // room. Otherwise a wall just silently vanishes —
                // a player who was waiting for it to expire (or who
                // got cornered behind one) would have no signal it's
                // safe to move now. Mirrors the bash-crumble UX.
                if let Some((dir, kind_label)) = expiring {
                    let players: Vec<bevy_ecs::entity::Entity> = {
                        let mut q = world.query_filtered::<(bevy_ecs::entity::Entity, &mud_world::Located), bevy_ecs::prelude::With<mud_world::Player>>();
                        q.iter(world)
                            .filter(|(_, l)| l.0 == target)
                            .map(|(e, _)| e)
                            .collect()
                    };
                    let dir_name = crate::commands::direction_name(dir);
                    let msg = format!(
                        "The {kind_label} {dir_name} shudders and dissolves into nothing.\r\n"
                    );
                    for p in players {
                        crate::commands::send_to(world, p, msg.clone());
                    }
                }
            }
            // J1 globe teardown — MINOR/MAJOR_GLOBE both spawn an
            // EffectInstance named "globe" with the maxCircle stored
            // in `strength`. When the last one fades, recompute the
            // marker from any remaining instances rather than
            // removing outright — stacking MAJOR over MINOR shouldn't
            // drop coverage to nothing when only one expires.
            if name.eq_ignore_ascii_case("globe") {
                let highest = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world)
                        .filter(|(eff, applied)| {
                            applied.0 == target && eff.name.eq_ignore_ascii_case("globe")
                        })
                        .map(|(eff, _)| eff.strength)
                        .max()
                };
                match highest {
                    Some(s) if s > 0 => {
                        try_insert(world, target, mud_world::MaxAbsorbCircle(s));
                    }
                    _ => {
                        try_remove::<mud_world::MaxAbsorbCircle>(world, target);
                    }
                }
            }
            // Room-light teardown: ILLUMINATION / MAGIC_TORCH land
            // an EffectInstance named "light" with AppliedTo(room).
            // When the last one fades, drop the RoomMagicalLight
            // marker so room_is_dark / room_has_light read normally.
            if name.eq_ignore_ascii_case("light") {
                let still_lit = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target && eff.name.eq_ignore_ascii_case("light")
                    })
                };
                if !still_lit {
                    try_remove::<mud_world::RoomMagicalLight>(world, target);
                    let players: Vec<bevy_ecs::entity::Entity> = {
                        let mut q = world.query_filtered::<(bevy_ecs::entity::Entity, &mud_world::Located), bevy_ecs::prelude::With<mud_world::Player>>();
                        q.iter(world)
                            .filter(|(_, l)| l.0 == target)
                            .map(|(e, _)| e)
                            .collect()
                    };
                    let msg = "The magical radiance fades.\r\n".to_string();
                    for p in players {
                        crate::commands::send_to(world, p, msg.clone());
                    }
                }
            }
            // Mirror for DARKNESS.
            if name.eq_ignore_ascii_case("darkness") {
                let still_dark = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target && eff.name.eq_ignore_ascii_case("darkness")
                    })
                };
                if !still_dark {
                    try_remove::<mud_world::RoomMagicalDarkness>(world, target);
                    let players: Vec<bevy_ecs::entity::Entity> = {
                        let mut q = world.query_filtered::<(bevy_ecs::entity::Entity, &mud_world::Located), bevy_ecs::prelude::With<mud_world::Player>>();
                        q.iter(world)
                            .filter(|(_, l)| l.0 == target)
                            .map(|(e, _)| e)
                            .collect()
                    };
                    let msg = "The unnatural darkness dissipates.\r\n".to_string();
                    for p in players {
                        crate::commands::send_to(world, p, msg.clone());
                    }
                }
            }
            // CIRCLE_OF_FIRE teardown.
            if name.eq_ignore_ascii_case("burning") {
                let still_burning = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target && eff.name.eq_ignore_ascii_case("burning")
                    })
                };
                if !still_burning {
                    try_remove::<mud_world::RoomBurningEffect>(world, target);
                    // Broadcast the flames dying down to anyone in
                    // the room so they know it's safe to walk again
                    // without taking damage. Without it the hazard
                    // just silently lifts.
                    let players: Vec<bevy_ecs::entity::Entity> = {
                        let mut q = world.query_filtered::<(bevy_ecs::entity::Entity, &mud_world::Located), bevy_ecs::prelude::With<mud_world::Player>>();
                        q.iter(world)
                            .filter(|(_, l)| l.0 == target)
                            .map(|(e, _)| e)
                            .collect()
                    };
                    let msg = "The flames lash one last time, then sputter out.\r\n".to_string();
                    for p in players {
                        crate::commands::send_to(world, p, msg.clone());
                    }
                }
            }
            // DetectInvis teardown — flag-based effect with name
            // "detect_invisible" (the data carries it as a status
            // effect with that name).
            if name.eq_ignore_ascii_case("detect_invisible") {
                let still_seeing = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target
                            && eff.name.eq_ignore_ascii_case("detect_invisible")
                    })
                };
                if !still_seeing {
                    try_remove::<mud_world::DetectInvis>(world, target);
                }
            }
            // Invisible marker: the expiring effect itself doesn't
            // need a name match — we tag the install with
            // `InvisibleSource` and walk those at expiry. When no
            // other tagged effect remains on the target, the
            // Invisible marker drops.
            {
                let still_invisible = {
                    let mut q = world.query::<(&mud_world::InvisibleSource, &AppliedTo)>();
                    q.iter(world).any(|(_, applied)| applied.0 == target)
                };
                if !still_invisible {
                    try_remove::<mud_world::Invisible>(world, target);
                }
            }
        }
    }
    if killed_by_bleed > 0 {
        info!(killed_by_bleed, "bleed deaths");
    }

    if expired > 0 || orphaned > 0 {
        info!(expired, orphaned, "effects tick");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::EffectSource;

    fn make_effect(world: &mut World, target: Entity, secs: i32) -> Entity {
        world
            .spawn((
                EffectInstance {
                    kind: 1,
                    name: "test ward".to_string(),
                    strength: 1,
                    remaining_secs: secs,
                    source: EffectSource::Admin,
                    ability_id: None,
                },
                AppliedTo(target),
            ))
            .id()
    }

    fn run_effects_tick(world: &mut World) {
        // effects_tick is gated on tick % 10 == 0.
        world.insert_resource(TickCount(EFFECT_PERIOD_TICKS));
        effects_tick(world);
    }

    #[test]
    fn decrements_remaining_secs() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let eff = make_effect(&mut world, target, 5);

        run_effects_tick(&mut world);

        let inst = world.get::<EffectInstance>(eff).expect("effect still alive");
        assert_eq!(inst.remaining_secs, 4);
    }

    #[test]
    fn despawns_when_duration_expires() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let eff = make_effect(&mut world, target, 1);

        run_effects_tick(&mut world);

        assert!(
            world.get_entity(eff).is_err(),
            "effect despawned when remaining_secs hit 0"
        );
    }

    #[test]
    fn permanent_effect_is_left_alone() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let eff = make_effect(&mut world, target, -1);

        run_effects_tick(&mut world);
        run_effects_tick(&mut world);

        let inst = world.get::<EffectInstance>(eff).expect("permanent stays");
        assert_eq!(inst.remaining_secs, -1, "permanent flag preserved");
    }

    #[test]
    fn orphaned_effect_when_target_despawns() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let eff = make_effect(&mut world, target, 100);
        // Target goes away mid-game.
        world.get_entity_mut(target).unwrap().despawn();

        run_effects_tick(&mut world);

        assert!(
            world.get_entity(eff).is_err(),
            "orphan cleanup despawned the effect"
        );
    }

    #[test]
    fn stun_marker_cleared_when_last_stun_expires() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        // Two stacked stuns: one expires this tick, one keeps going.
        let _stun_a = world
            .spawn((
                EffectInstance {
                    kind: 21,
                    name: "stun".to_string(),
                    strength: 1,
                    remaining_secs: 1,
                    source: EffectSource::Spell,
                    ability_id: None,
                },
                AppliedTo(target),
            ))
            .id();
        let _stun_b = world
            .spawn((
                EffectInstance {
                    kind: 21,
                    name: "stun".to_string(),
                    strength: 1,
                    remaining_secs: 5,
                    source: EffectSource::Spell,
                    ability_id: None,
                },
                AppliedTo(target),
            ))
            .id();
        world.entity_mut(target).insert(Stunned);

        run_effects_tick(&mut world);
        // First stun expires; second still active → Stunned must remain.
        assert!(
            world.get::<Stunned>(target).is_some(),
            "Stunned stays while another stun is alive"
        );

        // Tick down four more times to expire the longer stun.
        for _ in 0..5 {
            run_effects_tick(&mut world);
        }
        assert!(
            world.get::<Stunned>(target).is_none(),
            "Stunned cleared after the last stun fades"
        );
    }

    #[test]
    fn skips_off_period_ticks() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let eff = make_effect(&mut world, target, 5);
        world.insert_resource(TickCount(EFFECT_PERIOD_TICKS - 1));
        effects_tick(&mut world);
        let inst = world.get::<EffectInstance>(eff).expect("untouched");
        assert_eq!(inst.remaining_secs, 5, "off-period tick is a no-op");
    }
}
