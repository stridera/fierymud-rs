//! Where a brand-new character starts, and where `recall` goes before they
//! bind a point of their own.
//!
//! Each race names a start room in `Races.start_room_*`. A room that would
//! strand a new player (airborne, dark, a death trap, restricted, missing,
//! or in a god zone) is never used: the race falls back to
//! [`DEFAULT_START_ROOM`], the legacy mortal start room.

use bevy_ecs::prelude::*;

use crate::{
    BaseLightLevel, DeathTrap, EntryRestriction, RaceDefaults, RoomSector, WorldKeyIndex,
    room_in_god_zone,
};

/// Legacy `mortal_start_room` (3001): the Forest Temple of Mielikki. Only
/// used when a race's own start room is unset or unusable.
pub const DEFAULT_START_ROOM: (i32, i32) = (30, 1);

/// Why `room` is not a usable start room, or `None` when it is.
#[must_use]
pub fn start_room_problem(world: &World, room: Entity) -> Option<&'static str> {
    if room_in_god_zone(world, room) {
        return Some("god zone");
    }
    let sector = world.get::<RoomSector>(room).map(|s| s.0)?;
    let light = world.get::<BaseLightLevel>(room).map_or(0, |l| l.0);
    mud_db::rooms::start_room_problem(
        sector,
        light,
        world.get::<EntryRestriction>(room).is_some(),
        world.get::<DeathTrap>(room).is_some(),
    )
}

/// The start room key for a character of `race`: the race's own room when it
/// is usable. A race that names a room which is unusable (airborne, dark,
/// restricted, missing) gets [`DEFAULT_START_ROOM`] when that is usable. A
/// race with no start room configured gets `None` (callers then fall to the
/// Void). Logs a warning when a configured room had to be replaced.
#[must_use]
pub fn race_start_room(world: &World, race: &str) -> Option<(i32, i32)> {
    let index = world.get_resource::<WorldKeyIndex>()?;
    let usable = |key: (i32, i32)| {
        index
            .rooms
            .get(&key)
            .is_some_and(|r| start_room_problem(world, *r).is_none())
    };
    let key = world
        .get_resource::<RaceDefaults>()?
        .start_room_by_race
        .get(race)
        .copied()?;
    if usable(key) {
        return Some(key);
    }
    tracing::warn!(
        race,
        zone = key.0,
        room = key.1,
        "race start room is unusable; falling back to the default start room"
    );
    usable(DEFAULT_START_ROOM).then_some(DEFAULT_START_ROOM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Located, resources::WorldKeyIndex};
    use mud_db::enums::Sector;

    fn room(world: &mut World, key: (i32, i32), sector: Sector, light: i32) -> Entity {
        let e = world
            .spawn((RoomSector(sector), BaseLightLevel(light)))
            .id();
        world.resource_mut::<WorldKeyIndex>().rooms.insert(key, e);
        e
    }

    fn world_with(races: &[(&str, (i32, i32))]) -> World {
        let mut world = World::new();
        world.insert_resource(WorldKeyIndex::default());
        let mut defaults = RaceDefaults::default();
        for (r, k) in races {
            defaults.start_room_by_race.insert((*r).into(), *k);
        }
        world.insert_resource(defaults);
        world
    }

    #[test]
    fn usable_race_room_is_kept() {
        let mut world = world_with(&[("ELF", (35, 5))]);
        room(&mut world, (35, 5), Sector::Field, 0);
        room(&mut world, DEFAULT_START_ROOM, Sector::Structure, 5);
        assert_eq!(race_start_room(&world, "ELF"), Some((35, 5)));
    }

    #[test]
    fn airborne_dark_and_missing_rooms_fall_back_to_the_default() {
        let mut world = world_with(&[
            ("SKY", (488, 60)),
            ("DEEP", (237, 22)),
            ("CAVE", (133, 1)),
            ("GONE", (999, 9)),
        ]);
        room(&mut world, (488, 60), Sector::Air, 0);
        room(&mut world, (237, 22), Sector::Underdark, -3);
        room(&mut world, (133, 1), Sector::Cave, 0);
        room(&mut world, DEFAULT_START_ROOM, Sector::Structure, 5);
        for race in ["SKY", "DEEP", "CAVE", "GONE"] {
            assert_eq!(
                race_start_room(&world, race),
                Some(DEFAULT_START_ROOM),
                "{race}"
            );
        }
        // No start room configured at all: nothing to replace.
        assert_eq!(race_start_room(&world, "UNLISTED"), None);
    }

    #[test]
    fn lit_cave_is_fine_and_no_usable_default_means_none() {
        let mut world = world_with(&[("DRAGONBORN", (52, 72))]);
        room(&mut world, (52, 72), Sector::Cave, 5);
        assert_eq!(race_start_room(&world, "DRAGONBORN"), Some((52, 72)));
        let mut world = world_with(&[("SKY", (488, 60))]);
        room(&mut world, (488, 60), Sector::Air, 0);
        assert_eq!(race_start_room(&world, "SKY"), None);
    }

    #[test]
    fn death_traps_restrictions_and_god_zones_are_refused() {
        let mut world = world_with(&[]);
        let dt = room(&mut world, (1, 1), Sector::City, 0);
        world.entity_mut(dt).insert(DeathTrap);
        assert!(start_room_problem(&world, dt).is_some());
        let restricted = room(&mut world, (1, 2), Sector::City, 0);
        world
            .entity_mut(restricted)
            .insert(EntryRestriction("return true".into()));
        assert!(start_room_problem(&world, restricted).is_some());
        let zone = world.spawn(crate::GodZone).id();
        let god = room(&mut world, (1, 3), Sector::City, 0);
        world.entity_mut(god).insert(Located(zone));
        assert_eq!(start_room_problem(&world, god), Some("god zone"));
    }
}
