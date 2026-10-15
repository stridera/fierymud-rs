//! `give all`, `give all.<name>` and `give N <item>` (legacy `do_give`):
//! each carried, unworn item goes through the single-give gates, RECEIVE
//! fires per item, and one grouped summary is printed. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{EntityType, ObjectRestriction, ObjectType};
use mud_world::{
    AttachedTriggers, EntityVariableCache, EquippedSlot, Keywords, Located, Mob, Named,
    ObjectPrototypes, ObjectRestrictions, Slot, TriggerAttach, TriggerCatalog, TriggerDef,
    TriggerEvent, WorldKey,
};

use super::dispatch;
use super::god_zone_tests::Fx;
use super::item_guard_tests::item_at;
use super::test_support::{Rx, drain, object_proto};

struct Scene {
    fx: Fx,
    room: Entity,
    giver: Entity,
    giver_rx: Rx,
}

fn scene() -> Scene {
    let mut fx = Fx::new();
    let mut objects = ObjectPrototypes::default();
    // 60 lb ingots: a level-1 carrier (105 lb) holds one.
    let mut ingot = object_proto(30, 20, ObjectType::Other);
    ingot.weight = 60.0;
    objects.by_key.insert((30, 20), ingot);
    fx.world.insert_resource(objects);
    let zone = fx.zone(30, false);
    let room = fx.room(zone, 30, 1);
    let (giver, giver_rx) = fx.person("Tester", 30, room);
    fx.world
        .entity_mut(giver)
        .insert(Keywords(vec!["tester".into()]));
    Scene {
        fx,
        room,
        giver,
        giver_rx,
    }
}

impl Scene {
    fn player(&mut self, name: &str, level: i32) -> (Entity, Rx) {
        let (e, rx) = self.fx.person(name, level, self.room);
        self.fx
            .world
            .entity_mut(e)
            .insert(Keywords(vec![name.to_lowercase()]));
        (e, rx)
    }

    fn cmd(&mut self, line: &str) -> String {
        dispatch(&mut self.fx.world, self.giver, line);
        drain(&mut self.giver_rx)
    }

    fn holder(&self, item: Entity) -> Entity {
        self.fx.world.get::<Located>(item).unwrap().0
    }
}

#[test]
fn give_all_hands_over_every_carried_item_with_a_grouped_summary() {
    let mut s = scene();
    let (bob, mut bob_rx) = s.player("Bob", 30);
    let a = item_at(&mut s.fx.world, s.giver, 7, "a steel sword", "sword");
    let b = item_at(&mut s.fx.world, s.giver, 7, "a steel sword", "sword");
    let c = item_at(&mut s.fx.world, s.giver, 8, "a red apple", "apple");
    let out = s.cmd("give all bob");
    for item in [a, b, c] {
        assert_eq!(s.holder(item), bob, "{out}");
    }
    assert!(out.contains("You give a steel sword to Bob. (x2)"), "{out}");
    assert!(out.contains("You give a red apple to Bob."), "{out}");
    assert!(!out.contains("apple to Bob. (x"), "{out}");
    let heard = drain(&mut bob_rx);
    assert!(
        heard.contains("Tester gives you a steel sword. (x2)"),
        "{heard}"
    );
}

#[test]
fn give_all_leaves_worn_gear_and_reports_an_empty_pack() {
    let mut s = scene();
    let (bob, _rx) = s.player("Bob", 30);
    let worn = item_at(&mut s.fx.world, s.giver, 7, "a steel sword", "sword");
    s.fx.world
        .entity_mut(worn)
        .insert(EquippedSlot(Slot::Wield));
    let out = s.cmd("give all bob");
    assert_eq!(s.holder(worn), s.giver, "{out}");
    assert!(
        out.contains("You don't seem to be holding anything."),
        "{out}"
    );
    let _ = bob;
}

#[test]
fn give_all_dot_name_only_hands_over_matches() {
    let mut s = scene();
    let (bob, _rx) = s.player("Bob", 30);
    let sword = item_at(&mut s.fx.world, s.giver, 7, "a steel sword", "sword");
    let apple = item_at(&mut s.fx.world, s.giver, 8, "a red apple", "apple");
    let out = s.cmd("give all.sword bob");
    assert_eq!(s.holder(sword), bob, "{out}");
    assert_eq!(s.holder(apple), s.giver, "{out}");
    let out = s.cmd("give all.shield bob");
    assert!(out.contains("You don't even have one shield!"), "{out}");
}

#[test]
fn give_n_item_hands_over_exactly_n() {
    let mut s = scene();
    let (bob, _rx) = s.player("Bob", 30);
    let items: Vec<Entity> = (0..3)
        .map(|_| item_at(&mut s.fx.world, s.giver, 7, "a steel sword", "sword"))
        .collect();
    let out = s.cmd("give 2 sword to bob");
    let given = items.iter().filter(|&&i| s.holder(i) == bob).count();
    assert_eq!(given, 2, "{out}");
    assert!(out.contains("You give a steel sword to Bob. (x2)"), "{out}");
}

