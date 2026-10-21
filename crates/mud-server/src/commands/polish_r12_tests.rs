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
use super::test_support::{Rx, drain, object_proto};

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

#[test]
fn counted_get_groups_identical_items_into_one_line() {
    let (mut fx, room, p, mut rx) = scene();
    let sticks: Vec<Entity> = (0..3)
        .map(|_| item_at(&mut fx.world, room, 7, "a small stick", "stick"))
        .collect();
    dispatch(&mut fx.world, p, "get 2 stick");
    let out = drain(&mut rx);
    assert_eq!(count(&out, "You pick up"), 1, "{out}");
    assert!(out.contains("You pick up a small stick. (x2)"), "{out}");
    let held = sticks
        .iter()
        .filter(|&&s| fx.world.get::<Located>(s).unwrap().0 == p)
        .count();
    assert_eq!(held, 2, "{out}");
}

#[test]
fn counted_get_of_one_available_item_keeps_the_plain_line() {
    let (mut fx, room, p, mut rx) = scene();
    item_at(&mut fx.world, room, 7, "a small stick", "stick");
    dispatch(&mut fx.world, p, "get 3 stick");
    let out = drain(&mut rx);
    assert!(out.contains("You pick up a small stick.\r\n"), "{out}");
    assert!(!out.contains("(x"), "{out}");
}

/// Drop ANSI colour escapes so a line can be compared as plain text.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn give_coins_states_the_denominations_the_player_typed() {
    let (mut fx, room, p, mut rx) = scene();
    fx.world.entity_mut(p).insert(mud_world::Wealth(5_000));
    let (bob, mut bob_rx) = fx.person("Bob", 30, room);
    dispatch(&mut fx.world, p, "give 500 copper bob");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("You give Bob 500 copper."), "{out}");
    assert!(!out.contains("gold"), "{out}");
    assert!(plain(&drain(&mut bob_rx)).contains("Tester gives you 500 copper."));
    dispatch(&mut fx.world, p, "give 2 gold 3 silver 4 copper bob");
    let out = plain(&drain(&mut rx));
    assert!(
        out.contains("You give Bob 2 gold, 3 silver, and 4 copper."),
        "{out}"
    );
    dispatch(&mut fx.world, p, "give 1 gold 7 copper bob");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("You give Bob 1 gold and 7 copper."), "{out}");
    assert_eq!(
        fx.world.get::<mud_world::Wealth>(bob).unwrap().0,
        500 + 234 + 107
    );
}

#[test]
fn give_coins_rejects_a_non_positive_count_in_any_pair() {
    let (mut fx, room, p, mut rx) = scene();
    fx.world
        .insert_resource(mud_world::ObjectPrototypes::default());
    fx.world.entity_mut(p).insert(mud_world::Wealth(20_000));
    let (bob, mut bob_rx) = fx.person("Bob", 30, room);
    for line in [
        "give 10 plat -9999 copper bob",
        "give -5 copper bob",
        "give 0 copper bob",
        "give 3 gold -1 silver bob",
    ] {
        dispatch(&mut fx.world, p, line);
        let out = plain(&drain(&mut rx));
        assert!(
            out.contains("Give a positive number of coins."),
            "{line}: {out}"
        );
        assert!(!out.contains("-9999"), "{line}: {out}");
    }
    assert_eq!(fx.world.get::<mud_world::Wealth>(p).unwrap().0, 20_000);
    assert!(
        fx.world
            .get::<mud_world::Wealth>(bob)
            .is_none_or(|w| w.0 == 0)
    );
    assert!(drain(&mut bob_rx).is_empty());
}

