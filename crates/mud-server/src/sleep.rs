use std::collections::HashSet;

use bevy_ecs::prelude::*;
use mud_db::enums::Sector;
use mud_world::{
    Fighting, Located, Mob, MudClock, Named, Posture, PostureKind, RiddenBy, RoomSector, SnoopedBy,
    SwitchedFrom,
};

use crate::TickCount;
use crate::commands::{
    Connection, broadcast_room_except_players_rendered, broadcast_room_except_rendered,
    cap_sentence_start, sector_is_outdoor_for_weather,
};
use mud_world::Player;

/// Hour night falls — also when the dark-room gate kicks in
/// (`commands::room_is_dark`). Keep these two windows aligned so "it just
/// got dark" and "the wildlife settled in" land on the same tick.
const NIGHT_START_HOUR: i32 = 22;
/// Hour mobs rouse for the day. The dark-room gate flips back at
/// hour 5; mobs wake the same tick to keep the world consistent.
const MORNING_HOUR: i32 = 5;

/// Chance, per eligible mob per night-hour sleep check, that it lies down.
/// Spread over the seven night hours this staggers bedtime so a whole
/// field does not drop at the same instant (#132).
const FALL_ASLEEP_PERCENT: u32 = 35;

/// Overrides [`FALL_ASLEEP_PERCENT`] (tests pin it to 100 for determinism).
#[derive(Resource, Debug, Clone, Copy)]
pub struct NightSleepChance(pub u32);

/// True for the game hours between nightfall and the morning wake-up.
const fn is_night(hour: i32) -> bool {
    hour >= NIGHT_START_HOUR || hour < MORNING_HOUR
}

/// Mobs put to sleep by the day/night cycle. Lets the morning sweep
/// distinguish "I tucked this mob in last night" from "this mob is
/// sleeping because of a sleep spell" — we never wake the latter at
/// sunrise. Combat's "jolt awake" path leaves the entry dangling on a
/// now-Standing mob; the morning sweep cleans it up either way.
///
/// A resource, not a marker component: tagging thousands of mobs at once
/// moved each to a new archetype, and the first move out of each of a prod
/// world's thousands of archetypes builds the target archetype and its
/// table (a 240-310 ms stall on the first sweep after every boot).
#[derive(Resource, Debug, Default)]
pub struct NightSleepers(HashSet<Entity>);

impl NightSleepers {
    pub fn contains(&self, mob: Entity) -> bool {
        self.0.contains(&mob)
    }
}

