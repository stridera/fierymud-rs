//! Who is in a room, without scanning the world.
//!
//! `Located` is a bevy relationship whose reverse index is [`Contents`]:
//! every container (room, bag, inventory) carries the list of entities
//! directly inside it, kept in sync by bevy's relationship hooks at every
//! insert, replace, remove and despawn. That is the room index. Moves,
//! logins, logouts, linkdead / reconnect, switch / snoop and despawns all
//! go through those hooks, so there is no second copy that can drift.
//!
//! These helpers are the O(room contents) replacement for the old
//! `query::<(Entity, &Located)>().filter(|l| l.0 == room)` pattern, which
//! cost O(every located entity in the world) per call (a room broadcast
//! on prod walked ~10k mobs and items for every line of output).
//!
//! Order is `Contents` order (oldest arrival first), not archetype order.
//! Callers must not depend on either; recipients of a broadcast each
//! receive their own lines in call order regardless.

use bevy_ecs::prelude::*;
use mud_world::{Contents, Mob, Player, SnoopedBy, SwitchedFrom};

use crate::commands::Connection;

/// Everything directly inside `container` (a room, bag or inventory), in
/// `Contents` order. Empty for entities that hold nothing.
pub(crate) fn contents_of(world: &World, container: Entity) -> impl Iterator<Item = Entity> + '_ {
    world
        .get::<Contents>(container)
        .into_iter()
        .flat_map(Contents::iter)
}

/// True when output sent to `e` reaches someone: it has a live
/// `Connection`, is a switch puppet (`SwitchedFrom`) or is being snooped
/// (`SnoopedBy`). These are the only ways `commands::send_raw` delivers
/// anything, so every other entity can be skipped without changing what
/// anyone sees.
pub(crate) fn can_receive_output(world: &World, e: Entity) -> bool {
    world.get::<Connection>(e).is_some()
        || world.get::<SwitchedFrom>(e).is_some()
        || world.get::<SnoopedBy>(e).is_some()
}

/// Entities in `room` that can receive output (see [`can_receive_output`]).
pub(crate) fn listeners_in(world: &World, room: Entity) -> Vec<Entity> {
    contents_of(world, room)
        .filter(|&e| can_receive_output(world, e))
        .collect()
}

/// Players in `room` that can receive output.
pub(crate) fn player_listeners_in(world: &World, room: Entity) -> Vec<Entity> {
    contents_of(world, room)
        .filter(|&e| world.get::<Player>(e).is_some() && can_receive_output(world, e))
        .collect()
}

/// Every player in `room`, connected or not (linkdead included).
pub(crate) fn players_in(world: &World, room: Entity) -> Vec<Entity> {
    contents_of(world, room)
        .filter(|&e| world.get::<Player>(e).is_some())
        .collect()
}

