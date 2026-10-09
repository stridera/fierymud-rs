//! Colour markup in item names survives every listing, with the item tags
//! after a reset (issue #86, #87). Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectFlag, ObjectType};
use mud_world::{
    EquippedSlot, Item, Keywords, Located, Named, ObjectFlags, ObjectPrototypes, Slot, WorldKey,
};

use super::dispatch;
use super::gmcp_tests::{Fx, fixture, player};
use super::test_support::{Rx, drain, object_proto};

const NAME: &str = "<b:yellow>Slippers of the Seer</>";
const OPEN_NAME: &str = "<b:yellow>Slippers of the Seer";

fn setup(name: &str) -> (Fx, Entity, Rx) {
    let mut fx = fixture();
    let mut protos = ObjectPrototypes::default();
    protos
        .by_key
        .insert((551, 1), object_proto(551, 1, ObjectType::Other));
    protos
        .by_key
        .insert((551, 2), object_proto(551, 2, ObjectType::Container));
    fx.world.insert_resource(protos);
    let a = fx.a;
    let (p, rx) = player(&mut fx.world, a, "Viewer");
    let _ = name;
    // Detect Magic, so a magic item carries a "(magic)" tag.
    fx.world.spawn((
        mud_world::EffectInstance {
            kind: 1,
            name: "detect_magic".into(),
            strength: 1,
            remaining_secs: 600,
            source: mud_world::EffectSource::Spell,
            ability_id: None,
        },
        mud_world::AppliedTo(p),
    ));
    (fx, p, rx)
}

fn item(fx: &mut Fx, id: i32, name: &str, at: Entity, tagged: bool) -> Entity {
    let flags = if tagged {
        vec![ObjectFlag::Magic]
    } else {
        vec![]
    };
    fx.world
        .spawn((
            Item,
            Named { name: name.into() },
            Keywords(vec!["slippers".to_string()]),
            WorldKey { zone: 551, id },
            Located(at),
            ObjectFlags(flags),
        ))
        .id()
}

fn run(fx: &mut Fx, p: Entity, rx: &mut Rx, cmd: &str) -> String {
    let _ = drain(rx);
    dispatch(&mut fx.world, p, cmd);
    drain(rx)
}

/// The yellow-bold SGR a `<b:yellow>` open tag renders to, however the
/// renderer spells it (`1;33`, `33;1`, ...): the raw output must carry the
/// escape and no literal markup or legacy `&` code.
fn assert_coloured(out: &str) {
    assert!(out.contains("Slippers of the Seer"), "{out:?}");
    assert!(out.contains('\u{1b}'), "no colour escape: {out:?}");
    assert!(!out.contains("<b:yellow>"), "markup leaked: {out:?}");
    assert!(!out.contains('&'), "legacy code leaked: {out:?}");
}

#[test]
fn coloured_names_render_in_inventory_equipment_room_and_containers() {
    for name in [NAME, OPEN_NAME] {
        for tagged in [false, true] {
            let (mut fx, p, mut rx) = setup(name);
            let room = fx.a;
            item(&mut fx, 1, name, room, tagged);
            item(&mut fx, 1, name, p, tagged);
            let worn = item(&mut fx, 1, name, p, tagged);
            fx.world.entity_mut(worn).insert(EquippedSlot(Slot::Feet));
            let bag = item(&mut fx, 2, "a leather bag", p, false);
            fx.world
                .entity_mut(bag)
                .insert(Keywords(vec!["bag".to_string()]));
            item(&mut fx, 1, name, bag, tagged);
            for cmd in ["look", "inventory", "equipment", "look in bag"] {
                let out = run(&mut fx, p, &mut rx, cmd);
                assert_coloured(&out);
                if tagged {
                    // The tag is its own span: whatever colour the name left
                    // open is reset before "(magic)" starts.
                    let at = out.find("magic").expect("tag");
                    let before = &out[..at];
                    let last_reset = before.rfind("\u{1b}[0m").expect("reset before tag");
                    let name_end = before.find("Slippers of the Seer").unwrap();
                    assert!(last_reset > name_end, "{cmd}: {out:?}");
                }
            }
        }
    }
}
