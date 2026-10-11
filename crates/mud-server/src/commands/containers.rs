//! Open / closed / locked containers (legacy `act.movement.cpp`
//! `open_object` / `close_object` / `lock_object` / `unlock_object` /
//! `pick_object`, and the `CONT_CLOSED` checks in `act.get.cpp` /
//! `act.item.cpp`).
//!
//! State lives in [`mud_world::ContainerState`]. An instance has no
//! component until a command first changes it: [`container_state`] derives
//! the authored default from the proto's `values` JSON (`Flags` + `Key`),
//! so every spawn path (zone reset, shop, login, script) gets the right
//! state without being touched. A container respawned by the object-reset
//! pass is a fresh entity and so comes back in its authored state.
//!
//! A carried container's state persists in the `Flags` key of its
//! `CharacterItems.custom_values` (the layout legacy imports wrote, so a
//! chest a player left unlocked in legacy stays unlocked). [`restore_saved`]
//! seeds the component from it at login, `save_player` writes it back, and
//! a house item or account-chest row carries the same flags.
//!
//! The door verbs (`open`, `close`, `lock`, `unlock`, `pick`) call
//! [`try_container_verb`] first and fall through to the door code when the
//! argument is not a container.

use bevy_ecs::prelude::*;
use mud_db::enums::ObjectType;
use mud_world::{
    ContainerState, CoreAbilities, Item, KnownAbilities, Located, ObjectPrototypes, Stamina,
    WorldKey,
};

use super::item_target::{ItemClass, find_item};
use super::{broadcast_room_anonymised, name_of, parse_direction, send_to};

/// The five door verbs, applied to a container.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Verb {
    Open,
    Close,
    Lock,
    Unlock,
    Pick,
}

/// The container state of `item`: its override component, else the proto
/// default. `None` when `item` is not an `ObjectType::Container` object
/// (corpses, bags of coins, weapons, ...), which are always open.
pub(crate) fn container_state(world: &World, item: Entity) -> Option<ContainerState> {
    if let Some(s) = world.get::<ContainerState>(item) {
        return Some(*s);
    }
    let key = world.get::<WorldKey>(item)?;
    let proto = world
        .get_resource::<ObjectPrototypes>()?
        .by_key
        .get(&(key.zone, key.id))?;
    (proto.r#type == ObjectType::Container).then(|| ContainerState::from_values(&proto.values))
}

/// Seed `item`'s state from a saved row: the proto default with the
/// `Flags` / `Key` of `saved` (a `custom_values` blob) laid over it. The
/// component is only attached when that differs from the proto, so an
/// untouched container keeps following its prototype. Does nothing for a
/// non-container.
pub(crate) fn restore_saved(world: &mut World, item: Entity, saved: &serde_json::Value) {
    let Some(base) = container_state(world, item) else {
        return;
    };
    let seeded = base.overlay(saved);
    if seeded != base {
        world.entity_mut(item).insert(seeded);
    }
}

/// Seed `item`'s state from saved `Flags` entries (house / chest rows).
pub(crate) fn restore_saved_flags(world: &mut World, item: Entity, flags: &[String]) {
    restore_saved(world, item, &serde_json::json!({ "Flags": flags }));
}

/// `true` (after telling `player`) when `container` is closed. Gate for
/// `get`, `put`, `look in` and `search` into a container.
pub(crate) fn refuse_if_closed(world: &World, player: Entity, container: Entity) -> bool {
    if !container_state(world, container).is_some_and(|s| s.closed) {
        return false;
    }
    let name = name_of(world, container);
    send_to(
        world,
        player,
        format!("{} is closed.\r\n", capitalize_first(&name)),
    );
    true
}

/// Uppercase the first visible character (skipping leading `<tag>`s).
fn capitalize_first(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut done = false;
    for c in s.chars() {
        match c {
            '<' if !done => {
                in_tag = true;
                out.push(c);
            }
            '>' if in_tag => {
                in_tag = false;
                out.push(c);
            }
            _ if !done && !in_tag => {
                out.extend(c.to_uppercase());
                done = true;
            }
            _ => out.push(c),
        }
    }
    out
}

fn set_state(world: &mut World, item: Entity, edit: impl FnOnce(&mut ContainerState)) {
    if let Some(mut state) = container_state(world, item) {
        edit(&mut state);
        world.entity_mut(item).insert(state);
    }
}

/// Run `verb` on the container `arg` names. Returns `false` (nothing
/// sent) when `arg` is empty, a direction, or names no container, so the
/// caller can treat it as a door.
pub(crate) fn try_container_verb(world: &mut World, player: Entity, verb: Verb, arg: &str) -> bool {
    let arg = arg.trim();
    if arg.is_empty() {
        return false;
    }
    // A direction word is the door (`open d`, `unlock s`), even when a
    // keyword of some item in reach happens to match it.
    if parse_direction(arg).is_some() {
        return false;
    }
    let Some(item) = find_item(world, player, arg, ItemClass::RoomFirst) else {
        return false;
    };
    // A non-container item that merely shares a keyword with the door
    // (`open gate` with a gate-shaped trinket in reach) is not what was
    // meant: the door code answers.
    let Some(state) = container_state(world, item) else {
        return false;
    };
    match verb {
        Verb::Open => open(world, player, item, state),
        Verb::Close => close(world, player, item, state),
        Verb::Unlock => unlock(world, player, item, state),
        Verb::Lock => lock(world, player, item, state),
        Verb::Pick => pick(world, player, item, state),
    }
    true
}

fn say_room(world: &mut World, player: Entity, text: &str) {
    let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
        return;
    };
    let me = name_of(world, player);
    broadcast_room_anonymised(world, room, &[player], &[(player, me.as_str())], text);
}

