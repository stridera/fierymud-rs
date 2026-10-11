//! The Lua room accessors read the room's `Contents` index instead of
//! scanning every located entity in the world.

use std::time::Instant;

use super::*;

const ROOMS: i32 = 500;
const MOBS_PER_ROOM: i32 = 11;
const ITEMS_PER_ROOM: i32 = 6;

/// A world shaped like prod: ~5.5k mobs and ~3k items over 500 rooms, a
/// player and a rider-less mob in room `(99, 1)`.
pub(crate) fn big_world() -> (World, Entity) {
    let mut world = World::new();
    let mut index = WorldKeyIndex::default();
    let mut first_mob = None;
    for r in 1..=ROOMS {
        let room = world.spawn_empty().id();
        index.rooms.insert((99, r), room);
        for m in 0..MOBS_PER_ROOM {
            let mob = world
                .spawn((
                    Mob,
                    Named {
                        name: format!("mob {r} {m}"),
                    },
                    Keywords(vec![format!("m{r}x{m}")]),
                    WorldKey { zone: 99, id: m },
                    Health { hp: 10, max: 10 },
                    Located(room),
                ))
                .id();
            first_mob.get_or_insert(mob);
        }
        for i in 0..ITEMS_PER_ROOM {
            world.spawn((
                Item,
                Named {
                    name: format!("item {r} {i}"),
                },
                Keywords(vec![format!("i{r}x{i}")]),
                WorldKey { zone: 98, id: i },
                Located(room),
            ));
        }
        if r == 1 {
            world.spawn((Player, Named { name: "Pat".into() }, Located(room)));
        }
    }
    world.insert_resource(index);
    (world, first_mob.unwrap())
}

/// The pre-index implementation: every located non-item in the world.
fn scan_characters(world: &mut World, room: Entity) -> Vec<Entity> {
    let mut q = world.query_filtered::<(Entity, &Located), Without<Item>>();
    let mut v: Vec<Entity> = q
        .iter(world)
        .filter(|(_, l)| l.0 == room)
        .map(|(e, _)| e)
        .collect();
    v.sort_unstable();
    v
}

#[test]
fn room_accessors_match_a_full_scan() {
    let (mut world, mob) = big_world();
    let room = world.resource::<WorldKeyIndex>().rooms[&(99, 1)];
    let expected = scan_characters(&mut world, room);
    assert_eq!(expected.len(), usize::try_from(MOBS_PER_ROOM).unwrap() + 1);
    let mut host = LuaHost::new();
    let out = host
        .exec_for_actor(
            &mut world,
            mob,
            "local r = get_room(99, 1)\n\
             local a, p = r.actors, r.people\n\
             local ids = {}\n\
             for _, x in ipairs(a) do ids[#ids + 1] = x.name end\n\
             table.sort(ids)\n\
             print(#a, #p, r.actor_count, #r:get_people(), table.concat(ids, ','))\n\
             print(r:find_actor('m1x4').name, r:find_actor('Pat').name, tostring(r:find_actor('m2x4')))\n\
             print(r:find_object('i1x3').name, tostring(r:find_object('i2x3')), tostring(r:find_object('m1x4')))\n\
             print(r:get_people(99, 4).name, r:get_objects(98, 2).name)",
        )
        .unwrap();
    let n = expected.len();
    let mut names: Vec<String> = expected
        .iter()
        .map(|&e| world.get::<Named>(e).unwrap().name.clone())
        .collect();
    names.sort();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        format!("{n}\t{n}\t{n}\t{n}\t{}", names.join(",")),
        "{out}"
    );
    assert_eq!(lines[1], "mob 1 4\tPat\tnil", "{out}");
    assert_eq!(lines[2], "item 1 3\tnil\tnil", "{out}");
    assert_eq!(lines[3], "mob 1 4\titem 1 2", "{out}");
}

#[test]
fn moved_and_removed_actors_leave_the_room_listing() {
    let (mut world, mob) = big_world();
    let room = world.resource::<WorldKeyIndex>().rooms[&(99, 1)];
    let other = world.resource::<WorldKeyIndex>().rooms[&(99, 2)];
    let mut host = LuaHost::new();
    let count = |world: &mut World, host: &mut LuaHost, r: i32| -> String {
        host.exec_for_actor(world, mob, &format!("print(get_room(99, {r}).actor_count)"))
            .unwrap()
            .trim()
            .to_string()
    };
    assert_eq!(count(&mut world, &mut host, 1), "12");
    world.entity_mut(mob).insert(Located(other));
    assert_eq!(count(&mut world, &mut host, 1), "11");
    assert_eq!(count(&mut world, &mut host, 2), "12");
    let victim = scan_characters(&mut world, room)[0];
    world.despawn(victim);
    assert_eq!(count(&mut world, &mut host, 1), "10");
}

/// Release benchmark: `cargo test -p mud-script --release room_accessor_bench
/// -- --ignored --nocapture`. Prints microseconds per accessor call.
#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn room_accessor_bench() {
    const CALLS: u32 = 2000;
    let (mut world, mob) = big_world();
    let mut host = LuaHost::new();
    for (label, body) in [
        ("room.actors", "local a = r.actors"),
        ("room.people", "local a = r.people"),
        ("room.actor_count", "local a = r.actor_count"),
        ("room:find_actor", "local a = r:find_actor('m1x4')"),
        ("room:find_object", "local a = r:find_object('i1x3')"),
        ("room:get_people()", "local a = r:get_people()"),
        ("room:get_objects(z,i)", "local a = r:get_objects(98, 2)"),
    ] {
        let code = format!("local r = get_room(99, 1)\nfor _ = 1, {CALLS} do {body} end");
        host.exec_for_actor(&mut world, mob, &code).unwrap();
        let start = Instant::now();
        host.exec_for_actor(&mut world, mob, &code).unwrap();
        let us = start.elapsed().as_secs_f64() * 1e6 / f64::from(CALLS);
        println!("BENCH {label}: {us:.2} us/call");
    }
}
