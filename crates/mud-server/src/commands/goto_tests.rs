//! Staff `goto` (issue #36): `goto home`, no-teleport rooms don't stop staff,
//! name resolution to a mob's room, and legacy poofout / poofin messages.
//! Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{UserRole, effective_rank};
use mud_world::{
    Account, Exits, Follower, Located, Mob, Mounted, Named, NoTeleportRoom, Online, Player, Poofs,
    Profile, RecallPoint, Room, WorldKey, WorldKeyIndex,
};

use super::dispatch;
use super::test_support::{Rx, drain};
use crate::commands::Connection;

fn world() -> World {
    let mut world = World::new();
    world.insert_resource(WorldKeyIndex::default());
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::WeatherCatalog::default());
    world
}

fn room(world: &mut World, id: i32) -> Entity {
    let r = world
        .spawn((
            Room,
            WorldKey { zone: 30, id },
            Named {
                name: format!("Room {id}"),
            },
            Exits::default(),
        ))
        .id();
    world
        .resource_mut::<WorldKeyIndex>()
        .rooms
        .insert((30, id), r);
    r
}

fn person(world: &mut World, name: &str, level: i32, room: Entity) -> (Entity, Rx) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let e = world
        .spawn((
            Player,
            Online,
            Named { name: name.into() },
            Located(room),
            Connection(tx),
            Account {
                user_id: String::new(),
                character_id: format!("c-{name}"),
                role: effective_rank(level, UserRole::Player),
                account_role: UserRole::Player,
                perms: vec![],
            },
            Profile {
                level,
                class_id: None,
                race: "Human".into(),
                experience: 0,
                gender: "neutral".into(),
            },
        ))
        .id();
    (e, rx)
}

fn where_is(world: &World, e: Entity) -> Entity {
    world.get::<Located>(e).unwrap().0
}

#[test]
fn goto_home_goes_to_the_recall_point() {
    let mut w = world();
    let (home, elsewhere) = (room(&mut w, 2), room(&mut w, 3));
    let (god, mut rx) = person(&mut w, "Strider", 104, elsewhere);
    w.entity_mut(god).insert(RecallPoint(home));
    dispatch(&mut w, god, "goto home");
    assert_eq!(where_is(&w, god), home, "{}", drain(&mut rx));
    // `go` is the same command.
    w.entity_mut(god).insert(Located(elsewhere));
    dispatch(&mut w, god, "go HOME");
    assert_eq!(where_is(&w, god), home);
}

#[test]
fn goto_home_without_a_home_room_says_so_and_stays_put() {
    let mut w = world();
    let here = room(&mut w, 1);
    let (god, mut rx) = person(&mut w, "Strider", 104, here);
    dispatch(&mut w, god, "goto home");
    assert_eq!(where_is(&w, god), here);
    assert!(drain(&mut rx).contains("Your home room is invalid."));
}

#[test]
fn goto_into_a_no_teleport_room_works_for_staff() {
    let mut w = world();
    let (here, sealed) = (room(&mut w, 1), room(&mut w, 2));
    w.entity_mut(sealed).insert(NoTeleportRoom);
    let (god, mut rx) = person(&mut w, "Strider", 104, here);
    dispatch(&mut w, god, "goto 2");
    let out = drain(&mut rx);
    assert_eq!(where_is(&w, god), sealed, "{out}");
    assert!(!out.contains("refuses inbound teleports"), "{out}");
}

#[test]
fn goto_a_mob_by_name_lands_in_its_room_even_if_no_teleport() {
    let mut w = world();
    let (here, lair) = (room(&mut w, 1), room(&mut w, 2));
    w.entity_mut(lair).insert(NoTeleportRoom);
    w.spawn((
        Mob,
        Named {
            name: "Puff the dragon".into(),
        },
        Located(lair),
    ));
    let (god, _rx) = person(&mut w, "Strider", 104, here);
    dispatch(&mut w, god, "goto puff");
    assert_eq!(where_is(&w, god), lair);
}

