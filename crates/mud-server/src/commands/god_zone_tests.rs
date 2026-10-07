//! God zones, room entry restrictions and random-teleport destination rules.
//!
//! Covers: god zones hidden from mortals (`where`, `exits`, zone lists,
//! GMCP area), no exploration credit, `Room.entry_restriction` enforced on
//! walking / summon / teleport / flee / recall (staff bypass, fails closed),
//! and the random teleport picker + spell (exclusions, zone vs world,
//! "sputters out", skill fail chance).

use std::collections::HashSet;

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Direction, ExitState, UserRole, effective_rank};
use mud_world::{
    Account, DeathTrap, EntryRestriction, ExitData, Exits, GodZone, Located, Named, NoTeleportRoom,
    Online, PeacefulRoom, Player, Profile, Room, RoomCapacity, WorldKey, WorldKeyIndex, Zone,
};

use super::test_support::{Rx, drain};
use super::{Connection, dispatch};
use crate::room_access::{
    self, RANDOM_TELEPORT_TRIES, RandomRange, entry_allowed, pick_random_destination,
};

const GOD_ONLY: &str = "return actor:is_god()";

struct Fx {
    world: World,
}

impl Fx {
    fn new() -> Self {
        let mut world = World::new();
        world.insert_resource(mud_script::LuaHost::default());
        world.insert_resource(WorldKeyIndex::default());
        world.insert_resource(mud_world::WeatherCatalog::default());
        world.insert_resource(mud_world::AbilityCatalog::default());
        world.insert_resource(mud_world::EffectCatalog::default());
        world.insert_resource(mud_world::RaceCatalog::default());
        Self { world }
    }

    fn zone(&mut self, id: i32, god: bool) -> Entity {
        let z = self
            .world
            .spawn((
                Zone,
                WorldKey { zone: id, id: 0 },
                Named {
                    name: format!("Zone {id}"),
                },
            ))
            .id();
        if god {
            self.world.entity_mut(z).insert(GodZone);
        }
        self.world
            .resource_mut::<WorldKeyIndex>()
            .zones
            .insert(id, z);
        z
    }

    fn room(&mut self, zone: Entity, zone_id: i32, id: i32) -> Entity {
        let r = self
            .world
            .spawn((
                Room,
                WorldKey { zone: zone_id, id },
                Named {
                    name: format!("Room {zone_id}:{id}"),
                },
                Located(zone),
                Exits::default(),
            ))
            .id();
        self.world
            .resource_mut::<WorldKeyIndex>()
            .rooms
            .insert((zone_id, id), r);
        r
    }

    fn link(&mut self, from: Entity, dir: Direction, to: Entity) {
        self.world.get_mut::<Exits>(from).unwrap().0.insert(
            dir,
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
            },
        );
    }

    /// A player the way `login::spawn_player` builds one: cached rank from
    /// (level, website role).
    fn person(&mut self, name: &str, level: i32, room: Entity) -> (Entity, Rx) {
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
        let e = self
            .world
            .spawn((
                Player,
                Online,
                Named {
                    name: name.to_string(),
                },
                Located(room),
                Connection(tx),
                Account {
                    user_id: String::new(),
                    character_id: format!("c-{name}"),
                    role: effective_rank(level, UserRole::Player),
                    account_role: UserRole::Player,
                    perms: vec![],
                },
                Profile {
                    level,
                    class_id: None,
                    race: "Human".into(),
                    experience: 0,
                    gender: "neutral".into(),
                },
            ))
            .id();
        (e, rx)
    }

    fn room_of(&self, e: Entity) -> Entity {
        self.world.get::<Located>(e).unwrap().0
    }
}

fn restrict(fx: &mut Fx, room: Entity, expr: &str) {
    fx.world
        .entity_mut(room)
        .insert(EntryRestriction(expr.to_string()));
}

// ---------------------------------------------------------------------------
// Visibility
// ---------------------------------------------------------------------------

#[test]
fn where_hides_god_zone_from_mortals_but_not_immortals() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let heavens = fx.zone(12, true);
    let square = fx.room(town, 30, 1);
    let hall = fx.room(heavens, 12, 4);
    let (_god, _grx) = fx.person("Chinok", 105, hall);
    let (mortal, mut mrx) = fx.person("Mortal", 20, square);
    let (imm, mut irx) = fx.person("Laoris", 100, square);

    dispatch(&mut fx.world, mortal, "where Chinok");
    let out = drain(&mut mrx);
    assert!(out.contains("isn't online"), "{out}");
    assert!(!out.contains("Room 12"), "{out}");

    dispatch(&mut fx.world, imm, "where Chinok");
    let out = drain(&mut irx);
    assert!(out.contains("Room 12:4"), "{out}");
    assert!(out.contains("[12:4]"), "{out}");
}

