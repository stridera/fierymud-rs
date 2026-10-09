//! `attach` / `detach` — legacy `dg_scripts.cpp` `do_attach` / `do_detach`.
//!
//! Bolt a trigger onto a live mob, object or room, or take one off, without
//! touching the database: the change lives on the entity's
//! [`AttachedTriggers`] and is gone when the entity is (a respawned mob gets
//! its prototype's triggers again; a trigger reload resets every room). The
//! trigger dispatchers read `AttachedTriggers` on every event, so a runtime
//! attachment fires and a detached one stops at once.
//!
//! Triggers are named by their composite key, `zone:id`. A bare number is
//! read as a legacy vnum (`3045` is `30:45`, `45` is `1000:45`).
//!
//! `trigattach` / `trigdetach` are the builder-level variants that take a
//! `<target> <zone> <id>` and search only the caller's room; these are the
//! legacy-syntax implementor commands.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{
    AttachedTriggers, Item, Keywords, Located, Mob, Named, Player, Room, TriggerAttach,
    TriggerCatalog, WorldKeyIndex,
};

use crate::commands::{
    Category, Command, EquipFilter, Help, find_actor_in_room, find_carried_by, find_in_room,
    matches, name_of, parse_indexed_needle, record_admin_action, send_to,
};

inventory::submit! {
    Command {
        names: &["attach"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "attach { mtr | otr | wtr } { zone:id } { name | room } [ position ]",
            summary: "Attach a trigger to a live mob, object or room.",
            long: "Implementor only. 'attach mtr 30:5 guard' bolts trigger \
                   30:5 onto the mob 'guard'; otr takes an object name, wtr a \
                   room as 'zone:id'. The change is live only: it is not \
                   saved, and a respawned mob starts over with its \
                   prototype's triggers. <position> is 0 for the front of the \
                   list, otherwise the index to insert at (default: the \
                   end). The trigger's own type has to match: mtr for a mob \
                   trigger, otr for an object trigger, wtr for a world \
                   trigger.",
        },
        run: cmd_attach,
    }
}

inventory::submit! {
    Command {
        names: &["detach"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "detach [ mob | object ] { target } { trigger | all }  |  detach room { trigger | all }",
            summary: "Remove a trigger from a live mob, object or room.",
            long: "Implementor only. The trigger is 'zone:id', its place in \
                   the list (1 is the first), or a word of its name \
                   ('2.beggar' for the second match). 'all' removes every \
                   trigger. 'detach <target> <trigger>' finds the target \
                   itself, preferring what you wear or carry. Live only; \
                   nothing is saved.",
        },
        run: cmd_detach,
    }
}

/// Legacy `is_abbrev`: a non-empty prefix of `full`, case-insensitive.
fn abbrev(typed: &str, full: &str) -> bool {
    !typed.is_empty()
        && full
            .get(..typed.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(typed))
}

/// `zone:id`, or a bare legacy vnum (`zone * 100 + id`, zone 0 is 1000).
pub(crate) fn parse_key(s: &str) -> Option<(i32, i32)> {
    if let Some((z, i)) = s.split_once(':') {
        return Some((z.parse().ok()?, i.parse().ok()?));
    }
    let n: i32 = s.parse().ok()?;
    if n < 0 {
        return None;
    }
    let zone = n / 100;
    Some((if zone == 0 { 1000 } else { zone }, n % 100))
}

/// Legacy `find_obj_around_char`: carried or worn, then the room, then
/// anywhere in the world.
fn find_object_around(world: &mut World, player: Entity, needle: &str) -> Option<Entity> {
    if let Some(e) = find_carried_by(world, needle, player, EquipFilter::Anywhere) {
        return Some(e);
    }
    if let Some(room) = world.get::<Located>(player).map(|l| l.0)
        && let Some(e) = find_in_room(world, needle, room)
    {
        return Some(e);
    }
    find_object_in_world(world, needle)
}

fn find_object_in_world(world: &mut World, needle: &str) -> Option<Entity> {
    let (index, needle) = parse_indexed_needle(needle);
    let needle = needle.to_ascii_lowercase();
    let mut q = world.query_filtered::<(Entity, &Named, Option<&Keywords>), With<Item>>();
    let mut hits: Vec<Entity> = q
        .iter(world)
        .filter(|(_, n, kw)| matches(&needle, n, *kw))
        .map(|(e, _, _)| e)
        .collect();
    hits.sort_by_key(|e| e.index_u32());
    hits.get(index - 1).copied()
}

fn trigger_label(world: &World, key: (i32, i32)) -> String {
    world
        .get_resource::<TriggerCatalog>()
        .and_then(|c| c.by_key.get(&key))
        .map_or_else(|| "?".to_string(), |d| d.name.clone())
}

