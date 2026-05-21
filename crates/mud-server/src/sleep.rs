use bevy_ecs::prelude::*;
use mud_db::enums::Sector;
use mud_world::{
    Fighting, Located, Mob, MudClock, Named, Posture, PostureKind, RiddenBy, RoomSector,
};

use crate::TickCount;
use crate::commands::{
    broadcast_room_except_players_rendered, broadcast_room_except_rendered, cap_sentence_start,
    sector_is_outdoor_for_weather,
};
use mud_world::Player;

/// Hour the village shutters and bandits curl up — also when the
/// dark-room gate kicks in (`commands::room_is_dark`). Keep these
/// two windows aligned so "it just got dark" and "the wildlife
/// settled in" land on the same tick.
const NIGHT_START_HOUR: i32 = 22;
/// Hour mobs rouse for the day. The dark-room gate flips back at
/// hour 5; mobs wake the same tick to keep the world consistent.
const MORNING_HOUR: i32 = 5;

/// Marker for mobs put to sleep by the day/night cycle. Lets the
/// morning sweep distinguish "I tucked this mob in last night" from
/// "this mob is sleeping because of a sleep spell" — we never wake
/// the latter at sunrise. Combat's "jolt awake" path leaves the
/// marker dangling on a now-Standing mob; the morning sweep cleans
/// it up either way.
#[derive(Component, Debug, Clone, Copy)]
pub struct SleptByNight;

/// At hour 22 outdoor mobs lie down, at hour 5 they get up. Runs only
/// on game-hour boundaries (every 750 ticks, same cadence as the
/// clock). Skipped mobs: anything Fighting, anything currently being
/// ridden, and at sleep time anything already asleep (so spell-sleep
/// stays load-bearing for the marker check at morning).
pub fn mob_sleep_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(750) {
        return;
    }
    let hour = world.resource::<MudClock>().hour;
    match hour {
        NIGHT_START_HOUR => {
            sleep_outdoor_mobs(world);
            announce_to_outdoor_players(
                world,
                "The sun dips below the horizon, and darkness gathers.\r\n",
            );
        }
        MORNING_HOUR => {
            wake_night_sleepers(world);
            announce_to_outdoor_players(
                world,
                "The sky pales and the sun begins to rise.\r\n",
            );
        }
        _ => {}
    }
}

/// Broadcast a single line to every player currently standing in an
/// outdoor room. Skips dungeons/caves/underdark (no sky to see) and
/// indoor structures. Used for the sunrise / sunset transitions.
fn announce_to_outdoor_players(world: &mut World, msg: &str) {
    // Distinct rooms each get one broadcast call (which itself sends
    // per-player), so dedupe before iterating.
    let rooms: Vec<Entity> = {
        let mut q = world.query_filtered::<&Located, With<Player>>();
        let mut seen = std::collections::HashSet::new();
        q.iter(world)
            .map(|l| l.0)
            .filter(|r| seen.insert(*r))
            .collect()
    };
    for room in rooms {
        let outdoor = world
            .get::<RoomSector>(room)
            .is_some_and(|s| sector_is_outdoor_for_weather(s.0));
        if !outdoor {
            continue;
        }
        broadcast_room_except_players_rendered(world, room, &[], msg);
    }
}

fn sleep_outdoor_mobs(world: &mut World) {
    let candidates: Vec<(Entity, Entity)> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &Posture),
            (With<Mob>, Without<Fighting>, Without<RiddenBy>),
        >();
        q.iter(world)
            .filter(|(_, _, posture)| posture.0 != PostureKind::Sleeping)
            .map(|(e, l, _)| (e, l.0))
            .collect()
    };
    for (mob, room) in candidates {
        // Cities still get dark, but city-dweller mobs (guards,
        // shopkeepers) staying alert at night reads better than
        // them snoring on the cobblestones. Cave/dungeon mobs are
        // excluded by `sector_is_outdoor_for_weather` already —
        // they live on a different cycle.
        let sector = world.get::<RoomSector>(room).map(|s| s.0);
        let eligible =
            sector.is_some_and(|s| sector_is_outdoor_for_weather(s) && !is_settlement(s));
        if !eligible {
            continue;
        }
        if let Ok(mut em) = world.get_entity_mut(mob) {
            em.insert((Posture(PostureKind::Sleeping), SleptByNight));
        }
        let name = world
            .get::<Named>(mob)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        if !name.is_empty() {
            broadcast_room_except_rendered(
                world,
                room,
                &[mob],
                &cap_sentence_start(&format!("{name} settles down to sleep.\r\n")),
            );
        }
    }
}

