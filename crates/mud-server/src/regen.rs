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
    use super::*;

    fn make_player(
        world: &mut World,
        hp: i32,
        hp_max: i32,
        stamina: i32,
        stamina_max: i32,
        posture: PostureKind,
    ) -> Entity {
        world
            .spawn((
                Online,
                Health { hp, max: hp_max },
                Stamina {
                    current: stamina,
                    max: stamina_max,
                },
                Posture(posture),
            ))
            .id()
    }

    fn run_regen_tick(world: &mut World) {
        // regen_tick gates on tick % 10 == 0.
        world.insert_resource(TickCount(REGEN_PERIOD_TICKS));
        regen_tick(world);
    }

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

    #[test]
    fn standing_regens_stamina_only() {
        let mut world = World::new();
        let p = make_player(&mut world, 50, 100, 30, 50, PostureKind::Standing);
        run_regen_tick(&mut world);
        assert_eq!(world.get::<Stamina>(p).unwrap().current, 31);
        assert_eq!(world.get::<Health>(p).unwrap().hp, 50, "standing doesn't auto-heal");
    }

    #[test]
    fn sleeping_regens_both_pools_at_full_rate() {
        let mut world = World::new();
        let p = make_player(&mut world, 50, 100, 30, 50, PostureKind::Sleeping);
        run_regen_tick(&mut world);
        assert_eq!(world.get::<Stamina>(p).unwrap().current, 38, "+8 stamina");
        assert_eq!(world.get::<Health>(p).unwrap().hp, 54, "+4 hp");
    }

    #[test]
    fn caps_at_max() {
        let mut world = World::new();
        // Already at max — no change.
        let at_max = make_player(&mut world, 100, 100, 50, 50, PostureKind::Sleeping);
        // Within one tick of max — clamp.
        let near_max = make_player(&mut world, 97, 100, 49, 50, PostureKind::Sleeping);
        run_regen_tick(&mut world);
        assert_eq!(world.get::<Stamina>(at_max).unwrap().current, 50);
        assert_eq!(world.get::<Health>(at_max).unwrap().hp, 100);
        assert_eq!(world.get::<Stamina>(near_max).unwrap().current, 50, "+8 clamped to 50");
        assert_eq!(world.get::<Health>(near_max).unwrap().hp, 100, "+4 clamped to 100");
    }

    #[test]
    fn fighting_blocks_regen() {
        let mut world = World::new();
        let dummy = world.spawn_empty().id();
        let p = make_player(&mut world, 50, 100, 30, 50, PostureKind::Resting);
        world
            .get_entity_mut(p)
            .unwrap()
            .insert(Fighting(dummy));
        run_regen_tick(&mut world);
        assert_eq!(world.get::<Stamina>(p).unwrap().current, 30, "no stamina regen while fighting");
        assert_eq!(world.get::<Health>(p).unwrap().hp, 50, "no hp regen while fighting");
    }

    #[test]
    fn offline_players_skip() {
        let mut world = World::new();
        // Spawn directly without Online (skips make_player).
        let p = world
            .spawn((
                Health { hp: 50, max: 100 },
                Stamina { current: 30, max: 50 },
                Posture(PostureKind::Sleeping),
            ))
            .id();
        run_regen_tick(&mut world);
        assert_eq!(world.get::<Stamina>(p).unwrap().current, 30);
        assert_eq!(world.get::<Health>(p).unwrap().hp, 50);
    }

    #[test]
    fn skips_off_period_ticks() {
        let mut world = World::new();
        let p = make_player(&mut world, 50, 100, 30, 50, PostureKind::Sleeping);
        world.insert_resource(TickCount(REGEN_PERIOD_TICKS - 1));
        regen_tick(&mut world);
        assert_eq!(world.get::<Stamina>(p).unwrap().current, 30);
        assert_eq!(world.get::<Health>(p).unwrap().hp, 50);
    }
}