/// Resolve the `attach` target to an entity and a name for messages,
/// sending the refusal itself when there is none. The target is resolved
/// before the trigger so "no such mob" beats "no such trigger", as in
/// legacy.
fn attach_target(
    world: &mut World,
    player: Entity,
    want: TriggerAttach,
    target: &str,
) -> Option<(Entity, String)> {
    match want {
        TriggerAttach::Mob => {
            let Some(e) = super::admin_world::find_actor_anywhere(world, player, target) else {
                send_to(world, player, "That mob does not exist.\r\n");
                return None;
            };
            if world.get::<Mob>(e).is_none() {
                send_to(
                    world,
                    player,
                    if world.get::<Player>(e).is_some() {
                        "Players can't have scripts.\r\n"
                    } else {
                        "That mob does not exist.\r\n"
                    },
                );
                return None;
            }
            Some((e, name_of(world, e)))
        }
        TriggerAttach::Object => {
            let Some(e) = find_object_around(world, player, target) else {
                send_to(world, player, "That object does not exist.\r\n");
                return None;
            };
            Some((e, name_of(world, e)))
        }
        TriggerAttach::World => {
            let Some(key) = parse_key(target) else {
                send_to(world, player, "You need to supply a room number.\r\n");
                return None;
            };
            let room = world
                .get_resource::<WorldKeyIndex>()
                .and_then(|i| i.rooms.get(&key).copied());
            let Some(room) = room else {
                send_to(world, player, "No room exists with that number.\r\n");
                return None;
            };
            Some((room, format!("room {}:{}", key.0, key.1)))
        }
    }
}

fn cmd_attach(world: &mut World, player: Entity, args: &str) {
    const USAGE: &str =
        "Usage: attach { mtr | otr | wtr } { zone:id } { name | room } [ position ]\r\n";
    let words: Vec<&str> = args.split_whitespace().collect();
    let [kind, trig, target, rest @ ..] = words.as_slice() else {
        send_to(world, player, USAGE);
        return;
    };
    record_admin_action(world, player, "attach", args);
    // Legacy `loc`: -1 (or none) is the end, 0 the front.
    let position = rest
        .first()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(-1);

    let want = if abbrev(kind, "mtr") {
        TriggerAttach::Mob
    } else if abbrev(kind, "otr") {
        TriggerAttach::Object
    } else if abbrev(kind, "wtr") {
        TriggerAttach::World
    } else {
        send_to(world, player, "Please specify 'mtr', 'otr', or 'wtr'.\r\n");
        return;
    };

    // Resolve the target first so "no such mob" beats "no such trigger",
    // as in legacy.
    let Some((entity, short)) = attach_target(world, player, want, target) else {
        return;
    };

    let Some(key) = parse_key(trig) else {
        send_to(world, player, "That trigger does not exist.\r\n");
        return;
    };
    let def = world
        .get_resource::<TriggerCatalog>()
        .and_then(|c| c.by_key.get(&key))
        .map(|d| (d.name.clone(), d.attach_type));
    let Some((trig_name, attach_type)) = def else {
        send_to(world, player, "That trigger does not exist.\r\n");
        return;
    };
    if attach_type != want {
        let wanted = match attach_type {
            TriggerAttach::Mob => "mtr",
            TriggerAttach::Object => "otr",
            TriggerAttach::World => "wtr",
        };
        send_to(
            world,
            player,
            format!("That trigger is not a {kind} trigger; attach it with '{wanted}'.\r\n"),
        );
        return;
    }

    if world.get::<AttachedTriggers>(entity).is_none()
        && let Ok(mut em) = world.get_entity_mut(entity)
    {
        em.insert(AttachedTriggers::default());
    }
    let Some(mut list) = world.get_mut::<AttachedTriggers>(entity) else {
        send_to(world, player, "That cannot take a trigger.\r\n");
        return;
    };
    if list.0.contains(&key) {
        send_to(
            world,
            player,
            format!(
                "Trigger {}:{} ({trig_name}) is already attached to {short}.\r\n",
                key.0, key.1
            ),
        );
        return;
    }
    let at = if position < 0 {
        list.0.len()
    } else {
        usize::try_from(position).unwrap_or(0).min(list.0.len())
    };
    list.0.insert(at, key);
    send_to(
        world,
        player,
        format!(
            "Trigger {}:{} ({trig_name}) attached to {short}.\r\n",
            key.0, key.1
        ),
    );
}

/// What `detach` found: the entity, and a name for messages.
enum Found {
    Mob(Entity),
    Object(Entity),
    Room(Entity),
}

