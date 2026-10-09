//! Mob aggro timing (issue #100 follow-up): mobs decide to attack only on
//! the `PULSE_MOBILE` mob AI pulse (legacy `mobile_activity`,
//! mobact.cpp:283), and the engaging mob strikes in that same tick. Walking
//! into a room never engages by itself. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState};
use mud_world::{
    CombatStats, ExitData, Exits, Fighting, Health, Located, Mob, Named, Posture, PostureKind,
    Room, WeatherCatalog,
};

use super::dispatch;
use super::test_support::{Rx, drain, make_aggro_target, player_in};
use crate::TickCount;
use crate::wander::{SCAVENGER_PERIOD_TICKS, aggro_tick};

struct Fx {
    world: World,
    start: Entity,
    den: Entity,
    player: Entity,
    wolf: Entity,
    rx: Rx,
}

fn open_exit(to: Entity) -> ExitData {
    ExitData {
        to: Some(to),
        state: ExitState::Open,
        key: None,
        description: None,
        keywords: Vec::new(),
        is_hidden: false,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    }
}

fn fixture() -> Fx {
    let mut world = World::new();
    world.insert_resource(WeatherCatalog::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(TickCount(1));
    let room = |world: &mut World, name: &str| {
        world
            .spawn((Room, Named { name: name.into() }, Exits::default()))
            .id()
    };
    let start = room(&mut world, "A road");
    let den = room(&mut world, "A den");
    world
        .get_mut::<Exits>(start)
        .unwrap()
        .0
        .insert(Direction::North, open_exit(den));
    world
        .get_mut::<Exits>(den)
        .unwrap()
        .0
        .insert(Direction::South, open_exit(start));
    let wolf = world
        .spawn((
            Mob,
            Named {
                name: "a wolf".into(),
            },
            Located(den),
            CombatStats {
                alignment: -1000,
                ..CombatStats::default()
            },
            Health { hp: 50, max: 50 },
            Posture(PostureKind::Standing),
        ))
        .id();
    let (player, rx) = player_in(&mut world, start);
    make_aggro_target(&mut world, player);
    Fx {
        world,
        start,
        den,
        player,
        wolf,
        rx,
    }
}

fn run_pulse(fx: &mut Fx) {
    fx.world.insert_resource(TickCount(SCAVENGER_PERIOD_TICKS));
    aggro_tick(&mut fx.world);
}

#[test]
fn walking_into_an_aggro_room_takes_no_blow_before_the_pulse() {
    let mut fx = fixture();
    dispatch(&mut fx.world, fx.player, "north");
    let out = drain(&mut fx.rx);
    assert_eq!(
        fx.world.get::<Located>(fx.player).map(|l| l.0),
        Some(fx.den)
    );
    assert!(!out.contains("sees you and attacks"), "{out}");
    assert!(fx.world.get::<Fighting>(fx.wolf).is_none());
    assert!(fx.world.get::<Fighting>(fx.player).is_none());
    // Off-pulse ticks decide nothing either.
    for t in 1..SCAVENGER_PERIOD_TICKS {
        fx.world.insert_resource(TickCount(t));
        aggro_tick(&mut fx.world);
    }
    assert!(fx.world.get::<Fighting>(fx.wolf).is_none());
}

#[test]
fn passing_through_and_leaving_before_the_pulse_is_never_hit() {
    let mut fx = fixture();
    dispatch(&mut fx.world, fx.player, "north");
    dispatch(&mut fx.world, fx.player, "south");
    assert_eq!(
        fx.world.get::<Located>(fx.player).map(|l| l.0),
        Some(fx.start)
    );
    let _ = drain(&mut fx.rx);
    run_pulse(&mut fx);
    assert!(fx.world.get::<Fighting>(fx.wolf).is_none());
    assert!(fx.world.get::<Fighting>(fx.player).is_none());
    assert!(!drain(&mut fx.rx).contains("attacks"));
}

#[test]
fn on_the_pulse_the_mob_engages_and_strikes_in_the_same_tick() {
    let mut fx = fixture();
    dispatch(&mut fx.world, fx.player, "north");
    let _ = drain(&mut fx.rx);
    run_pulse(&mut fx);
    assert_eq!(
        fx.world.get::<Fighting>(fx.wolf).map(|f| f.0),
        Some(fx.player)
    );
    // No combat_tick has run: the blow came with the decision.
    let out = drain(&mut fx.rx);
    assert!(out.contains("sees you and attacks!"), "{out}");
    let swung = out
        .lines()
        .any(|l| l.contains("wolf") && !l.contains("sees you and attacks"));
    assert!(swung, "first blow on engage: {out}");
}