#[test]
fn where_own_location_omits_coordinates_for_a_mortal_in_a_god_zone() {
    let mut fx = Fx::new();
    let heavens = fx.zone(12, true);
    let hall = fx.room(heavens, 12, 4);
    let (mortal, mut mrx) = fx.person("Lost", 20, hall);
    let (imm, mut irx) = fx.person("Laoris", 100, hall);

    dispatch(&mut fx.world, mortal, "where");
    let out = drain(&mut mrx);
    assert!(out.contains("You are in: Room 12:4"), "{out}");
    assert!(!out.contains("[12:4]"), "{out}");

    dispatch(&mut fx.world, imm, "where");
    assert!(drain(&mut irx).contains("[12:4]"));
}

#[test]
fn exits_listing_hides_god_room_names_from_mortals() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let heavens = fx.zone(12, true);
    let square = fx.room(town, 30, 1);
    let hall = fx.room(heavens, 12, 4);
    fx.link(square, Direction::Up, hall);
    let (mortal, mut mrx) = fx.person("Mortal", 20, square);
    let (imm, mut irx) = fx.person("Laoris", 100, square);

    dispatch(&mut fx.world, mortal, "exits");
    let out = drain(&mut mrx);
    assert!(out.contains("No exits"), "{out}");
    assert!(!out.contains("Room 12:4"), "{out}");
    assert!(!out.contains("(beyond)"), "{out}");

    dispatch(&mut fx.world, imm, "exits");
    assert!(drain(&mut irx).contains("Room 12:4"));
}

#[test]
fn visibility_helpers_follow_rank() {
    let mut fx = Fx::new();
    let heavens = fx.zone(12, true);
    let town = fx.zone(30, false);
    let hall = fx.room(heavens, 12, 4);
    let square = fx.room(town, 30, 1);
    let (mortal, _a) = fx.person("Mortal", 20, square);
    let (imm, _b) = fx.person("Laoris", 100, square);
    assert!(!room_access::zone_visible_to(&fx.world, mortal, 12));
    assert!(room_access::zone_visible_to(&fx.world, imm, 12));
    assert!(room_access::zone_visible_to(&fx.world, mortal, 30));
    assert!(!room_access::room_visible_to(&fx.world, mortal, hall));
    assert!(room_access::room_visible_to(&fx.world, imm, hall));
    assert!(room_access::room_visible_to(&fx.world, mortal, square));
}

// ---------------------------------------------------------------------------
// Exploration credit
// ---------------------------------------------------------------------------

fn achievement_catalog(codes: &[(&str, i32)]) -> mud_world::AchievementCatalog {
    let mut cat = mud_world::AchievementCatalog::default();
    for (code, id) in codes {
        let def = mud_world::AchievementDef {
            id: *id,
            code: (*code).to_string(),
            title: format!("Walked {code}"),
            description: String::new(),
            category: mud_db::enums::AchievementCategory::Exploration,
            hidden: false,
            sort_order: 0,
        };
        cat.by_code.insert((*code).to_string(), def.clone());
        cat.by_id.insert(*id, def);
    }
    cat
}

#[test]
fn entering_a_god_zone_grants_no_exploration_credit() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let heavens = fx.zone(12, true);
    let square = fx.room(town, 30, 1);
    let hall = fx.room(heavens, 12, 4);
    fx.world.insert_resource(achievement_catalog(&[
        ("zone_30_cleared", 1),
        ("zone_12_cleared", 2),
    ]));
    let (p, _rx) = fx.person("Walker", 20, square);

    // Control: a one-room normal zone is "cleared" by visiting it.
    crate::commands::mark_room_visited(&mut fx.world, p, square);
    let unlocked = |fx: &Fx| -> HashSet<i32> {
        fx.world
            .get::<mud_world::CharacterAchievements>(p)
            .map(|c| c.unlocked.keys().copied().collect())
            .unwrap_or_default()
    };
    assert_eq!(unlocked(&fx), HashSet::from([1]));

    // A one-room god zone would clear the same way; it must not.
    crate::commands::mark_room_visited(&mut fx.world, p, hall);
    assert_eq!(unlocked(&fx), HashSet::from([1]));
    // ...even for staff.
    let (imm, _irx) = fx.person("Laoris", 100, square);
    crate::commands::mark_room_visited(&mut fx.world, imm, hall);
    assert!(
        fx.world
            .get::<mud_world::CharacterAchievements>(imm)
            .is_none_or(|c| c.unlocked.is_empty())
    );
}

// ---------------------------------------------------------------------------
// Entry restrictions
// ---------------------------------------------------------------------------

#[test]
fn walking_into_a_restricted_room_is_blocked_for_mortals() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let sanctum = fx.room(town, 30, 2);
    fx.link(square, Direction::North, sanctum);
    restrict(&mut fx, sanctum, GOD_ONLY);
    let (mortal, mut rx) = fx.person("Mortal", 20, square);

    dispatch(&mut fx.world, mortal, "north");
    let out = drain(&mut rx);
    assert!(out.contains("mysterious powerful force"), "{out}");
    assert_eq!(fx.room_of(mortal), square);
}

