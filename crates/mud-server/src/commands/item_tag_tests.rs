//! Legacy `print_obj_flags_to_char` item tags: floating, illuminated,
//! invisible, hidden, magic, glowing, humming, poisoned (plus the
//! detect-align aura covered in `item_aura_tests`), hovering, and the
//! `CAN_SEE_OBJ` invisibility / hiddenness filter, wherever item lines
//! are listed.

use bevy_ecs::prelude::*;
use mud_db::enums::{Alignment, ObjectFlag, ObjectType, PlayerFlag, Sector, UserRole};
use mud_world::{
    Account, AppliedTo, DetectAlign, DetectInvis, EffectInstance, EffectSource, EquippedSlot,
    Hiddenness, Item, Keywords, LiquidContainer, Lit, Located, Named, ObjectFlags,
    ObjectPrototypes, Perception, PlayerFlags, RoomSector, Slot, WorldKey,
};

use super::dispatch;
use super::gmcp_tests::{Fx, fixture, player};
use super::test_support::{Rx, drain, object_proto};

const PLAIN: i32 = 1;
const BAG: i32 = 2;
const BANE: i32 = 3;
const PIE: i32 = 4;

fn setup() -> (Fx, Entity, Rx) {
    let mut fx = fixture();
    let mut protos = ObjectPrototypes::default();
    protos
        .by_key
        .insert((551, PLAIN), object_proto(551, PLAIN, ObjectType::Other));
    protos
        .by_key
        .insert((551, BAG), object_proto(551, BAG, ObjectType::Container));
    let mut bane = object_proto(551, BANE, ObjectType::Other);
    bane.restricted_alignments = vec![Alignment::Good, Alignment::Neutral];
    protos.by_key.insert((551, BANE), bane);
    let mut pie = object_proto(551, PIE, ObjectType::Food);
    pie.food_poisoned = true;
    protos.by_key.insert((551, PIE), pie);
    fx.world.insert_resource(protos);
    let a = fx.a;
    let (p, rx) = player(&mut fx.world, a, "Viewer");
    (fx, p, rx)
}

fn item(fx: &mut Fx, id: i32, name: &str, at: Entity, flags: &[ObjectFlag]) -> Entity {
    fx.world
        .spawn((
            Item,
            Named { name: name.into() },
            Keywords(vec![name.rsplit(' ').next().unwrap_or("x").to_string()]),
            WorldKey { zone: 551, id },
            Located(at),
            ObjectFlags(flags.to_vec()),
        ))
        .id()
}

