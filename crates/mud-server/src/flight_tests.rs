//! Falling, falling to the ground, the `land` command and the
//! overweight-flier rule. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, Sector, UserRole};
use mud_world::{
    Account, AppliedTo, EffectInstance, EffectSource, ExitData, Exits, Falling, Flying, Health,
    Item, Located, Named, ObjectPrototypes, Posture, PostureKind, Profile, Room, RoomSector,
    WorldKey,
};

use super::{fall_damage, gravity_tick, on_flight_lost};
use crate::TickCount;
use crate::commands::info;
use crate::commands::test_support::{Rx, drain, object_proto, player_in};

struct Fx {
    world: World,
    /// Two air rooms stacked over a field: `sky1` -> `sky2` -> `ground`.
    sky1: Entity,
    sky2: Entity,
    ground: Entity,
    p: Entity,
    rx: Rx,
}

fn open_exit(to: Entity) -> ExitData {
    ExitData {
        to: Some(to),
        state: ExitState::Open,
        key: None,
        description: None,
        keywords: Vec::new(),
        is_hidden: false,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    }
}

fn room(world: &mut World, name: &str, sector: Sector) -> Entity {
    world
        .spawn((
            Room,
            Named { name: name.into() },
            RoomSector(sector),
            Exits::default(),
        ))
        .id()
}

fn connect(world: &mut World, from: Entity, dir: Direction, to: Entity) {
    world
        .get_mut::<Exits>(from)
        .unwrap()
        .0
        .insert(dir, open_exit(to));
}

/// A level-10 human (100 HP, 150 lb capacity) standing in `sky1`.
fn fx() -> Fx {
    let mut world = World::new();
    world.insert_resource(TickCount(0));
    world.insert_resource(ObjectPrototypes::default());
    world.insert_resource(mud_world::WeatherCatalog::default());
    crate::commands::test_support::install_core_abilities(&mut world);
    let ground = room(&mut world, "The ground", Sector::Field);
    let sky2 = room(&mut world, "Lower sky", Sector::Air);
    let sky1 = room(&mut world, "Upper sky", Sector::Air);
    connect(&mut world, sky1, Direction::Down, sky2);
    connect(&mut world, sky2, Direction::Up, sky1);
    connect(&mut world, sky2, Direction::Down, ground);
    connect(&mut world, ground, Direction::Up, sky2);
    let (p, rx) = player_in(&mut world, sky1);
    world.entity_mut(p).insert((
        Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        },
        Health { hp: 100, max: 100 },
        profile(10),
    ));
    Fx {
        world,
        sky1,
        sky2,
        ground,
        p,
        rx,
    }
}

fn profile(level: i32) -> Profile {
    Profile {
        level,
        class_id: None,
        race: "HUMAN".into(),
        experience: 0,
        gender: "male".into(),
    }
}

fn effect(world: &mut World, target: Entity, name: &str, source: EffectSource) -> Entity {
    world
        .spawn((
            EffectInstance {
                kind: 4,
                name: name.into(),
                strength: 1,
                remaining_secs: -1,
                source,
                ability_id: None,
            },
            AppliedTo(target),
        ))
        .id()
}

fn is_falling(world: &mut World, e: Entity) -> bool {
    world.get::<Falling>(e).is_some()
}

/// Tick the world until nothing is falling (bounded).
fn run_falls(world: &mut World) {
    for t in 1..400 {
        world.resource_mut::<TickCount>().0 = t;
        gravity_tick(world);
        if world.query::<&Falling>().iter(world).count() == 0 {
            return;
        }
    }
    panic!("still falling after 400 ticks");
}

/// Give `p` a heavy item: `weight` lb, carried.
fn load(world: &mut World, p: Entity, weight: f64) -> Entity {
    let mut proto = object_proto(9, 1, mud_db::enums::ObjectType::Other);
    proto.weight = weight;
    world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .insert((9, 1), proto);
    world
        .spawn((Item, WorldKey { zone: 9, id: 1 }, Located(p)))
        .id()
}

