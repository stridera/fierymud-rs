//! Prod-scale timing guards for sweeps that used to create archetypes (or
//! scan the whole world) per entity. Each builds a world with ~4k
//! archetypes, like prod, and times the first sweep, which is the one that
//! paid for archetype and table creation. Hard limits are enforced in
//! release only.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::too_many_lines
)]

use std::time::Instant;

use bevy_ecs::prelude::*;
use mud_db::enums::{MobBehavior, MobProfession, MobTrait, ObjectFlag, ObjectType, WearFlag};
use mud_db::mob_reset_equipment::MobResetEquipment;
use mud_world::{
    AppliedTo, EffectCatalog, EffectDef, EffectInstance, EffectSource, FromMobReset, Located, Mob,
    MobDefaultEffect, MobDefaultEffectCatalog, MobGearCatalog, MobPrototypes, MobResetCatalog,
    MobResetEntry, ObjectContentsCatalog, ObjectPrototypes, ObjectResetCatalog, RaceEffect,
    RaceEffectCatalog, Room, RuntimeConfig, ShopCatalog, TriggerCatalog, WorldKey,
};

use crate::TickCount;
use crate::commands::test_support::{mob_proto, object_proto};
use crate::respawn::{MobRespawnTimers, respawn_tick};

const FLAGS: [&str; 8] = [
    "fly",
    "sneak",
    "bless",
    "sanctuary",
    "infravision",
    "haste",
    "waterwalk",
    "blur",
];
const BEHAVIORS: [MobBehavior; 6] = [
    MobBehavior::Scavenger,
    MobBehavior::Wimpy,
    MobBehavior::Aware,
    MobBehavior::Helper,
    MobBehavior::Protector,
    MobBehavior::Memory,
];

fn status_effect(id: i32) -> EffectDef {
    EffectDef {
        id,
        name: "status".into(),
        description: None,
        effect_type: "status".into(),
        tags: vec![],
        presence_override: None,
        default_params: serde_json::json!({}),
        prevents_speaking: false,
        prevents_casting: false,
        prevents_movement: false,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    }
}