#[test]
fn walking_into_a_restricted_room_is_allowed_for_a_god() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let sanctum = fx.room(town, 30, 2);
    fx.link(square, Direction::North, sanctum);
    restrict(&mut fx, sanctum, GOD_ONLY);
    let (god, _rx) = fx.person("Chinok", 105, square);
    let (imm, _irx) = fx.person("Laoris", 100, square);

    assert!(entry_allowed(&mut fx.world, god, sanctum));
    assert!(entry_allowed(&mut fx.world, imm, sanctum));
    crate::commands::cmd_move(&mut fx.world, god, Direction::North);
    assert_eq!(fx.room_of(god), sanctum);
}

#[test]
fn restriction_decides_by_its_script_for_non_staff() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let vault = fx.room(town, 30, 2);
    let (mortal, _rx) = fx.person("Mortal", 20, square);
    let (veteran, _vrx) = fx.person("Veteran", 60, square);
    restrict(&mut fx, vault, "return actor.level >= 50");
    assert!(!entry_allowed(&mut fx.world, mortal, vault));
    assert!(entry_allowed(&mut fx.world, veteran, vault));
    // A bare expression (no `return`) is accepted too.
    restrict(&mut fx, vault, "actor.level >= 50");
    assert!(!entry_allowed(&mut fx.world, mortal, vault));
    assert!(entry_allowed(&mut fx.world, veteran, vault));
}

#[test]
fn an_expression_that_merely_mentions_return_is_still_an_expression() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let vault = fx.room(town, 30, 2);
    let (mortal, _rx) = fx.person("Mortal", 20, square);
    let (veteran, _vrx) = fx.person("Veteran", 60, square);
    // "return" inside a string and "returning" as an identifier part.
    restrict(&mut fx, vault, r#"actor.level >= 50 and #"return" == 6"#);
    assert!(!entry_allowed(&mut fx.world, mortal, vault));
    assert!(entry_allowed(&mut fx.world, veteran, vault));
    restrict(&mut fx, vault, "actor.level >= 50 -- no return here");
    assert!(entry_allowed(&mut fx.world, veteran, vault));
    // A real statement chunk still runs as written.
    restrict(
        &mut fx,
        vault,
        "if actor.level >= 50 then return true end return false",
    );
    assert!(!entry_allowed(&mut fx.world, mortal, vault));
    assert!(entry_allowed(&mut fx.world, veteran, vault));
}

#[test]
fn restriction_fails_closed_on_script_errors() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let vault = fx.room(town, 30, 2);
    let (mortal, _rx) = fx.person("Mortal", 20, square);
    let (god, _grx) = fx.person("Chinok", 105, square);
    for bad in [
        "return nosuchfunction()", // runtime error
        "return (",                // syntax error
        "return 42",               // non-boolean
        "",                        // nothing to evaluate
        "while true do end",       // budget exhaustion
        "error('boom')",           // explicit error
    ] {
        restrict(&mut fx, vault, bad);
        // An empty body is ignored by the loader, but if it reaches us it
        // still must not admit.
        assert!(
            !entry_allowed(&mut fx.world, mortal, vault),
            "script {bad:?} must fail closed"
        );
        // Staff bypass skips evaluation entirely.
        assert!(
            entry_allowed(&mut fx.world, god, vault),
            "staff bypass {bad:?}"
        );
    }
}

#[test]
fn restriction_fails_closed_without_a_lua_host() {
    let mut fx = Fx::new();
    fx.world.remove_resource::<mud_script::LuaHost>();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let vault = fx.room(town, 30, 2);
    restrict(&mut fx, vault, "return true");
    let (mortal, _rx) = fx.person("Mortal", 20, square);
    assert!(!entry_allowed(&mut fx.world, mortal, vault));
}

#[test]
fn followers_of_a_god_enter_only_once_the_god_is_inside() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let sanctum = fx.room(town, 30, 2);
    restrict(&mut fx, sanctum, GOD_ONLY);
    let (god, _grx) = fx.person("Chinok", 105, square);
    let (follower, _frx) = fx.person("Pet", 20, square);
    assert!(!room_access::entry_allowed_following(
        &mut fx.world,
        follower,
        sanctum,
        Some(god),
        false
    ));
    fx.world.entity_mut(god).insert(Located(sanctum));
    assert!(room_access::entry_allowed_following(
        &mut fx.world,
        follower,
        sanctum,
        Some(god),
        false
    ));
}

#[test]
fn a_mortal_following_an_immortal_into_a_god_room_is_admitted() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let sanctum = fx.room(town, 30, 2);
    fx.link(square, Direction::North, sanctum);
    restrict(&mut fx, sanctum, GOD_ONLY);
    let (god, _grx) = fx.person("Chinok", 105, square);
    let (follower, _frx) = fx.person("Pet", 20, square);
    fx.world
        .entity_mut(follower)
        .insert(mud_world::Follower(god));

    crate::commands::cmd_move(&mut fx.world, god, Direction::North);
    assert_eq!(fx.room_of(god), sanctum);
    assert_eq!(
        fx.room_of(follower),
        sanctum,
        "legacy: followers of a deity enter once the master is in"
    );
}