#[test]
fn fly_expiring_in_an_air_room_falls_every_room_and_hurts() {
    let mut f = fx();
    f.world.entity_mut(f.p).insert(Flying);
    let fly = effect(&mut f.world, f.p, "fly", EffectSource::Spell);
    // The spell runs out: effects_tick despawns it and tears the marker down.
    f.world.despawn(fly);
    crate::effects::teardown_markers_after_removal(&mut f.world, f.p, "fly");
    assert!(f.world.get::<Flying>(f.p).is_none());
    assert!(is_falling(&mut f.world, f.p), "fall starts when fly ends");

    run_falls(&mut f.world);
    assert_eq!(f.world.get::<Located>(f.p).unwrap().0, f.ground);
    assert!(!is_falling(&mut f.world, f.p));
    // Two rooms, medium size: 2 * (2 + 1) / 50 of 100 max HP.
    assert_eq!(f.world.get::<Health>(f.p).unwrap().hp, 88);
    assert_eq!(
        f.world.get::<Posture>(f.p).map(|p| p.0),
        Some(PostureKind::Sitting)
    );
    let out = drain(&mut f.rx);
    assert!(out.contains("fall"), "{out}");
    assert!(out.contains("DOWN!"), "{out}");
    assert!(out.contains("resounding"), "{out}");
}

#[test]
fn the_rooms_are_told_about_the_fall() {
    let mut f = fx();
    let (watcher_up, mut up_rx) = player_in(&mut f.world, f.sky1);
    let (watcher_down, mut down_rx) = player_in(&mut f.world, f.ground);
    let _ = (watcher_up, watcher_down);
    f.world.entity_mut(f.p).insert(Flying);
    f.world.entity_mut(f.p).remove::<Flying>();
    on_flight_lost(&mut f.world, f.p);
    run_falls(&mut f.world);
    let up = drain(&mut up_rx);
    assert!(up.contains("finds himself on thin air and falls"), "{up}");
    let down = drain(&mut down_rx);
    assert!(down.contains("falls screaming from above"), "{down}");
    assert!(down.contains("lands with a resounding"), "{down}");
}

#[test]
fn flight_lost_in_an_ordinary_room_drops_to_the_ground() {
    let mut f = fx();
    f.world.entity_mut(f.p).insert(Located(f.ground));
    f.world.entity_mut(f.p).insert(Flying);
    let fly = effect(&mut f.world, f.p, "fly", EffectSource::Spell);
    f.world.despawn(fly);
    crate::effects::teardown_markers_after_removal(&mut f.world, f.p, "fly");
    assert!(f.world.get::<Flying>(f.p).is_none());
    assert!(!is_falling(&mut f.world, f.p));
    assert_eq!(f.world.get::<Health>(f.p).unwrap().hp, 100, "no damage");
    assert!(drain(&mut f.rx).contains("You fall to the ground."));
}

#[test]
fn another_fly_source_keeps_the_flier_up() {
    let mut f = fx();
    f.world.entity_mut(f.p).insert(Flying);
    let spell = effect(&mut f.world, f.p, "fly", EffectSource::Spell);
    effect(
        &mut f.world,
        f.p,
        "fly",
        EffectSource::Other(mud_world::mob_effects::WORN_ITEM_EFFECT_SOURCE.into()),
    );
    f.world.despawn(spell);
    crate::effects::teardown_markers_after_removal(&mut f.world, f.p, "fly");
    assert!(f.world.get::<Flying>(f.p).is_some());
    assert!(!is_falling(&mut f.world, f.p));
    run_falls(&mut f.world);
    assert_eq!(f.world.get::<Located>(f.p).unwrap().0, f.sky1);
    assert_eq!(f.world.get::<Health>(f.p).unwrap().hp, 100);
}

#[test]
fn entering_an_air_room_without_wings_starts_a_fall_but_a_flier_hovers() {
    let mut f = fx();
    f.world.entity_mut(f.p).insert(Located(f.ground));
    crate::combat::relocate(&mut f.world, f.p, f.sky2);
    assert!(is_falling(&mut f.world, f.p));
    run_falls(&mut f.world);
    assert_eq!(f.world.get::<Located>(f.p).unwrap().0, f.ground);
    // One room: 1 * 3 / 50 of 100 HP.
    assert_eq!(f.world.get::<Health>(f.p).unwrap().hp, 94);

    f.world.entity_mut(f.p).insert(Flying);
    crate::combat::relocate(&mut f.world, f.p, f.sky2);
    assert!(!is_falling(&mut f.world, f.p));
}

#[test]
fn land_over_thin_air_is_a_fall_and_on_the_ground_is_not() {
    let mut f = fx();
    f.world.entity_mut(f.p).insert(Flying);
    info::cmd_walk(&mut f.world, f.p, "");
    assert!(f.world.get::<Flying>(f.p).is_none());
    assert!(is_falling(&mut f.world, f.p));
    run_falls(&mut f.world);
    assert_eq!(f.world.get::<Located>(f.p).unwrap().0, f.ground);

    let mut g = fx();
    g.world.entity_mut(g.p).insert((Flying, Located(g.ground)));
    info::cmd_walk(&mut g.world, g.p, "");
    assert!(g.world.get::<Flying>(g.p).is_none());
    assert!(!is_falling(&mut g.world, g.p));
    assert!(drain(&mut g.rx).contains("You touch down"));
}