fn add_effect(fx: &mut Fx, on: Entity, name: &str) {
    fx.world.spawn((
        EffectInstance {
            kind: 1,
            name: name.to_string(),
            strength: 1,
            remaining_secs: 600,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(on),
    ));
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

fn has_line(out: &str, needle: &str) -> bool {
    out.lines().any(|l| l.contains(needle))
}

/// Room, inventory and container listings all trail the same tags.
fn listings(fx: &mut Fx, p: Entity, rx: &mut Rx, needle: &str) -> [String; 3] {
    [
        line(&run(fx, p, rx, "look"), needle).to_string(),
        line(&run(fx, p, rx, "inventory"), needle).to_string(),
        line(&run(fx, p, rx, "look in bag"), needle).to_string(),
    ]
}

/// A room item, a carried item and a bagged item sharing `name`/`flags`.
fn trio(fx: &mut Fx, p: Entity, name: &str, flags: &[ObjectFlag]) -> [Entity; 3] {
    let room = fx.a;
    let bag = item(fx, BAG, "a leather bag", p, &[]);
    fx.world
        .entity_mut(bag)
        .insert(Keywords(vec!["bag".to_string()]));
    [
        item(fx, PLAIN, name, room, flags),
        item(fx, PLAIN, name, p, flags),
        item(fx, PLAIN, name, bag, flags),
    ]
}

#[test]
fn glowing_tag_shows_to_every_viewer() {
    let (mut fx, p, mut rx) = setup();
    trio(&mut fx, p, "a pale orb", &[ObjectFlag::Glow]);
    for l in listings(&mut fx, p, &mut rx, "pale orb") {
        assert!(l.contains("(glowing)"), "{l}");
    }
}

#[test]
fn humming_tag_shows_to_every_viewer() {
    let (mut fx, p, mut rx) = setup();
    trio(&mut fx, p, "a thrumming rod", &[ObjectFlag::Hum]);
    for l in listings(&mut fx, p, &mut rx, "thrumming rod") {
        assert!(l.contains("(humming)"), "{l}");
    }
}

#[test]
fn magic_tag_needs_detect_magic() {
    let (mut fx, p, mut rx) = setup();
    trio(&mut fx, p, "a runed wand", &[ObjectFlag::Magic]);
    for l in listings(&mut fx, p, &mut rx, "runed wand") {
        assert!(!l.contains("(magic)"), "{l}");
    }
    add_effect(&mut fx, p, "detect_magic");
    for l in listings(&mut fx, p, &mut rx, "runed wand") {
        assert!(l.contains("(magic)"), "{l}");
    }
}

#[test]
fn non_magic_item_never_gets_magic_tag() {
    let (mut fx, p, mut rx) = setup();
    trio(&mut fx, p, "a wooden cup", &[]);
    add_effect(&mut fx, p, "detect_magic");
    for l in listings(&mut fx, p, &mut rx, "wooden cup") {
        assert!(!l.contains('('), "{l}");
    }
}

#[test]
fn poisoned_tag_needs_detect_poison_and_a_poisoned_container() {
    let (mut fx, p, mut rx) = setup();
    let items = trio(&mut fx, p, "a murky flask", &[]);
    for e in items {
        fx.world.entity_mut(e).insert(LiquidContainer {
            liquid: "WATER".into(),
            capacity: 5,
            remaining: 5,
            poisoned: true,
        });
    }
    for l in listings(&mut fx, p, &mut rx, "murky flask") {
        assert!(!l.contains("(poisoned)"), "{l}");
    }
    add_effect(&mut fx, p, "detect_poison");
    for l in listings(&mut fx, p, &mut rx, "murky flask") {
        assert!(l.contains("(poisoned)"), "{l}");
    }
    for e in items {
        fx.world.get_mut::<LiquidContainer>(e).unwrap().poisoned = false;
    }
    for l in listings(&mut fx, p, &mut rx, "murky flask") {
        assert!(!l.contains("(poisoned)"), "{l}");
    }
}

#[test]
fn illuminated_tag_marks_a_lit_light() {
    let (mut fx, p, mut rx) = setup();
    let items = trio(&mut fx, p, "a brass lantern", &[]);
    for l in listings(&mut fx, p, &mut rx, "brass lantern") {
        assert!(!l.contains("illuminated"), "{l}");
    }
    for e in items {
        fx.world.entity_mut(e).insert(Lit);
    }
    for l in listings(&mut fx, p, &mut rx, "brass lantern") {
        assert!(l.contains("(illuminated)"), "{l}");
    }
}

#[test]
fn floating_tag_only_on_the_floor_of_a_water_room() {
    let (mut fx, p, mut rx) = setup();
    let room = fx.a;
    trio(&mut fx, p, "a drifting log", &[]);
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(!line(&out, "drifting log").contains("floating"), "{out}");
    fx.world
        .entity_mut(room)
        .insert(RoomSector(Sector::Shallows));
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(line(&out, "drifting log").contains("(floating)"), "{out}");
    // Carried and bagged items have no room, so they never float.
    let inv = run(&mut fx, p, &mut rx, "inventory");
    assert!(!line(&inv, "drifting log").contains("floating"), "{inv}");
    let inside = run(&mut fx, p, &mut rx, "look in bag");
    assert!(
        !line(&inside, "drifting log").contains("floating"),
        "{inside}"
    );
    fx.world.entity_mut(room).insert(RoomSector(Sector::Water));
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(line(&out, "drifting log").contains("(floating)"), "{out}");
    // Underwater rooms are dark; check the tag directly.
    fx.world
        .entity_mut(room)
        .insert(RoomSector(Sector::Underwater));
    let log = fx
        .world
        .query_filtered::<(Entity, &Located), With<Item>>()
        .iter(&fx.world)
        .find(|(_, l)| l.0 == room)
        .map(|(e, _)| e)
        .unwrap();
    let tags = super::senses::item_tags(&fx.world, p, log);
    assert_eq!(tags.len(), 1, "{tags:?}");
    assert!(tags[0].contains("floating"), "{tags:?}");
    fx.world.entity_mut(room).insert(RoomSector(Sector::Beach));
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(!line(&out, "drifting log").contains("floating"), "{out}");
}

#[test]
fn invisible_items_are_hidden_without_detect_invis() {
    let (mut fx, p, mut rx) = setup();
    trio(&mut fx, p, "a shimmer blade", &[ObjectFlag::Invisible]);
    let room = fx.a;
    item(&mut fx, PLAIN, "a wooden cup", room, &[]);
    let worn = item(
        &mut fx,
        PLAIN,
        "a shimmer ring",
        p,
        &[ObjectFlag::Invisible],
    );
    fx.world.entity_mut(worn).insert(EquippedSlot(Slot::Wield));

    let out = run(&mut fx, p, &mut rx, "look");
    assert!(has_line(&out, "wooden cup"), "{out}");
    assert!(!out.contains("shimmer"), "{out}");
    let inv = run(&mut fx, p, &mut rx, "inventory");
    assert!(!inv.contains("shimmer"), "{inv}");
    let inside = run(&mut fx, p, &mut rx, "look in bag");
    assert!(!inside.contains("shimmer"), "{inside}");
    let eq = run(&mut fx, p, &mut rx, "equipment");
    assert!(!eq.contains("shimmer"), "{eq}");
    assert!(eq.contains("Something."), "{eq}");
    let look_at = run(&mut fx, p, &mut rx, "look blade");
    assert!(!look_at.contains("shimmer"), "{look_at}");
}

#[test]
fn invisible_items_show_tagged_to_detect_invis_viewers() {
    let (mut fx, p, mut rx) = setup();
    trio(&mut fx, p, "a shimmer blade", &[ObjectFlag::Invisible]);
    let worn = item(
        &mut fx,
        PLAIN,
        "a shimmer ring",
        p,
        &[ObjectFlag::Invisible],
    );
    fx.world.entity_mut(worn).insert(EquippedSlot(Slot::Wield));
    fx.world.entity_mut(p).insert(DetectInvis);
    for l in listings(&mut fx, p, &mut rx, "shimmer blade") {
        assert!(l.contains("(invisible)"), "{l}");
    }
    let eq = run(&mut fx, p, &mut rx, "equipment");
    assert!(line(&eq, "shimmer ring").contains("(invisible)"), "{eq}");
    assert!(!eq.contains("Something."), "{eq}");
}

#[test]
fn tags_trail_in_legacy_order() {
    let (mut fx, p, mut rx) = setup();
    let room = fx.a;
    fx.world
        .entity_mut(room)
        .insert(RoomSector(Sector::Shallows));
    let e = item(
        &mut fx,
        BANE,
        "a strange idol",
        room,
        &[
            ObjectFlag::Hum,
            ObjectFlag::Glow,
            ObjectFlag::Magic,
            ObjectFlag::Invisible,
            ObjectFlag::NoFall,
        ],
    );
    fx.world.entity_mut(e).insert((
        Lit,
        Hiddenness(40),
        LiquidContainer {
            liquid: "WATER".into(),
            capacity: 1,
            remaining: 1,
            poisoned: true,
        },
    ));
    fx.world
        .entity_mut(p)
        .insert((DetectInvis, DetectAlign, Perception(500)));
    add_effect(&mut fx, p, "detect_magic");
    add_effect(&mut fx, p, "detect_poison");
    let out = run(&mut fx, p, &mut rx, "look");
    let l = line(&out, "strange idol");
    let tags = [
        "(floating)",
        "(illuminated)",
        "(invisible)",
        "(hidden)",
        "(magic)",
        "(glowing)",
        "(humming)",
        "(poisoned)",
        "(Red Aura)",
        "(hovering)",
    ];
    let mut last = 0;
    for t in tags {
        let at = l.find(t).unwrap_or_else(|| panic!("{t} missing: {l}"));
        assert!(at >= last, "{t} out of order: {l}");
        last = at;
    }
}

#[test]
fn hovering_tag_marks_no_fall_items() {
    let (mut fx, p, mut rx) = setup();
    trio(&mut fx, p, "a drifting spark", &[ObjectFlag::NoFall]);
    for l in listings(&mut fx, p, &mut rx, "drifting spark") {
        assert!(l.contains("(hovering)"), "{l}");
    }
    let room = fx.a;
    item(&mut fx, PLAIN, "a fallen leaf", room, &[ObjectFlag::Float]);
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(!line(&out, "fallen leaf").contains("hovering"), "{out}");
}

#[test]
fn poisoned_food_is_tagged_to_detect_poison() {
    let (mut fx, p, mut rx) = setup();
    let room = fx.a;
    item(&mut fx, PIE, "a grey pie", room, &[]);
    item(&mut fx, PLAIN, "a fresh pear", room, &[]);
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(!line(&out, "grey pie").contains("(poisoned)"), "{out}");
    add_effect(&mut fx, p, "detect_poison");
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(line(&out, "grey pie").contains("(poisoned)"), "{out}");
    assert!(!line(&out, "fresh pear").contains("(poisoned)"), "{out}");
}

fn hidden_cache(fx: &mut Fx, hid: i32) -> Entity {
    let room = fx.a;
    let e = item(fx, PLAIN, "a mossy cache", room, &[]);
    fx.world.entity_mut(e).insert(Hiddenness(hid));
    e
}

#[test]
fn hidden_items_are_unseen_until_perception_reaches_them() {
    let (mut fx, p, mut rx) = setup();
    let cache = hidden_cache(&mut fx, 300);
    let room = fx.a;
    item(&mut fx, PLAIN, "a wooden cup", room, &[]);
    for perception in [0, 299] {
        fx.world.entity_mut(p).insert(Perception(perception));
        let out = run(&mut fx, p, &mut rx, "look");
        assert!(has_line(&out, "wooden cup"), "{out}");
        assert!(!out.contains("mossy cache"), "{out}");
    }
    // Perception at (not below) the hiddenness sees it, tagged.
    fx.world.entity_mut(p).insert(Perception(300));
    let out = run(&mut fx, p, &mut rx, "look");
    let l = line(&out, "mossy cache");
    assert!(l.contains("(hidden)") && !l.contains("(h300)"), "{l}");
    // Holylight sees it whatever the perception.
    fx.world.entity_mut(p).insert(Perception(0));
    fx.world
        .entity_mut(p)
        .insert(PlayerFlags(vec![PlayerFlag::HolyLight]));
    assert!(super::senses::item_visible_to(&fx.world, p, cache));
}

#[test]
fn staff_see_the_hiddenness_number() {
    let (mut fx, p, mut rx) = setup();
    hidden_cache(&mut fx, 300);
    fx.world.entity_mut(p).insert((
        Perception(1000),
        Account {
            user_id: String::new(),
            character_id: "g".into(),
            role: UserRole::Immortal,
            account_role: UserRole::Immortal,
            perms: vec![],
        },
    ));
    let out = run(&mut fx, p, &mut rx, "look");
    let l = line(&out, "mossy cache");
    assert!(l.contains("(h300)") && !l.contains("(hidden)"), "{l}");
}

#[test]
fn a_hidden_item_cannot_be_taken_until_found_and_pickup_clears_it() {
    let (mut fx, p, mut rx) = setup();
    let cache = hidden_cache(&mut fx, 300);
    let out = run(&mut fx, p, &mut rx, "get cache");
    assert!(out.contains("You don't see 'cache' here."), "{out}");
    let out = run(&mut fx, p, &mut rx, "get all");
    assert!(!out.contains("mossy cache"), "{out}");
    assert_eq!(fx.world.get::<Located>(cache).map(|l| l.0), Some(fx.a));
    // Seen but still hidden (perception 300): taking it ends the hiding.
    fx.world.entity_mut(p).insert(Perception(300));
    let out = run(&mut fx, p, &mut rx, "get cache");
    assert!(out.contains("mossy cache"), "{out}");
    assert_eq!(fx.world.get::<Located>(cache).map(|l| l.0), Some(p));
    assert!(fx.world.get::<Hiddenness>(cache).is_none());
    // Dropped again, it lies in plain sight.
    fx.world.entity_mut(p).insert(Perception(0));
    run(&mut fx, p, &mut rx, "drop cache");
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(has_line(&out, "mossy cache"), "{out}");
}

#[test]
fn a_hidden_prototype_starts_hidden_and_clamps() {
    let mut proto = object_proto(551, PLAIN, ObjectType::Other);
    assert!(proto.initial_hiddenness().is_none());
    proto.concealment = 250;
    assert_eq!(proto.initial_hiddenness().map(|h| h.0), Some(250));
    proto.concealment = 99_999;
    assert_eq!(
        proto.initial_hiddenness().map(|h| h.0),
        Some(mud_world::MAX_HIDDENNESS)
    );
}