#[test]
fn a_mortal_following_a_mortal_into_a_restricted_room_is_refused() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let vault = fx.room(town, 30, 2);
    fx.link(square, Direction::North, vault);
    restrict(&mut fx, vault, "return actor.level >= 50");
    let (leader, _lrx) = fx.person("Veteran", 60, square);
    let (follower, mut frx) = fx.person("Pup", 20, square);
    fx.world
        .entity_mut(follower)
        .insert(mud_world::Follower(leader));

    crate::commands::cmd_move(&mut fx.world, leader, Direction::North);
    assert_eq!(fx.room_of(leader), vault);
    assert_eq!(fx.room_of(follower), square, "mortal follower stays out");
    assert!(drain(&mut frx).contains("mysterious powerful force"));
}

#[test]
fn accepting_a_summon_into_a_restricted_room_is_refused() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let sanctum = fx.room(town, 30, 2);
    restrict(&mut fx, sanctum, GOD_ONLY);
    let (god, mut grx) = fx.person("Chinok", 105, sanctum);
    let (mortal, mut mrx) = fx.person("Mortal", 20, square);
    fx.world
        .entity_mut(mortal)
        .insert(mud_world::PendingSummon {
            from: god,
            from_name: "Chinok".into(),
            dest_room: sanctum,
            dest_room_name: "Sanctum".into(),
            at: std::time::Instant::now(),
        });

    dispatch(&mut fx.world, mortal, "accept");
    assert_eq!(fx.room_of(mortal), square, "{}", drain(&mut mrx));
    assert!(fx.world.get::<mud_world::PendingSummon>(mortal).is_none());
    assert!(drain(&mut grx).contains("fizzles"));

    // A staff member accepting the same summon is let in.
    let (imm, _irx) = fx.person("Laoris", 100, square);
    fx.world.entity_mut(imm).insert(mud_world::PendingSummon {
        from: god,
        from_name: "Chinok".into(),
        dest_room: sanctum,
        dest_room_name: "Sanctum".into(),
        at: std::time::Instant::now(),
    });
    dispatch(&mut fx.world, imm, "accept");
    assert_eq!(fx.room_of(imm), sanctum);
}

#[test]
fn flee_never_picks_an_exit_into_a_restricted_room() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let sanctum = fx.room(town, 30, 2);
    let alley = fx.room(town, 30, 3);
    fx.link(square, Direction::North, sanctum);
    fx.link(square, Direction::South, alley);
    restrict(&mut fx, sanctum, GOD_ONLY);
    let (mortal, _rx) = fx.person("Mortal", 20, square);
    for _ in 0..40 {
        fx.world.entity_mut(mortal).insert(Located(square));
        dispatch(&mut fx.world, mortal, "flee");
        assert_ne!(fx.room_of(mortal), sanctum);
    }
}

// ---------------------------------------------------------------------------
// Random teleport destinations
// ---------------------------------------------------------------------------

struct Map {
    fx: Fx,
    home_zone: i32,
    here: Entity,
    ok_home: Vec<Entity>,
    ok_far: Vec<Entity>,
    bad: HashSet<Entity>,
    caster: Entity,
}

/// One caster in zone 30; zone 31 (far) has plain rooms; and a pile of rooms
/// that each break exactly one rule.
fn map() -> Map {
    let mut fx = Fx::new();
    let home = fx.zone(30, false);
    let far = fx.zone(31, false);
    let god_zone = fx.zone(12, true);
    let here = fx.room(home, 30, 0);
    let mut ok_home = vec![];
    let mut ok_far = vec![];
    for id in 1..=6 {
        ok_home.push(fx.room(home, 30, id));
    }
    for id in 1..=6 {
        ok_far.push(fx.room(far, 31, id));
    }
    let mut bad = HashSet::new();
    let mut id = 100;
    // Each rule broken once in the home zone and once in the far zone.
    for (zone, zid) in [(home, 30), (far, 31)] {
        let r = fx.room(zone, zid, id);
        fx.world.entity_mut(r).insert(NoTeleportRoom);
        bad.insert(r);
        id += 1;
        let r = fx.room(zone, zid, id);
        fx.world.entity_mut(r).insert(DeathTrap);
        bad.insert(r);
        id += 1;
        let r = fx.room(zone, zid, id);
        fx.world.entity_mut(r).insert(PeacefulRoom);
        bad.insert(r);
        id += 1;
        let r = fx.room(zone, zid, id);
        fx.world
            .entity_mut(r)
            .insert(EntryRestriction(GOD_ONLY.into()));
        bad.insert(r);
        id += 1;
        let r = fx.room(zone, zid, id);
        fx.world.entity_mut(r).insert(RoomCapacity(2)); // private
        bad.insert(r);
        id += 1;
        // Full room: capacity 1 and already occupied (and capacity is also
        // at the private threshold; use 3 with 3 occupants to isolate).
        let r = fx.room(zone, zid, id);
        fx.world.entity_mut(r).insert(RoomCapacity(3));
        for n in 0..3 {
            fx.world.spawn((
                mud_world::Mob,
                Located(r),
                Named {
                    name: format!("mob{n}"),
                },
            ));
        }
        bad.insert(r);
        id += 1;
    }
    for rid in 200..210 {
        bad.insert(fx.room(god_zone, 12, rid));
    }
    let (caster, rx) = fx.person("Mage", 30, here);
    std::mem::forget(rx);
    Map {
        fx,
        home_zone: 30,
        here,
        ok_home,
        ok_far,
        bad,
        caster,
    }
}

