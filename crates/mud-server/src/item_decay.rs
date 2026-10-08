//! Generic item lifetime + decay tick (B1, parity with legacy
//! `Object.timer` / `Object.decompose_timer`). Items spawned with
//! `ObjectProto.timer_hours > 0` AND without the PERMANENT flag get
//! an `ItemTimer` component at spawn time. The `item_decay_tick`
//! decrements every game-second and destroys the entity at zero.
//!
//! The DECOMPOSING two-phase mode (`decompose_window_secs` > 0) is
//! plumbed through the component but not yet activated — today the
//! tick just destroys at zero. A follow-up can split into "expired"
//! + "decomposing" states with separate flavor lines.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectFlag, ObjectType};
use mud_world::{
    CoinPile, Corpse, Decomposing, Description, Item, ItemTimer, Keywords, Located, LooseCoins,
    Named, ObjectFlags, ObjectProto, ObjectPrototypes, WorldKey,
};

use crate::commands::{broadcast_room_except_rendered, cap_sentence_start, send_to};

/// Legacy MUD-hour to wall seconds. Matches the constant used by
/// effect duration resolution; centralized here to keep the timer
/// math grounded in one place.
pub(crate) const SECS_PER_MUD_HOUR: i32 = 75;

/// Run-at-spawn hook: if the proto has a positive `timer_hours`
/// AND the object isn't flagged PERMANENT, attach an `ItemTimer`
/// to the entity. Caller passes the freshly-spawned entity + its
/// proto. Safe to call after any `world.spawn(...)` that produced
/// an Item entity.
pub fn attach_timer_if_decaying(world: &mut World, entity: Entity, proto: &ObjectProto) {
    if proto.timer_hours <= 0 {
        return;
    }
    if proto.flags.contains(&ObjectFlag::Permanent) {
        return;
    }
    let remaining = proto.timer_hours.saturating_mul(SECS_PER_MUD_HOUR);
    let decompose = proto
        .decompose_timer
        .saturating_mul(SECS_PER_MUD_HOUR)
        .max(0);
    let Ok(mut em) = world.get_entity_mut(entity) else {
        return;
    };
    em.insert(ItemTimer {
        remaining_secs: remaining,
        decompose_window_secs: decompose,
    });
}

/// Legacy `stop_decomposing` (handler.cpp `obj_to_char`, `equip_char`,
/// `obj_to_obj` into a carried container): a rotting item that a
/// creature now holds stops rotting, contents included. Items move
/// through too many paths (get, loot, give, wear, shops, scripts) to
/// hook one by one, so the sweep checks where each `Decomposing` item
/// sits: following `Located` up through containers, a `Player` or
/// `Mob` at the root means carried or worn. Only the timer
/// `start_decomposing` added is removed; an item's own `timer_hours`
/// clock never carries the marker.
fn stop_decomposing_carried(world: &mut World) {
    let rotting: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Decomposing>, With<Item>)>();
        q.iter(world).collect()
    };
    for item in rotting {
        if held_by_creature(world, item) {
            let mut em = world.entity_mut(item);
            em.remove::<Decomposing>();
            em.remove::<ItemTimer>();
        }
    }
}

/// True when the outermost holder of `item` (through any depth of
/// containers) is a player or mob rather than a room.
fn held_by_creature(world: &World, item: Entity) -> bool {
    let mut at = item;
    for _ in 0..64 {
        let Some(holder) = world.get::<Located>(at).map(|l| l.0) else {
            return false;
        };
        if world.get::<mud_world::Player>(holder).is_some()
            || world.get::<mud_world::Mob>(holder).is_some()
        {
            return true;
        }
        if world.get::<mud_world::Room>(holder).is_some() {
            return false;
        }
        at = holder;
    }
    false
}

