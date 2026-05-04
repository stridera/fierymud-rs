use bevy_ecs::prelude::*;
use mud_world::{
    Drunkenness, Fighting, Frozen, Ghost, Health, Hunger, LightFuel, Lit, Located, Mob, Named,
    Online, Player, Posture, PostureKind, Stamina, Thirst,
};

use crate::TickCount;
use crate::commands::{broadcast_room_except_rendered, send_to};

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

    // Resolve the survival thresholds once before the query
    // iterator borrows World — same numbers feed every player.
    // `get_resource` (vs `resource`) so tests that build a fresh
    // World without the GameConfig loader pass don't panic;
    // production always has the resource installed by the loader.
    let (starving_at, parched_at) = world
        .get_resource::<mud_world::RuntimeConfig>()
        .map_or((DEFAULT_STARVING_AT, DEFAULT_PARCHED_AT), |cfg| {
            (
                cfg.get_i32("survival", "starving_at", DEFAULT_STARVING_AT),
                cfg.get_i32("survival", "parched_at", DEFAULT_PARCHED_AT),
            )
        });

    // Snapshot the (entity, new_stamina, new_hp) tuples while no mutable
    // borrows are live, then apply. Pattern matches combat_tick / effects_tick.
    // Starving / parched players regen at half rate (rounded down) — they
    // can still slowly recover, but the hunger drain on top makes net
    // progress glacial. Mirrors the stat-survival contract in
    // hunger_thirst_tick.
    // Ghost players don't regen — they're dead. `release` restores
    // hp to max in one shot when the spirit returns to the body;
    // gradually healing a corpse over time would be wrong both
    // mechanically (`release` becomes pointless) and thematically.
    let updates: Vec<(Entity, Option<i32>, Option<i32>)> = {
        let mut q = world.query_filtered::<
            (Entity, Option<&Stamina>, Option<&Health>, &Posture, Option<&Hunger>, Option<&Thirst>),
            (With<Online>, Without<Fighting>, Without<Ghost>),
        >();
        q.iter(world)
            .map(|(e, stamina, hp, posture, hunger, thirst)| {
                let starved = hunger.is_some_and(|h| h.0 >= starving_at)
                    || thirst.is_some_and(|t| t.0 >= parched_at);
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
/// Default thresholds for the soft "you're hungry/thirsty" warning.
/// Live values come from `survival.hungry_at` / `survival.thirsty_at`
/// in `GameConfig`; these are the call-site fallbacks. Hours of
/// game time, since `Hunger.0` and `Thirst.0` are game-hours since
/// last meal/drink.
const DEFAULT_HUNGRY_AT: i32 = 24;
const DEFAULT_THIRSTY_AT: i32 = 12;
/// Default thresholds at which actual stamina/HP drain starts.
/// Live values: `survival.starving_at` / `survival.parched_at`.
/// Drain is 1 stamina per game-hour; once stamina is at 0,
/// 1 HP per hour clamped at 1 — starvation never KILLS in v1,
/// just incapacitates.
const DEFAULT_STARVING_AT: i32 = 48;
const DEFAULT_PARCHED_AT: i32 = 24;

/// Increment Hunger and Thirst once per game-hour, emit threshold
/// crossing messages, and drain stamina/HP when starving / parched.
/// Skips ghosts, frozen players, and offline players (only Online +
/// Player + non-Ghost + non-Frozen tick).
pub fn hunger_thirst_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(HUNGER_TICK_TICKS) {
        return;
    }

    // Resolve the four thresholds once per tick — every player in
    // the snapshot uses the same values. `get_resource` so test
    // worlds without the GameConfig loader pass fall through to
    // the legacy defaults instead of panicking.
    let (hungry_at, thirsty_at, starving_at, parched_at) = world
        .get_resource::<mud_world::RuntimeConfig>()
        .map_or(
            (
                DEFAULT_HUNGRY_AT,
                DEFAULT_THIRSTY_AT,
                DEFAULT_STARVING_AT,
                DEFAULT_PARCHED_AT,
            ),
            |cfg| {
                (
                    cfg.get_i32("survival", "hungry_at", DEFAULT_HUNGRY_AT),
                    cfg.get_i32("survival", "thirsty_at", DEFAULT_THIRSTY_AT),
                    cfg.get_i32("survival", "starving_at", DEFAULT_STARVING_AT),
                    cfg.get_i32("survival", "parched_at", DEFAULT_PARCHED_AT),
                )
            },
        );

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
        if old_hunger < hungry_at && new_hunger >= hungry_at {
            send_to(world, entity, "You are hungry.\r\n");
        }
        if old_hunger < starving_at && new_hunger >= starving_at {
            send_to(world, entity, "You feel weak from hunger.\r\n");
        }
        if old_thirst < thirsty_at && new_thirst >= thirsty_at {
            send_to(world, entity, "You are thirsty.\r\n");
        }
        if old_thirst < parched_at && new_thirst >= parched_at {
            send_to(world, entity, "Your throat is parched!\r\n");
        }

        // Drain when over either threshold. 1 stamina/hour; once
        // stamina is at 0, 1 HP/hour clamped at 1 — survival
        // mechanic, not death.
        if new_hunger >= starving_at || new_thirst >= parched_at {
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

/// Decrement fuel on Lit items once per game-hour. When `remaining`
/// hits 0 the `Lit` marker is removed and a "the X gutters out"
/// line is broadcast to the holding actor's room (or the room
/// directly if the item is on the floor). `remaining < 0` means
/// infinite — eternal flames are skipped entirely.
pub fn light_fuel_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(750) {
        return;
    }
    // Snapshot lit items + their fuel + their location chain.
    // We resolve the carrier's room (if applicable) up front so the
    // gutter-out line lands at the right address.
    let snapshot: Vec<(Entity, i32, Entity, String)> = {
        let mut q = world.query_filtered::<(Entity, &LightFuel, &Located, &Named), With<Lit>>();
        q.iter(world)
            .filter(|(_, fuel, _, _)| fuel.remaining > 0)
            .map(|(e, fuel, l, n)| (e, fuel.remaining, l.0, n.name.clone()))
            .collect()
    };
    for (item, remaining, parent, item_name) in snapshot {
        // Decrement first.
        let new_remaining = remaining - 1;
        if let Some(mut f) = world.get_mut::<LightFuel>(item) {
            f.remaining = new_remaining;
        }
        if new_remaining > 0 {
            continue;
        }
        // Out of fuel — clear Lit and announce.
        if let Ok(mut em) = world.get_entity_mut(item) {
            em.remove::<Lit>();
        }
        // Resolve the eventual room for broadcast. If `parent` is a
        // Player or Mob, fall through to its Located room; otherwise
        // it IS a room (or some non-actor container).
        let room = if world.get::<Player>(parent).is_some() || world.get::<Mob>(parent).is_some() {
            world.get::<Located>(parent).map_or(parent, |l| l.0)
        } else {
            parent
        };
        let line = format!("{item_name} gutters and goes out.\r\n");
        // Player carriers see a personal line; everyone else in the
        // room sees the third-person broadcast.
        if world.get::<Player>(parent).is_some() {
            send_to(world, parent, format!("\r\n{line}"));
            broadcast_room_except_rendered(
                world,
                room,
                &[parent],
                &format!("{item_name} carried by someone here gutters and goes out.\r\n"),
            );
        } else {
            broadcast_room_except_rendered(world, room, &[], &line);
        }
    }
}

/// Drunkenness decay period: 300 ticks @ 10 Hz = 30 seconds per
/// point of sobering up. A worst-case 100-drunk player thus
/// sobers up over 50 minutes of real time.
const DRUNKENNESS_PERIOD_TICKS: u64 = 300;

/// Tick down `Drunkenness` for online players once per
/// `DRUNKENNESS_PERIOD_TICKS`. Threshold crossings emit a flavor
/// line so the player knows when they're regaining clarity.
/// When the counter reaches 0 the component is removed so the
/// idle case stops occupying a row in queries.
pub fn drunkenness_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(DRUNKENNESS_PERIOD_TICKS) {
        return;
    }
    let snapshot: Vec<(Entity, i32)> = {
        let mut q = world
            .query_filtered::<(Entity, &Drunkenness), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(_, d)| d.0 > 0)
            .map(|(e, d)| (e, d.0))
            .collect()
    };
    for (entity, old) in snapshot {
        let new = old - 1;
        // Threshold crossings (going down).
        if old >= 80 && new < 80 {
            send_to(world, entity, "The room steadies — the worst is passing.\r\n");
        }
        if old >= 40 && new < 40 {
            send_to(world, entity, "Your head clears a little.\r\n");
        }
        if new <= 0 {
            send_to(world, entity, "You feel sober again.\r\n");
            if let Ok(mut e) = world.get_entity_mut(entity) {
                e.remove::<Drunkenness>();
            }
        } else if let Some(mut d) = world.get_mut::<Drunkenness>(entity) {
            d.0 = new;
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
    fn starving_player_regens_at_half_rate() {
        // Once Hunger crosses the starving threshold, regen halves
        // (rounded down). Sleeping baseline is +8 stamina / +4 hp;
        // halved → +4 / +2. Hunger value uses the default 48-game-
        // hour threshold inlined as DEFAULT_STARVING_AT.
        let mut world = World::new();
        let p = make_player(&mut world, 50, 100, 30, 50, PostureKind::Sleeping);
        world
            .get_entity_mut(p)
            .unwrap()
            .insert(Hunger(DEFAULT_STARVING_AT));
        run_regen_tick(&mut world);
        assert_eq!(
            world.get::<Stamina>(p).unwrap().current,
            34,
            "starving halves stamina regen (8 → 4)"
        );
        assert_eq!(
            world.get::<Health>(p).unwrap().hp,
            52,
            "starving halves hp regen (4 → 2)"
        );
    }

    #[test]
    fn parched_player_regens_at_half_rate() {
        // Same shape as starving, but driven by Thirst. Either
        // axis triggers the half-rate scale — they don't stack
        // for double-halving.
        let mut world = World::new();
        let p = make_player(&mut world, 50, 100, 30, 50, PostureKind::Sleeping);
        world
            .get_entity_mut(p)
            .unwrap()
            .insert(Thirst(DEFAULT_PARCHED_AT));
        run_regen_tick(&mut world);
        assert_eq!(world.get::<Stamina>(p).unwrap().current, 34);
        assert_eq!(world.get::<Health>(p).unwrap().hp, 52);
    }

    #[test]
    fn ghost_players_skip_regen() {
        // Ghosts are dead — they don't slowly heal back to max.
        // release is the only path back from Ghost and it sets
        // hp = max in one shot. If regen ran for ghosts, the
        // release transition becomes pointless and "I'm dead but
        // recovering" reads as a contradiction.
        let mut world = World::new();
        let p = make_player(&mut world, 0, 100, 0, 50, PostureKind::Sleeping);
        world
            .get_entity_mut(p)
            .unwrap()
            .insert(Ghost);
        run_regen_tick(&mut world);
        assert_eq!(
            world.get::<Health>(p).unwrap().hp,
            0,
            "ghost HP doesn't regen"
        );
        assert_eq!(
            world.get::<Stamina>(p).unwrap().current,
            0,
            "ghost stamina doesn't regen"
        );
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
