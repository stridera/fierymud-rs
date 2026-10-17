//! Round-14 playtest fixes: `consider` and `kill` resolve a shared keyword
//! to the same mob, and (see the other modules) NPC corpses, `&_` and extra
//! description wrapping. Test-only.

use bevy_ecs::prelude::*;
use mud_world::{CombatStats, Fighting, Health, Keywords, Located, Mob, Named};

use super::dispatch;
use super::god_zone_tests::Fx;
use super::test_support::drain;

fn spider(world: &mut World, room: Entity, name: &str) -> Entity {
    world
        .spawn((
            Mob,
            Named { name: name.into() },
            Keywords(vec!["spider".into()]),
            Located(room),
            CombatStats::default(),
            Health { hp: 50, max: 50 },
        ))
        .id()
}

#[test]
fn consider_and_kill_pick_the_same_mob_for_a_shared_keyword() {
    for target in ["spider", "1.spider", "2.spider"] {
        let mut fx = Fx::new();
        let zone = fx.zone(30, false);
        let room = fx.room(zone, 30, 1);
        let (p, mut rx) = fx.person("Tester", 20, room);
        fx.world
            .entity_mut(p)
            .insert((CombatStats::default(), Health { hp: 100, max: 100 }));
        let canopy = spider(&mut fx.world, room, "a giant canopy spider");
        let black = spider(&mut fx.world, room, "the huge black spider");

        dispatch(&mut fx.world, p, &format!("consider {target}"));
        let considered = drain(&mut rx);
        let expect_canopy = considered.contains("giant canopy spider");
        let expect_black = considered.contains("huge black spider");
        assert!(expect_canopy != expect_black, "{target}: {considered}");

        dispatch(&mut fx.world, p, &format!("kill {target}"));
        let attacked = drain(&mut rx);
        let fighting = fx.world.get::<Fighting>(p).map(|f| f.0);
        let (want, other) = if expect_canopy {
            (canopy, black)
        } else {
            (black, canopy)
        };
        assert_eq!(fighting, Some(want), "{target}: {considered} / {attacked}");
        assert_ne!(fighting, Some(other));
    }
}

#[test]
fn indexed_keyword_selects_different_mobs_for_consider_and_kill() {
    let mut fx = Fx::new();
    let zone = fx.zone(30, false);
    let room = fx.room(zone, 30, 1);
    let (p, mut rx) = fx.person("Tester", 20, room);
    fx.world
        .entity_mut(p)
        .insert((CombatStats::default(), Health { hp: 100, max: 100 }));
    spider(&mut fx.world, room, "a giant canopy spider");
    spider(&mut fx.world, room, "the huge black spider");
    dispatch(&mut fx.world, p, "consider 1.spider");
    let first = drain(&mut rx);
    dispatch(&mut fx.world, p, "consider 2.spider");
    let second = drain(&mut rx);
    assert_ne!(first, second);
}

fn longest_line(out: &str) -> usize {
    out.lines()
        .map(|l| crate::layout::text_width(l.trim_end()))
        .max()
        .unwrap_or(0)
}

#[test]
fn room_extra_descriptions_wrap_to_the_players_width() {
    let mut fx = Fx::new();
    let zone = fx.zone(30, false);
    let room = fx.room(zone, 30, 1);
    let (p, mut rx) = fx.person("Tester", 20, room);
    let curtain = "The heavy velvet curtain hangs from a brass rail and sways gently in the \
                   draught, hiding a narrow doorway that few visitors ever think to look for."
        .to_string();
    fx.world.entity_mut(room).insert(mud_world::RoomExtras {
        entries: vec![
            (vec!["curtain".into()], curtain.clone()),
            (
                vec!["sign".into()],
                "North - Arena\nEast - Chapel".to_string(),
            ),
        ],
    });

    dispatch(&mut fx.world, p, "look curtain");
    let out = drain(&mut rx);
    assert!(out.contains("curtain hangs"), "{out}");
    assert!(longest_line(&out) <= 80, "{out}");
    assert!(out.lines().count() >= 3, "{out}");

    // A narrower client gets narrower lines.
    fx.world.entity_mut(p).insert(mud_world::ClientWidth(50));
    dispatch(&mut fx.world, p, "look curtain");
    let out = drain(&mut rx);
    assert!(longest_line(&out) <= 50, "{out}");

    // The builder's own line breaks survive.
    dispatch(&mut fx.world, p, "look sign");
    let out = drain(&mut rx).replace("\r\n", "\n");
    assert!(out.contains("North - Arena\nEast - Chapel"), "{out}");
}