/// Decrement every `ItemTimer` by 1 second per call. Items hitting
/// zero are destroyed; when the holder is a player or the item is
/// on the floor of a populated room, a flavor line announces the
/// disappearance so players aren't left wondering where their
/// torch went. Runs at the same 1-Hz cadence as corpse decay.
pub fn item_decay_tick(world: &mut World) {
    stop_decomposing_carried(world);
    // Snapshot first so we can both mutate timers AND despawn
    // without re-borrowing the query.
    let snapshots: Vec<(Entity, i32)> = {
        let mut q = world.query_filtered::<(Entity, &ItemTimer), With<Item>>();
        q.iter(world).map(|(e, t)| (e, t.remaining_secs)).collect()
    };
    let mut destroyed: Vec<Entity> = Vec::new();
    for (entity, current) in snapshots {
        let next = current.saturating_sub(1);
        if next <= 0 {
            destroyed.push(entity);
            continue;
        }
        if let Some(mut t) = world.get_mut::<ItemTimer>(entity) {
            t.remaining_secs = next;
        }
    }
    for entity in destroyed {
        // Look up where the item sits BEFORE despawn so we can
        // route the flavor message. Items can be Located on a
        // room (floor), a player (carried/equipped), or another
        // item (inside a container).
        let (holder_entity, holder_kind) = location_kind(world, entity);
        let item_name = world
            .get::<Named>(entity)
            .map_or_else(|| String::from("an item"), |n| n.name.clone());
        match holder_kind {
            HolderKind::Player => {
                send_to(
                    world,
                    holder_entity,
                    cap_sentence_start(&format!(
                        "<dim>{item_name} crumbles to dust in your hands.</>\r\n"
                    )),
                );
            }
            HolderKind::Room => {
                broadcast_room_except_rendered(
                    world,
                    holder_entity,
                    &[],
                    &cap_sentence_start(&format!(
                        "<dim>{item_name} crumbles to dust and blows away.</>\r\n"
                    )),
                );
            }
            HolderKind::Container | HolderKind::Unknown => {
                // Inside a container or unrooted — silent destroy.
            }
        }
        release_contents(world, entity, holder_entity, &holder_kind);
        // A worn item's bonuses and flag effects go with it.
        crate::equip_apply::despawn_item(world, entity);
    }
}

/// A decaying container must not orphan what it holds. Legacy
/// `extract_corpse` moves each item to the container's own container,
/// or to the room (the carrier's room when the container is carried).
/// A container with no resolvable place (unrooted) takes its contents
/// with it. Items nested deeper stay with their own parent.
pub(crate) fn release_contents(
    world: &mut World,
    container: Entity,
    holder: Entity,
    kind: &HolderKind,
) {
    let contents: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == container)
            .map(|(e, _)| e)
            .collect()
    };
    // A loose pile of coins *is* the money: when it rots the coins go with
    // it (legacy `decay_object` extracts the money object). Releasing its
    // amount would respawn a fresh pile that rots again, forever.
    let coins = if world.get::<LooseCoins>(container).is_some() {
        0
    } else {
        world.get::<CoinPile>(container).map_or(0, |p| p.0).max(0)
    };
    if contents.is_empty() && coins == 0 {
        return;
    }
    let dest = match kind {
        HolderKind::Room | HolderKind::Container => Some(holder),
        HolderKind::Player => world.get::<Located>(holder).map(|l| l.0),
        HolderKind::Unknown => None,
    };
    if coins > 0 {
        release_coins(world, dest, kind, coins);
    }
    // Legacy `extract_corpse` starts decomposing only what a rotting
    // *corpse* spills onto the floor; contents handed to an enclosing
    // container or a carrier's room are left alone.
    let decompose = matches!(kind, HolderKind::Room) && world.get::<Corpse>(container).is_some();
    for item in contents {
        match dest {
            Some(d) => {
                world.entity_mut(item).insert(Located(d));
                if decompose {
                    start_decomposing(world, item);
                }
            }
            None => {
                if let Ok(em) = world.get_entity_mut(item) {
                    em.despawn();
                }
            }
        }
    }
}

