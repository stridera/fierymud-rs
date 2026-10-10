//! Room object listing (issue #82): one object per line using the
//! prototype's long description (legacy `list_obj_to_char`,
//! `SHOW_LONG_DESC`), identical objects stacked unless `ExpandObjs` is set,
//! and a looked-at mob's gear listed one piece per line.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, PlayerFlag, Sector};
use mud_world::{
    EquippedSlot, Item, Located, Mob, Named, ObjectPrototypes, PlayerFlags, Room, RoomSector, Slot,
    WorldKey,
};

use super::info::{cmd_examine, cmd_look};
use super::test_support::{Rx, drain, object_proto, player_in};

fn plain(rx: &mut Rx) -> String {
    let raw = drain(rx);
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn setup() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.init_resource::<mud_world::MudClock>();
    let mut protos = ObjectPrototypes::default();
    for (id, name, long) in [
        (1, "a rusty key", "A rusty key lies in the dust."),
        (2, "a copper coin", "A copper coin glints here."),
        (3, "a bent spoon", ""),
    ] {
        let mut p = object_proto(30, id, ObjectType::Other);
        p.name = name.to_string();
        p.room_description = long.to_string();
        protos.by_key.insert((30, id), p);
    }
    world.insert_resource(protos);
    let room = world.spawn((Room, RoomSector(Sector::Structure))).id();
    let (player, rx) = player_in(&mut world, room);
    (world, room, player, rx)
}

fn drop_item(world: &mut World, room: Entity, id: i32, short: &str) -> Entity {
    world
        .spawn((
            Item,
            Named {
                name: short.to_string(),
            },
            WorldKey { zone: 30, id },
            Located(room),
        ))
        .id()
}

#[test]
fn room_objects_list_one_per_line_with_long_descriptions() {
    let (mut world, room, player, mut rx) = setup();
    drop_item(&mut world, room, 1, "a rusty key");
    drop_item(&mut world, room, 3, "a bent spoon");
    cmd_look(&mut world, player, "");
    let out = plain(&mut rx);
    assert!(out.contains("A rusty key lies in the dust.\r\n"), "{out:?}");
    // Empty long description falls back to the short name.
    assert!(out.contains("a bent spoon\r\n"), "{out:?}");
    assert!(!out.contains("On the ground"), "{out:?}");
    assert!(!out.contains(", "), "{out:?}");
}

/// A corpse in the room reads as a sentence, not as a bare noun phrase (#97).
#[test]
fn a_corpse_in_the_room_is_a_full_sentence() {
    let (mut world, room, player, mut rx) = setup();
    for name in ["the corpse of a creeping vine", "the corpse of Bob"] {
        world.spawn((
            Item,
            mud_world::Corpse,
            Named {
                name: name.to_string(),
            },
            Located(room),
        ));
    }
    cmd_look(&mut world, player, "");
    let out = plain(&mut rx);
    assert!(
        out.contains("The corpse of a creeping vine is lying here.\r\n"),
        "{out:?}"
    );
    assert!(
        out.contains("The corpse of Bob is lying here.\r\n"),
        "{out:?}"
    );
    assert!(!out.contains("\r\nthe corpse of"), "{out:?}");
}

#[test]
fn identical_room_objects_stack_unless_expandobjs() {
    let (mut world, room, player, mut rx) = setup();
    for _ in 0..3 {
        drop_item(&mut world, room, 2, "a copper coin");
    }
    cmd_look(&mut world, player, "");
    let out = plain(&mut rx);
    assert_eq!(
        out.matches("A copper coin glints here.").count(),
        1,
        "{out:?}"
    );
    assert!(
        out.contains("(3) A copper coin glints here.\r\n"),
        "{out:?}"
    );

    let mut flags = PlayerFlags::default();
    flags.toggle(PlayerFlag::ExpandObjs);
    world.entity_mut(player).insert(flags);
    cmd_look(&mut world, player, "");
    let out = plain(&mut rx);
    assert_eq!(
        out.matches("A copper coin glints here.\r\n").count(),
        3,
        "{out:?}"
    );
    assert!(!out.contains("(3)"), "{out:?}");
}

#[test]
fn looked_at_mob_gear_is_listed_one_piece_per_line() {
    let (mut world, room, player, mut rx) = setup();
    let guard = world
        .spawn((
            Mob,
            Named {
                name: "a guard".to_string(),
            },
            Located(room),
        ))
        .id();
    for (name, slot) in [("a pike", Slot::Wield), ("an iron helm", Slot::Head)] {
        world.spawn((
            Item,
            Named {
                name: name.to_string(),
            },
            Located(guard),
            EquippedSlot(slot),
        ));
    }
    cmd_examine(&mut world, player, "guard");
    let out = plain(&mut rx);
    assert!(out.contains("is using:\r\n"), "{out:?}");
    assert!(out.contains(": an iron helm\r\n"), "{out:?}");
    assert!(out.contains(": a pike\r\n"), "{out:?}");
    assert!(!out.contains("Equipped:"), "{out:?}");
}
