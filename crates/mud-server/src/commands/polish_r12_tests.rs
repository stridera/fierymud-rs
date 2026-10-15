//! Round-12 playtest polish: bulk get / drop / junk group identical items,
//! `effects` works mid-cast, and `hold` of a held item says so. Test-only.

use bevy_ecs::prelude::*;
use mud_world::{
    AttachedTriggers, EquippedSlot, Located, Slot, TriggerAttach, TriggerCatalog, TriggerDef,
    TriggerEvent,
};

use super::dispatch;
use super::god_zone_tests::Fx;
use super::item_guard_tests::item_at;
use super::test_support::{Rx, drain};

fn scene() -> (Fx, Entity, Entity, Rx) {
    let mut fx = Fx::new();
    fx.world
        .insert_resource(mud_world::ObjectPrototypes::default());
    let zone = fx.zone(30, false);
    let room = fx.room(zone, 30, 1);
    let (player, rx) = fx.person("Tester", 30, room);
    (fx, room, player, rx)
}

fn count(out: &str, needle: &str) -> usize {
    out.matches(needle).count()
}

#[test]
fn get_all_groups_identical_items_into_one_line() {
    let (mut fx, room, p, mut rx) = scene();
    for _ in 0..3 {
        item_at(&mut fx.world, room, 7, "a small stick", "stick");
    }
    item_at(&mut fx.world, room, 8, "a red apple", "apple");
    dispatch(&mut fx.world, p, "get all");
    let out = drain(&mut rx);
    assert_eq!(count(&out, "You pick up a small stick."), 1, "{out}");
    assert!(out.contains("You pick up a small stick. (x3)"), "{out}");
    assert!(out.contains("You pick up a red apple.\r\n"), "{out}");
}

#[test]
fn drop_all_and_junk_all_group_identical_items() {
    let (mut fx, room, p, mut rx) = scene();
    let sticks: Vec<Entity> = (0..3)
        .map(|_| item_at(&mut fx.world, p, 7, "a small stick", "stick"))
        .collect();
    dispatch(&mut fx.world, p, "drop all.stick");
    let out = drain(&mut rx);
    assert!(out.contains("You drop a small stick. (x3)"), "{out}");
    assert_eq!(count(&out, "You drop"), 1, "{out}");
    assert!(
        sticks
            .iter()
            .all(|&s| fx.world.get::<Located>(s).unwrap().0 == room)
    );
    for _ in 0..2 {
        item_at(&mut fx.world, p, 9, "a bent twig", "twig");
    }
    dispatch(&mut fx.world, p, "junk all.twig");
    let out = drain(&mut rx);
    assert!(out.contains("You destroy a bent twig. (x2)"), "{out}");
    assert_eq!(count(&out, "You destroy"), 1, "{out}");
}

#[test]
fn effects_is_allowed_while_casting() {
    let (mut fx, _room, p, mut rx) = scene();
    fx.world.entity_mut(p).insert(mud_world::Casting {
        ability_id: 1,
        ability_name: "Mend".into(),
        args: String::new(),
        kind_label: "spell".into(),
        verb: "cast".into(),
        ticks_remaining: 8,
        ticks_total: 8,
        target: mud_world::CastTarget::Caster,
        recognized_by: Vec::new(),
        slot_reservation: None,
        via_innate: false,
    });
    dispatch(&mut fx.world, p, "effects");
    let out = drain(&mut rx);
    assert!(!out.contains("busy spellcasting"), "{out}");
    dispatch(&mut fx.world, p, "get stick");
    assert!(drain(&mut rx).contains("busy spellcasting"));
}

#[test]
fn holding_what_you_already_hold_says_so() {
    let (mut fx, _room, p, mut rx) = scene();
    let torch = item_at(&mut fx.world, p, 7, "a torch", "torch");
    fx.world.entity_mut(torch).insert(EquippedSlot(Slot::Hold));
    dispatch(&mut fx.world, p, "hold torch");
    let out = drain(&mut rx);
    assert!(out.contains("You are already holding a torch."), "{out}");
    assert!(!out.contains("aren't carrying"), "{out}");
    dispatch(&mut fx.world, p, "hold lantern");
    assert!(drain(&mut rx).contains("You aren't carrying 'lantern'."));
}

#[test]
fn get_all_prints_the_grouped_line_before_the_get_trigger_output() {
    let (mut fx, room, p, mut rx) = scene();
    let mut catalog = TriggerCatalog::default();
    catalog.by_key.insert(
        (99, 1),
        TriggerDef {
            zone_id: 99,
            id: 1,
            name: "twitch".into(),
            attach_type: TriggerAttach::Object,
            commands: "actor:send('The stick twitches.')".into(),
            flags: vec![TriggerEvent::Get],
            arg_list: vec![],
            num_args: 0,
        },
    );
    fx.world.insert_resource(catalog);
    for _ in 0..2 {
        let stick = item_at(&mut fx.world, room, 7, "a small stick", "stick");
        fx.world
            .entity_mut(stick)
            .insert(AttachedTriggers(vec![(99, 1)]));
    }
    dispatch(&mut fx.world, p, "get all");
    let out = drain(&mut rx);
    let line = out.find("You pick up a small stick. (x2)").expect(&out);
    let first = out.find("The stick twitches.").expect(&out);
    assert!(line < first, "{out}");
    assert_eq!(count(&out, "The stick twitches."), 2, "{out}");
}
