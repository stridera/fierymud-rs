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
    world
        .entity_mut(far)
        .insert((Named { name: "Zzfar".into() }, mud_world::Online));
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