#[test]
fn counted_get_leaves_npc_corpses_to_mobs_and_players_but_staff_take_them() {
    let (mut fx, room, p, mut rx) = scene();
    let corpses: Vec<Entity> = (0..2)
        .map(|_| {
            let c = item_at(&mut fx.world, room, 7, "the corpse of a rat", "corpse");
            fx.world.entity_mut(c).insert(mud_world::Corpse);
            c
        })
        .collect();
    let mob = fx
        .world
        .spawn((
            mud_world::Mob,
            mud_world::Named {
                name: "Ghoul".into(),
            },
            mud_world::Keywords(vec!["ghoul".into()]),
            Located(room),
        ))
        .id();
    dispatch(&mut fx.world, mob, "get 2 corpse");
    for &c in &corpses {
        assert_eq!(fx.world.get::<Located>(c).unwrap().0, room);
    }
    // Players are refused too (legacy corpses have no TAKE flag)...
    dispatch(&mut fx.world, p, "get 2 corpse");
    let out = drain(&mut rx);
    for &c in &corpses {
        assert_eq!(fx.world.get::<Located>(c).unwrap().0, room, "{out}");
    }
    // ...but staff still take them.
    fx.world.insert_resource(crate::DevMode(true));
    dispatch(&mut fx.world, p, "get 2 corpse");
    let out = drain(&mut rx);
    for &c in &corpses {
        assert_eq!(fx.world.get::<Located>(c).unwrap().0, p, "{out}");
    }
}

#[test]
fn players_cannot_pick_up_npc_corpses() {
    let (mut fx, room, p, mut rx) = scene();
    let corpse = item_at(&mut fx.world, room, 7, "the corpse of a rat", "corpse");
    fx.world.entity_mut(corpse).insert(mud_world::Corpse);
    let stick = item_at(&mut fx.world, room, 8, "a small stick", "stick");

    dispatch(&mut fx.world, p, "get corpse");
    let out = drain(&mut rx);
    assert!(
        out.contains("The corpse of a rat: you can't take that!"),
        "{out}"
    );
    assert_eq!(fx.world.get::<Located>(corpse).unwrap().0, room);

    // `get all` leaves the corpse alone without a line about it.
    dispatch(&mut fx.world, p, "get all");
    let out = drain(&mut rx);
    assert!(out.contains("You pick up a small stick."), "{out}");
    assert!(!out.contains("corpse"), "{out}");
    assert_eq!(fx.world.get::<Located>(stick).unwrap().0, p);
    assert_eq!(fx.world.get::<Located>(corpse).unwrap().0, room);

    // A sweep with nothing else to take says so rather than staying mute.
    dispatch(&mut fx.world, p, "get all");
    let out = drain(&mut rx);
    assert!(out.contains("nothing here to pick up"), "{out}");

    // An explicit `get all.corpse` is told it can't.
    dispatch(&mut fx.world, p, "get all.corpse");
    let out = drain(&mut rx);
    assert!(out.contains("can't take"), "{out}");
    assert_eq!(fx.world.get::<Located>(corpse).unwrap().0, room);

    // Staff can.
    fx.world.insert_resource(crate::DevMode(true));
    dispatch(&mut fx.world, p, "get corpse");
    let out = drain(&mut rx);
    assert!(out.contains("You pick up the corpse of a rat."), "{out}");
    assert_eq!(fx.world.get::<Located>(corpse).unwrap().0, p);
}