#[test]
fn give_n_all_is_refused() {
    let mut s = scene();
    let (bob, _rx) = s.player("Bob", 30);
    let sword = item_at(&mut s.fx.world, s.giver, 7, "a steel sword", "sword");
    let out = s.cmd("give 3 all bob");
    assert!(
        out.contains("Do you want to give '3' or 'all'?  Make up your mind!"),
        "{out}"
    );
    assert_eq!(s.holder(sword), s.giver);
    let _ = bob;
}

#[test]
fn an_overloaded_recipient_stops_the_run_but_keeps_what_fit() {
    let mut s = scene();
    let (bob, mut bob_rx) = s.player("Bob", 1);
    let ingots: Vec<Entity> = (0..3)
        .map(|_| item_at(&mut s.fx.world, s.giver, 20, "an iron ingot", "ingot"))
        .collect();
    let out = s.cmd("give all bob");
    let given = ingots.iter().filter(|&&i| s.holder(i) == bob).count();
    assert_eq!(given, 1, "{out}");
    assert!(
        out.contains("Bob is too laden to take an iron ingot."),
        "{out}"
    );
    assert_eq!(
        out.matches("too laden").count(),
        1,
        "stops at the first: {out}"
    );
    assert!(out.contains("You give an iron ingot to Bob."), "{out}");
    assert!(!out.contains("(x"), "{out}");
    drain(&mut bob_rx);
}

#[test]
fn no_drop_items_are_skipped_and_the_rest_still_go() {
    let mut s = scene();
    let (bob, _rx) = s.player("Bob", 30);
    let sword = item_at(&mut s.fx.world, s.giver, 7, "a steel sword", "sword");
    // Newest first, so the cursed ring is tried before the sword.
    let ring = item_at(&mut s.fx.world, s.giver, 9, "a cursed ring", "ring");
    s.fx.world
        .entity_mut(ring)
        .insert(ObjectRestrictions(vec![ObjectRestriction::NoDrop]));
    let out = s.cmd("give all bob");
    assert_eq!(s.holder(ring), s.giver, "{out}");
    assert_eq!(s.holder(sword), bob, "{out}");
    assert!(
        out.contains("You can't seem to let go of a cursed ring."),
        "{out}"
    );
}

#[test]
fn mobs_can_give_all_through_a_script_command() {
    let mut s = scene();
    let (bob, mut bob_rx) = s.player("Bob", 30);
    let keeper =
        s.fx.world
            .spawn((
                Mob,
                Named {
                    name: "Keeper".into(),
                },
                Keywords(vec!["keeper".into()]),
                Located(s.room),
            ))
            .id();
    let a = item_at(&mut s.fx.world, keeper, 7, "a steel sword", "sword");
    let b = item_at(&mut s.fx.world, keeper, 8, "a red apple", "apple");
    dispatch(&mut s.fx.world, keeper, "give all bob");
    assert_eq!(s.holder(a), bob);
    assert_eq!(s.holder(b), bob);
    assert!(drain(&mut bob_rx).contains("Keeper gives you"));
}

#[test]
fn receive_fires_once_per_item() {
    let mut s = scene();
    let mut catalog = TriggerCatalog::default();
    catalog.by_key.insert(
        (99, 1),
        TriggerDef {
            zone_id: 99,
            id: 1,
            name: "t1".into(),
            attach_type: TriggerAttach::Mob,
            commands: "self:setvar('n', (self:getvar('n') or 0) + 1)".into(),
            flags: vec![TriggerEvent::Receive],
            arg_list: vec![],
            num_args: 0,
        },
    );
    s.fx.world.insert_resource(catalog);
    let keeper =
        s.fx.world
            .spawn((
                Mob,
                Named {
                    name: "Keeper".into(),
                },
                Keywords(vec!["keeper".into()]),
                WorldKey { zone: 99, id: 5 },
                Located(s.room),
                AttachedTriggers(vec![(99, 1)]),
            ))
            .id();
    let items: Vec<Entity> = [(7, "sword"), (7, "sword"), (8, "apple")]
        .iter()
        .map(|(id, kw)| item_at(&mut s.fx.world, s.giver, *id, &format!("a {kw}"), kw))
        .collect();
    let out = s.cmd("give all keeper");
    for item in items {
        assert_eq!(s.holder(item), keeper, "{out}");
    }
    let n =
        s.fx.world
            .get_resource::<EntityVariableCache>()
            .and_then(|c| c.get(EntityType::Mob, 99, 5, "n").cloned());
    assert_eq!(n, Some(3.into()), "{out}");
}

#[test]
fn coins_still_take_the_bribe_path() {
    let mut s = scene();
    let (bob, mut bob_rx) = s.player("Bob", 30);
    s.fx.world
        .entity_mut(s.giver)
        .insert(mud_world::Wealth(1_000));
    let out = s.cmd("give 5 gold bob");
    assert!(out.contains("You give Bob"), "{out}");
    assert!(drain(&mut bob_rx).contains("Tester gives you"));
    assert_eq!(s.fx.world.get::<mud_world::Wealth>(s.giver).unwrap().0, 500);
    assert_eq!(s.fx.world.get::<mud_world::Wealth>(bob).unwrap().0, 500);
}