#[test]
fn random_teleport_never_picks_an_excluded_room() {
    let mut m = map();
    let victim = m.caster;
    let mut seen_home = HashSet::new();
    let mut seen_far = HashSet::new();
    for _ in 0..3000 {
        let dest = pick_random_destination(
            &mut m.fx.world,
            victim,
            RandomRange::World,
            RANDOM_TELEPORT_TRIES,
        )
        .expect("plenty of legal rooms");
        assert!(!m.bad.contains(&dest), "picked an excluded room");
        assert_ne!(dest, m.here, "never the current room");
        if m.ok_home.contains(&dest) {
            seen_home.insert(dest);
        } else if m.ok_far.contains(&dest) {
            seen_far.insert(dest);
        } else {
            panic!("picked a room outside the legal set");
        }
    }
    // Whole-world range really spans both zones.
    assert_eq!(seen_home.len(), m.ok_home.len());
    assert_eq!(seen_far.len(), m.ok_far.len());
}

#[test]
fn zone_range_stays_in_the_casters_zone_world_range_does_not() {
    let mut m = map();
    let victim = m.caster;
    for _ in 0..1000 {
        let dest = pick_random_destination(
            &mut m.fx.world,
            victim,
            RandomRange::Zone,
            RANDOM_TELEPORT_TRIES,
        )
        .expect("legal rooms in the home zone");
        assert_eq!(m.fx.world.get::<WorldKey>(dest).unwrap().zone, m.home_zone);
    }
    let left_zone = (0..1000).any(|_| {
        let dest = pick_random_destination(
            &mut m.fx.world,
            victim,
            RandomRange::World,
            RANDOM_TELEPORT_TRIES,
        )
        .unwrap();
        m.fx.world.get::<WorldKey>(dest).unwrap().zone != m.home_zone
    });
    assert!(left_zone, "world range should reach other zones");
}

#[test]
fn random_teleport_gives_up_when_nothing_qualifies() {
    let mut fx = Fx::new();
    let zone = fx.zone(30, false);
    let here = fx.room(zone, 30, 0);
    let only = fx.room(zone, 30, 1);
    fx.world.entity_mut(only).insert(NoTeleportRoom);
    let (p, _rx) = fx.person("Mage", 30, here);
    assert!(
        pick_random_destination(&mut fx.world, p, RandomRange::World, RANDOM_TELEPORT_TRIES)
            .is_none()
    );
    assert!(
        pick_random_destination(&mut fx.world, p, RandomRange::Zone, RANDOM_TELEPORT_TRIES)
            .is_none()
    );
}

#[test]
fn restricted_destinations_depend_on_who_is_moving() {
    let mut fx = Fx::new();
    let zone = fx.zone(30, false);
    let here = fx.room(zone, 30, 0);
    let sanctum = fx.room(zone, 30, 1);
    restrict(&mut fx, sanctum, GOD_ONLY);
    let (mortal, _mrx) = fx.person("Mortal", 20, here);
    let (god, _grx) = fx.person("Chinok", 105, here);
    assert!(
        pick_random_destination(&mut fx.world, mortal, RandomRange::World, 200).is_none(),
        "a god room is not a legal landing for a mortal"
    );
    assert_eq!(
        pick_random_destination(&mut fx.world, god, RandomRange::World, 200),
        Some(sanctum),
        "...but is for a god"
    );
}

#[test]
fn random_params_come_from_the_effect_data() {
    use serde_json::json;
    let p = room_access::parse_random_params(
        Some(
            &json!({"destination": "random", "range": "zone", "success_base_pct": 10, "success_per_skill_pct": 2}),
        ),
        Some(&json!({"destination": "home"})),
    );
    assert_eq!(p.range, RandomRange::Zone);
    assert_eq!(p.success, Some((10, 2)));
    // Missing data falls back to the legacy spell: zone-limited, with the
    // 10 + 2*skill roll. Never world-wide, never a guaranteed success.
    let p = room_access::parse_random_params(Some(&json!({"destination": "random"})), None);
    assert_eq!(p.range, RandomRange::Zone);
    assert_eq!(p.success, Some((10, 2)));
    let p = room_access::parse_random_params(None, None);
    assert_eq!(p.range, RandomRange::Zone);
    assert_eq!(p.success, Some((10, 2)));
    assert!(!room_access::teleport_roll_succeeds(&p, 0, 11));
    let p = room_access::parse_random_params(Some(&json!({"range": "nonsense"})), None);
    assert_eq!(p.range, RandomRange::Zone);
    // Legacy roll: succeed iff roll <= 10 + skill*2.
    let p = room_access::parse_random_params(
        Some(&json!({"range": "world", "success_base_pct": 10, "success_per_skill_pct": 2})),
        None,
    );
    assert!(room_access::teleport_roll_succeeds(&p, 0, 10));
    assert!(!room_access::teleport_roll_succeeds(&p, 0, 11));
    assert!(room_access::teleport_roll_succeeds(&p, 45, 100));
    assert!(!room_access::teleport_roll_succeeds(&p, 44, 100));
}