/// Every player or mob in `room`.
pub(crate) fn actors_in(world: &World, room: Entity) -> Vec<Entity> {
    contents_of(world, room)
        .filter(|&e| world.get::<Player>(e).is_some() || world.get::<Mob>(e).is_some())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use mud_world::{
        Item, Located, MudClock, Named, Online, Posture, PostureKind, Profile, Room, RoomSector,
    };
    use rand::rngs::StdRng;
    use rand::{RngExt, SeedableRng};
    use tokio::sync::mpsc::{Receiver, channel};

    use super::*;
    use crate::commands::{
        broadcast_room_anonymised, broadcast_room_except_players_rendered,
        broadcast_room_except_rendered, broadcast_room_visible, broadcast_room_visual,
    };

    fn sorted(v: impl IntoIterator<Item = Entity>) -> Vec<Entity> {
        let mut v: Vec<Entity> = v.into_iter().collect();
        v.sort();
        v
    }

    /// The pre-index implementation: scan every `Located`.
    fn scan_listeners(world: &mut World, room: Entity) -> Vec<Entity> {
        let mut q = world.query::<(Entity, &Located)>();
        let all: Vec<Entity> = q
            .iter(world)
            .filter(|(_, l)| l.0 == room)
            .map(|(e, _)| e)
            .collect();
        sorted(all.into_iter().filter(|&e| can_receive_output(world, e)))
    }

    fn scan_all_in(world: &mut World, room: Entity) -> Vec<Entity> {
        let mut q = world.query::<(Entity, &Located)>();
        sorted(
            q.iter(world)
                .filter(|(_, l)| l.0 == room)
                .map(|(e, _)| e)
                .collect::<Vec<_>>(),
        )
    }

    fn base_world() -> World {
        let mut world = World::new();
        world.insert_resource(MudClock {
            year: 1,
            month: 1,
            day: 1,
            hour: 12,
            minute: 0,
            stamp: 0,
        });
        world
    }

    fn spawn_player(world: &mut World, room: Entity) -> (Entity, Receiver<Vec<u8>>) {
        let (tx, rx) = channel::<Vec<u8>>(4096);
        let e = world
            .spawn((Player, Located(room), Connection(tx), Online))
            .id();
        (e, rx)
    }

    #[test]
    fn index_follows_move_login_logout_linkdead_switch_snoop_despawn() {
        let mut world = base_world();
        let r1 = world
            .spawn((Room, RoomSector(mud_db::enums::Sector::City)))
            .id();
        let r2 = world
            .spawn((Room, RoomSector(mud_db::enums::Sector::City)))
            .id();
        let mob = world.spawn((Mob, Located(r1))).id();
        let item = world.spawn((Item, Located(r1))).id();

        // Login: a connected player appears in the room's listeners.
        let (p, _rx) = spawn_player(&mut world, r1);
        assert_eq!(sorted(listeners_in(&world, r1)), sorted([p]));
        assert_eq!(sorted(contents_of(&world, r1)), sorted([mob, item, p]));

        // Move: leaves r1, appears in r2.
        world.entity_mut(p).insert(Located(r2));
        assert!(listeners_in(&world, r1).is_empty());
        assert_eq!(listeners_in(&world, r2), vec![p]);

        // Linkdead: Connection dropped; still a player in the room but
        // no longer a listener.
        let conn = world.entity_mut(p).take::<Connection>().unwrap();
        assert!(listeners_in(&world, r2).is_empty());
        assert_eq!(players_in(&world, r2), vec![p]);
        assert!(player_listeners_in(&world, r2).is_empty());

        // Reconnect: Connection back.
        world.entity_mut(p).insert(conn);
        assert_eq!(listeners_in(&world, r2), vec![p]);

        // Switch: the puppet mob becomes a listener, then stops.
        world.entity_mut(mob).insert(SwitchedFrom(p));
        assert_eq!(sorted(listeners_in(&world, r1)), vec![mob]);
        world.entity_mut(mob).remove::<SwitchedFrom>();
        assert!(listeners_in(&world, r1).is_empty());

        // Snoop.
        world.entity_mut(mob).insert(SnoopedBy(p));
        assert_eq!(listeners_in(&world, r1), vec![mob]);
        world.entity_mut(mob).remove::<SnoopedBy>();
        assert!(listeners_in(&world, r1).is_empty());

        // Logout (despawn).
        world.despawn(p);
        assert!(listeners_in(&world, r2).is_empty());
        assert!(contents_of(&world, r2).next().is_none());

        // Despawning a mob removes it from its room's contents; the
        // item stays.
        world.despawn(mob);
        assert_eq!(contents_of(&world, r1).collect::<Vec<_>>(), vec![item]);

        // Despawning the room strips Located from what was inside; the
        // index holds no stale room entry.
        world.despawn(r1);
        assert!(world.get::<Located>(item).is_none());
        assert!(contents_of(&world, r1).next().is_none());
    }

    /// Everything one entity heard, decoded.
    ///
    /// Sorted: a switch or snoop watcher hears the same line once per
    /// watched entity in the room, and the order those copies arrive in
    /// followed the (arbitrary) archetype order of the old scan. Each
    /// copy is still delivered exactly as many times and bytes-identical.
    fn drain(rx: &mut Receiver<Vec<u8>>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(b) = rx.try_recv() {
            out.push(String::from_utf8_lossy(&b).into_owned());
        }
        out.sort();
        out
    }

    /// The pre-index broadcast, kept as the reference: scan every
    /// `Located`, filter in the closure, then `send_to`.
    fn old_broadcast(
        world: &mut World,
        kind: u8,
        room: Entity,
        sender: Entity,
        except: &[Entity],
        msg: &str,
    ) {
        let targets: Vec<Entity> = {
            let mut q = world.query::<(Entity, &Located, Has<Player>)>();
            q.iter(world)
                .filter(|(e, l, is_player)| {
                    l.0 == room
                        && !except.contains(e)
                        && match kind {
                            0 | 1 => true,
                            _ => *is_player,
                        }
                })
                .map(|(e, ..)| e)
                .collect()
        };
        match kind {
            0 | 2 => {
                for t in targets {
                    crate::commands::send_to(world, t, msg);
                }
            }
            1 => {
                let actors = [(sender, "Bob")];
                for t in targets {
                    let m = crate::commands::anonymise_for(world, t, &actors, msg);
                    crate::commands::send_to(world, t, m);
                }
            }
            3 => {
                for t in targets {
                    if crate::commands::can_see_player(world, t, sender) {
                        crate::commands::send_to(world, t, msg);
                    }
                }
            }
            _ => {
                let targets: Vec<Entity> = targets
                    .into_iter()
                    .filter(|&t| crate::commands::can_see_player(world, t, sender))
                    .collect();
                if targets.is_empty() {
                    return;
                }
                let visible_here = !crate::commands::room_is_dark(world, room)
                    || crate::commands::room_has_light(world, room);
                for t in targets {
                    if visible_here || crate::commands::sees_characters_in_dark(world, t) {
                        crate::commands::send_to(world, t, msg);
                    }
                }
            }
        }
    }

    fn new_broadcast(
        world: &mut World,
        kind: u8,
        room: Entity,
        sender: Entity,
        except: &[Entity],
        msg: &str,
    ) {
        match kind {
            0 => broadcast_room_except_rendered(world, room, except, msg),
            1 => broadcast_room_anonymised(world, room, except, &[(sender, "Bob")], msg),
            2 => broadcast_room_except_players_rendered(world, room, except, msg),
            3 => broadcast_room_visible(world, room, sender, except, msg),
            _ => broadcast_room_visual(world, room, sender, except, msg),
        }
    }

    struct RandomWorld {
        world: World,
        rng: StdRng,
        rooms: Vec<Entity>,
        rxs: BTreeMap<Entity, Receiver<Vec<u8>>>,
        players: Vec<Entity>,
    }

    /// Dark and lit rooms, players (some linkdead or wiz-invisible), mobs,
    /// lit and unlit torches on the floor and in hands, a switched puppet
    /// and a snooped mob, then some churn (moves and a despawn).
    fn random_world(seed: u64) -> RandomWorld {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut world = base_world();
        let sectors = [
            mud_db::enums::Sector::City,
            mud_db::enums::Sector::Cave,
            mud_db::enums::Sector::Field,
        ];
        let rooms: Vec<Entity> = (0..6)
            .map(|_| {
                world
                    .spawn((Room, RoomSector(sectors[rng.random_range(0..3)])))
                    .id()
            })
            .collect();
        let mut rxs: BTreeMap<Entity, Receiver<Vec<u8>>> = BTreeMap::new();
        let mut actors: Vec<Entity> = Vec::new();
        let mut players: Vec<Entity> = Vec::new();
        for _ in 0..rng.random_range(8..24) {
            let room = rooms[rng.random_range(0..rooms.len())];
            match rng.random_range(0..5) {
                0 | 1 => {
                    let (p, rx) = spawn_player(&mut world, room);
                    world.entity_mut(p).insert((
                        Named { name: "Bob".into() },
                        Profile {
                            level: rng.random_range(1..60),
                            class_id: None,
                            race: "Human".into(),
                            experience: 0,
                            gender: "neutral".into(),
                        },
                        Posture(PostureKind::Standing),
                    ));
                    if rng.random_bool(0.25) {
                        world.entity_mut(p).insert(mud_world::WizInvis(30));
                    }
                    if rng.random_bool(0.2) {
                        // linkdead: still a player in the room, no socket
                        world.entity_mut(p).remove::<Connection>();
                    } else {
                        rxs.insert(p, rx);
                    }
                    players.push(p);
                    actors.push(p);
                }
                2 | 3 => {
                    let m = world
                        .spawn((
                            Mob,
                            Named {
                                name: "a rat".into(),
                            },
                            Located(room),
                        ))
                        .id();
                    actors.push(m);
                }
                _ => {
                    world.spawn((Item, Located(room)));
                }
            }
        }
        // Torches: floor and carried, lit and unlit.
        for _ in 0..rng.random_range(0..5) {
            let holder = if rng.random_bool(0.5) || actors.is_empty() {
                rooms[rng.random_range(0..rooms.len())]
            } else {
                actors[rng.random_range(0..actors.len())]
            };
            let t = world.spawn((Item, Located(holder))).id();
            if rng.random_bool(0.5) {
                world.entity_mut(t).insert(mud_world::Lit);
            }
        }
        // A switched puppet and a snooped mob watched by a connected player.
        let connected: Vec<Entity> = rxs.keys().copied().collect();
        if let Some(&boss) = connected.first() {
            let room = rooms[rng.random_range(0..rooms.len())];
            let puppet = world.spawn((Mob, Located(room), SwitchedFrom(boss))).id();
            let snooped = world.spawn((Mob, Located(room), SnoopedBy(boss))).id();
            actors.push(puppet);
            actors.push(snooped);
        }
        // Churn: move a few entities so Contents has seen removes.
        for _ in 0..6 {
            if actors.is_empty() {
                break;
            }
            let a = actors[rng.random_range(0..actors.len())];
            let room = rooms[rng.random_range(0..rooms.len())];
            world.entity_mut(a).insert(Located(room));
        }
        // Take out one actor entirely.
        if actors.len() > 3 {
            let gone = actors.swap_remove(0);
            rxs.remove(&gone);
            world.despawn(gone);
        }
        RandomWorld {
            world,
            rng,
            rooms,
            rxs,
            players,
        }
    }

    /// For every helper, room and exclusion set on randomised worlds, the
    /// old full scan and the index must deliver the same bytes to the same
    /// recipients.
    #[test]
    fn broadcast_recipients_match_the_old_scan_on_random_worlds() {
        let mut delivered = 0usize;
        for seed in 0..12u64 {
            let RandomWorld {
                mut world,
                mut rng,
                rooms,
                mut rxs,
                players,
            } = random_world(seed);
            for &room in &rooms {
                assert_eq!(
                    sorted(listeners_in(&world, room)),
                    scan_listeners(&mut world, room),
                    "seed {seed} listeners"
                );
                assert_eq!(
                    sorted(contents_of(&world, room)),
                    scan_all_in(&mut world, room),
                    "seed {seed} contents"
                );
            }

            for kind in 0..5u8 {
                for &room in &rooms {
                    let alive: Vec<Entity> = players
                        .iter()
                        .copied()
                        .filter(|p| world.get_entity(*p).is_ok())
                        .collect();
                    if alive.is_empty() {
                        continue;
                    }
                    let sender = alive[rng.random_range(0..alive.len())];
                    let except: Vec<Entity> = if rng.random_bool(0.5) {
                        vec![sender]
                    } else {
                        vec![]
                    };
                    // Old behaviour.
                    for rx in rxs.values_mut() {
                        drain(rx);
                    }
                    old_broadcast(&mut world, kind, room, sender, &except, "Bob waves.\r\n");
                    let old: BTreeMap<Entity, Vec<String>> =
                        rxs.iter_mut().map(|(e, rx)| (*e, drain(rx))).collect();
                    // Index.
                    new_broadcast(&mut world, kind, room, sender, &except, "Bob waves.\r\n");
                    let new: BTreeMap<Entity, Vec<String>> =
                        rxs.iter_mut().map(|(e, rx)| (*e, drain(rx))).collect();
                    assert_eq!(old, new, "seed {seed} kind {kind} room {room:?}");
                    delivered += old.values().map(Vec::len).sum::<usize>();
                }
            }
        }
        assert!(
            delivered > 200,
            "test world delivered too little: {delivered}"
        );
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
    #[derive(Component)]
    struct A5;
    #[derive(Component)]
    struct A6;

    /// Spread an entity over many archetypes the way prod's varied mobs
    /// and items do (flags, effects, postures...); a world of identical
    /// entities scans unrealistically fast.
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
        if bits & 32 != 0 {
            ent.insert(A5);
        }
        if bits & 64 != 0 {
            ent.insert(A6);
        }
    }

    /// Prod scale with 50 players online: 5,500 mobs, 4,000 items, 10k
    /// rooms. 1,000 broadcasts used to walk every located entity each time
    /// (~1 ms apiece, so ~1 s of tick). Hard limit only in release.
    #[test]
    fn thousand_broadcasts_at_prod_scale_are_fast() {
        const ROOMS: usize = 10_000;
        const MOBS: usize = 5500;
        const ITEMS: usize = 4000;
        const PLAYERS: usize = 50;
        let mut world = base_world();
        let rooms: Vec<Entity> = (0..ROOMS)
            .map(|_| {
                world
                    .spawn((Room, RoomSector(mud_db::enums::Sector::City)))
                    .id()
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
                    Located(rooms[(i * 7) % ROOMS]),
                ))
                .id();
            scatter(&mut world, mob, i.wrapping_mul(2_654_435_761) >> 7);
            mobs.push(mob);
        }
        for i in 0..ITEMS {
            let holder = if i % 2 == 0 {
                mobs[i % MOBS]
            } else {
                rooms[(i * 13) % ROOMS]
            };
            let item = world.spawn((Item, Located(holder))).id();
            scatter(&mut world, item, i.wrapping_mul(40503) >> 3);
        }
        let mut rxs = Vec::new();
        let mut player_rooms = Vec::new();
        for i in 0..PLAYERS {
            let room = rooms[(i % 10) * 7];
            // Half the player rooms are dark caves: visual broadcasts there
            // must also decide whether anyone carries a light.
            if (i % 10) % 2 == 0 {
                world
                    .entity_mut(room)
                    .insert(RoomSector(mud_db::enums::Sector::Cave));
            }
            let (p, rx) = spawn_player(&mut world, room);
            world.entity_mut(p).insert(Named {
                name: format!("Player{i}"),
            });
            player_rooms.push((p, room));
            rxs.push(rx);
        }

        let start = std::time::Instant::now();
        for i in 0..1000 {
            let (p, room) = player_rooms[i % PLAYERS];
            match i % 4 {
                0 => broadcast_room_except_rendered(&mut world, room, &[p], "x says hi.\r\n"),
                1 => broadcast_room_visible(&mut world, room, p, &[p], "x waves.\r\n"),
                2 => broadcast_room_visual(&mut world, room, p, &[p], "x leaves.\r\n"),
                _ => broadcast_room_except_players_rendered(&mut world, room, &[p], "x nods.\r\n"),
            }
        }
        let elapsed = start.elapsed();
        eprintln!(
            "1000 room broadcasts, 5500 mobs / 4000 items / 10k rooms / 50 players: {elapsed:?}"
        );
        if !cfg!(debug_assertions) {
            assert!(elapsed.as_millis() < 20, "1000 broadcasts took {elapsed:?}");
        }
        drop(rxs);
    }
}