/// Night sleep is opt-in (#132): only mobs of a zone with
/// `Zones.mobs_sleep_at_night`, or carrying the `SleepsAtNight` behavior
/// (`AwakeAtNight` opts out of an on zone), take part
/// ([`mud_world::mob_sleeps_at_night`]). Runs only on game-hour boundaries
/// (every 750 ticks, same cadence as the clock). Every night hour (22-04)
/// is a sleep check: each eligible standing mob in an outdoor, non-settlement
/// room rolls [`FALL_ASLEEP_PERCENT`] to lie down where it stands, so bedtime
/// is staggered and a mob that was busy at one check gets another a game hour
/// later if it is still night. A mob never lies down while fighting, being
/// ridden, hunting (hate list), or with a hostile in the room; spell-sleepers
/// are left alone, and only the mobs tucked in here wake at hour 5.
pub fn mob_sleep_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(750) {
        return;
    }
    let hour = world.resource::<MudClock>().hour;
    world.init_resource::<NightSleepers>();
    if hour == MORNING_HOUR {
        wake_night_sleepers(world);
        announce_to_outdoor_players(world, "The sky pales and the sun begins to rise.\r\n");
    } else if is_night(hour) {
        sleep_outdoor_mobs(world);
        if hour == NIGHT_START_HOUR {
            announce_to_outdoor_players(
                world,
                "The sun dips below the horizon, and darkness gathers.\r\n",
            );
        }
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

/// Rooms where a broadcast line can actually be received: those holding
/// an entity with a live `Connection`, a switch puppet, or a snooper
/// watching it (the only ways `send_to` delivers anything). Mobs lying
/// down or getting up elsewhere need no line, so the per-room audience
/// scan is skipped for them. Nobody moves during the sweep, so one
/// snapshot serves it.
fn listener_rooms(world: &mut World) -> HashSet<Entity> {
    let mut q = world
        .query_filtered::<&Located, Or<(With<Connection>, With<SwitchedFrom>, With<SnoopedBy>)>>();
    q.iter(world).map(|l| l.0).collect()
}

/// True when `mob` has a reason to stay on its feet: it is hunting (a
/// non-empty hate list), it is a servant, a fight is going on in its room, or
/// a player it would attack is standing there (one it remembers, or any
/// player when the mob is aggressive by alignment or aggression formula).
fn has_hostile_nearby(world: &World, mob: Entity, room: Entity) -> bool {
    if world
        .get::<crate::combat::HateList>(mob)
        .is_some_and(|h| !h.0.is_empty())
        || crate::commands::is_servant(world, mob)
    {
        return true;
    }
    if crate::room_index::contents_of(world, room).any(|e| world.get::<Fighting>(e).is_some()) {
        return true;
    }
    let players = crate::room_index::players_in(world, room);
    if players.is_empty() {
        return false;
    }
    if let Some(mem) = world.get::<crate::combat::MobMemory>(mob)
        && players.iter().any(|p| mem.0.contains(p))
    {
        return true;
    }
    let aggressive_by_alignment = world
        .get::<mud_world::CombatStats>(mob)
        .is_some_and(|c| c.alignment <= crate::commands::aggro_alignment(world));
    let aggressive_by_formula = world.get::<mud_world::WorldKey>(mob).is_some_and(|k| {
        world
            .get_resource::<mud_world::MobPrototypes>()
            .and_then(|p| p.by_key.get(&(k.zone, k.id)))
            .is_some_and(|p| p.aggression_formula.is_some())
    });
    aggressive_by_alignment || aggressive_by_formula
}

fn sleep_outdoor_mobs(world: &mut World) {
    let percent = world
        .get_resource::<NightSleepChance>()
        .map_or(FALL_ASLEEP_PERCENT, |c| c.0);
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
    let candidates: Vec<(Entity, Entity)> = candidates
        .into_iter()
        .filter(|&(mob, _)| mud_world::mob_sleeps_at_night(world, mob))
        .collect();
    if candidates.is_empty() {
        return;
    }
    let listeners = listener_rooms(world);
    let mut tucked_in = Vec::new();
    for (mob, room) in candidates {
        // Cities still get dark, but city-dweller mobs (guards,
        // shopkeepers) staying alert at night reads better than
        // them snoring on the cobblestones. Cave/dungeon mobs are
        // excluded by `sector_is_outdoor_for_weather` already —
        // they live on a different cycle.
        let sector = world.get::<RoomSector>(room).map(|s| s.0);
        let eligible =
            sector.is_some_and(|s| sector_is_outdoor_for_weather(s) && !is_settlement(s));
        if !eligible || rand::random_range(0..100u32) >= percent {
            continue;
        }
        if has_hostile_nearby(world, mob, room) {
            continue;
        }
        if let Ok(mut em) = world.get_entity_mut(mob) {
            em.insert(Posture(PostureKind::Sleeping));
            tucked_in.push(mob);
        }
        if !listeners.contains(&room) {
            continue;
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
    let mut night = world.resource_mut::<NightSleepers>();
    night.0.extend(tucked_in);
}

fn wake_night_sleepers(world: &mut World) {
    // Drained, not iterated: the sweep ends with nobody tagged, and
    // despawned mobs (killed overnight) drop out with the rest.
    let to_wake: Vec<Entity> = std::mem::take(&mut world.resource_mut::<NightSleepers>().0)
        .into_iter()
        .filter(|&mob| world.get::<Mob>(mob).is_some())
        .collect();
    let listeners = listener_rooms(world);
    for mob in to_wake {
        let was_sleeping = world.get::<Posture>(mob).map(|p| p.0) == Some(PostureKind::Sleeping);
        let in_combat = world.get::<Fighting>(mob).is_some();
        let room = world.get::<Located>(mob).map(|l| l.0);
        if was_sleeping
            && !in_combat
            && let Ok(mut em) = world.get_entity_mut(mob)
        {
            em.insert(Posture(PostureKind::Standing));
        }
        if was_sleeping
            && !in_combat
            && let Some(room) = room
            && listeners.contains(&room)
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
    use mud_db::enums::MobBehavior;
    use mud_world::{
        Item, Located, Mob, MobBehaviors, MobsSleepAtNight, Named, Posture, PostureKind, Room,
        RoomSector, WorldKey, WorldKeyIndex, Zone,
    };

    /// A zone entity (schema id 7) registered in the key index; `sleeps` is its
    /// `mobs_sleep_at_night` flag.
    fn add_zone(world: &mut World, sleeps: bool) {
        let zone = world.spawn((Zone, WorldKey { zone: 7, id: 0 })).id();
        if sleeps {
            world.entity_mut(zone).insert(MobsSleepAtNight);
        }
        world
            .get_resource_or_insert_with(WorldKeyIndex::default)
            .zones
            .insert(7, zone);
    }

    /// Standing mob "a wolf" from zone 7 in a Field room, in a zone whose night
    /// sleep flag is `zone_sleeps`. Every sleep check lands (chance 100%).
    fn make_world_with(zone_sleeps: bool) -> (World, Entity, Entity) {
        let (mut world, room, mob) = make_world_bare();
        add_zone(&mut world, zone_sleeps);
        world.entity_mut(mob).insert(WorldKey { zone: 7, id: 1 });
        (world, room, mob)
    }

    /// Night-sleep zone on: the wolf lies down at night.
    fn make_world() -> (World, Entity, Entity) {
        make_world_with(true)
    }

    fn make_world_bare() -> (World, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(NightSleepChance(100));
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
                Named {
                    name: "a wolf".into(),
                },
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
        assert!(world.resource::<NightSleepers>().contains(mob));

        world.resource_mut::<MudClock>().hour = MORNING_HOUR;
        world.resource_mut::<TickCount>().0 = 1500;
        mob_sleep_tick(&mut world);
        assert_eq!(
            world.get::<Posture>(mob).map(|p| p.0),
            Some(PostureKind::Standing),
            "outdoor mob should wake at morning"
        );
        assert!(!world.resource::<NightSleepers>().contains(mob));
    }

    #[test]
    fn cave_mob_stays_awake_at_night() {
        let mut world = World::new();
        add_zone(&mut world, true);
        world.insert_resource(NightSleepChance(100));
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
                Named {
                    name: "a goblin".into(),
                },
                WorldKey { zone: 7, id: 2 },
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
        assert!(!world.resource::<NightSleepers>().contains(mob));
    }

    fn night_tick(world: &mut World, hour: i32, tick: u64) {
        world.resource_mut::<MudClock>().hour = hour;
        world.resource_mut::<TickCount>().0 = tick;
        mob_sleep_tick(world);
    }

    fn posture(world: &World, mob: Entity) -> Option<PostureKind> {
        world.get::<Posture>(mob).map(|p| p.0)
    }

    #[test]
    fn default_off_no_mob_sleeps_at_night() {
        let (mut world, _room, mob) = make_world_with(false);
        for (i, hour) in [22, 23, 0, 1, 2, 3, 4].into_iter().enumerate() {
            night_tick(&mut world, hour, 750 * (i as u64 + 1));
            assert_eq!(
                posture(&world, mob),
                Some(PostureKind::Standing),
                "zone flag off: nothing sleeps (hour {hour})"
            );
        }
        assert!(!world.resource::<NightSleepers>().contains(mob));
    }

    #[test]
    fn mob_with_no_zone_row_does_not_sleep() {
        let (mut world, _room, mob) = make_world_bare();
        night_tick(&mut world, NIGHT_START_HOUR, 750);
        assert_eq!(posture(&world, mob), Some(PostureKind::Standing));
    }

    #[test]
    fn only_the_zone_that_is_on_sleeps() {
        let (mut world, room, wolf) = make_world_with(true);
        // A second zone (id 8), flag off, with its own mob in the same field.
        let off_zone = world.spawn((Zone, WorldKey { zone: 8, id: 0 })).id();
        world
            .resource_mut::<WorldKeyIndex>()
            .zones
            .insert(8, off_zone);
        let bear = world
            .spawn((
                Mob,
                Named {
                    name: "a bear".into(),
                },
                WorldKey { zone: 8, id: 1 },
                Located(room),
                Posture(PostureKind::Standing),
            ))
            .id();
        night_tick(&mut world, NIGHT_START_HOUR, 750);
        assert_eq!(posture(&world, wolf), Some(PostureKind::Sleeping));
        assert_eq!(posture(&world, bear), Some(PostureKind::Standing));
    }

    #[test]
    fn sleeps_at_night_behavior_overrides_an_off_zone() {
        let (mut world, _room, mob) = make_world_with(false);
        world
            .entity_mut(mob)
            .insert(MobBehaviors(vec![MobBehavior::SleepsAtNight]));
        night_tick(&mut world, NIGHT_START_HOUR, 750);
        assert_eq!(posture(&world, mob), Some(PostureKind::Sleeping));
    }

    #[test]
    fn awake_at_night_behavior_overrides_an_on_zone() {
        let (mut world, _room, mob) = make_world_with(true);
        world
            .entity_mut(mob)
            .insert(MobBehaviors(vec![MobBehavior::AwakeAtNight]));
        night_tick(&mut world, NIGHT_START_HOUR, 750);
        assert_eq!(posture(&world, mob), Some(PostureKind::Standing));
    }

    #[test]
    fn mob_that_was_fighting_sleeps_at_the_next_check_if_still_night() {
        let (mut world, _room, mob) = make_world();
        world.get_entity_mut(mob).unwrap().insert(Fighting(mob));
        night_tick(&mut world, 23, 750);
        assert_eq!(posture(&world, mob), Some(PostureKind::Standing));
        // The fight ends; the next hourly check puts it down.
        world.get_entity_mut(mob).unwrap().remove::<Fighting>();
        night_tick(&mut world, 0, 1500);
        assert_eq!(posture(&world, mob), Some(PostureKind::Sleeping));
    }

    #[test]
    fn no_sleep_in_the_daytime() {
        let (mut world, _room, mob) = make_world();
        for (i, hour) in [5, 8, 12, 18, 21].into_iter().enumerate() {
            night_tick(&mut world, hour, 750 * (i as u64 + 1));
            assert_eq!(
                posture(&world, mob),
                Some(PostureKind::Standing),
                "hour {hour}"
            );
        }
    }

    #[test]
    fn bedtime_is_staggered_by_the_sleep_chance() {
        let (mut world, _room, mob) = make_world();
        world.insert_resource(NightSleepChance(0));
        night_tick(&mut world, NIGHT_START_HOUR, 750);
        assert_eq!(posture(&world, mob), Some(PostureKind::Standing));
    }

    #[test]
    fn hunting_mob_does_not_sleep() {
        let (mut world, room, mob) = make_world();
        let prey = world.spawn((Located(room),)).id();
        world
            .entity_mut(mob)
            .insert(crate::combat::HateList(vec![prey]));
        night_tick(&mut world, NIGHT_START_HOUR, 750);
        assert_eq!(posture(&world, mob), Some(PostureKind::Standing));
    }

    #[test]
    fn mob_does_not_sleep_through_a_fight_in_its_room() {
        let (mut world, room, mob) = make_world();
        let a = world.spawn((Located(room),)).id();
        world.entity_mut(a).insert(Fighting(a));
        night_tick(&mut world, NIGHT_START_HOUR, 750);
        assert_eq!(posture(&world, mob), Some(PostureKind::Standing));
    }

    #[test]
    fn aggressive_mob_does_not_sleep_with_a_player_in_the_room() {
        let (mut world, room, mob) = make_world();
        let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
        world.spawn((Player, Located(room), crate::commands::Connection(tx)));
        let stats = mud_world::CombatStats {
            alignment: -1000,
            ..Default::default()
        };
        world.entity_mut(mob).insert(stats);
        night_tick(&mut world, NIGHT_START_HOUR, 750);
        assert_eq!(posture(&world, mob), Some(PostureKind::Standing));
    }

    #[test]
    fn peaceful_mob_sleeps_with_a_player_watching() {
        let (mut world, room, mob) = make_world();
        let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
        world.spawn((Player, Located(room), crate::commands::Connection(tx)));
        night_tick(&mut world, NIGHT_START_HOUR, 750);
        assert_eq!(posture(&world, mob), Some(PostureKind::Sleeping));
    }

    #[test]
    fn remembered_player_in_the_room_keeps_the_mob_up() {
        let (mut world, room, mob) = make_world();
        let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
        let player = world
            .spawn((Player, Located(room), crate::commands::Connection(tx)))
            .id();
        world
            .entity_mut(mob)
            .insert(crate::combat::MobMemory([player].into_iter().collect()));
        night_tick(&mut world, NIGHT_START_HOUR, 750);
        assert_eq!(posture(&world, mob), Some(PostureKind::Standing));
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
        // Spell sleep — Posture flipped, not in NightSleepers.
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

    #[test]
    fn only_rooms_with_a_listener_hear_mobs_settle_and_wake() {
        let (mut world, room, _mob) = make_world();
        let quiet = world.spawn((Room, RoomSector(Sector::Field))).id();
        world.spawn((
            Mob,
            Named {
                name: "a fox".into(),
            },
            Located(quiet),
            Posture(PostureKind::Standing),
        ));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
        world.spawn((Player, Located(room), crate::commands::Connection(tx)));
        let mut heard = || {
            let mut out = String::new();
            while let Ok(b) = rx.try_recv() {
                out.push_str(&String::from_utf8_lossy(&b));
            }
            out
        };

        world.resource_mut::<MudClock>().hour = NIGHT_START_HOUR;
        world.resource_mut::<TickCount>().0 = 750;
        mob_sleep_tick(&mut world);
        let out = heard();
        assert!(out.contains("A wolf settles down to sleep."), "{out:?}");
        assert!(!out.contains("fox"), "{out:?}");

        world.resource_mut::<MudClock>().hour = MORNING_HOUR;
        world.resource_mut::<TickCount>().0 = 1500;
        mob_sleep_tick(&mut world);
        let out = heard();
        assert!(out.contains("A wolf wakes and stretches."), "{out:?}");
        assert!(!out.contains("fox"), "{out:?}");
    }

    #[derive(Component)]
    struct A0;
    #[derive(Component)]
    struct A1;
    #[derive(Component)]
    struct A2;
    #[derive(Component)]
    struct A3;
    #[derive(Component)]
    struct A4;

    fn scatter(world: &mut World, e: Entity, bits: usize) {
        let mut ent = world.entity_mut(e);
        if bits & 1 != 0 {
            ent.insert(A0);
        }
        if bits & 2 != 0 {
            ent.insert(A1);
        }
        if bits & 4 != 0 {
            ent.insert(A2);
        }
        if bits & 8 != 0 {
            ent.insert(A3);
        }
        if bits & 16 != 0 {
            ent.insert(A4);
        }
    }

    /// Timing guard at prod scale (5500 mobs, 4000 items, 10k rooms, no
    /// players). Prod saw 1.0-1.5 s stalls every few hours when each mob
    /// that lay down or got up scanned every located entity to find the
    /// room's audience. Hard limit only enforced in release.
    #[test]
    fn sleep_tick_prod_scale_is_fast() {
        const ROOMS: usize = 10_000;
        const MOBS: usize = 5500;
        const ITEMS: usize = 4000;
        let mut world = World::new();
        add_zone(&mut world, true);
        world.insert_resource(NightSleepChance(100));
        world.insert_resource(TickCount(750));
        world.insert_resource(MudClock {
            year: 1,
            month: 1,
            day: 1,
            hour: NIGHT_START_HOUR,
            minute: 0,
            stamp: 0,
        });
        let rooms: Vec<Entity> = (0..ROOMS)
            .map(|i| {
                let sector = match i % 10 {
                    0..=1 => Sector::Cave,
                    2 => Sector::City,
                    3 => Sector::Water,
                    _ => Sector::Field,
                };
                world.spawn((Room, RoomSector(sector))).id()
            })
            .collect();
        let mut mobs = Vec::with_capacity(MOBS);
        for i in 0..MOBS {
            let mob = world
                .spawn((
                    Mob,
                    Named {
                        name: format!("mob {i}"),
                    },
                    WorldKey { zone: 7, id: 1 },
                    Located(rooms[(i * 7) % ROOMS]),
                    Posture(if i % 11 == 0 {
                        PostureKind::Sleeping
                    } else {
                        PostureKind::Standing
                    }),
                ))
                .id();
            if i % 50 == 0 {
                world.entity_mut(mob).insert(Fighting(mob));
            }
            scatter(&mut world, mob, i);
            mobs.push(mob);
        }
        for i in 0..ITEMS {
            let holder = if i % 2 == 0 {
                mobs[i % MOBS]
            } else {
                rooms[(i * 13) % ROOMS]
            };
            let item = world
                .spawn((
                    Item,
                    Named {
                        name: format!("item {i}"),
                    },
                    Located(holder),
                ))
                .id();
            scatter(&mut world, item, i);
        }

        let start = std::time::Instant::now();
        mob_sleep_tick(&mut world);
        let sleep = start.elapsed();
        assert!(
            world.resource::<NightSleepers>().0.len() > MOBS / 2,
            "most mobs should have been tucked in"
        );

        world.resource_mut::<MudClock>().hour = MORNING_HOUR;
        world.resource_mut::<TickCount>().0 = 1500;
        let start = std::time::Instant::now();
        mob_sleep_tick(&mut world);
        let wake = start.elapsed();
        assert_eq!(world.resource::<NightSleepers>().0.len(), 0);
        eprintln!(
            "mob_sleep_tick 5500 mobs / 4000 items / 10k rooms: sleep={sleep:?} wake={wake:?}"
        );
        if !cfg!(debug_assertions) {
            assert!(sleep.as_millis() < 20, "sleep pass took {sleep:?}");
            assert!(wake.as_millis() < 20, "wake pass took {wake:?}");
        }
    }

    /// Prod stalled 240-310 ms on the first night/morning sweep after each
    /// boot (never again until the next restart): tagging a mob
    /// structurally moves it to a new archetype, and the first move out of
    /// each of a prod world's thousands of archetypes builds the target
    /// archetype and its table. Day one is the number that matters.
    #[test]
    fn sleep_tick_first_day_with_many_archetypes_is_fast() {
        const ROOMS: usize = 10_000;
        const MOBS: usize = 5500;
        let mut world = World::new();
        add_zone(&mut world, true);
        world.insert_resource(NightSleepChance(100));
        world.insert_resource(TickCount(750));
        world.insert_resource(MudClock {
            year: 1,
            month: 1,
            day: 1,
            hour: NIGHT_START_HOUR,
            minute: 0,
            stamp: 0,
        });
        let rooms: Vec<Entity> = (0..ROOMS)
            .map(|i| {
                world
                    .spawn((
                        Room,
                        RoomSector(if i % 5 == 0 {
                            Sector::Cave
                        } else {
                            Sector::Field
                        }),
                    ))
                    .id()
            })
            .collect();
        for i in 0..MOBS {
            let mob = world
                .spawn((
                    Mob,
                    Named {
                        name: format!("mob {i}"),
                    },
                    WorldKey { zone: 7, id: 1 },
                    Located(rooms[(i * 7) % ROOMS]),
                    Posture(PostureKind::Standing),
                ))
                .id();
            crate::archetype_noise::shape(&mut world, mob, i + 1);
        }
        let archetypes = world.archetypes().len();

        let mut times = Vec::new();
        for day in 0..2u64 {
            world.resource_mut::<MudClock>().hour = NIGHT_START_HOUR;
            world.resource_mut::<TickCount>().0 = 1500 * day + 750;
            let start = std::time::Instant::now();
            mob_sleep_tick(&mut world);
            let sleep = start.elapsed();
            world.resource_mut::<MudClock>().hour = MORNING_HOUR;
            world.resource_mut::<TickCount>().0 = 1500 * day + 1500;
            let start = std::time::Instant::now();
            mob_sleep_tick(&mut world);
            times.push((sleep, start.elapsed()));
        }
        eprintln!(
            "mob_sleep_tick {MOBS} mobs / {archetypes} archetypes: day1 sleep={:?} wake={:?}; day2 sleep={:?} wake={:?}",
            times[0].0, times[0].1, times[1].0, times[1].1
        );
        if !cfg!(debug_assertions) {
            for (sleep, wake) in &times {
                assert!(sleep.as_millis() < 20, "sleep pass took {sleep:?}");
                assert!(wake.as_millis() < 20, "wake pass took {wake:?}");
            }
        }
    }
}
