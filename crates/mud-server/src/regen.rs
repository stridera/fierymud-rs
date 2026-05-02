use bevy_ecs::prelude::*;
use mud_world::{Fighting, Frozen, Ghost, Health, Hunger, Online, Player, Posture, PostureKind, Stamina, Thirst};

use crate::TickCount;
use crate::commands::send_to;

/// One regen tick = one second.
const REGEN_PERIOD_TICKS: u64 = 10;

/// Stamina recovered per second by posture. Kneeling regens like
/// standing — it's an alert posture, just lower-profile.
fn stamina_per_tick(p: PostureKind) -> i32 {
    match p {
        PostureKind::Standing | PostureKind::Kneeling => 1,
        PostureKind::Sitting => 2,
        PostureKind::Resting => 4,
        PostureKind::Sleeping => 8,
    }
}

/// HP recovered per second by posture. Standing players don't auto-heal —
/// resting/sleeping does the work, like classic MUDs. Kneeling = standing.
fn health_per_tick(p: PostureKind) -> i32 {
    match p {
        PostureKind::Standing | PostureKind::Kneeling => 0,
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
    // Starving / parched players regen at half rate (rounded down) — they
    // can still slowly recover, but the hunger drain on top makes net
    // progress glacial. Mirrors the stat-survival contract in
    // hunger_thirst_tick.
    let updates: Vec<(Entity, Option<i32>, Option<i32>)> = {
        let mut q = world.query_filtered::<
            (Entity, Option<&Stamina>, Option<&Health>, &Posture, Option<&Hunger>, Option<&Thirst>),
            (With<Online>, Without<Fighting>),
        >();
        q.iter(world)
            .map(|(e, stamina, hp, posture, hunger, thirst)| {
                let starved = hunger.is_some_and(|h| h.0 >= STARVING_AT)
                    || thirst.is_some_and(|t| t.0 >= PARCHED_AT);
                let scale = |amt: i32| if starved { amt / 2 } else { amt };
                let new_stamina = stamina.and_then(|s| {
                    if s.current >= s.max {
                        None
                    } else {
                        let regen = scale(stamina_per_tick(posture.0));
                        if regen == 0 {
                            None
                        } else {
                            Some((s.current + regen).min(s.max))
                        }
                    }
                });
                let new_hp = hp.and_then(|h| {
                    if h.hp >= h.max {
                        None
                    } else {
                        let regen = scale(health_per_tick(posture.0));
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

/// One game-hour at 10Hz × 75s/hour. Hunger / Thirst tick at this
/// cadence; matches `mud_clock_tick`'s hour rollover.
const HUNGER_TICK_TICKS: u64 = 750;
/// Threshold at which the player feels the gauge — soft warning.
const HUNGRY_AT: i32 = 24;
const THIRSTY_AT: i32 = 12;
/// Threshold at which actual stamina/HP drain starts. Drain is 1
/// stamina per game-hour; once stamina is at 0, 1 HP per hour
/// (clamped at 1 — starvation never KILLS in v1, just incapacitates).
const STARVING_AT: i32 = 48;
const PARCHED_AT: i32 = 24;

/// Increment Hunger and Thirst once per game-hour, emit threshold
/// crossing messages, and drain stamina/HP when starving / parched.
/// Skips ghosts, frozen players, and offline players (only Online +
/// Player + non-Ghost + non-Frozen tick).
pub fn hunger_thirst_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(HUNGER_TICK_TICKS) {
        return;
    }

    // Snapshot pre-tick state so all mutations and notifications can
    // happen in a single pass without juggling re-borrows.
    let snapshot: Vec<(Entity, i32, i32, i32, i32, i32, i32)> = {
        let mut q = world.query_filtered::<
            (Entity, &Hunger, &Thirst, &Stamina, &Health),
            (With<Player>, With<Online>, Without<Ghost>, Without<Frozen>),
        >();
        q.iter(world)
            .map(|(e, h, t, s, hp)| (e, h.0, t.0, s.current, s.max, hp.hp, hp.max))
            .collect()
    };

    for (entity, old_hunger, old_thirst, stam, _stam_max, hp, _hp_max) in snapshot {
        let new_hunger = old_hunger + 1;
        let new_thirst = old_thirst + 1;
        if let Some(mut h) = world.get_mut::<Hunger>(entity) {
            h.0 = new_hunger;
        }
        if let Some(mut t) = world.get_mut::<Thirst>(entity) {
            t.0 = new_thirst;
        }

        // Threshold-crossing messages — fire once per crossing, not
        // every tick the player is over.
        if old_hunger < HUNGRY_AT && new_hunger >= HUNGRY_AT {
            send_to(world, entity, "You are hungry.\r\n");
        }
        if old_hunger < STARVING_AT && new_hunger >= STARVING_AT {
            send_to(world, entity, "You feel weak from hunger.\r\n");
        }
        if old_thirst < THIRSTY_AT && new_thirst >= THIRSTY_AT {
            send_to(world, entity, "You are thirsty.\r\n");
        }
        if old_thirst < PARCHED_AT && new_thirst >= PARCHED_AT {
            send_to(world, entity, "Your throat is parched!\r\n");
        }

        // Drain when over either threshold. 1 stamina/hour; once
        // stamina is at 0, 1 HP/hour clamped at 1 — survival
        // mechanic, not death.
        if new_hunger >= STARVING_AT || new_thirst >= PARCHED_AT {
            if stam > 0 {
                if let Some(mut s) = world.get_mut::<Stamina>(entity) {
                    s.current = (s.current - 1).max(0);
                }
            } else if hp > 1
                && let Some(mut h) = world.get_mut::<Health>(entity)
            {
                h.hp = (h.hp - 1).max(1);
            }
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