/// Legacy `start_decomposing` (limits.cpp): an object below level 100
/// and not PERMANENT rots in `level + 11` ticks (+192 for a key); a
/// tick is one MUD hour, so the clock is that many `SECS_PER_MUD_HOUR`.
/// Contents recurse, except that a corpse's contents never decompose.
/// An item that already carries an `ItemTimer` keeps it: the Rust timer
/// is a single clock, and legacy only ever raises a decomp timer.
pub(crate) fn start_decomposing(world: &mut World, item: Entity) {
    let key = world.get::<WorldKey>(item).map(|k| (k.zone, k.id));
    let proto = key.and_then(|k| {
        world
            .get_resource::<ObjectPrototypes>()
            .and_then(|p| p.by_key.get(&k))
            .map(|p| (p.level, p.r#type == ObjectType::Key))
    });
    let (level, is_key) = proto.unwrap_or((0, false));
    let permanent = world
        .get::<ObjectFlags>(item)
        .is_some_and(|f| f.has(ObjectFlag::Permanent));
    if level < 100 && !permanent && world.get::<ItemTimer>(item).is_none() {
        let ticks = level
            .saturating_add(11)
            .saturating_add(if is_key { 192 } else { 0 });
        world.entity_mut(item).insert((
            ItemTimer {
                remaining_secs: ticks.saturating_mul(SECS_PER_MUD_HOUR),
                decompose_window_secs: 0,
            },
            Decomposing,
        ));
    }
    if world.get::<Corpse>(item).is_some() {
        return;
    }
    let inner: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == item)
            .map(|(e, _)| e)
            .collect()
    };
    for e in inner {
        start_decomposing(world, e);
    }
}

/// Legacy `extract_corpse` treats a corpse's money like any other
/// content (`obj_to_obj` into the enclosing container, else
/// `obj_to_room` + `start_decomposing`). Coins here live as a
/// `CoinPile` component on their container, so: an enclosing
/// container absorbs them into its own pile (merging with one it
/// already has), a room gets a loose pile item that rots on the
/// normal item timer, and an unrooted corpse takes them with it.
fn release_coins(world: &mut World, dest: Option<Entity>, kind: &HolderKind, coins: i64) {
    let Some(dest) = dest else {
        return;
    };
    if matches!(kind, HolderKind::Container) {
        let held = world.get::<CoinPile>(dest).map_or(0, |p| p.0.max(0));
        if let Ok(mut em) = world.get_entity_mut(dest) {
            em.insert(CoinPile(held.saturating_add(coins)));
        }
        return;
    }
    spawn_loose_coin_pile(world, dest, coins);
}

/// Legacy `start_decomposing` on a level-0 object: 11 MUD hours.
const LOOSE_COIN_DECAY_HOURS: i32 = 11;

/// Drop `coins` copper as a pickup-able pile on the floor of `room`.
/// `get`/`get all` convert it straight into the taker's `Wealth`.
/// Carries an `ItemTimer` so an unclaimed pile rots away.
pub(crate) fn spawn_loose_coin_pile(world: &mut World, room: Entity, coins: i64) -> Entity {
    world
        .spawn((
            Item,
            CoinPile(coins),
            LooseCoins,
            Named {
                name: "a pile of coins".to_string(),
            },
            Keywords(
                ["coins", "coin", "pile", "gold", "money"]
                    .map(String::from)
                    .to_vec(),
            ),
            Description("A pile of coins is lying here.".to_string()),
            Located(room),
            ItemTimer {
                remaining_secs: LOOSE_COIN_DECAY_HOURS * SECS_PER_MUD_HOUR,
                decompose_window_secs: 0,
            },
        ))
        .id()
}

#[derive(Debug)]
pub(crate) enum HolderKind {
    Player,
    Room,
    Container,
    Unknown,
}

