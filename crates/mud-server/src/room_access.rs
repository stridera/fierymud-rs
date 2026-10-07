//! Room access rules shared by every movement path.
//!
//! * **Entry restrictions** — `Room.entry_restriction` is a Lua body (the
//!   importer turns legacy GODROOM into `return actor:is_god()`). It is
//!   evaluated, with the mover as `actor`, on *every* way into a room:
//!   walking, followers, flee/retreat, recall, summon, teleport, portals,
//!   drag, mob wandering. Staff (Immortal+) bypass. The gate fails
//!   **closed**: a script error, a missing Lua host, or a non-boolean
//!   result all refuse entry (and log).
//! * **God zones** — `Zones.is_god_zone` content is invisible to mortals;
//!   [`zone_visible_to`] / [`room_visible_to`] are the single test every
//!   mortal-facing zone surface uses.
//! * **Random teleport destinations** — [`pick_random_destination`] picks a
//!   legal landing room for the `random` teleport destination, honouring the
//!   no-teleport / death-trap / god-zone / entry-restriction / private /
//!   peaceful exclusions, in either zone-limited or whole-world range.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{
    Account, Contents, DeathTrap, EntryRestriction, Located, Mob, NoTeleportRoom, PeacefulRoom,
    Player, RoomCapacity, WorldKey, WorldKeyIndex, room_in_god_zone, zone_is_god,
};

/// Legacy refusal (act.movement.cpp `do_simple_move`, GODROOM branch).
pub(crate) const ENTRY_REFUSED: &str = "A mysterious powerful force pushes you back.\r\n";

/// Legacy `perform_teleport_spell` retry budget (spells.cpp).
pub(crate) const RANDOM_TELEPORT_TRIES: u32 = 100;

/// A room whose capacity is at or below this is a private room: the
/// importer maps legacy PRIVATE to capacity 2 and TUNNEL to 1.
const PRIVATE_ROOM_MAX_CAPACITY: i32 = 2;

/// Staff = effective rank Immortal or above. Deliberately ignores
/// `DevMode`: visibility and entry rules are about who the character *is*.
pub(crate) fn is_immortal(world: &World, entity: Entity) -> bool {
    world
        .get::<Account>(entity)
        .is_some_and(|a| a.role.at_least(UserRole::Immortal))
}

// ---------------------------------------------------------------------------
// God-zone visibility
// ---------------------------------------------------------------------------

/// May `viewer` see (be told about) zone `zone_id`?
pub(crate) fn zone_visible_to(world: &World, viewer: Entity, zone_id: i32) -> bool {
    !zone_is_god(world, zone_id) || is_immortal(world, viewer)
}

/// May `viewer` be told about `room` (its zone, name-in-listings, ...)?
pub(crate) fn room_visible_to(world: &World, viewer: Entity, room: Entity) -> bool {
    !room_in_god_zone(world, room) || is_immortal(world, viewer)
}

// ---------------------------------------------------------------------------
// Entry restrictions
// ---------------------------------------------------------------------------

/// Evaluate an entry-restriction body for `mover`. Only an explicit boolean
/// `true` admits; everything else (false, non-boolean, error, no host)
/// refuses.
fn evaluate_restriction(world: &mut World, mover: Entity, dest: Entity, expr: &str) -> bool {
    let body = if expr.contains("return") {
        expr.to_string()
    } else {
        format!("return ({expr})")
    };
    let room = world.get::<WorldKey>(dest).map(|k| (k.zone, k.id));
    if !world.contains_resource::<mud_script::LuaHost>() {
        tracing::error!(
            ?room,
            "entry restriction not evaluable (no Lua host); refusing entry"
        );
        return false;
    }
    let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
        host.exec_for_event_with_value(world, mover, mover, None, &body, &[])
    });
    match result {
        Ok((_out, Some(allowed))) => allowed,
        Ok((_out, None)) => {
            tracing::warn!(?room, expr = %expr, "entry restriction returned a non-boolean; refusing entry");
            false
        }
        Err(e) => {
            tracing::warn!(?room, expr = %expr, error = %e, "entry restriction script failed; refusing entry");
            false
        }
    }
}

/// May `mover` enter `dest`? Rooms without a restriction admit everyone;
/// staff bypass; otherwise the room's Lua restriction decides.
pub(crate) fn entry_allowed(world: &mut World, mover: Entity, dest: Entity) -> bool {
    let Some(expr) = world.get::<EntryRestriction>(dest).map(|r| r.0.clone()) else {
        return true;
    };
    if is_immortal(world, mover) {
        return true;
    }
    evaluate_restriction(world, mover, dest, &expr)
}

/// Like [`entry_allowed`] for a mover being led by `leader`. Legacy lets a
/// follower of a deity into a restricted room once the deity is already
/// standing in it (act.movement.cpp: `ch->master->in_room == dest`).
pub(crate) fn entry_allowed_following(
    world: &mut World,
    mover: Entity,
    dest: Entity,
    leader: Option<Entity>,
) -> bool {
    if world.get::<EntryRestriction>(dest).is_none() {
        return true;
    }
    if let Some(leader) = leader
        && is_immortal(world, leader)
        && world.get::<Located>(leader).is_some_and(|l| l.0 == dest)
    {
        return true;
    }
    entry_allowed(world, mover, dest)
}

