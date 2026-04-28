use bevy_ecs::prelude::*;
use mud_world::{AppliedTo, EffectInstance};
use tracing::info;

use crate::TickCount;
use crate::commands::{send_prompt, send_to};

/// One effect tick = one second.
const EFFECT_PERIOD_TICKS: u64 = 10;

/// Decrement remaining duration on every active effect; despawn ones whose
/// duration hit zero (with a "fades" message to the target if it has a
/// connection); also despawn any effect whose target entity has gone away.
pub fn effects_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(EFFECT_PERIOD_TICKS) {
        return;
    }

    // Snapshot all active effects: (effect_entity, target_entity, remaining_secs, name).
    // Doing this in a scoped block releases the query borrow before we mutate.
    let snapshots: Vec<(Entity, Entity, i32, String)> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .map(|(eff, inst, applied)| (eff, applied.0, inst.remaining_secs, inst.name.clone()))
            .collect()
    };

    let mut expired = 0usize;
    let mut orphaned = 0usize;
    let mut prompted = std::collections::HashSet::new();
    for (eff_entity, target, ticks, name) in snapshots {
        if world.get_entity(target).is_err() {
            // Target gone — orphaned effect.
            if let Ok(e) = world.get_entity_mut(eff_entity) {
                e.despawn();
            }
            orphaned += 1;
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
            send_to(world, target, format!("Your {name} fades.\r\n"));
            if let Ok(e) = world.get_entity_mut(eff_entity) {
                e.despawn();
            }
            // Refresh prompt for the affected target (mobs without
            // Connection are a no-op, despawned targets already skipped).
            if prompted.insert(target) {
                send_prompt(world, target);
            }
            expired += 1;
        }
    }

    if expired > 0 || orphaned > 0 {
        info!(expired, orphaned, "effects tick");
    }
}