#[test]
fn no_teleport_flag_on_the_casters_own_room_pins_mortals_but_not_staff() {
    let mut fx = Fx::new();
    let zone = fx.zone(30, false);
    let here = fx.room(zone, 30, 0);
    fx.world.entity_mut(here).insert(NoTeleportRoom);
    let (mortal, _mrx) = fx.person("Mortal", 20, here);
    let (imm, _irx) = fx.person("Laoris", 100, here);
    assert!(room_access::teleport_blocked_here(&fx.world, mortal));
    assert!(!room_access::teleport_blocked_here(&fx.world, imm));
}

// ---------------------------------------------------------------------------
// The spell, end to end
// ---------------------------------------------------------------------------

const TELEPORT_ABILITY: i32 = 362;
const TELEPORT_EFFECT: i32 = 8;

/// Install the Teleport spell (data-driven: `range`, success params) and a
/// caster who knows it at `proficiency` (0..=1000 in the catalog scale).
fn teleport_fixture(fx: &mut Fx, caster: Entity, params: serde_json::Value, proficiency: i32) {
    let mut abilities = mud_world::AbilityCatalog::default();
    let mut def =
        super::test_support::ability_def(TELEPORT_ABILITY, "Teleport", AbilityKind::Spell);
    def.cast_time_rounds = 0;
    abilities.by_name.insert("teleport".to_string(), def);
    abilities
        .effects_for
        .insert(TELEPORT_ABILITY, vec![(TELEPORT_EFFECT, Some(params))]);
    fx.world.insert_resource(abilities);
    let mut effects = mud_world::EffectCatalog::default();
    effects.by_id.insert(
        TELEPORT_EFFECT,
        mud_world::EffectDef {
            id: TELEPORT_EFFECT,
            name: "teleport".into(),
            description: None,
            effect_type: "teleport".into(),
            tags: vec![],
            presence_override: None,
            default_params: serde_json::json!({"scope": "self", "destination": "home"}),
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        },
    );
    fx.world.insert_resource(effects);
    fx.world
        .entity_mut(caster)
        .insert(mud_world::KnownAbilities {
            entries: vec![(TELEPORT_ABILITY, proficiency, true)],
        });
}

fn cast_teleport(fx: &mut Fx, caster: Entity) {
    crate::commands::invoke_ability_with(
        &mut fx.world,
        caster,
        "teleport",
        AbilityKind::Spell,
        "cast",
        false,
        true,
        true,
        None,
    );
}

#[test]
fn spell_sputters_out_when_no_room_qualifies() {
    let mut fx = Fx::new();
    let zone = fx.zone(30, false);
    let here = fx.room(zone, 30, 0);
    let only = fx.room(zone, 30, 1);
    fx.world.entity_mut(only).insert(DeathTrap);
    let (caster, mut rx) = fx.person("Mage", 30, here);
    teleport_fixture(
        &mut fx,
        caster,
        serde_json::json!({"destination": "random", "range": "world"}),
        1000,
    );
    cast_teleport(&mut fx, caster);
    let out = drain(&mut rx);
    assert!(out.contains("The spell sputters out."), "{out}");
    assert_eq!(fx.room_of(caster), here);
}

#[test]
fn spell_can_fail_on_the_skill_roll() {
    let mut fx = Fx::new();
    let zone = fx.zone(30, false);
    let here = fx.room(zone, 30, 0);
    let _there = fx.room(zone, 30, 1);
    let (caster, mut rx) = fx.person("Mage", 30, here);
    // base 0, 0 per skill: the roll (1..=100) can never succeed.
    teleport_fixture(
        &mut fx,
        caster,
        serde_json::json!({"destination": "random", "range": "world",
                           "success_base_pct": 0, "success_per_skill_pct": 0}),
        1000,
    );
    cast_teleport(&mut fx, caster);
    let out = drain(&mut rx);
    assert!(out.contains("swirls about and dies away"), "{out}");
    assert_eq!(fx.room_of(caster), here);
}

#[test]
fn spell_teleports_within_the_zone_and_never_into_excluded_rooms() {
    let mut fx = Fx::new();
    let home = fx.zone(30, false);
    let far = fx.zone(31, false);
    let heavens = fx.zone(12, true);
    let here = fx.room(home, 30, 0);
    let good = fx.room(home, 30, 1);
    let dt = fx.room(home, 30, 2);
    fx.world.entity_mut(dt).insert(DeathTrap);
    let godroom = fx.room(home, 30, 3);
    restrict(&mut fx, godroom, GOD_ONLY);
    let _far_room = fx.room(far, 31, 1);
    let _hall = fx.room(heavens, 12, 1);
    let (caster, mut rx) = fx.person("Mage", 30, here);
    // Skill 100 -> 10 + 200 >= 100: always succeeds.
    teleport_fixture(
        &mut fx,
        caster,
        serde_json::json!({"destination": "random", "range": "zone",
                           "success_base_pct": 10, "success_per_skill_pct": 2}),
        1000,
    );
    for _ in 0..30 {
        fx.world.entity_mut(caster).insert(Located(here));
        cast_teleport(&mut fx, caster);
        assert_eq!(fx.room_of(caster), good, "{}", drain(&mut rx));
    }
}