/// A respawn wave on a prod-sized world: 5.5k live mobs spread over ~4k
/// archetypes carrying 8k permanent effect instances, 300 prototypes that
/// differ in behaviors, traits, perception, default effects and gear, and
/// 400 reset rows that are all empty (a zone just wiped, or the first
/// sweep after a boot that skipped them).
fn respawn_world() -> (World, Vec<Entity>) {
    let mut world = World::new();
    world.insert_resource(TickCount(0));
    world.insert_resource(RuntimeConfig::default());
    world.insert_resource(ShopCatalog::default());
    world.insert_resource(TriggerCatalog::default());
    world.insert_resource(MobRespawnTimers::default());
    world.insert_resource(ObjectResetCatalog::default());
    world.insert_resource(ObjectContentsCatalog::default());

    let mut effects = EffectCatalog::default();
    effects.by_id.insert(4, status_effect(4));
    world.insert_resource(effects);
    let mut races = RaceEffectCatalog::default();
    for flag in ["infravision", "detect_invisible"] {
        races.insert(
            "human",
            RaceEffect {
                effect_id: 4,
                strength: 1,
                modifier_data: serde_json::json!({ "flags": [flag] }),
            },
        );
    }
    world.insert_resource(races);

    let mut objs = ObjectPrototypes::default();
    for n in 0..60usize {
        let mut p = object_proto(1, 100 + n as i32, ObjectType::Other);
        p.name = format!("object {n}");
        if n % 2 == 0 {
            p.examine_description = Some(format!("a closer look at {n}"));
        }
        if n % 3 == 0 {
            p.wear_flags = vec![WearFlag::Head];
        }
        if n % 5 == 0 {
            p.flags = vec![ObjectFlag::Glow];
        }
        if n % 7 == 0 {
            p.timer_hours = 24;
        }
        objs.by_key.insert((1, p.id), p);
    }
    world.insert_resource(objs);

    let mut mobs = MobPrototypes::default();
    let mut default_effects = MobDefaultEffectCatalog::default();
    for n in 1..=300usize {
        let mut p = mob_proto(1, n as i32, MobProfession::Trainer);
        p.name = format!("mob {n}");
        p.behaviors = BEHAVIORS
            .iter()
            .enumerate()
            .filter(|(i, _)| (n >> i) & 1 != 0)
            .map(|(_, b)| *b)
            .collect();
        if n % 4 == 0 {
            p.traits = vec![MobTrait::Mount];
        }
        if n % 3 == 0 {
            p.perception = 40;
        }
        if n % 7 == 0 {
            p.concealment = 30;
        }
        if n % 2 == 0 {
            p.move_points = 100;
        }
        if n % 5 == 0 {
            p.examine_description = format!("mob {n} up close");
        }
        let flags: Vec<&str> = FLAGS
            .iter()
            .enumerate()
            .filter(|(i, _)| (n.wrapping_mul(37) >> i) & 1 != 0)
            .map(|(_, f)| *f)
            .collect();
        if !flags.is_empty() {
            default_effects.by_key.insert(
                (1, n as i32),
                vec![MobDefaultEffect {
                    effect_id: 4,
                    strength: 1,
                    modifier_data: serde_json::json!({ "flags": flags }),
                }],
            );
        }
        mobs.by_key.insert((1, n as i32), p);
    }
    world.insert_resource(mobs);
    world.insert_resource(default_effects);

    let rooms: Vec<Entity> = (0..300).map(|_| world.spawn(Room).id()).collect();
    let mut resets = MobResetCatalog::default();
    let mut rows = Vec::new();
    for reset_id in 1..=400usize {
        let proto = (reset_id - 1) % 300 + 1;
        resets.entries.push(MobResetEntry {
            reset_id: reset_id as i32,
            mob_zone_id: 1,
            mob_id: proto as i32,
            room_entity: rooms[reset_id % rooms.len()],
            max_instances: 10,
        });
        for k in 0..3usize {
            rows.push(MobResetEquipment {
                id: (reset_id * 10 + k) as i32,
                reset_id: reset_id as i32,
                object_zone_id: 1,
                object_id: 100 + ((reset_id * 7 + k * 13) % 60) as i32,
                wear_location: (k == 0).then(|| "HEAD".to_string()),
                max_instances: 100_000,
                probability: 0.99,
                decorative: false,
            });
        }
    }
    world.insert_resource(resets);
    let (gear, _) =
        mud_world::reset_gear::build_gear_entries(&rows, &[], world.resource::<ObjectPrototypes>());
    let mut catalog = MobGearCatalog::default();
    catalog.by_reset.extend(gear);
    world.insert_resource(catalog);

    // Live world: mobs of other zones on ~4k archetypes, each with a
    // couple of permanent effect instances hanging off it.
    for i in 0..5500usize {
        let mob = world
            .spawn((
                Mob,
                WorldKey {
                    zone: 99,
                    id: (i % 50) as i32,
                },
                Located(rooms[i % rooms.len()]),
            ))
            .id();
        crate::archetype_noise::shape(&mut world, mob, i + 1);
        if i % 3 != 2 {
            for name in ["infravision", "bless"] {
                world.spawn((
                    EffectInstance {
                        kind: 4,
                        name: name.into(),
                        strength: 1,
                        remaining_secs: -1,
                        source: EffectSource::Other("mob_default".to_string()),
                        ability_id: None,
                    },
                    AppliedTo(mob),
                ));
            }
        }
    }
    (world, rooms)
}

/// Wipe every reset-owned mob (and what it carries) so the next sweep
/// refills them all.
fn wipe_reset_mobs(world: &mut World) {
    let mobs: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Mob>, With<FromMobReset>)>();
        q.iter(world).collect()
    };
    for mob in mobs {
        let carried: Vec<Entity> = world
            .get::<mud_world::Contents>(mob)
            .map(|c| c.iter().collect())
            .unwrap_or_default();
        for item in carried {
            world.despawn(item);
        }
        let effects: Vec<Entity> = {
            let mut q = world.query::<(Entity, &AppliedTo)>();
            q.iter(world)
                .filter(|(_, a)| a.0 == mob)
                .map(|(e, _)| e)
                .collect()
        };
        for e in effects {
            world.despawn(e);
        }
        world.despawn(mob);
    }
}

fn reset_mob_count(world: &mut World) -> usize {
    let mut q = world.query_filtered::<(), (With<Mob>, With<FromMobReset>)>();
    q.iter(world).count()
}