#[test]
fn players_cannot_take_npc_corpses_out_of_containers() {
    let (mut fx, room, p, mut rx) = scene();
    // A corpse nested inside another corpse.
    let outer = item_at(&mut fx.world, room, 7, "the corpse of an orc", "corpse orc");
    fx.world.entity_mut(outer).insert(mud_world::Corpse);
    let inner = item_at(&mut fx.world, outer, 8, "the corpse of a rat", "corpse rat");
    fx.world.entity_mut(inner).insert(mud_world::Corpse);
    let sword = item_at(&mut fx.world, outer, 9, "a rusty sword", "sword");

    // `get all` from the outer corpse takes the loot and skips the nested
    // corpse without a line about it.
    dispatch(&mut fx.world, p, "get all orc");
    let out = drain(&mut rx);
    assert!(out.contains("You take a rusty sword"), "{out}");
    assert!(!out.contains("rat"), "{out}");
    assert_eq!(fx.world.get::<Located>(sword).unwrap().0, p);
    assert_eq!(fx.world.get::<Located>(inner).unwrap().0, outer);

    // A corpse put in a chest cannot be taken by name.
    let mut proto = object_proto(30, 10, mud_db::enums::ObjectType::Container);
    proto.name = "a wooden chest".into();
    proto.keywords = vec!["chest".into()];
    fx.world
        .resource_mut::<mud_world::ObjectPrototypes>()
        .by_key
        .insert((30, 10), proto);
    let chest = item_at(&mut fx.world, room, 10, "a wooden chest", "chest");
    let boxed = item_at(
        &mut fx.world,
        chest,
        11,
        "the corpse of a bat",
        "corpse bat",
    );
    fx.world.entity_mut(boxed).insert(mud_world::Corpse);
    dispatch(&mut fx.world, p, "get corpse chest");
    let out = drain(&mut rx);
    assert!(
        out.contains("The corpse of a bat: you can't take that!"),
        "{out}"
    );
    assert_eq!(fx.world.get::<Located>(boxed).unwrap().0, chest);

    // Staff are exempt.
    fx.world.insert_resource(crate::DevMode(true));
    dispatch(&mut fx.world, p, "get corpse chest");
    let out = drain(&mut rx);
    assert!(out.contains("You take the corpse of a bat"), "{out}");
    assert_eq!(fx.world.get::<Located>(boxed).unwrap().0, p);
    dispatch(&mut fx.world, p, "get all orc");
    assert_eq!(fx.world.get::<Located>(inner).unwrap().0, p);
}

#[test]
fn nobody_takes_a_player_corpse_out_of_a_container() {
    let (mut fx, room, p, mut rx) = scene();
    let mut proto = object_proto(30, 10, mud_db::enums::ObjectType::Container);
    proto.name = "a wooden chest".into();
    proto.keywords = vec!["chest".into()];
    fx.world
        .resource_mut::<mud_world::ObjectPrototypes>()
        .by_key
        .insert((30, 10), proto);
    let chest = item_at(&mut fx.world, room, 10, "a wooden chest", "chest");
    let pc = item_at(&mut fx.world, chest, 11, "the corpse of Bob", "corpse bob");
    fx.world
        .entity_mut(pc)
        .insert((mud_world::Corpse, mud_world::PlayerCorpse));
    let gem = item_at(&mut fx.world, chest, 12, "a red gem", "gem");

    for staff in [false, true] {
        if staff {
            fx.world.insert_resource(crate::DevMode(true));
        }
        dispatch(&mut fx.world, p, "get corpse chest");
        let out = drain(&mut rx);
        assert!(
            out.contains("The corpse of Bob: you can't take that!")
                || out.contains("the corpse of Bob: you can't take that!"),
            "staff={staff}: {out}"
        );
        assert_eq!(fx.world.get::<Located>(pc).unwrap().0, chest);
    }

    // `get all chest` takes the rest and leaves the corpse.
    dispatch(&mut fx.world, p, "get all chest");
    let out = drain(&mut rx);
    assert!(out.contains("You take a red gem"), "{out}");
    assert!(out.contains("you can't take that!"), "{out}");
    assert_eq!(fx.world.get::<Located>(gem).unwrap().0, p);
    assert_eq!(fx.world.get::<Located>(pc).unwrap().0, chest);
}