fn cmd_detach(world: &mut World, player: Entity, args: &str) {
    const USAGE: &str = "Usage: detach [ mob | object ] { target } { trigger | 'all' }  |  detach room { trigger | 'all' }\r\n";
    let words: Vec<&str> = args.split_whitespace().collect();
    let [first, second, rest @ ..] = words.as_slice() else {
        send_to(world, player, USAGE);
        return;
    };
    record_admin_action(world, player, "detach", args);
    let third = rest.first().copied().unwrap_or("");

    let (found, trigger) = if first.eq_ignore_ascii_case("room") {
        let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
            send_to(world, player, "You're nowhere.\r\n");
            return;
        };
        (Found::Room(room), *second)
    } else if abbrev(first, "mob") {
        let Some(e) = super::admin_world::find_actor_anywhere(world, player, second) else {
            send_to(world, player, "No such mobile around.\r\n");
            return;
        };
        if third.is_empty() {
            send_to(world, player, "You must specify a trigger to remove.\r\n");
            return;
        }
        (Found::Mob(e), third)
    } else if abbrev(first, "object") {
        let Some(e) = find_object_around(world, player, second) else {
            send_to(world, player, "No such object around.\r\n");
            return;
        };
        if third.is_empty() {
            send_to(world, player, "You must specify a trigger to remove.\r\n");
            return;
        }
        (Found::Object(e), third)
    } else {
        let Some(found) = find_any(world, player, first) else {
            send_to(world, player, "Nothing around by that name.\r\n");
            return;
        };
        (found, *second)
    };

    let (entity, kind, short) = match found {
        Found::Mob(e) => {
            if world.get::<Player>(e).is_some() || world.get::<Mob>(e).is_none() {
                send_to(world, player, "Players don't have triggers.\r\n");
                return;
            }
            (e, "mob", name_of(world, e))
        }
        Found::Object(e) => (e, "object", name_of(world, e)),
        Found::Room(e) => {
            if world.get::<Room>(e).is_none() {
                send_to(world, player, "You're nowhere.\r\n");
                return;
            }
            (e, "room", "room".to_string())
        }
    };

    let has_any = world
        .get::<AttachedTriggers>(entity)
        .is_some_and(|a| !a.0.is_empty());
    if !has_any {
        let msg = match kind {
            "room" => "This room does not have any triggers.\r\n".to_string(),
            _ => format!("That {kind} doesn't have any triggers.\r\n"),
        };
        send_to(world, player, msg);
        return;
    }

    if trigger.eq_ignore_ascii_case("all") {
        if let Ok(mut em) = world.get_entity_mut(entity) {
            em.remove::<AttachedTriggers>();
        }
        send_to(
            world,
            player,
            format!("All triggers removed from {short}.\r\n"),
        );
        return;
    }

    let index = {
        let list = &world
            .get::<AttachedTriggers>(entity)
            .map_or(&[][..], |a| &a.0[..]);
        find_trigger_index(world, list, trigger)
    };
    let Some(index) = index else {
        send_to(world, player, "That trigger was not found.\r\n");
        return;
    };
    let now_empty = world
        .get_mut::<AttachedTriggers>(entity)
        .is_some_and(|mut a| {
            a.0.remove(index);
            a.0.is_empty()
        });
    if now_empty && let Ok(mut em) = world.get_entity_mut(entity) {
        em.remove::<AttachedTriggers>();
    }
    send_to(world, player, "Trigger removed.\r\n");
}

/// Legacy `do_detach` short form: what you wear or carry first, then a
/// mob in the room, an object in the room, any mob, any object.
fn find_any(world: &mut World, player: Entity, name: &str) -> Option<Found> {
    if let Some(e) = find_carried_by(world, name, player, EquipFilter::Anywhere) {
        return Some(Found::Object(e));
    }
    let room = world.get::<Located>(player).map(|l| l.0);
    if let Some(room) = room {
        if let Some(e) = find_actor_in_room(world, name, room, player) {
            return Some(Found::Mob(e));
        }
        if let Some(e) = find_in_room(world, name, room) {
            return Some(Found::Object(e));
        }
    }
    if let Some(e) = super::admin_world::find_actor_anywhere(world, player, name) {
        return Some(Found::Mob(e));
    }
    find_object_in_world(world, name).map(Found::Object)
}

/// Legacy `remove_trigger` addressing: `zone:id`; a plain number is the
/// 1-based place in the list; otherwise a word of the trigger's name,
/// `N.word` for the Nth match.
fn find_trigger_index(world: &World, list: &[(i32, i32)], spec: &str) -> Option<usize> {
    if spec.contains(':') {
        let key = parse_key(spec)?;
        return list.iter().position(|k| *k == key);
    }
    if spec.chars().next().is_some_and(|c| c.is_ascii_digit()) && !spec.contains('.') {
        let n: usize = spec.parse().ok()?;
        let n = n.max(1);
        return (n <= list.len()).then(|| n - 1);
    }
    let (nth, needle) = parse_indexed_needle(spec);
    let needle = needle.to_ascii_lowercase();
    list.iter()
        .enumerate()
        .filter(|(_, k)| {
            trigger_label(world, **k)
                .to_ascii_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| !w.is_empty() && w.starts_with(&needle))
        })
        .nth(nth - 1)
        .map(|(i, _)| i)
}