fn open(world: &mut World, player: Entity, item: Entity, state: ContainerState) {
    let name = name_of(world, item);
    let me = name_of(world, player);
    if !state.closeable {
        send_to(world, player, "You can't open that.\r\n");
    } else if !state.closed {
        send_to(world, player, "It's already open.\r\n");
    } else if state.locked {
        send_to(world, player, "It seems to be locked.\r\n");
        say_room(
            world,
            player,
            &format!("{me} tries to open {name}, but it's locked.\r\n"),
        );
    } else {
        set_state(world, item, |s| s.closed = false);
        send_to(world, player, format!("You open {name}.\r\n"));
        say_room(world, player, &format!("{me} opens {name}.\r\n"));
    }
}

fn close(world: &mut World, player: Entity, item: Entity, state: ContainerState) {
    let name = name_of(world, item);
    let me = name_of(world, player);
    if !state.closeable {
        send_to(world, player, "You can't close that.\r\n");
    } else if state.closed {
        send_to(world, player, "It's already closed.\r\n");
    } else {
        set_state(world, item, |s| s.closed = true);
        send_to(world, player, format!("You close {name}.\r\n"));
        say_room(world, player, &format!("{me} closes {name}.\r\n"));
    }
}

/// The carried key a keyed lock wants, `Ok(None)` when the actor needs
/// none (typed immortals), `Err` after telling the player why not.
fn require_key(
    world: &mut World,
    player: Entity,
    state: &ContainerState,
) -> Result<Option<Entity>, ()> {
    if crate::room_access::can_unlock_without_key(world, player) {
        return Ok(None);
    }
    let Some(key_req) = state.key else {
        send_to(world, player, "Odd - you can't seem to find a keyhole.\r\n");
        return Err(());
    };
    let Some(key) = carried_key(world, player, key_req) else {
        send_to(world, player, "You don't seem to have the proper key.\r\n");
        return Err(());
    };
    Ok(Some(key))
}

/// An item `player` carries (or wears) whose proto is `key`.
fn carried_key(world: &mut World, player: Entity, key: (i32, i32)) -> Option<Entity> {
    let mut q = world.query_filtered::<(Entity, &Located, &WorldKey), With<Item>>();
    q.iter(world)
        .find(|(_, l, k)| l.0 == player && (k.zone, k.id) == key)
        .map(|(e, _, _)| e)
}

