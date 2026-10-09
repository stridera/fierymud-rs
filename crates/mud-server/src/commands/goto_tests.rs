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

// -- legacy find_target_room privacy checks (act.wizard.cpp:233-238) --------

/// A Builder-role staffer whose character is below `LVL_GOD` (101): the
/// legacy `find_target_room` restrictions apply to them.
fn lowly_builder(w: &mut World, name: &str, room: Entity) -> (Entity, Rx) {
    let (e, rx) = person(w, name, 102, room);
    w.get_mut::<Profile>(e).unwrap().level = 60;
    (e, rx)
}

fn private_room_with_two(w: &mut World, id: i32) -> Entity {
    let r = room(w, id);
    w.entity_mut(r).insert(mud_world::RoomCapacity(2));
    person(w, "Alice", 30, r);
    person(w, "Bob", 30, r);
    r
}

#[test]
fn goto_refuses_a_crowded_private_room_below_lvl_god_only() {
    let mut w = world();
    let (start, private) = (room(&mut w, 2), private_room_with_two(&mut w, 3));
    let (immortal, mut rx) = lowly_builder(&mut w, "Imm", start);
    dispatch(&mut w, immortal, "goto 30:3");
    let out = drain(&mut rx);
    assert!(out.contains("private conversation"), "{out}");
    assert_eq!(where_is(&w, immortal), start);
    // `at` shares the resolver.
    dispatch(&mut w, immortal, "at 30:3 look");
    assert!(drain(&mut rx).contains("private conversation"));
    assert_eq!(where_is(&w, immortal), start);
    // LVL_GOD (101) walks right in.
    let (god, _grx) = person(&mut w, "God", 101, start);
    dispatch(&mut w, god, "goto 30:3");
    assert_eq!(where_is(&w, god), private);
}

#[test]
fn goto_allows_a_private_room_with_one_occupant() {
    let mut w = world();
    let (start, private) = (room(&mut w, 2), room(&mut w, 3));
    w.entity_mut(private).insert(mud_world::RoomCapacity(2));
    person(&mut w, "Alice", 30, private);
    let (immortal, _rx) = lowly_builder(&mut w, "Imm", start);
    dispatch(&mut w, immortal, "goto 30:3");
    assert_eq!(where_is(&w, immortal), private);
}

#[test]
fn goto_refuses_a_house_the_staffer_cannot_enter() {
    let mut w = world();
    let (start, house) = (room(&mut w, 2), room(&mut w, 3));
    w.entity_mut(house).insert(mud_world::HouseRoom {
        house_id: 7,
        local_index: 0,
    });
    let (owner, _orx) = person(&mut w, "Owner", 30, house);
    let (imm, mut rx) = lowly_builder(&mut w, "Imm", start);
    let summary = |guests: Vec<mud_world::HouseGuestEntry>| mud_world::HouseSummary {
        house_id: 7,
        entrance_room: WorldKey { zone: 30, id: 3 },
        return_room: None,
        rooms: vec![],
        exits: vec![],
        items: vec![],
        guests,
    };
    w.entity_mut(owner).insert(summary(vec![]));
    dispatch(&mut w, imm, "goto Owner");
    assert!(drain(&mut rx).contains("no trespassing"));
    assert_eq!(where_is(&w, imm), start);
    // On the guest list: allowed.
    w.entity_mut(owner)
        .insert(summary(vec![mud_world::HouseGuestEntry {
            character_id: "c-Imm".into(),
            can_place: false,
        }]));
    dispatch(&mut w, imm, "goto Owner");
    assert_eq!(where_is(&w, imm), house);
    // LVL_GOD ignores the list.
    w.entity_mut(owner).insert(summary(vec![]));
    let (god, _grx) = person(&mut w, "God", 102, start);
    dispatch(&mut w, god, "goto Owner");
    assert_eq!(where_is(&w, god), house);
}