#[test]
fn spell_is_smothered_in_a_no_teleport_room() {
    let mut fx = Fx::new();
    let zone = fx.zone(30, false);
    let here = fx.room(zone, 30, 0);
    fx.world.entity_mut(here).insert(NoTeleportRoom);
    let _there = fx.room(zone, 30, 1);
    let (caster, mut rx) = fx.person("Mage", 30, here);
    teleport_fixture(
        &mut fx,
        caster,
        serde_json::json!({"destination": "random", "range": "world"}),
        1000,
    );
    cast_teleport(&mut fx, caster);
    let out = drain(&mut rx);
    assert!(out.contains("smothers the spell"), "{out}");
    assert_eq!(fx.room_of(caster), here);
}

#[test]
fn is_god_is_a_real_lua_method_not_a_script_error() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let (mortal, _mrx) = fx.person("Mortal", 20, square);
    let (god, _grx) = fx.person("Chinok", 105, square);
    let mut host = mud_script::LuaHost::default();
    for (who, expected) in [(mortal, false), (god, true)] {
        let (_out, value) = host
            .exec_for_event_with_value(&mut fx.world, who, who, None, "return actor:is_god()", &[])
            .expect("is_god must not raise");
        assert_eq!(value, Some(expected));
    }
}

// ---------------------------------------------------------------------------
// Review follow-ups: read-only restriction scripts, look/exit hiding,
// login room, is_god, caster skill
// ---------------------------------------------------------------------------

#[test]
fn restriction_scripts_cannot_act_yield_or_leave_a_paused_script() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let elsewhere = fx.room(town, 30, 9);
    let square = fx.room(town, 30, 1);
    let vault = fx.room(town, 30, 2);
    let (mortal, mut rx) = fx.person("Mortal", 20, square);
    for script in [
        "wait(1) return true",
        "coroutine.yield(1) return true",
        "actor:teleport(30, 9) return true",
        "actor:send('hi') return true",
        "actor:command('say hi') return true",
        "actor:award_exp(1000) return true",
        "world.destroy(actor) return true",
        "spells.cast(actor, 'fireball') return true",
        "skills.execute(actor, 'kick', nil) return true",
        "actor.level = 99 return true",
        "return true, wait(1)",
    ] {
        restrict(&mut fx, vault, script);
        assert!(
            !entry_allowed(&mut fx.world, mortal, vault),
            "{script:?} must refuse"
        );
        assert_eq!(fx.room_of(mortal), square, "{script:?}");
        assert_eq!(
            fx.world.resource::<mud_script::LuaHost>().yielded_count(),
            0,
            "{script:?} left a paused script behind"
        );
        crate::commands::drain_lua_outbox(&mut fx.world);
        assert_eq!(drain(&mut rx), "", "{script:?} had a side effect");
    }
    let _ = elsewhere;
    assert_eq!(
        fx.world.get::<Profile>(mortal).unwrap().experience,
        0,
        "award_exp must not have run"
    );
    // The read accessors still work.
    restrict(
        &mut fx,
        vault,
        "return actor.is_player and actor.level < 50 and room.zone_id == 30 and room.id == 2",
    );
    assert!(entry_allowed(&mut fx.world, mortal, vault));
}

#[test]
fn read_only_restriction_still_has_the_instruction_budget() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let vault = fx.room(town, 30, 2);
    let (mortal, _rx) = fx.person("Mortal", 20, square);
    restrict(&mut fx, vault, "while true do end return true");
    assert!(!entry_allowed(&mut fx.world, mortal, vault));
    // The host is usable afterwards.
    restrict(&mut fx, vault, "return true");
    assert!(entry_allowed(&mut fx.world, mortal, vault));
}

#[test]
fn look_direction_into_a_god_zone_shows_nothing_to_mortals() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let heavens = fx.zone(12, true);
    let square = fx.room(town, 30, 1);
    let hall = fx.room(heavens, 12, 4);
    fx.world
        .entity_mut(hall)
        .insert(mud_world::Description("A shining hall.".into()));
    fx.link(square, Direction::Up, hall);
    let (mortal, mut mrx) = fx.person("Mortal", 20, square);
    let (imm, mut irx) = fx.person("Laoris", 100, square);

    dispatch(&mut fx.world, mortal, "look up");
    let out = drain(&mut mrx);
    assert!(out.contains("You see nothing in that direction"), "{out}");
    assert!(
        !out.contains("Room 12") && !out.contains("shining"),
        "{out}"
    );

    dispatch(&mut fx.world, imm, "look up");
    assert!(!drain(&mut irx).contains("nothing in that direction"));
}

