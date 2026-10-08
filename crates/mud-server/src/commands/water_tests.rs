//! "Water, no swim" movement gate (legacy `SECT_WATER` in
//! `do_simple_move`): stepping into or out of a deep-water room needs a
//! boat, flight or waterwalk; gods, AQUATIC mobs and riders on a qualifying
//! mount pass; a follower without the means stays behind.

use bevy_ecs::prelude::*;
use mud_db::enums::{
    Direction, ExitState, MobTrait, MovementMode, ObjectType, Sector, UserRole, WearFlag,
    effective_rank,
};
use mud_world::{
    Account, EquippedSlot, ExitData, Exits, Flying, Follower, Item, Located, Mob, MobTraits,
    Mounted, MovementModeTag, Named, ObjectPrototypes, Online, Player, Profile, Room, RoomSector,
    Slot, WaterWalk, WorldKey,
};

use super::Connection;
use super::test_support::{Rx, drain, object_proto};
use crate::room_access::NEED_BOAT;

struct Fx {
    world: World,
    shore: Entity,
    lake: Entity,
}

impl Fx {
    fn new() -> Self {
        let mut world = World::new();
        world.insert_resource(mud_script::LuaHost::default());
        world.insert_resource(mud_world::WorldKeyIndex::default());
        world.insert_resource(mud_world::AbilityCatalog::default());
        world.insert_resource(mud_world::EffectCatalog::default());
        world.insert_resource(mud_world::RaceCatalog::default());
        world.insert_resource(mud_world::WeatherCatalog::default());
        world.insert_resource(ObjectPrototypes::default());
        let shore = room(&mut world, 30, 1, Sector::Field);
        let lake = room(&mut world, 30, 2, Sector::Water);
        link(&mut world, shore, Direction::North, lake);
        link(&mut world, lake, Direction::South, shore);
        Self { world, shore, lake }
    }