fn wake_night_sleepers(world: &mut World) {
    let to_wake: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Mob>, With<SleptByNight>)>();
        q.iter(world).collect()
    };
    for mob in to_wake {
        let was_sleeping =
            world.get::<Posture>(mob).map(|p| p.0) == Some(PostureKind::Sleeping);
        let in_combat = world.get::<Fighting>(mob).is_some();
        let room = world.get::<Located>(mob).map(|l| l.0);
        if let Ok(mut em) = world.get_entity_mut(mob) {
            em.remove::<SleptByNight>();
            if was_sleeping && !in_combat {
                em.insert(Posture(PostureKind::Standing));
            }
        }
        if was_sleeping && !in_combat
            && let Some(room) = room
        {
            let name = world
                .get::<Named>(mob)
                .map(|n| n.name.clone())
                .unwrap_or_default();
            if !name.is_empty() {
                broadcast_room_except_rendered(
                    world,
                    room,
                    &[mob],
                    &cap_sentence_start(&format!("{name} wakes and stretches.\r\n")),
                );
            }
        }
    }
}

/// Sectors with permanent residents (city streets, ruins) — wildlife
/// in these doesn't make sense as a thing to put to sleep.
fn is_settlement(sector: Sector) -> bool {
    matches!(sector, Sector::City | Sector::Ruins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::{Located, Mob, Named, Posture, PostureKind, Room, RoomSector};

    fn make_world() -> (World, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(TickCount(0));
        world.insert_resource(MudClock {
            year: 1,
            month: 1,
            day: 1,
            hour: 12,
            minute: 0,
            stamp: 0,
        });
        let room = world.spawn((Room, RoomSector(Sector::Field))).id();
        let mob = world
            .spawn((
                Mob,
                Named { name: "a wolf".into() },
                Located(room),
                Posture(PostureKind::Standing),
            ))
            .id();
        (world, room, mob)
    }

    #[test]
    fn outdoor_mob_sleeps_at_night_and_wakes_at_morning() {
        let (mut world, _room, mob) = make_world();

        // Tick to hour 22 alignment.
        world.resource_mut::<MudClock>().hour = NIGHT_START_HOUR;
        world.resource_mut::<TickCount>().0 = 750;
        mob_sleep_tick(&mut world);
        assert_eq!(
            world.get::<Posture>(mob).map(|p| p.0),
            Some(PostureKind::Sleeping),
            "outdoor mob should be asleep at night"
        );
        assert!(world.get::<SleptByNight>(mob).is_some());

        world.resource_mut::<MudClock>().hour = MORNING_HOUR;
        world.resource_mut::<TickCount>().0 = 1500;
        mob_sleep_tick(&mut world);
        assert_eq!(
            world.get::<Posture>(mob).map(|p| p.0),
            Some(PostureKind::Standing),
            "outdoor mob should wake at morning"
        );
        assert!(world.get::<SleptByNight>(mob).is_none());
    }

    #[test]
    fn cave_mob_stays_awake_at_night() {
        let mut world = World::new();
        world.insert_resource(TickCount(750));
        world.insert_resource(MudClock {
            year: 1,
            month: 1,
            day: 1,
            hour: NIGHT_START_HOUR,
            minute: 0,
            stamp: 0,
        });
        let room = world.spawn((Room, RoomSector(Sector::Cave))).id();
        let mob = world
            .spawn((
                Mob,
                Named { name: "a goblin".into() },
                Located(room),
                Posture(PostureKind::Standing),
            ))
            .id();
        mob_sleep_tick(&mut world);
        assert_eq!(
            world.get::<Posture>(mob).map(|p| p.0),
            Some(PostureKind::Standing),
            "cave mobs are not subject to day/night sleeping"
        );
        assert!(world.get::<SleptByNight>(mob).is_none());
    }

    #[test]
    fn fighting_mob_does_not_sleep() {
        let (mut world, _room, mob) = make_world();
        world.get_entity_mut(mob).unwrap().insert(Fighting(mob));
        world.resource_mut::<MudClock>().hour = NIGHT_START_HOUR;
        world.resource_mut::<TickCount>().0 = 750;
        mob_sleep_tick(&mut world);
        assert_eq!(
            world.get::<Posture>(mob).map(|p| p.0),
            Some(PostureKind::Standing),
            "fighting mob should not be put to sleep"
        );
    }

    #[test]
    fn morning_does_not_wake_spell_sleepers() {
        let (mut world, _room, mob) = make_world();
        // Spell sleep — Posture flipped, no SleptByNight marker.
        world
            .get_entity_mut(mob)
            .unwrap()
            .insert(Posture(PostureKind::Sleeping));
        world.resource_mut::<MudClock>().hour = MORNING_HOUR;
        world.resource_mut::<TickCount>().0 = 750;
        mob_sleep_tick(&mut world);
        assert_eq!(
            world.get::<Posture>(mob).map(|p| p.0),
            Some(PostureKind::Sleeping),
            "spell-induced sleep must survive sunrise"
        );
    }

    #[test]
    fn off_boundary_tick_is_noop() {
        let (mut world, _room, mob) = make_world();
        world.resource_mut::<MudClock>().hour = NIGHT_START_HOUR;
        world.resource_mut::<TickCount>().0 = 749;
        mob_sleep_tick(&mut world);
        assert_eq!(
            world.get::<Posture>(mob).map(|p| p.0),
            Some(PostureKind::Standing),
            "system runs only on game-hour boundaries"
        );
    }
}