#[test]
fn land_is_registered_under_the_name_land() {
    let mut f = fx();
    f.world.entity_mut(f.p).insert((Flying, Located(f.ground)));
    crate::commands::dispatch(&mut f.world, f.p, "land");
    assert!(f.world.get::<Flying>(f.p).is_none());
}

#[test]
fn overweight_flight_is_refused() {
    let mut f = fx();
    f.world.entity_mut(f.p).insert(Located(f.ground));
    effect(&mut f.world, f.p, "fly", EffectSource::Spell);
    // 150 lb capacity at level 10; the limit is 95% = 142.5.
    load(&mut f.world, f.p, 143.0);
    info::cmd_fly(&mut f.world, f.p, "");
    assert!(f.world.get::<Flying>(f.p).is_none());
    assert!(drain(&mut f.rx).contains("can't get off the ground"));

    let mut g = fx();
    g.world.entity_mut(g.p).insert(Located(g.ground));
    effect(&mut g.world, g.p, "fly", EffectSource::Spell);
    load(&mut g.world, g.p, 142.0);
    info::cmd_fly(&mut g.world, g.p, "");
    assert!(g.world.get::<Flying>(g.p).is_some(), "just under the limit");
}

#[test]
fn flying_needs_something_that_grants_it() {
    let mut f = fx();
    info::cmd_fly(&mut f.world, f.p, "");
    assert!(f.world.get::<Flying>(f.p).is_none());
    assert!(drain(&mut f.rx).contains("do not have the means to fly"));
}

#[test]
fn a_heavy_fly_spell_leaves_the_target_earthbound() {
    let mut f = fx();
    f.world.entity_mut(f.p).insert((Flying, Located(f.ground)));
    effect(&mut f.world, f.p, "fly", EffectSource::Spell);
    load(&mut f.world, f.p, 148.0);
    super::refuse_heavy_flier(&mut f.world, f.p, f.p);
    assert!(f.world.get::<Flying>(f.p).is_none());
    assert!(drain(&mut f.rx).contains("You feel somewhat lighter."));
}

#[test]
fn picking_up_too_much_grounds_a_flier_or_drops_one_from_the_sky() {
    // On the ground: a fall to the floor, no damage.
    let mut f = fx();
    f.world.entity_mut(f.p).insert((Flying, Located(f.ground)));
    effect(&mut f.world, f.p, "fly", EffectSource::Spell);
    load(&mut f.world, f.p, 146.0);
    super::after_command(&mut f.world, f.p);
    assert!(f.world.get::<Flying>(f.p).is_none());
    assert!(!is_falling(&mut f.world, f.p));
    let out = drain(&mut f.rx);
    assert!(out.contains("The spell supporting you falters"), "{out}");
    assert!(out.contains("You fall down!"), "{out}");
    assert_eq!(f.world.get::<Health>(f.p).unwrap().hp, 100);

    // In the sky: the same load is a long way down.
    let mut g = fx();
    g.world.entity_mut(g.p).insert(Flying);
    effect(
        &mut g.world,
        g.p,
        "fly",
        EffectSource::Other(mud_world::mob_effects::WORN_ITEM_EFFECT_SOURCE.into()),
    );
    load(&mut g.world, g.p, 146.0);
    super::after_command(&mut g.world, g.p);
    assert!(g.world.get::<Flying>(g.p).is_none());
    assert!(is_falling(&mut g.world, g.p));
    assert!(drain(&mut g.rx).contains("You cannot fly with so much weight!"));
    run_falls(&mut g.world);
    assert_eq!(g.world.get::<Located>(g.p).unwrap().0, g.ground);
    assert!(g.world.get::<Health>(g.p).unwrap().hp < 100);
}

#[test]
fn the_light_and_the_divine_are_never_too_heavy() {
    let mut f = fx();
    load(&mut f.world, f.p, 5.0);
    assert!(!super::too_heavy_to_fly(&mut f.world, f.p));
    let mut g = fx();
    g.world.entity_mut(g.p).insert(profile(105));
    load(&mut g.world, g.p, 9000.0);
    assert!(!super::too_heavy_to_fly(&mut g.world, g.p));
}

#[test]
fn gods_do_not_fall() {
    let mut f = fx();
    f.world.entity_mut(f.p).insert(profile(105));
    crate::combat::relocate(&mut f.world, f.p, f.sky2);
    run_falls(&mut f.world);
    assert_eq!(f.world.get::<Located>(f.p).unwrap().0, f.sky2);
}