    fn person(&mut self, name: &str, level: i32, role: UserRole) -> (Entity, Rx) {
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
        let e = self
            .world
            .spawn((
                Player,
                Online,
                Named {
                    name: name.to_string(),
                },
                Located(self.shore),
                Connection(tx),
                Account {
                    user_id: String::new(),
                    character_id: format!("c-{name}"),
                    role: effective_rank(level, role),
                    account_role: role,
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

    fn mortal(&mut self, name: &str) -> (Entity, Rx) {
        self.person(name, 20, UserRole::Player)
    }

    fn boat(&mut self, owner: Entity, id: i32, wear: Vec<WearFlag>, worn: Option<Slot>) {
        let mut proto = object_proto(30, id, ObjectType::Boat);
        proto.wear_flags = wear;
        self.world
            .resource_mut::<ObjectPrototypes>()
            .by_key
            .insert((30, id), proto);
        let item = self
            .world
            .spawn((
                Item,
                WorldKey { zone: 30, id },
                Named {
                    name: "a canoe".into(),
                },
                Located(owner),
            ))
            .id();
        if let Some(slot) = worn {
            self.world.entity_mut(item).insert(EquippedSlot(slot));
        }
    }

    fn walk(&mut self, who: Entity, dir: Direction) {
        crate::commands::cmd_move(&mut self.world, who, dir);
    }

    fn at(&self, e: Entity) -> Entity {
        self.world.get::<Located>(e).unwrap().0
    }
}

fn room(world: &mut World, zone: i32, id: i32, sector: Sector) -> Entity {
    world
        .spawn((
            Room,
            WorldKey { zone, id },
            Named {
                name: format!("Room {zone}:{id}"),
            },
            RoomSector(sector),
            Exits::default(),
        ))
        .id()
}

fn link(world: &mut World, from: Entity, dir: Direction, to: Entity) {
    world.get_mut::<Exits>(from).unwrap().0.insert(
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

#[test]
fn deep_water_is_refused_without_a_boat_waterwalk_or_flight() {
    let mut fx = Fx::new();
    let (p, mut rx) = fx.mortal("Walker");
    fx.walk(p, Direction::North);
    assert_eq!(fx.at(p), fx.shore);
    assert!(drain(&mut rx).contains("You need a boat or wings to go there."));
}

#[test]
fn leaving_deep_water_without_the_means_is_refused_too() {
    let mut fx = Fx::new();
    let (p, mut rx) = fx.mortal("Swimmer");
    fx.world.entity_mut(p).insert(Located(fx.lake));
    fx.walk(p, Direction::South);
    assert_eq!(fx.at(p), fx.lake);
    assert!(drain(&mut rx).contains("You need a boat or wings"));
}

#[test]
fn shallows_and_underwater_are_not_gated() {
    for sector in [Sector::Shallows, Sector::Underwater] {
        let mut fx = Fx::new();
        fx.world.entity_mut(fx.lake).insert(RoomSector(sector));
        let (p, _rx) = fx.mortal("Wader");
        fx.walk(p, Direction::North);
        assert_eq!(fx.at(p), fx.lake, "{sector:?}");
    }
}

#[test]
fn waterwalk_flight_and_boats_each_allow_entry() {
    let mut fx = Fx::new();
    let (walker, _r1) = fx.mortal("Walker");
    fx.world.entity_mut(walker).insert(WaterWalk);
    fx.walk(walker, Direction::North);
    assert_eq!(fx.at(walker), fx.lake, "waterwalk");

    let (flier, _r2) = fx.mortal("Flier");
    fx.world.entity_mut(flier).insert(Flying);
    fx.walk(flier, Direction::North);
    assert_eq!(fx.at(flier), fx.lake, "flying");

    let (winged, _r3) = fx.mortal("Winged");
    fx.world
        .entity_mut(winged)
        .insert(MovementModeTag(MovementMode::Flying));
    fx.walk(winged, Direction::North);
    assert_eq!(fx.at(winged), fx.lake, "flying movement mode");

    let (sailor, _r4) = fx.mortal("Sailor");
    fx.boat(sailor, 60, vec![], None);
    fx.walk(sailor, Direction::North);
    assert_eq!(fx.at(sailor), fx.lake, "carried unwearable boat");

    let (rower, _r5) = fx.mortal("Rower");
    fx.boat(rower, 61, vec![WearFlag::Offhand], Some(Slot::Hold));
    fx.walk(rower, Direction::North);
    assert_eq!(fx.at(rower), fx.lake, "worn boat");
}

#[test]
fn a_wearable_boat_must_be_worn_to_count() {
    let mut fx = Fx::new();
    let (p, _rx) = fx.mortal("Packrat");
    fx.boat(p, 61, vec![WearFlag::Offhand], None);
    fx.walk(p, Direction::North);
    assert_eq!(
        fx.at(p),
        fx.shore,
        "legacy: carried wearable boat is not enough"
    );
}

#[test]
fn gods_pass() {
    let mut fx = Fx::new();
    let (god, _rx) = fx.person("Chinok", 105, UserRole::Player);
    fx.walk(god, Direction::North);
    assert_eq!(fx.at(god), fx.lake);
}

#[test]
fn a_rider_is_carried_by_a_mount_that_can_swim_or_fly() {
    let mut fx = Fx::new();
    let (rider, _rx) = fx.mortal("Rider");
    let mount = fx
        .world
        .spawn((
            Mob,
            Named {
                name: "a horse".into(),
            },
            Located(fx.shore),
        ))
        .id();
    fx.world.entity_mut(rider).insert(Mounted(mount));
    fx.walk(rider, Direction::North);
    assert_eq!(fx.at(rider), fx.shore, "plain horse cannot cross");

    fx.world.entity_mut(mount).insert(Flying);
    fx.walk(rider, Direction::North);
    assert_eq!(fx.at(rider), fx.lake);
    assert_eq!(fx.at(mount), fx.lake, "mount travels with the rider");
}

#[test]
fn a_follower_without_the_means_stays_behind() {
    let mut fx = Fx::new();
    let (leader, _lrx) = fx.mortal("Leader");
    fx.world.entity_mut(leader).insert(WaterWalk);
    let (pup, mut prx) = fx.mortal("Pup");
    fx.world.entity_mut(pup).insert(Follower(leader));
    let (flier, _frx) = fx.mortal("Flier");
    fx.world
        .entity_mut(flier)
        .insert((Follower(leader), Flying));

    fx.walk(leader, Direction::North);
    assert_eq!(fx.at(leader), fx.lake);
    assert_eq!(fx.at(flier), fx.lake, "a follower who can fly goes along");
    assert_eq!(fx.at(pup), fx.shore, "legacy: follower is left behind");
    assert!(drain(&mut prx).contains(NEED_BOAT.trim_end()));
}

#[test]
fn aquatic_mobs_are_not_gated() {
    let mut fx = Fx::new();
    let fish = fx
        .world
        .spawn((
            Mob,
            Named {
                name: "a pike".into(),
            },
            Located(fx.lake),
            MobTraits(vec![MobTrait::Aquatic]),
        ))
        .id();
    assert!(!crate::room_access::deep_water_blocks(
        &fx.world, fish, fx.lake, fx.shore
    ));
    let cow = fx
        .world
        .spawn((
            Mob,
            Named {
                name: "a cow".into(),
            },
            Located(fx.shore),
        ))
        .id();
    assert!(crate::room_access::deep_water_blocks(
        &fx.world, cow, fx.shore, fx.lake
    ));
}
