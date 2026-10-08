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
use mud_db::enums::{MobTrait, MovementMode, ObjectType, Sector, UserRole};
use mud_world::{
    Account, Contents, DeathTrap, EntryRestriction, EquippedSlot, Flying, Item, Located, Mob,
    MobTraits, Mounted, MovementModeTag, NoTeleportRoom, ObjectPrototypes, PeacefulRoom, Player,
    Profile, RoomCapacity, RoomSector, WaterWalk, WorldKey, WorldKeyIndex, room_in_god_zone,
    zone_is_god,
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

/// Legacy `rooms.cpp` (can-go-direction): characters at `LVL_GOD` (101) or
/// above ignore hidden and closed/locked exits. The role ladder maps levels
/// 101-102 to `Builder`, so the gate is a `Builder`+ role *and* character
/// level 101+; plain `Immortal` (`LVL_IMMORT`, 100) still has to open doors.
///
/// Only a line the staff member typed themself counts: a trigger-queued or
/// `force`d command never borrows the acting character's rank.
pub(crate) fn can_pass_closed_doors(world: &World, entity: Entity) -> bool {
    is_god_level_character(world, entity)
}

/// Legacy `GET_LEVEL(ch) >= LVL_GOD` (101) judged on the *character's*
/// level as well as the role, so a low-level character on a staff account
/// does not get god powers (`kill` as an outright slay).
pub(crate) fn is_god_level_character(world: &World, entity: Entity) -> bool {
    is_god_level(world, entity) && world.get::<Profile>(entity).is_some_and(|p| p.level >= 101)
}

/// Legacy `GET_LEVEL(ch) >= LVL_GOD` (101): effective role `Builder`+.
pub(crate) fn is_god_level(world: &World, entity: Entity) -> bool {
    crate::commands::command_is_typed()
        && world
            .get::<Account>(entity)
            .is_some_and(|a| a.role.at_least(UserRole::Builder))
}

/// Legacy `rooms.cpp` `unlock_door`: only `GET_LEVEL(ch) < LVL_IMMORT` need a
/// key (or a keyhole); staff (`Immortal`+ role and character level 100+)
/// unlock anything without one.
/// Typed lines only, as for [`can_pass_closed_doors`].
pub(crate) fn can_unlock_without_key(world: &World, entity: Entity) -> bool {
    crate::commands::command_is_typed()
        && is_immortal(world, entity)
        && world.get::<Profile>(entity).is_some_and(|p| p.level >= 100)
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

/// Does the Lua source contain a `return` *keyword* (as opposed to the word
/// inside a string, a comment, or a longer identifier such as `returning`)?
/// A body with one is a statement chunk and runs as-is; a bare expression is
/// wrapped in `return (...)`.
fn has_return_statement(src: &str) -> bool {
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'[' if b.get(i + 1) == Some(&b'[') => {
                i = src[i + 2..].find("]]").map_or(b.len(), |n| i + 2 + n + 2);
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                if &src[start..i] == "return" {
                    return true;
                }
            }
            _ => i += 1,
        }
    }
    false
}

/// Evaluate an entry-restriction body for `mover` in the read-only condition
/// environment (`LuaHost::eval_condition`): `actor` can be queried but not
/// acted through, nothing can yield or be parked, and the instruction budget
/// and wall-clock watchdog apply. Only an explicit boolean `true` admits;
/// everything else (false, non-boolean, yield attempt, error, no host)
/// refuses.
fn evaluate_restriction(world: &mut World, mover: Entity, dest: Entity, expr: &str) -> bool {
    let body = if has_return_statement(expr) {
        expr.to_string()
    } else {
        format!("return ({expr}\n)")
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
        host.eval_condition(world, mover, Some(dest), &body)
    });
    match result {
        Ok(allowed) => allowed,
        Err(e) => {
            tracing::warn!(?room, expr = %expr, error = %e, "entry restriction script refused or failed; refusing entry");
            false
        }
    }
}

/// Legacy refusal (act.movement.cpp `do_simple_move`, `SECT_WATER` branch).
pub(crate) const NEED_BOAT: &str = "You need a boat or wings to go there.\r\n";

/// Legacy `can_travel_on_water` (movement.cpp) for one body: an immortal, a
/// `waterwalk` holder, an AQUATIC mob, or someone with a boat. A boat counts
/// when worn, or carried in the inventory and not wearable at all (legacy
/// `find_eq_pos < 0`); a wearable boat (a canoe worn on the off hand) has to
/// actually be worn. Boats inside containers do not count.
fn body_can_travel_on_water(world: &World, who: Entity) -> bool {
    if is_immortal(world, who)
        || world.get::<WaterWalk>(who).is_some()
        || world
            .get::<MobTraits>(who)
            .is_some_and(|t| t.has(MobTrait::Aquatic))
    {
        return true;
    }
    let Some(contents) = world.get::<Contents>(who) else {
        return false;
    };
    let protos = world.get_resource::<ObjectPrototypes>();
    contents.iter().any(|item| {
        if world.get::<Item>(item).is_none() {
            return false;
        }
        let Some(proto) = world
            .get::<WorldKey>(item)
            .and_then(|k| protos?.by_key.get(&(k.zone, k.id)))
        else {
            return false;
        };
        proto.r#type == ObjectType::Boat
            && (world.get::<EquippedSlot>(item).is_some()
                || mud_world::wear_flags_slots(&proto.wear_flags).is_empty())
    })
}

