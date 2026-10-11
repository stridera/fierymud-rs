//! The Lua actor inventory accessors (`has_item`, `has_equipped`,
//! `get_worn`, `wearing`) and the `world.destroy(name)` resolver read the
//! carrier's `Contents` index instead of scanning every item in the world.

use std::time::Instant;

use mud_world::Slot;

use super::room_scan_tests::big_world;
use super::*;

fn give(
    world: &mut World,
    carrier: Entity,
    name: &str,
    key: (i32, i32),
    slot: Option<Slot>,
) -> Entity {
    let mut e = world.spawn((
        Item,
        Named { name: name.into() },
        Keywords(vec![name.to_string()]),
        WorldKey {
            zone: key.0,
            id: key.1,
        },
        Located(carrier),
    ));
    if let Some(s) = slot {
        e.insert(EquippedSlot(s));
    }
    e.id()
}

/// `big_world` plus a loaded-down mob (carried, worn and bagged items) and a
/// decoy mob holding some of the same prototypes. Returns `(world, carrier)`.
fn geared_world() -> (World, Entity) {
    let (mut world, carrier) = big_world();
    give(&mut world, carrier, "coin", (97, 1), None);
    give(&mut world, carrier, "gem", (97, 2), None);
    give(&mut world, carrier, "blade", (97, 10), Some(Slot::Wield));
    give(&mut world, carrier, "helm", (97, 11), Some(Slot::Head));
    let bag = give(&mut world, carrier, "bag", (97, 20), None);
    give(&mut world, bag, "pearl", (97, 21), None);
    let room = world.resource::<WorldKeyIndex>().rooms[&(99, 2)];
    let decoy = world
        .spawn((
            Mob,
            Named {
                name: "decoy".into(),
            },
            Located(room),
        ))
        .id();
    give(&mut world, decoy, "coin", (97, 1), None);
    give(&mut world, decoy, "mace", (97, 12), Some(Slot::Hold));
    (world, carrier)
}

/// The pre-index implementations: every item in the world.
fn scan_has_item(world: &mut World, who: Entity, zone: i32, id: i32, worn_only: bool) -> bool {
    let mut q = world.query_filtered::<(&Located, &WorldKey, Has<EquippedSlot>), With<Item>>();
    q.iter(world)
        .any(|(l, wk, worn)| l.0 == who && wk.zone == zone && wk.id == id && (!worn_only || worn))
}

fn scan_worn(world: &mut World, who: Entity, slot: Slot) -> Option<Entity> {
    let mut q = world.query_filtered::<(Entity, &Located, &EquippedSlot), With<Item>>();
    q.iter(world)
        .find(|(_, l, eq)| l.0 == who && eq.0 == slot)
        .map(|(e, _, _)| e)
}

#[test]
fn actor_inventory_accessors_match_a_full_scan() {
    let (mut world, carrier) = geared_world();
    let mut host = LuaHost::new();
    let probes = [
        (97, 1),
        (97, 2),
        (97, 10),
        (97, 11),
        (97, 12),
        (97, 20),
        (97, 21),
        (98, 0),
    ];
    for (zone, id) in probes {
        let out = host
            .exec_for_actor(
                &mut world,
                carrier,
                &format!("print(tostring(self:has_item({zone}, {id})), tostring(self:has_equipped({zone}, {id})))"),
            )
            .unwrap();
        let item = scan_has_item(&mut world, carrier, zone, id, false);
        let eq = scan_has_item(&mut world, carrier, zone, id, true);
        assert_eq!(out.trim(), format!("{item}\t{eq}"), "probe ({zone}, {id})");
    }
    // Spot checks on the shape: a bagged item is not inventory; a worn one is.
    assert!(!scan_has_item(&mut world, carrier, 97, 21, false));
    assert!(scan_has_item(&mut world, carrier, 97, 10, false));

    for slot in [Slot::Wield, Slot::Head, Slot::Hold, Slot::Body] {
        let label = format!("{slot:?}").to_ascii_lowercase();
        let out = host
            .exec_for_actor(
                &mut world,
                carrier,
                &format!("local w = self:get_worn('{label}')\nprint(w and w.name or 'nil')"),
            )
            .unwrap();
        let expected = scan_worn(&mut world, carrier, slot).map_or_else(
            || "nil".to_string(),
            |e| world.get::<Named>(e).unwrap().name.clone(),
        );
        assert_eq!(out.trim(), expected, "slot {label}");
    }

    let out = host
        .exec_for_actor(
            &mut world,
            carrier,
            "local n = {}\nfor k, v in pairs(self.wearing) do n[#n + 1] = k .. '=' .. v.name end\n\
             table.sort(n)\nprint(table.concat(n, ','))",
        )
        .unwrap();
    assert_eq!(out.trim(), "9710=blade,9711=helm");
}

#[test]
fn destroy_by_name_prefers_carried_then_room() {
    let (mut world, carrier) = geared_world();
    let mut host = LuaHost::new();
    // "coin" is carried by both mobs; only the caller's is reachable.
    host.exec_for_actor(&mut world, carrier, "world.destroy('coin')")
        .unwrap();
    assert!(!scan_has_item(&mut world, carrier, 97, 1, false));
    let mut q = world.query_filtered::<&WorldKey, With<Item>>();
    let coins = q.iter(&world).filter(|k| (k.zone, k.id) == (97, 1)).count();
    assert_eq!(coins, 1, "the decoy's coin survives");
}

/// Release benchmark: `cargo test -p mud-script --release actor_accessor_bench
/// -- --ignored --nocapture`. Prints microseconds per accessor call.
#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn actor_accessor_bench() {
    const CALLS: u32 = 2000;
    let (mut world, carrier) = geared_world();
    let mut host = LuaHost::new();
    for (label, body) in [
        ("actor:has_item", "local a = self:has_item(97, 21)"),
        ("actor:has_equipped", "local a = self:has_equipped(97, 11)"),
        ("actor:get_worn", "local a = self:get_worn('head')"),
        ("actor.wearing", "local a = self.wearing"),
        ("world.destroy(name)", "local a = world.destroy('zzz')"),
    ] {
        let code = format!("for _ = 1, {CALLS} do {body} end");
        host.exec_for_actor(&mut world, carrier, &code).unwrap();
        let start = Instant::now();
        host.exec_for_actor(&mut world, carrier, &code).unwrap();
        let us = start.elapsed().as_secs_f64() * 1e6 / f64::from(CALLS);
        println!("BENCH {label}: {us:.2} us/call");
    }
}