#[test]
fn feather_fall_floats_down_unhurt() {
    let mut f = fx();
    effect(&mut f.world, f.p, "featherfall", EffectSource::Spell);
    crate::combat::relocate(&mut f.world, f.p, f.sky2);
    run_falls(&mut f.world);
    assert_eq!(f.world.get::<Located>(f.p).unwrap().0, f.ground);
    assert_eq!(f.world.get::<Health>(f.p).unwrap().hp, 100);
    let out = drain(&mut f.rx);
    assert!(out.contains("You float slowly downward."), "{out}");
    assert!(
        out.contains("You come to rest just above the ground."),
        "{out}"
    );
}

#[test]
fn a_fall_with_no_way_down_goes_nowhere() {
    let mut f = fx();
    f.world.get_mut::<Exits>(f.sky1).unwrap().0.clear();
    f.world.entity_mut(f.p).insert(Falling {
        start_room: f.sky1,
        distance: 0,
        due_tick: 0,
    });
    run_falls(&mut f.world);
    assert_eq!(f.world.get::<Located>(f.p).unwrap().0, f.sky1);
}

#[test]
fn a_fall_through_a_room_cycle_ends_after_fifty_rooms() {
    // sky1 -> b -> c -> b -> ...: never returns to the start room.
    let mut f = fx();
    let b = room(&mut f.world, "Loop B", Sector::Air);
    let c = room(&mut f.world, "Loop C", Sector::Air);
    f.world.get_mut::<Exits>(f.sky1).unwrap().0.clear();
    connect(&mut f.world, f.sky1, Direction::Down, b);
    connect(&mut f.world, b, Direction::Down, c);
    connect(&mut f.world, c, Direction::Down, b);
    f.world.entity_mut(f.p).insert(Falling {
        start_room: f.sky1,
        distance: 0,
        due_tick: 0,
    });
    run_falls(&mut f.world);
    assert!(!is_falling(&mut f.world, f.p));
    // 50 drops: sky1 -> b, then alternating c, b, ... ends on c (even count).
    assert_eq!(f.world.get::<Located>(f.p).unwrap().0, c);
    // A 50-room drop is lethal (50 * 3 / 50 of max HP): the cap ended it by
    // landing, and the landing killed.
    assert!(f.world.get::<mud_world::Ghost>(f.p).is_some());
}

#[test]
fn fall_damage_follows_the_legacy_formula() {
    // distance * (size + 1) / 50 of max HP.
    assert_eq!(fall_damage(5, 2, 100, false, false), 30);
    assert_eq!(fall_damage(5, 0, 100, false, false), 10);
    // Water takes a quarter.
    assert_eq!(fall_damage(5, 2, 100, true, false), 7);
    // Safefall: none up to 5 rooms, scaled by distance / 15 up to 15.
    assert_eq!(fall_damage(5, 2, 100, false, true), 0);
    assert_eq!(fall_damage(10, 2, 100, false, true), 40);
    assert_eq!(fall_damage(15, 2, 100, false, true), 90);
}

#[test]
fn safefall_resolved_by_name_zeroes_a_short_fall() {
    let mut f = fx();
    let safefall = f
        .world
        .resource::<mud_world::CoreAbilities>()
        .safefall
        .expect("Safefall resolves");
    f.world.entity_mut(f.p).insert((
        Flying,
        mud_world::KnownAbilities {
            entries: vec![(safefall, 500, true)],
        },
    ));
    let fly = effect(&mut f.world, f.p, "fly", EffectSource::Spell);
    f.world.despawn(fly);
    crate::effects::teardown_markers_after_removal(&mut f.world, f.p, "fly");
    run_falls(&mut f.world);
    // Two rooms with Safefall: no damage (vs 12 HP without, see above).
    assert_eq!(f.world.get::<Health>(f.p).unwrap().hp, 100);
    let out = drain(&mut f.rx);
    assert!(out.contains("tuck and roll"), "{out}");
}

#[test]
fn old_hardcoded_safefall_id_does_not_count() {
    let mut f = fx();
    // Any id other than the one resolved from the catalog is not Safefall.
    let resolved = f
        .world
        .resource::<mud_world::CoreAbilities>()
        .safefall
        .unwrap();
    f.world.entity_mut(f.p).insert((
        Flying,
        mud_world::KnownAbilities {
            entries: vec![(resolved + 1, 1000, true)],
        },
    ));
    let fly = effect(&mut f.world, f.p, "fly", EffectSource::Spell);
    f.world.despawn(fly);
    crate::effects::teardown_markers_after_removal(&mut f.world, f.p, "fly");
    run_falls(&mut f.world);
    assert_eq!(f.world.get::<Health>(f.p).unwrap().hp, 88);
}