#[test]
fn respawn_wave_with_many_archetypes_is_fast() {
    let (mut world, _rooms) = respawn_world();
    let archetypes = world.archetypes().len();
    let mut times = Vec::new();
    for round in 0..2u64 {
        world.insert_resource(TickCount(60 * (round + 1)));
        let start = Instant::now();
        respawn_tick(&mut world);
        times.push(start.elapsed());
        assert_eq!(reset_mob_count(&mut world), 400, "every row refilled");
        wipe_reset_mobs(&mut world);
    }
    eprintln!(
        "respawn_tick 400 mobs / 1200 items, {archetypes} archetypes at start, {} at end: \
         first wave={:?} second wave={:?}",
        world.archetypes().len(),
        times[0],
        times[1],
    );
    if !cfg!(debug_assertions) {
        for t in &times {
            assert!(t.as_millis() < 60, "respawn wave took {t:?}");
        }
    }
}

/// The effects sweep on a prod-sized world: the first one after a boot has
/// to run the `on_apply` pre-pass over every permanent mob and race effect
/// (7k instances here), then every spell cast adds one more instance for
/// the next sweep to pick up.
#[test]
fn effects_tick_first_sweep_with_many_archetypes_is_fast() {
    let mut world = World::new();
    world.insert_resource(TickCount(10));
    let rooms: Vec<Entity> = (0..300).map(|_| world.spawn(Room).id()).collect();
    let mut mobs = Vec::new();
    for i in 0..5500usize {
        let mob = world.spawn((Mob, Located(rooms[i % rooms.len()]))).id();
        crate::archetype_noise::shape(&mut world, mob, i + 1);
        if i % 3 != 2 {
            for name in ["infravision", "bless"] {
                world.spawn((
                    EffectInstance {
                        kind: 4,
                        name: name.into(),
                        strength: 1,
                        remaining_secs: -1,
                        source: EffectSource::Other("mob_default".to_string()),
                        ability_id: None,
                    },
                    AppliedTo(mob),
                ));
            }
        }
        mobs.push(mob);
    }
    let archetypes = world.archetypes().len();
    let sweep = |world: &mut World, tick: u64| {
        world.insert_resource(TickCount(tick));
        let start = Instant::now();
        crate::effects::effects_tick(world);
        start.elapsed()
    };
    let first = sweep(&mut world, 10);
    let second = sweep(&mut world, 20);
    for &mob in mobs.iter().step_by(11) {
        world.spawn((
            EffectInstance {
                kind: 4,
                name: "armor".into(),
                strength: 1,
                remaining_secs: 60,
                source: EffectSource::Admin,
                ability_id: None,
            },
            AppliedTo(mob),
        ));
    }
    let after_casts = sweep(&mut world, 30);
    eprintln!(
        "effects_tick 7.3k permanent + 500 cast instances, {archetypes} archetypes: \
         first sweep={first:?} steady={second:?} after 500 casts={after_casts:?}"
    );
    if !cfg!(debug_assertions) {
        for t in [first, second, after_casts] {
            assert!(t.as_millis() < 30, "effects sweep took {t:?}");
        }
    }
}

/// A reset-owned spawn carries the same proto-derived components as a
/// plain one, plus the reset tag, and mob spawns are tagged the same way.
#[test]
fn reset_tagged_spawns_keep_every_proto_component() {
    let mut world = World::new();
    let room = world.spawn(Room).id();
    let mut proto = object_proto(1, 5, ObjectType::Other);
    proto.examine_description = Some("a closer look".into());
    proto.wear_flags = vec![WearFlag::Head];
    proto.flags = vec![ObjectFlag::Glow];
    let plain = mud_world::spawn_item_from_proto(&mut world, &proto, room);
    let tagged = mud_world::spawn_item_for_reset(&mut world, &proto, room, 77);
    assert!(world.get::<mud_world::FromObjectReset>(plain).is_none());
    assert_eq!(
        world.get::<mud_world::FromObjectReset>(tagged).unwrap().0,
        77
    );
    for e in [plain, tagged] {
        assert!(world.get::<mud_world::Description>(e).is_some());
        assert!(world.get::<mud_world::WearableIn>(e).is_some());
        assert!(world.get::<mud_world::ObjectFlags>(e).is_some());
        assert_eq!(world.get::<Located>(e).unwrap().0, room);
    }

    let mob_proto = mob_proto(1, 9, MobProfession::Trainer);
    let reset_mob = mud_world::spawn_mob_from_proto(&mut world, &mob_proto, room, Some(12));
    let loose_mob = mud_world::spawn_mob_from_proto(&mut world, &mob_proto, room, None);
    assert_eq!(world.get::<FromMobReset>(reset_mob).unwrap().0, 12);
    assert!(world.get::<FromMobReset>(loose_mob).is_none());
    for m in [reset_mob, loose_mob] {
        assert!(world.get::<mud_world::Health>(m).is_some());
        assert!(world.get::<mud_world::Sized>(m).is_some());
        assert!(world.get::<mud_world::MovementModeTag>(m).is_some());
    }
}