#[test]
fn mortals_cannot_walk_into_god_zone_rooms_even_without_a_restriction() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let heavens = fx.zone(12, true);
    let square = fx.room(town, 30, 1);
    let hall = fx.room(heavens, 12, 4);
    fx.link(square, Direction::Up, hall);
    let (mortal, mut mrx) = fx.person("Mortal", 20, square);
    dispatch(&mut fx.world, mortal, "up");
    assert!(drain(&mut mrx).contains("You can't go that way"));
    assert_eq!(fx.room_of(mortal), square);
    let (imm, _irx) = fx.person("Laoris", 100, square);
    crate::commands::cmd_move(&mut fx.world, imm, Direction::Up);
    assert_eq!(fx.room_of(imm), hall);
}

#[test]
fn login_room_falls_back_to_recall_for_mortals_in_restricted_or_god_rooms() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let heavens = fx.zone(12, true);
    let saved_plain = fx.room(town, 30, 1);
    let recall = fx.room(town, 30, 2);
    let race_start = fx.room(town, 30, 3);
    let sanctum = fx.room(town, 30, 4);
    restrict(&mut fx, sanctum, GOD_ONLY);
    let hall = fx.room(heavens, 12, 4);
    let _ = (saved_plain, race_start);
    let w = &fx.world;
    let go = |wanted, is_staff| {
        crate::login::resolve_login_room(w, wanted, Some(recall), Some((30, 3)), is_staff)
    };
    assert_eq!(go((30, 1), false), Some(saved_plain), "ordinary room kept");
    assert_eq!(go((30, 4), false), Some(recall), "restricted -> recall");
    assert_eq!(go((12, 4), false), Some(recall), "god zone -> recall");
    assert_eq!(go((30, 4), true), Some(sanctum), "staff keep their room");
    assert_eq!(go((12, 4), true), Some(hall));
    // No recall point: the race start room is next.
    assert_eq!(
        crate::login::resolve_login_room(w, (30, 4), None, Some((30, 3)), false),
        Some(race_start)
    );
}

#[test]
fn is_god_is_staff_characters_only_never_high_level_mobs() {
    let mut fx = Fx::new();
    let town = fx.zone(30, false);
    let square = fx.room(town, 30, 1);
    let (god, _grx) = fx.person("Chinok", 105, square);
    let (mortal, _mrx) = fx.person("Mortal", 20, square);
    // A level-110 mob (Profile, no Account) and a bare Profile entity.
    let mob = fx
        .world
        .spawn((
            mud_world::Mob,
            Named {
                name: "a titan".into(),
            },
            Located(square),
            Profile {
                level: 110,
                class_id: None,
                race: "Giant".into(),
                experience: 0,
                gender: "neutral".into(),
            },
        ))
        .id();
    let mut host = mud_script::LuaHost::default();
    for (who, expected) in [(god, true), (mortal, false), (mob, false)] {
        let (_out, value) = host
            .exec_for_event_with_value(&mut fx.world, who, who, None, "return actor:is_god()", &[])
            .unwrap();
        assert_eq!(value, Some(expected));
        assert_eq!(
            host.eval_condition(&mut fx.world, who, None, "return actor:is_god()"),
            Ok(expected)
        );
    }
    // The mob is refused by a god-only room (no staff bypass for mobs).
    let vault = fx.room(town, 30, 2);
    restrict(&mut fx, vault, GOD_ONLY);
    assert!(!entry_allowed(&mut fx.world, mob, vault));
}

#[test]
fn non_player_casters_use_their_level_as_teleport_skill() {
    // A scroll user / mob / Lua cast has no proficiency row: level counts.
    assert_eq!(room_access::effective_teleport_skill(None, 30), 30);
    assert_eq!(room_access::effective_teleport_skill(None, 250), 100);
    assert_eq!(room_access::effective_teleport_skill(None, -3), 0);
    // A caster who knows the spell uses their proficiency, even when it is 0.
    assert_eq!(room_access::effective_teleport_skill(Some(0), 30), 0);
    assert_eq!(room_access::effective_teleport_skill(Some(77), 30), 77);
}

#[test]
fn teleport_from_a_scroll_by_an_unskilled_caster_uses_level() {
    let mut fx = Fx::new();
    let zone = fx.zone(30, false);
    let here = fx.room(zone, 30, 0);
    let there = fx.room(zone, 30, 1);
    let (caster, mut rx) = fx.person("Scribe", 50, here);
    // No KnownAbilities row for the spell (scroll use): level 50 -> 10 + 100
    // >= 100, so the 10 + 2*skill roll can never fail. With skill 0 it would
    // fail ~90% of the time, so 40 straight successes proves the level is used.
    teleport_fixture(
        &mut fx,
        caster,
        serde_json::json!({"destination": "random", "range": "zone"}),
        0,
    );
    fx.world
        .entity_mut(caster)
        .remove::<mud_world::KnownAbilities>();
    for _ in 0..40 {
        fx.world.entity_mut(caster).insert(Located(here));
        cast_teleport(&mut fx, caster);
        assert_eq!(fx.room_of(caster), there, "{}", drain(&mut rx));
    }
}
