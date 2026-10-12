//! Playtest 2026-10-12, round 7 fixes. Test-only.

use bevy_ecs::prelude::*;
use mud_world::{Exits, Item, Keywords, Located, Named, Room};

use super::test_support::{Rx, drain, player_in};

fn quiet_room(world: &mut World) -> Entity {
    world
        .spawn((
            Room,
            Named {
                name: "A quiet hall".into(),
            },
            Exits::default(),
        ))
        .id()
}

// ---- #55: `pour <container> out` ----

#[test]
fn pour_container_out_empties_it_onto_the_ground() {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.init_resource::<mud_world::LiquidCatalog>();
    let r = quiet_room(&mut world);
    let (p, mut rx): (Entity, Rx) = player_in(&mut world, r);
    let skin = world
        .spawn((
            Item,
            Named {
                name: "a leather water skin".into(),
            },
            Keywords(vec!["skin".into()]),
            Located(p),
            mud_world::LiquidContainer {
                liquid: "WATER".into(),
                capacity: 10,
                remaining: 10,
                poisoned: false,
            },
        ))
        .id();
    super::info::cmd_pour(&mut world, p, "skin out");
    let out = drain(&mut rx);
    assert!(out.contains("onto the ground"), "{out}");
    assert!(!out.contains("aren't carrying"), "{out}");
    assert_eq!(
        world
            .get::<mud_world::LiquidContainer>(skin)
            .unwrap()
            .remaining,
        0
    );
}

// ---- #43: `stat` forms and cross-room stat ----

fn stat_world() -> (World, Entity, Rx, Entity) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    let here = quiet_room(&mut world);
    let there = quiet_room(&mut world);
    let (staff, rx) = player_in(&mut world, here);
    world.entity_mut(staff).insert(Named {
        name: "Gandalf".into(),
    });
    let (far, _far_rx) = player_in(&mut world, there);
    world.entity_mut(far).insert((
        Named {
            name: "Zzfar".into(),
        },
        mud_world::Online,
    ));
    world.spawn((
        mud_world::Mob,
        Named {
            name: "a brown fox".into(),
        },
        Keywords(vec!["fox".into()]),
        Located(there),
    ));
    (world, staff, rx, far)
}

#[test]
fn stat_forms_find_players_and_mobs_in_other_rooms() {
    let (mut world, staff, mut rx, _far) = stat_world();
    for cmd in [
        "stat zzfar",
        "stat player zzfar",
        "stat pl zzfar",
        "stat file zzfar",
    ] {
        super::admin_inspect::cmd_stat(&mut world, staff, &cmd["stat ".len()..]);
        let out = drain(&mut rx);
        assert!(out.contains("Zzfar"), "{cmd}: {out}");
    }
    super::admin_inspect::cmd_stat(&mut world, staff, "mobile fox");
    let out = drain(&mut rx);
    assert!(out.contains("brown fox"), "{out}");
    super::admin_inspect::cmd_stat(&mut world, staff, "mob fox");
    assert!(drain(&mut rx).contains("brown fox"));
}

#[test]
fn stat_type_words_filter_and_report_misses() {
    let (mut world, staff, mut rx, _far) = stat_world();
    // `stat mobile <player>` and `stat player <mob>` find nothing.
    super::admin_inspect::cmd_stat(&mut world, staff, "mobile zzfar");
    assert!(drain(&mut rx).contains("No such mobile around."));
    super::admin_inspect::cmd_stat(&mut world, staff, "player fox");
    assert!(drain(&mut rx).contains("No such player around."));
    super::admin_inspect::cmd_stat(&mut world, staff, "object zzfar");
    assert!(drain(&mut rx).contains("No such object around."));
    super::admin_inspect::cmd_stat(&mut world, staff, "player");
    assert!(drain(&mut rx).contains("Stats on which player?"));
    super::admin_inspect::cmd_stat(&mut world, staff, "nosuchthing");
    assert!(drain(&mut rx).contains("No 'nosuchthing' here."));
}

// ---- #9 / #10: repeat follow, inviting yourself ----

fn two_players() -> (World, Entity, Rx, Entity, Rx) {
    let mut world = World::new();
    let r = quiet_room(&mut world);
    let (a, a_rx) = player_in(&mut world, r);
    world.entity_mut(a).insert(Named {
        name: "Aria".into(),
    });
    let (b, b_rx) = player_in(&mut world, r);
    world.entity_mut(b).insert(Named {
        name: "Bram".into(),
    });
    (world, a, a_rx, b, b_rx)
}

#[test]
fn following_the_same_person_twice_says_so_and_does_not_spam_the_leader() {
    let (mut world, aria, mut a_rx, bram, mut b_rx) = two_players();
    super::info::cmd_follow(&mut world, aria, "bram");
    assert!(drain(&mut a_rx).contains("You start following Bram."));
    assert!(drain(&mut b_rx).contains("Aria starts following you."));
    super::info::cmd_follow(&mut world, aria, "bram");
    let out = drain(&mut a_rx);
    assert!(out.contains("You are already following Bram."), "{out}");
    assert!(!out.contains("You start following"), "{out}");
    assert!(drain(&mut b_rx).is_empty(), "the leader hears nothing");
    assert_eq!(
        world.get::<mud_world::Follower>(aria).map(|f| f.0),
        Some(bram)
    );
}

#[test]
fn inviting_yourself_is_refused_by_name() {
    let (mut world, aria, mut a_rx, _bram, _b_rx) = two_players();
    for arg in ["aria", "me", "self"] {
        super::info::cmd_invite(&mut world, aria, arg);
        let out = drain(&mut a_rx);
        assert!(out.contains("You can't invite yourself."), "{arg}: {out}");
    }
}