/// Legacy `flying`: airborne (the `Flying` marker, or a proto that flies) or
/// an immortal.
fn body_is_flying(world: &World, who: Entity) -> bool {
    is_immortal(world, who)
        || world.get::<Flying>(who).is_some()
        || world
            .get::<MovementModeTag>(who)
            .is_some_and(|m| m.0 == MovementMode::Flying)
}

/// Legacy "water, no swim" gate (`do_simple_move`): stepping into OR out of
/// a `Sector::Water` room needs a boat, flight or waterwalk. Shallows and
/// underwater rooms are not gated. A rider and the mount under it share
/// their gear, so either one satisfying the rule lets the pair through.
pub(crate) fn deep_water_blocks(world: &World, mover: Entity, from: Entity, to: Entity) -> bool {
    let deep = |room: Entity| {
        world
            .get::<RoomSector>(room)
            .is_some_and(|s| s.0 == Sector::Water)
    };
    if !deep(from) && !deep(to) {
        return false;
    }
    let mount = world.get::<Mounted>(mover).map(|m| m.0);
    let bodies = std::iter::once(mover).chain(mount);
    !bodies.clone().any(|b| body_can_travel_on_water(world, b))
        && !bodies.clone().any(|b| body_is_flying(world, b))
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
/// follower of a deity into a restricted room once the deity is standing in
/// it (act.movement.cpp: `ch->master->in_room == dest`; the master moves
/// first). `leader_arriving` says the leader has already been admitted and
/// is moving into `dest` in the same step (group walking), which counts the
/// same as the leader already being there.
pub(crate) fn entry_allowed_following(
    world: &mut World,
    mover: Entity,
    dest: Entity,
    leader: Option<Entity>,
    leader_arriving: bool,
) -> bool {
    if world.get::<EntryRestriction>(dest).is_none() {
        return true;
    }
    if let Some(leader) = leader
        && is_immortal(world, leader)
        && (leader_arriving || world.get::<Located>(leader).is_some_and(|l| l.0 == dest))
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
/// enter (including deep water without a boat, wings or waterwalk). Used by flee/retreat, which pick a random exit and must never
/// pick one into a room that refuses the fleer.
pub(crate) fn retain_admitted<T>(
    world: &mut World,
    movers: &[Entity],
    candidates: &mut Vec<(T, Entity)>,
) {
    candidates.retain(|(_, room)| {
        movers.iter().all(|m| {
            room_visible_to(world, *m, *room)
                && entry_allowed(world, *m, *room)
                && world
                    .get::<Located>(*m)
                    .is_none_or(|l| !deep_water_blocks(world, *m, l.0, *room))
        })
    });
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
    /// `random(1,100) <= base + skill * per_skill`. `None` (only built by
    /// hand in tests) always succeeds.
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
    // Missing data falls back to the legacy spell, never to something more
    // generous: zone-limited, with the skill-based success roll.
    let range = match get("range")
        .and_then(serde_json::Value::as_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("world") => RandomRange::World,
        _ => RandomRange::Zone,
    };
    let success = Some((
        int("success_base_pct").unwrap_or(LEGACY_SUCCESS_BASE_PCT),
        int("success_per_skill_pct").unwrap_or(LEGACY_SUCCESS_PER_SKILL_PCT),
    ));
    RandomTeleportParams { range, success }
}

/// Legacy `perform_teleport_spell`: succeed when `random(1,100) <= 10 +
/// skill*2`.
const LEGACY_SUCCESS_BASE_PCT: i32 = 10;
const LEGACY_SUCCESS_PER_SKILL_PCT: i32 = 2;

/// Skill used for the success roll. A character who knows the spell uses
/// their proficiency (0..=100); anything else casting it (a scroll or wand
/// user, a mob, a Lua `spells.cast`) uses the caster's level, as legacy
/// passes the caster/item level as the skill for those.
pub(crate) fn effective_teleport_skill(known_skill: Option<i32>, caster_level: i32) -> i32 {
    known_skill.unwrap_or_else(|| caster_level.clamp(0, 100))
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

#[cfg(test)]
mod tests {
    use super::has_return_statement;

    #[test]
    fn return_is_a_keyword_not_a_substring() {
        assert!(has_return_statement("return actor:is_god()"));
        assert!(has_return_statement("  return(true)"));
        assert!(has_return_statement("if a then return true end"));
        assert!(!has_return_statement(r#"actor:has_effect("returning")"#));
        assert!(!has_return_statement(r"actor:has_effect('return')"));
        assert!(!has_return_statement("actor.returning_home"));
        assert!(!has_return_statement("actor.level > 5 -- return later"));
        assert!(!has_return_statement("actor.name == [[return]]"));
        assert!(!has_return_statement(r#"actor.name == "a\"return""#));
    }
}