/// Refusal helper for typed movement: when `mover` may not enter `dest`,
/// tell them (players only) and return `true`.
pub(crate) fn refuse_entry(world: &mut World, mover: Entity, dest: Entity) -> bool {
    if entry_allowed(world, mover, dest) {
        return false;
    }
    crate::commands::send_to(world, mover, ENTRY_REFUSED);
    true
}

/// Drop every candidate `(direction, room)` that any of `movers` may not
/// enter. Used by flee/retreat, which pick a random exit and must never
/// pick one into a room that refuses the fleer.
pub(crate) fn retain_admitted<T>(
    world: &mut World,
    movers: &[Entity],
    candidates: &mut Vec<(T, Entity)>,
) {
    candidates.retain(|(_, room)| movers.iter().all(|m| entry_allowed(world, *m, *room)));
}

// ---------------------------------------------------------------------------
// Random teleport destinations
// ---------------------------------------------------------------------------

/// How far a random teleport may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RandomRange {
    /// Only rooms in the caster's current zone (legacy `spell_teleport`).
    Zone,
    /// Any room in the world (legacy `spell_world_teleport`).
    World,
}

/// Tunables for a `destination: "random"` teleport effect, read from the
/// effect params (`AbilityEffect.override_params` over `Effect.default_params`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RandomTeleportParams {
    pub(crate) range: RandomRange,
    /// `Some((base, per_skill))`: the cast succeeds when
    /// `random(1,100) <= base + skill * per_skill`. `None`: always succeeds.
    pub(crate) success: Option<(i32, i32)>,
}

pub(crate) fn parse_random_params(
    override_params: Option<&serde_json::Value>,
    default_params: Option<&serde_json::Value>,
) -> RandomTeleportParams {
    let get = |key: &str| -> Option<&serde_json::Value> {
        override_params
            .and_then(|p| p.get(key))
            .or_else(|| default_params.and_then(|p| p.get(key)))
    };
    let int = |key: &str| -> Option<i32> {
        get(key)
            .and_then(serde_json::Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
    };
    let range = match get("range")
        .and_then(serde_json::Value::as_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("zone") => RandomRange::Zone,
        _ => RandomRange::World,
    };
    let success =
        int("success_base_pct").map(|base| (base, int("success_per_skill_pct").unwrap_or(0)));
    RandomTeleportParams { range, success }
}

/// Legacy success roll (`random_number(1, 100) > 10 + skill * 2` fails).
pub(crate) fn teleport_roll_succeeds(
    params: &RandomTeleportParams,
    skill: i32,
    roll_1_to_100: i32,
) -> bool {
    match params.success {
        None => true,
        Some((base, per_skill)) => {
            roll_1_to_100 <= base.saturating_add(skill.saturating_mul(per_skill))
        }
    }
}

/// How many players and mobs currently stand in `room`.
fn occupants(world: &World, room: Entity) -> i32 {
    let Some(contents) = world.get::<Contents>(room) else {
        return 0;
    };
    let n = contents
        .iter()
        .filter(|e| world.get::<Player>(*e).is_some() || world.get::<Mob>(*e).is_some())
        .count();
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// Static (script-free) exclusions for a random teleport landing room.
fn room_excluded_from_random(world: &World, room: Entity, current: Option<Entity>) -> bool {
    Some(room) == current
        || world.get::<NoTeleportRoom>(room).is_some()
        || world.get::<DeathTrap>(room).is_some()
        || world.get::<PeacefulRoom>(room).is_some()
        || room_in_god_zone(world, room)
        || world
            .get::<RoomCapacity>(room)
            .is_some_and(|c| c.0 <= PRIVATE_ROOM_MAX_CAPACITY || occupants(world, room) >= c.0)
}

/// Pick a landing room for `victim`, trying up to `tries` random rooms.
/// `None` means nothing qualified (the caller prints the legacy "sputters
/// out" message). The `victim` is the entity that will move; its entry
/// restrictions are honoured so a mortal is never dropped into a god room.
pub(crate) fn pick_random_destination(
    world: &mut World,
    victim: Entity,
    range: RandomRange,
    tries: u32,
) -> Option<Entity> {
    let current = world.get::<Located>(victim).map(|l| l.0);
    let zone = current
        .and_then(|r| world.get::<WorldKey>(r))
        .map(|k| k.zone);
    let candidates: Vec<Entity> = {
        let index = world.get_resource::<WorldKeyIndex>()?;
        match range {
            RandomRange::World => index.rooms.values().copied().collect(),
            RandomRange::Zone => {
                let zone = zone?;
                index
                    .rooms
                    .iter()
                    .filter(|((z, _), _)| *z == zone)
                    .map(|(_, e)| *e)
                    .collect()
            }
        }
    };
    if candidates.is_empty() {
        return None;
    }
    for _ in 0..tries {
        let room = candidates[rand::random_range(0..candidates.len())];
        if room_excluded_from_random(world, room, current) {
            continue;
        }
        if !entry_allowed(world, victim, room) {
            continue;
        }
        return Some(room);
    }
    None
}

/// `true` when `entity` is a player below Immortal whose own room forbids
/// teleporting out (`Room.allows_teleport = false`). Staff are exempt.
pub(crate) fn teleport_blocked_here(world: &World, entity: Entity) -> bool {
    !is_immortal(world, entity)
        && world
            .get::<Located>(entity)
            .is_some_and(|l| world.get::<NoTeleportRoom>(l.0).is_some())
}
