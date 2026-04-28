use bevy_ecs::prelude::*;
use mud_world::{Fighting, Health, Online, Posture, PostureKind, Stamina};

use crate::TickCount;

/// One regen tick = one second.
const REGEN_PERIOD_TICKS: u64 = 10;

/// Stamina recovered per second by posture.
fn stamina_per_tick(p: PostureKind) -> i32 {
    match p {
        PostureKind::Standing => 1,
        PostureKind::Sitting => 2,
        PostureKind::Resting => 4,
        PostureKind::Sleeping => 8,
    }
}

/// HP recovered per second by posture. Standing players don't auto-heal —
/// resting/sleeping does the work, like classic MUDs.
fn health_per_tick(p: PostureKind) -> i32 {
    match p {
        PostureKind::Standing => 0,
        PostureKind::Sitting => 1,
        PostureKind::Resting => 2,
        PostureKind::Sleeping => 4,
    }
}

/// Top up Health and Stamina for online, non-fighting players based on their
/// Posture. Caps at max and never decreases. Silent — no per-tick messages.
pub fn regen_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(REGEN_PERIOD_TICKS) {
        return;
    }

    // Snapshot the (entity, new_stamina, new_hp) tuples while no mutable
    // borrows are live, then apply. Pattern matches combat_tick / effects_tick.
    let updates: Vec<(Entity, Option<i32>, Option<i32>)> = {
        let mut q = world.query_filtered::<
            (Entity, Option<&Stamina>, Option<&Health>, &Posture),
            (With<Online>, Without<Fighting>),
        >();
        q.iter(world)
            .map(|(e, stamina, hp, posture)| {
                let new_stamina = stamina.and_then(|s| {
                    if s.current >= s.max {
                        None
                    } else {
                        Some((s.current + stamina_per_tick(posture.0)).min(s.max))
                    }
                });
                let new_hp = hp.and_then(|h| {
                    if h.hp >= h.max {
                        None
                    } else {
                        let regen = health_per_tick(posture.0);
                        if regen == 0 {
                            None
                        } else {
                            Some((h.hp + regen).min(h.max))
                        }
                    }
                });
                (e, new_stamina, new_hp)
            })
            .filter(|(_, st, hp)| st.is_some() || hp.is_some())
            .collect()
    };

    for (entity, new_stamina, new_hp) in updates {
        if let Some(new_stamina) = new_stamina
            && let Some(mut s) = world.get_mut::<Stamina>(entity)
        {
            s.current = new_stamina;
        }
        if let Some(new_hp) = new_hp
            && let Some(mut h) = world.get_mut::<Health>(entity)
        {
            h.hp = new_hp;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{health_per_tick, stamina_per_tick};
    use mud_world::PostureKind;

    #[test]
    fn regen_rates_scale_with_posture() {
        // Stamina: 1/2/4/8 doubling cadence.
        assert_eq!(stamina_per_tick(PostureKind::Standing), 1);
        assert_eq!(stamina_per_tick(PostureKind::Sitting), 2);
        assert_eq!(stamina_per_tick(PostureKind::Resting), 4);
        assert_eq!(stamina_per_tick(PostureKind::Sleeping), 8);
        // Health: 0/1/2/4 — standing players don't auto-heal.
        assert_eq!(health_per_tick(PostureKind::Standing), 0);
        assert_eq!(health_per_tick(PostureKind::Sitting), 1);
        assert_eq!(health_per_tick(PostureKind::Resting), 2);
        assert_eq!(health_per_tick(PostureKind::Sleeping), 4);
    }
}