/// The effects sweep with the real-sized effect catalog (30 types on dev).
/// Each instance's `on_apply` / `on_tick` lookup goes through
/// `EffectCatalog::find_by_name`; most instance names (status flags such as
/// "infravision") are not catalog names at all, so each lookup is a miss.
/// Compares the same sweeps with and without the catalog to isolate that
/// cost, and times the lookups alone.
#[test]
fn effects_tick_catalog_lookup_share_of_the_sweep() {
    const CATALOG: [&str; 30] = [
        "damage",
        "heal",
        "modify",
        "status",
        "cleanse",
        "dispel",
        "reveal",
        "teleport",
        "extract",
        "move",
        "interrupt",
        "transform",
        "resurrect",
        "create",
        "summon",
        "enchant",
        "globe",
        "room",
        "inspect",
        "knockdown",
        "stun",
        "dismount",
        "stop_combat",
        "drag",
        "intercept",
        "portal",
        "conceal_item",
        "redirect",
        "Refreshed",
        "alter_object",
    ];
    let build = |with_catalog: bool| {
        let mut world = World::new();
        world.insert_resource(TickCount(10));
        if with_catalog {
            let mut catalog = EffectCatalog::default();
            for (i, name) in CATALOG.iter().enumerate() {
                let mut def = status_effect(i as i32 + 1);
                def.name = (*name).into();
                catalog.by_id.insert(def.id, def);
            }
            world.insert_resource(catalog);
        }
        let room = world.spawn(Room).id();
        let mut n = 0usize;
        for i in 0..2400usize {
            let mob = world.spawn((Mob, Located(room))).id();
            for name in FLAGS.iter().take(3 + i % 2) {
                world.spawn((
                    EffectInstance {
                        kind: 4,
                        name: (*name).into(),
                        strength: 1,
                        remaining_secs: -1,
                        source: EffectSource::Other("mob_default".to_string()),
                        ability_id: None,
                    },
                    AppliedTo(mob),
                ));
                n += 1;
            }
        }
        (world, n)
    };
    let run = |with_catalog: bool| {
        let (mut world, n) = build(with_catalog);
        let mut times = Vec::new();
        for tick in [10u64, 20, 30, 40, 50] {
            world.insert_resource(TickCount(tick));
            let start = Instant::now();
            crate::effects::effects_tick(&mut world);
            times.push(start.elapsed());
        }
        (n, times)
    };
    let (n, with) = run(true);
    let (_, without) = run(false);
    let catalog = {
        let mut c = EffectCatalog::default();
        for (i, name) in CATALOG.iter().enumerate() {
            let mut def = status_effect(i as i32 + 1);
            def.name = (*name).into();
            c.by_id.insert(def.id, def);
        }
        c
    };
    let start = Instant::now();
    let mut hits = 0usize;
    for i in 0..n {
        hits += usize::from(catalog.find_by_name(FLAGS[i % FLAGS.len()]).is_some());
    }
    let lookups = start.elapsed();
    eprintln!(
        "effects_tick {n} instances, 30-type catalog: first/steady with catalog={:?}/{:?} \
         without={:?}/{:?}; {n} find_by_name misses alone={lookups:?} (hits={hits})",
        with[0], with[4], without[0], without[4]
    );
    if !cfg!(debug_assertions) {
        assert!(with[4].as_millis() < 5, "effects sweep took {:?}", with[4]);
    }
}