fn unlock(world: &mut World, player: Entity, item: Entity, state: ContainerState) {
    let name = name_of(world, item);
    let me = name_of(world, player);
    if !state.closeable {
        send_to(world, player, "You can't unlock that.\r\n");
        return;
    }
    if !state.closed {
        send_to(world, player, "It's already open!\r\n");
        return;
    }
    let Ok(key) = require_key(world, player, &state) else {
        return;
    };
    if !state.locked {
        if let Some(key) = key {
            let key_name = name_of(world, key);
            send_to(
                world,
                player,
                format!("You insert {key_name}, only to find that {name} isn't locked.\r\n"),
            );
        } else {
            send_to(world, player, "Oh... it wasn't locked.\r\n");
        }
        return;
    }
    set_state(world, item, |s| s.locked = false);
    if let Some(key) = key {
        let key_name = name_of(world, key);
        send_to(
            world,
            player,
            format!("You unlock {name} with {key_name}.\r\n"),
        );
        say_room(
            world,
            player,
            &format!("{me} unlocks {name} with {key_name}.\r\n"),
        );
    } else {
        send_to(world, player, format!("You unlock {name}.\r\n"));
        say_room(world, player, &format!("{me} unlocks {name}.\r\n"));
    }
}

fn lock(world: &mut World, player: Entity, item: Entity, state: ContainerState) {
    let name = name_of(world, item);
    let me = name_of(world, player);
    if !state.closeable {
        send_to(world, player, "You can't lock that.\r\n");
        return;
    }
    if !state.closed {
        send_to(world, player, "You'll need to close it first.\r\n");
        return;
    }
    let Ok(key) = require_key(world, player, &state) else {
        return;
    };
    if state.locked {
        if let Some(key) = key {
            let key_name = name_of(world, key);
            send_to(
                world,
                player,
                format!("You insert {key_name}, only to find that {name} is already locked.\r\n"),
            );
        } else {
            send_to(world, player, "It seems to be locked already.\r\n");
        }
        return;
    }
    set_state(world, item, |s| s.locked = true);
    if let Some(key) = key {
        let key_name = name_of(world, key);
        send_to(
            world,
            player,
            format!("You insert {key_name} into {name} and lock it.\r\n"),
        );
        say_room(
            world,
            player,
            &format!("{me} inserts {key_name} into {name} and locks it.\r\n"),
        );
    } else {
        send_to(world, player, format!("You lock {name}.\r\n"));
        say_room(world, player, &format!("{me} locks {name}.\r\n"));
    }
}

/// `pick` on a container: same gates and skill roll as the door version
/// (`commands::spells::cmd_pick`), without the keyhole requirement
/// (legacy `pick_object` only checks pickproof).
fn pick(world: &mut World, player: Entity, item: Entity, state: ContainerState) {
    const STAMINA_COST: i32 = 5;
    let name = name_of(world, item);
    let me = name_of(world, player);
    if !state.closeable {
        send_to(world, player, "You can't pick that.\r\n");
        return;
    }
    if !state.closed {
        send_to(world, player, "It's already open!\r\n");
        return;
    }
    if !state.locked {
        send_to(world, player, "Oh... it wasn't locked, after all.\r\n");
        say_room(
            world,
            player,
            &format!("{me} sets to picking {name}, but soon realizes that it isn't locked.\r\n"),
        );
        return;
    }
    if state.pickproof {
        send_to(world, player, "It resists your attempts to pick it.\r\n");
        say_room(
            world,
            player,
            &format!("{me} sets to picking {name}, but soon gives up.\r\n"),
        );
        return;
    }
    let proficiency = world
        .get_resource::<CoreAbilities>()
        .and_then(|c| c.pick_lock)
        .and_then(|pick| {
            world
                .get::<KnownAbilities>(player)?
                .entries
                .iter()
                .find(|(id, _, _)| *id == pick)
                .map(|(_, p, _)| *p)
        });
    let Some(proficiency) = proficiency else {
        send_to(world, player, "You don't know how to pick locks.\r\n");
        return;
    };
    if world
        .get::<Stamina>(player)
        .is_none_or(|s| s.current < STAMINA_COST)
    {
        send_to(
            world,
            player,
            "You don't have the stamina for a steady hand.\r\n",
        );
        return;
    }
    if let Some(mut s) = world.get_mut::<Stamina>(player) {
        s.current = (s.current - STAMINA_COST).max(0);
    }
    let roll = rand::random_range(1..=100);
    let chance = (proficiency / 10).clamp(0, 100);
    if roll <= chance {
        set_state(world, item, |s| s.locked = false);
        send_to(world, player, "You skillfully pick the lock.\r\n");
        say_room(
            world,
            player,
            &format!("{me} skillfully picks the lock on {name}.\r\n"),
        );
    } else {
        send_to(world, player, "You failed to pick the lock.\r\n");
        say_room(
            world,
            player,
            &format!("{me} fumbles with the lock on {name}.\r\n"),
        );
    }
}