#[test]
fn goto_uses_default_poof_messages_for_both_rooms() {
    let mut w = world();
    let (from, to) = (room(&mut w, 1), room(&mut w, 2));
    let (god, _g) = person(&mut w, "Strider", 104, from);
    let (_, mut left) = person(&mut w, "Left", 10, from);
    let (_, mut met) = person(&mut w, "Met", 10, to);
    dispatch(&mut w, god, "goto 2");
    assert!(
        drain(&mut left).contains("Strider disappears in a puff of smoke."),
        "departure shown in the old room"
    );
    assert!(
        drain(&mut met).contains("Strider appears with an ear-splitting bang."),
        "arrival shown in the new room"
    );
}

#[test]
fn goto_uses_the_staffs_own_poofin_and_poofout() {
    let mut w = world();
    let (from, to) = (room(&mut w, 1), room(&mut w, 2));
    let (god, mut own) = person(&mut w, "Strider", 104, from);
    w.entity_mut(god).insert(Poofs {
        poof_in: Some("$n strides in, trailing sparks.".into()),
        poof_out: Some("$n folds into the dark.".into()),
    });
    let (_, mut left) = person(&mut w, "Left", 10, from);
    let (_, mut met) = person(&mut w, "Met", 10, to);
    dispatch(&mut w, god, "goto 2");
    let l = drain(&mut left);
    let m = drain(&mut met);
    assert!(l.contains("Strider folds into the dark."), "{l}");
    assert!(!l.contains("puff of smoke"), "{l}");
    assert!(m.contains("Strider strides in, trailing sparks."), "{m}");
    // The traveller does not see their own poof, just the new room.
    let own = drain(&mut own);
    assert!(!own.contains("trailing sparks"), "{own}");
    assert!(own.contains("Room 2"), "{own}");
}

#[test]
fn goto_brings_the_staffs_pets_but_not_strangers_or_others_pets() {
    let mut w = world();
    let (from, to) = (room(&mut w, 1), room(&mut w, 2));
    let (god, _g) = person(&mut w, "Strider", 104, from);
    let (other, _o) = person(&mut w, "Other", 10, from);
    let mob = |w: &mut World, name: &str, master: Option<Entity>| {
        let e = w
            .spawn((Mob, Named { name: name.into() }, Located(from)))
            .id();
        if let Some(m) = master {
            w.entity_mut(e).insert(Follower(m));
        }
        e
    };
    let pet = mob(&mut w, "a loyal hound", Some(god));
    let stranger = mob(&mut w, "a bystander", None);
    let others_pet = mob(&mut w, "a cat", Some(other));
    dispatch(&mut w, god, "goto 2");
    assert_eq!(where_is(&w, god), to);
    assert_eq!(where_is(&w, pet), to, "pet follows the staff member");
    assert_eq!(where_is(&w, stranger), from);
    assert_eq!(where_is(&w, others_pet), from);
}

#[test]
fn goto_brings_the_mount_along() {
    let mut w = world();
    let (from, to) = (room(&mut w, 1), room(&mut w, 2));
    let (god, _g) = person(&mut w, "Strider", 104, from);
    let horse = w
        .spawn((
            Mob,
            Named {
                name: "a horse".into(),
            },
            Located(from),
        ))
        .id();
    w.entity_mut(god).insert(Mounted(horse));
    dispatch(&mut w, god, "goto 2");
    assert_eq!(where_is(&w, horse), to);
}

#[test]
fn goto_ends_fights_only_when_the_room_changes() {
    let mut w = world();
    let (here, there) = (room(&mut w, 1), room(&mut w, 2));
    let (god, _g) = person(&mut w, "Strider", 104, here);
    let foe = w
        .spawn((
            Mob,
            Named {
                name: "a wolf".into(),
            },
            Located(here),
        ))
        .id();
    w.entity_mut(god).insert(mud_world::Fighting(foe));
    w.entity_mut(foe).insert(mud_world::Fighting(god));
    // Same room: nothing moves, the fight stands.
    dispatch(&mut w, god, "goto 1");
    assert!(w.get::<mud_world::Fighting>(god).is_some());
    assert!(w.get::<mud_world::Fighting>(foe).is_some());
    // Different room: both directions end.
    dispatch(&mut w, god, "goto 2");
    assert_eq!(where_is(&w, god), there);
    assert!(w.get::<mud_world::Fighting>(god).is_none());
    assert!(w.get::<mud_world::Fighting>(foe).is_none());
}