/// Classify what an item is Located on so the decay tick picks
/// the right announcement path.
pub(crate) fn location_kind(world: &World, item: Entity) -> (Entity, HolderKind) {
    let Some(loc) = world.get::<Located>(item).map(|l| l.0) else {
        return (item, HolderKind::Unknown);
    };
    if world.get::<mud_world::Player>(loc).is_some() {
        return (loc, HolderKind::Player);
    }
    if world.get::<mud_world::Room>(loc).is_some() {
        return (loc, HolderKind::Room);
    }
    (loc, HolderKind::Container)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::Keywords;

    fn item(world: &mut World, name: &str, at: Entity) -> Entity {
        world
            .spawn((
                Item,
                Named { name: name.into() },
                Keywords(vec![name.into()]),
                WorldKey { zone: 1, id: 1 },
                Located(at),
            ))
            .id()
    }

    fn decaying(world: &mut World, name: &str, at: Entity) -> Entity {
        let e = item(world, name, at);
        world.entity_mut(e).insert(ItemTimer {
            remaining_secs: 1,
            decompose_window_secs: 0,
        });
        e
    }

    #[test]
    fn rotting_loose_coin_pile_is_gone_for_good() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let pile = spawn_loose_coin_pile(&mut world, room, 10);
        world.get_mut::<ItemTimer>(pile).unwrap().remaining_secs = 1;
        item_decay_tick(&mut world);
        assert!(world.get_entity(pile).is_err());
        // The coins were destroyed, not re-dropped as a fresh pile that
        // would rot again and again (issue #64).
        let mut q = world.query_filtered::<&Located, With<CoinPile>>();
        assert_eq!(q.iter(&world).filter(|l| l.0 == room).count(), 0);
    }

    #[test]
    fn rotting_floor_item_message_starts_with_a_capital() {
        use crate::commands::test_support::{drain, player_in};
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let (_player, mut rx) = player_in(&mut world, room);
        let pile = spawn_loose_coin_pile(&mut world, room, 10);
        world.get_mut::<ItemTimer>(pile).unwrap().remaining_secs = 1;
        item_decay_tick(&mut world);
        let out = drain(&mut rx);
        assert!(
            out.contains("A pile of coins crumbles to dust and blows away."),
            "{out}"
        );
    }

    #[test]
    fn decaying_container_on_the_floor_spills_into_the_room() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let bag = decaying(&mut world, "bag", room);
        let gem = item(&mut world, "gem", bag);
        let inner = item(&mut world, "pouch", bag);
        let coin = item(&mut world, "coin", inner);
        item_decay_tick(&mut world);
        assert!(world.get_entity(bag).is_err());
        assert_eq!(world.get::<Located>(gem).unwrap().0, room);
        assert_eq!(world.get::<Located>(inner).unwrap().0, room);
        // Deeper nesting stays with its own parent.
        assert_eq!(world.get::<Located>(coin).unwrap().0, inner);
    }

    #[test]
    fn decaying_container_inside_another_hands_contents_to_it() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let chest = item(&mut world, "chest", room);
        let bag = decaying(&mut world, "bag", chest);
        let gem = item(&mut world, "gem", bag);
        item_decay_tick(&mut world);
        assert!(world.get_entity(bag).is_err());
        assert_eq!(world.get::<Located>(gem).unwrap().0, chest);
    }

    #[test]
    fn decaying_carried_container_drops_contents_in_the_carriers_room() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let player = world.spawn((mud_world::Player, Located(room))).id();
        let bag = decaying(&mut world, "bag", player);
        let gem = item(&mut world, "gem", bag);
        item_decay_tick(&mut world);
        assert!(world.get_entity(bag).is_err());
        assert_eq!(world.get::<Located>(gem).unwrap().0, room);
    }

    fn corpse(world: &mut World, at: Entity) -> Entity {
        let c = item(world, "corpse", at);
        world.entity_mut(c).insert(Corpse);
        c
    }

    fn proto_world(entries: &[(i32, i32, ObjectType, i32)]) -> World {
        let mut world = World::new();
        let mut protos = ObjectPrototypes::default();
        for &(zone, id, kind, level) in entries {
            let mut p = crate::commands::test_support::object_proto(zone, id, kind);
            p.level = level;
            protos.by_key.insert((zone, id), p);
        }
        world.insert_resource(protos);
        world
    }

    fn keyed(world: &mut World, name: &str, at: Entity, id: i32) -> Entity {
        let e = item(world, name, at);
        world.entity_mut(e).insert(WorldKey { zone: 1, id });
        e
    }

    #[test]
    fn rotting_floor_corpse_starts_level_based_timers_on_contents() {
        // level 5 -> 16 ticks; key level 5 -> 16 + 192 ticks;
        // level 100 and PERMANENT never rot.
        let mut world = proto_world(&[
            (1, 10, ObjectType::Other, 5),
            (1, 11, ObjectType::Key, 5),
            (1, 12, ObjectType::Other, 100),
            (1, 13, ObjectType::Other, 5),
        ]);
        let room = world.spawn(mud_world::Room).id();
        let c = corpse(&mut world, room);
        world.entity_mut(c).insert(ItemTimer {
            remaining_secs: 1,
            decompose_window_secs: 0,
        });
        let sword = keyed(&mut world, "sword", c, 10);
        let key = keyed(&mut world, "key", c, 11);
        let god = keyed(&mut world, "relic", c, 12);
        let perm = keyed(&mut world, "perm", c, 13);
        world
            .entity_mut(perm)
            .insert(ObjectFlags(vec![ObjectFlag::Permanent]));
        item_decay_tick(&mut world);
        assert!(world.get_entity(c).is_err());
        let secs = |w: &World, e| w.get::<ItemTimer>(e).map(|t| t.remaining_secs);
        assert_eq!(secs(&world, sword), Some(16 * SECS_PER_MUD_HOUR));
        assert_eq!(secs(&world, key), Some(208 * SECS_PER_MUD_HOUR));
        assert_eq!(secs(&world, god), None);
        assert_eq!(secs(&world, perm), None);
    }

    #[test]
    fn corpse_release_recurses_into_bags_but_not_nested_corpses() {
        let mut world = proto_world(&[]);
        let room = world.spawn(mud_world::Room).id();
        let c = corpse(&mut world, room);
        world.entity_mut(c).insert(ItemTimer {
            remaining_secs: 1,
            decompose_window_secs: 0,
        });
        let bag = item(&mut world, "bag", c);
        let gem = item(&mut world, "gem", bag);
        let inner_corpse = corpse(&mut world, c);
        let loot = item(&mut world, "loot", inner_corpse);
        item_decay_tick(&mut world);
        assert!(world.get::<ItemTimer>(bag).is_some());
        assert!(world.get::<ItemTimer>(gem).is_some());
        assert!(world.get::<ItemTimer>(inner_corpse).is_some());
        assert!(world.get::<ItemTimer>(loot).is_none());
    }

    #[test]
    fn rotting_corpse_in_a_container_or_hands_adds_no_timers() {
        let mut world = proto_world(&[]);
        let room = world.spawn(mud_world::Room).id();
        let chest = item(&mut world, "chest", room);
        let c1 = corpse(&mut world, chest);
        let in_chest = item(&mut world, "gem", c1);
        let player = world.spawn((mud_world::Player, Located(room))).id();
        let c2 = corpse(&mut world, player);
        let carried = item(&mut world, "ring", c2);
        for c in [c1, c2] {
            world.entity_mut(c).insert(ItemTimer {
                remaining_secs: 1,
                decompose_window_secs: 0,
            });
        }
        item_decay_tick(&mut world);
        assert!(world.get::<ItemTimer>(in_chest).is_none());
        assert!(world.get::<ItemTimer>(carried).is_none());
    }

    #[test]
    fn existing_timer_is_kept_by_start_decomposing() {
        let mut world = proto_world(&[]);
        let room = world.spawn(mud_world::Room).id();
        let e = item(&mut world, "torch", room);
        world.entity_mut(e).insert(ItemTimer {
            remaining_secs: 7,
            decompose_window_secs: 0,
        });
        start_decomposing(&mut world, e);
        assert_eq!(world.get::<ItemTimer>(e).unwrap().remaining_secs, 7);
    }

    fn rotting(world: &mut World, name: &str, at: Entity) -> Entity {
        let e = item(world, name, at);
        world.entity_mut(e).insert((
            ItemTimer {
                remaining_secs: 1000,
                decompose_window_secs: 0,
            },
            Decomposing,
        ));
        e
    }

    #[test]
    fn picked_up_rotting_item_stops_rotting() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let player = world.spawn((mud_world::Player, Located(room))).id();
        let sword = rotting(&mut world, "sword", room);
        let rope = rotting(&mut world, "rope", room);
        item_decay_tick(&mut world);
        assert_eq!(world.get::<ItemTimer>(sword).unwrap().remaining_secs, 999);
        // get
        world.entity_mut(sword).insert(Located(player));
        item_decay_tick(&mut world);
        assert!(world.get::<ItemTimer>(sword).is_none());
        assert!(world.get::<Decomposing>(sword).is_none());
        // Left on the floor, the other keeps rotting.
        assert_eq!(world.get::<ItemTimer>(rope).unwrap().remaining_secs, 998);
    }

    #[test]
    fn worn_rotting_item_stops_rotting() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let player = world.spawn((mud_world::Player, Located(room))).id();
        let helm = rotting(&mut world, "helm", player);
        world
            .entity_mut(helm)
            .insert(mud_world::EquippedSlot(mud_world::Slot::Head));
        item_decay_tick(&mut world);
        assert!(world.get::<ItemTimer>(helm).is_none());
    }

    #[test]
    fn rotting_contents_stop_when_their_bag_is_carried() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let player = world.spawn((mud_world::Player, Located(room))).id();
        let bag = rotting(&mut world, "bag", room);
        let gem = rotting(&mut world, "gem", bag);
        item_decay_tick(&mut world);
        // On the floor the bag and its contents both rot.
        assert!(world.get::<ItemTimer>(gem).is_some());
        world.entity_mut(bag).insert(Located(player));
        item_decay_tick(&mut world);
        assert!(world.get::<ItemTimer>(bag).is_none());
        assert!(world.get::<ItemTimer>(gem).is_none());
    }

    #[test]
    fn rotting_item_put_into_a_carried_bag_stops_rotting() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let player = world.spawn((mud_world::Player, Located(room))).id();
        let bag = item(&mut world, "bag", player);
        let gem = rotting(&mut world, "gem", room);
        world.entity_mut(gem).insert(Located(bag));
        item_decay_tick(&mut world);
        assert!(world.get::<ItemTimer>(gem).is_none());
    }

    #[test]
    fn rotting_item_in_a_floor_container_keeps_rotting() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let chest = item(&mut world, "chest", room);
        let gem = rotting(&mut world, "gem", chest);
        item_decay_tick(&mut world);
        assert_eq!(world.get::<ItemTimer>(gem).unwrap().remaining_secs, 999);
    }

    #[test]
    fn intrinsic_timers_keep_running_while_carried() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let player = world.spawn((mud_world::Player, Located(room))).id();
        let torch = item(&mut world, "torch", player);
        world.entity_mut(torch).insert(ItemTimer {
            remaining_secs: 1000,
            decompose_window_secs: 0,
        });
        // start_decomposing leaves an existing timer unmarked.
        start_decomposing(&mut world, torch);
        assert!(world.get::<Decomposing>(torch).is_none());
        item_decay_tick(&mut world);
        assert_eq!(world.get::<ItemTimer>(torch).unwrap().remaining_secs, 999);
    }
}