#[test]
fn counted_get_keeps_the_k_dot_index() {
    let (mut fx, room, p, mut rx) = scene();
    let sticks: Vec<Entity> = (0..3)
        .map(|_| item_at(&mut fx.world, room, 7, "a small stick", "stick"))
        .collect();
    let apple = item_at(&mut fx.world, room, 8, "a red apple", "apple");
    dispatch(&mut fx.world, p, "get 2 2.stick");
    let out = drain(&mut rx);
    let taken = sticks
        .iter()
        .filter(|&&s| fx.world.get::<Located>(s).unwrap().0 == p)
        .count();
    assert_eq!(taken, 2, "{out}");
    assert_eq!(fx.world.get::<Located>(apple).unwrap().0, room, "{out}");
    // The 2nd match is taken each time, so the 1st match (newest stick) is
    // never touched.
    let remaining: Vec<Entity> = sticks
        .iter()
        .copied()
        .filter(|&s| fx.world.get::<Located>(s).unwrap().0 == room)
        .collect();
    assert_eq!(remaining.len(), 1, "{out}");
    assert_eq!(remaining, vec![sticks[2]], "{out}");
}

#[test]
fn get_all_leaves_a_dead_players_corpse_and_its_purse_alone() {
    // Playtest r16: the bulk path treated any item carrying a `CoinPile` as a
    // loose pile, so a stranger's `get all` emptied the purse of someone
    // else's corpse and despawned it.
    let (mut fx, room, p, mut rx) = scene();
    let corpse = item_at(&mut fx.world, room, 7, "the corpse of Bob", "corpse bob");
    fx.world.entity_mut(corpse).insert((
        mud_world::Corpse,
        mud_world::PlayerCorpse,
        mud_world::CoinPile(5000),
    ));
    dispatch(&mut fx.world, p, "get all");
    let out = drain(&mut rx);
    assert!(!out.contains("You pick up"), "{out}");
    assert_eq!(fx.world.get::<Located>(corpse).unwrap().0, room, "{out}");
    assert_eq!(fx.world.get::<mud_world::CoinPile>(corpse).unwrap().0, 5000);
    assert!(
        fx.world
            .get::<mud_world::Wealth>(p)
            .is_none_or(|w| w.0 == 0)
    );

    // A real loose pile still goes straight to the purse.
    let pile = item_at(&mut fx.world, room, 8, "a pile of coins", "coins");
    fx.world
        .entity_mut(pile)
        .insert((mud_world::CoinPile(120), mud_world::LooseCoins));
    dispatch(&mut fx.world, p, "get all");
    let out = drain(&mut rx);
    assert!(out.contains("You pick up"), "{out}");
    assert_eq!(fx.world.get::<mud_world::Wealth>(p).unwrap().0, 120);
    assert!(fx.world.get::<mud_world::CoinPile>(corpse).is_some());
}

#[test]
fn a_bag_holding_a_rotted_corpses_coins_survives_get_bag_and_get_all() {
    // A decayed corpse's coins can land in a container as a plain `CoinPile`
    // (not a `LooseCoins` pile). `get bag` / `get all` must pick the bag up,
    // not pay the coins out and delete it.
    let (mut fx, room, p, mut rx) = scene();
    let bag = item_at(&mut fx.world, room, 7, "a canvas bag", "bag");
    fx.world.entity_mut(bag).insert(mud_world::CoinPile(700));
    dispatch(&mut fx.world, p, "get bag");
    let out = drain(&mut rx);
    assert!(out.contains("You pick up a canvas bag"), "{out}");
    assert_eq!(fx.world.get::<Located>(bag).unwrap().0, p, "{out}");
    assert_eq!(fx.world.get::<mud_world::CoinPile>(bag).unwrap().0, 700);
    assert!(
        fx.world
            .get::<mud_world::Wealth>(p)
            .is_none_or(|w| w.0 == 0)
    );

    let bag2 = item_at(&mut fx.world, room, 8, "a leather bag", "bag");
    fx.world.entity_mut(bag2).insert(mud_world::CoinPile(300));
    dispatch(&mut fx.world, p, "get all");
    let out = drain(&mut rx);
    assert_eq!(fx.world.get::<Located>(bag2).unwrap().0, p, "{out}");
    assert_eq!(fx.world.get::<mud_world::CoinPile>(bag2).unwrap().0, 300);
}
