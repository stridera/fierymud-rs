//! `detect_align` item auras (legacy `print_obj_flags_to_char`): a viewer
//! with the flag sees "(Red Aura)" / "(Gold Aura)" trailing items that
//! bar neutrals plus exactly one of good / evil, wherever item lines are
//! listed.

use bevy_ecs::prelude::*;
use mud_db::enums::{Alignment, ObjectType};
use mud_world::{
    DetectAlign, EquippedSlot, Item, Keywords, Located, Named, ObjectPrototypes, Slot, WorldKey,
};

use super::dispatch;
use super::gmcp_tests::{Fx, fixture, player};
use super::test_support::{Rx, drain, object_proto};

const BANE: i32 = 1;
const DAWN: i32 = 2;
const BOTH: i32 = 3;
const PLAIN: i32 = 4;
const BAG: i32 = 5;

fn world_with_protos() -> (Fx, Entity, Rx) {
    let mut fx = fixture();
    let mut protos = ObjectPrototypes::default();
    for (id, bars) in [
        (BANE, vec![Alignment::Good, Alignment::Neutral]),
        (DAWN, vec![Alignment::Evil, Alignment::Neutral]),
        (
            BOTH,
            vec![Alignment::Good, Alignment::Evil, Alignment::Neutral],
        ),
        (PLAIN, vec![]),
    ] {
        let mut p = object_proto(550, id, ObjectType::Other);
        p.restricted_alignments = bars;
        protos.by_key.insert((550, id), p);
    }
    protos
        .by_key
        .insert((550, BAG), object_proto(550, BAG, ObjectType::Container));
    fx.world.insert_resource(protos);
    let a = fx.a;
    let (p, rx) = player(&mut fx.world, a, "Viewer");
    (fx, p, rx)
}

fn item(fx: &mut Fx, id: i32, name: &str, at: Entity) -> Entity {
    fx.world
        .spawn((
            Item,
            Named { name: name.into() },
            Keywords(vec![name.rsplit(' ').next().unwrap_or("x").to_string()]),
            WorldKey { zone: 550, id },
            Located(at),
        ))
        .id()
}

fn run(fx: &mut Fx, p: Entity, rx: &mut Rx, cmd: &str) -> String {
    let _ = drain(rx);
    dispatch(&mut fx.world, p, cmd);
    strip_ansi(&drain(rx))
}

/// Drop SGR colour sequences so assertions read the plain text.
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
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

fn line<'a>(out: &'a str, needle: &str) -> &'a str {
    out.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no {needle} line: {out}"))
}

#[test]
fn detect_align_viewer_sees_aura_on_anti_good_and_anti_evil_items() {
    let (mut fx, p, mut rx) = world_with_protos();
    let room = fx.a;
    item(&mut fx, BANE, "a dull blade", room);
    item(&mut fx, DAWN, "a bright lance", room);
    item(&mut fx, PLAIN, "a wooden cup", room);
    fx.world.entity_mut(p).insert(DetectAlign);
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(line(&out, "dull blade").contains("(Red Aura)"), "{out}");
    assert!(line(&out, "bright lance").contains("(Gold Aura)"), "{out}");
    assert!(!line(&out, "wooden cup").contains("Aura"), "{out}");
}

#[test]
fn viewer_without_detect_align_sees_no_item_aura() {
    let (mut fx, p, mut rx) = world_with_protos();
    let room = fx.a;
    item(&mut fx, BANE, "a dull blade", room);
    item(&mut fx, BANE, "a dull blade", p);
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(line(&out, "dull blade").contains("dull blade"), "{out}");
    assert!(!out.contains("Aura"), "{out}");
    let inv = run(&mut fx, p, &mut rx, "inventory");
    assert!(line(&inv, "dull blade").contains("dull blade"), "{inv}");
    assert!(!inv.contains("Aura"), "{inv}");
}

#[test]
fn item_barring_both_good_and_evil_gets_no_aura() {
    let (mut fx, p, mut rx) = world_with_protos();
    let room = fx.a;
    item(&mut fx, BOTH, "a grey staff", room);
    fx.world.entity_mut(p).insert(DetectAlign);
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(line(&out, "grey staff").contains("grey staff"), "{out}");
    assert!(!out.contains("Aura"), "{out}");
}

#[test]
fn aura_trails_items_in_inventory_equipment_and_containers() {
    let (mut fx, p, mut rx) = world_with_protos();
    fx.world.entity_mut(p).insert(DetectAlign);
    item(&mut fx, BANE, "a dull blade", p);
    let worn = item(&mut fx, DAWN, "a bright lance", p);
    fx.world.entity_mut(worn).insert(EquippedSlot(Slot::Wield));
    let bag = item(&mut fx, BAG, "a leather bag", p);
    fx.world
        .entity_mut(bag)
        .insert(Keywords(vec!["bag".to_string()]));
    item(&mut fx, DAWN, "a gilded dagger", bag);
    item(&mut fx, PLAIN, "a wooden cup", bag);

    let inv = run(&mut fx, p, &mut rx, "inventory");
    assert!(line(&inv, "dull blade").contains("(Red Aura)"), "{inv}");
    assert!(!line(&inv, "leather bag").contains("Aura"), "{inv}");
    let eq = run(&mut fx, p, &mut rx, "equipment");
    assert!(line(&eq, "bright lance").contains("(Gold Aura)"), "{eq}");
    let inside = run(&mut fx, p, &mut rx, "look in bag");
    assert!(
        line(&inside, "gilded dagger").contains("(Gold Aura)"),
        "{inside}"
    );
    assert!(!line(&inside, "wooden cup").contains("Aura"), "{inside}");
}
