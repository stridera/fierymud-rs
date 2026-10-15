//! Lua API the converted trigger corpus calls but the core bindings in
//! `lib.rs` did not provide: `zone.echo`, `timestamp`, `trigger_log`,
//! `find_player`, `get_obj_noadesc`, the `actor:get_*` world-query methods,
//! `actor:set_skill`, and the `.group` / `.fighting` actor fields; plus
//! `wait_hours`, `wizard_notify`, `actor:cast`, `actor:has_effect_named`,
//! the no-argument `get_people()` list, and the `.composition`,
//! `.wearing` and `.val0`..`.val3` fields.
//!
//! Kept in its own module so `lib.rs` only needs three hook-up lines: the
//! global binding call in `bind_globals`, `add_actor_methods` /
//! `add_room_methods` in the `UserData` impls, and `actor_field` as the
//! fall-through of the actor `__index`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use bevy_ecs::prelude::*;
use mlua::{Lua, MultiValue, Table, UserDataMethods, Value, Variadic};
use mud_db::enums::{DamageType, ObjectType, UserRole};
use mud_world::components::{
    Account, Charges, EquippedSlot, Fighting, Item, KnownAbilities, LightFuel, LiquidContainer,
    Located, Mob, MobBehaviors, MobTraits, Named, ObjectFlags, ObjectRestrictions, Online, Player,
    PlayerFlags, Room, WorldKey,
};
use mud_world::liquid_legacy::LiquidLegacyIndex;
use mud_world::resources::{
    AbilityCatalog, LuaOutbox, MudClock, ObjectPrototypes, TriggerCatalog, WorldKeyIndex,
};

use super::id_arg::{IdArg, OptIdArg};
use super::{LuaActor, LuaRoom, SelfEntity, format_args, world_from_lua, world_mut_from_lua};

/// Hours in a MUD day, days in a MUD month, months in a MUD year. The
/// calendar is fixed (see `MudClock`), so `timestamp` can be a plain
/// count of game hours.
const HOURS_PER_DAY: i64 = 24;
const DAYS_PER_MONTH: i64 = 30;
const MONTHS_PER_YEAR: i64 = 16;

/// Highest proficiency `actor:set_skill` will write (the same ceiling the
/// staff `skillset` command documents).
const MAX_SET_SKILL: i32 = 1000;

/// Real seconds in one game hour (legacy `SECS_PER_MUD_HOUR`). The DG
/// `wait N t` unit is "N game hours", and `wait_hours(N)` sleeps that long.
const SECS_PER_MUD_HOUR: i64 = 75;

/// Container flag bits (container `val1`).
const CONTAINER_FLAGS: [&str; 4] = ["Closeable", "PickProof", "Closed", "Locked"];

/// Fingerprint of the body about to run, stashed by the host right before
/// a fire so `trigger_log` (and the `wizard_notify` rate limit) can tell the
/// trigger without the dispatcher having to pass its id. Only set for bodies
/// that mention `trigger_log` or `wizard_notify`.
#[derive(Clone, Copy)]
pub(crate) struct TriggerFingerprint(pub u64);

/// Cache of fingerprint -> `"zone:id"` labels already resolved.
#[derive(Default)]
struct TriggerLabels(HashMap<u64, String>);

/// Hash a trigger body. Deterministic within a process, which is all the
/// fingerprint needs.
pub(crate) fn code_fingerprint(code: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    code.hash(&mut h);
    h.finish()
}

/// Called by the host before each fresh fire: remembers the body's
/// fingerprint when the body can reach `trigger_log`, clears it otherwise.
pub(crate) fn note_fire(lua: &Lua, code: &str) {
    lua.remove_app_data::<TriggerFingerprint>();
    if code.contains("trigger_log") || code.contains("wizard_notify") {
        lua.set_app_data(TriggerFingerprint(code_fingerprint(code)));
    }
}

// ---------------------------------------------------------------------------
// Globals
// ---------------------------------------------------------------------------

/// Bind the extra global functions into a trigger environment.
pub(crate) fn bind_globals(lua: &Lua, globals: &Table) -> mlua::Result<()> {
    // zone.echo(zone_id, msg)
    let zone_tbl = lua.create_table()?;
    zone_tbl.set(
        "echo",
        lua.create_function(|lua, (zone, msg): (i32, String)| zone_echo(lua, zone, &msg))?,
    )?;
    globals.set("zone", zone_tbl)?;

    // timestamp(): legacy `%time.stamp%`, whole game hours.
    globals.set(
        "timestamp",
        lua.create_function(|lua, ()| -> mlua::Result<i64> {
            world_from_lua(lua, |w| w.get_resource::<MudClock>().map_or(0, game_hours))
        })?,
    )?;

    // wait_hours(n): legacy `wait N t`, sleep N game hours. Pure Lua so the
    // yield works inside the coroutine the way `wait` does.
    globals.set(
        "wait_hours",
        lua.load(format!(
            "local y = coroutine.yield; return function(n) y((n or 1) * {SECS_PER_MUD_HOUR}) end"
        ))
        .eval::<mlua::Function>()?,
    )?;

    // The fingerprint belongs to the trigger, so it lives in the trigger's
    // private environment and survives `wait`. `trigger_log` names the
    // trigger by it and `wizard_notify` rate-limits by it.
    let fp = if let Value::Integer(i) = globals.raw_get::<Value>("__trigger_fp")? {
        i.cast_unsigned()
    } else {
        let fp = lua
            .remove_app_data::<TriggerFingerprint>()
            .map_or(0, |f| f.0);
        globals.raw_set("__trigger_fp", fp.cast_signed())?;
        fp
    };
    // wizard_notify(msg): legacy `wizn`, a line on the staff channel. A
    // trigger may send one a minute per entity it runs for: a looping or
    // spammy script must not flood the staff, but one script body shared by
    // many mobs must not let one mob silence the rest.
    globals.set(
        "wizard_notify",
        lua.create_function(move |lua, args: Variadic<Value>| -> mlua::Result<()> {
            let runner = lua.app_data_ref::<SelfEntity>().map(|s| s.0);
            if !wizard_notify_allowed(lua, fp, runner, std::time::Instant::now()) {
                tracing::debug!(
                    trigger_fp = fp,
                    entity = ?runner,
                    "wizard_notify dropped: rate limited to one line a minute"
                );
                return Ok(());
            }
            wizard_notify(lua, &format_args(&args))
        })?,
    )?;

    // find_player(name)
    globals.set(
        "find_player",
        lua.create_function(|lua, name: String| find_player(lua, &name))?,
    )?;

    // get_obj_noadesc(zone, id)
    globals.set(
        "get_obj_noadesc",
        lua.create_function(|lua, IdArg { zone, id }: IdArg| -> mlua::Result<String> {
            world_from_lua(lua, |w| {
                match w
                    .get_resource::<ObjectPrototypes>()
                    .and_then(|p| p.by_key.get(&(zone, id)))
                {
                    Some(proto) => without_article(&proto.name).to_string(),
                    None => format!("[no description for object {zone}:{id}]"),
                }
            })
        })?,
    )?;

    globals.set(
        "trigger_log",
        lua.create_function(move |lua, args: Variadic<Value>| -> mlua::Result<()> {
            let label = trigger_label(lua, fp)?;
            tracing::info!("{}", trigger_log_line(&label, &format_args(&args)));
            Ok(())
        })?,
    )?;
    Ok(())
}

/// The line `trigger_log` writes, prefixed with the trigger id.
fn trigger_log_line(label: &str, msg: &str) -> String {
    format!("trigger {label}: {msg}")
}

/// `"zone:id"` of the trigger whose body has fingerprint `fp`, or
/// `"?"` when unknown (console fires, bodies not in the catalog).
fn trigger_label(lua: &Lua, fp: u64) -> mlua::Result<String> {
    if fp == 0 {
        return Ok("?".to_string());
    }
    if let Some(cache) = lua.app_data_ref::<TriggerLabels>()
        && let Some(label) = cache.0.get(&fp)
    {
        return Ok(label.clone());
    }
    let label = world_from_lua(lua, |w| {
        w.get_resource::<TriggerCatalog>().and_then(|c| {
            c.by_key
                .iter()
                .find(|(_, def)| code_fingerprint(&def.commands) == fp)
                .map(|((zone, id), _)| format!("{zone}:{id}"))
        })
    })?
    .unwrap_or_else(|| "?".to_string());
    if lua.app_data_ref::<TriggerLabels>().is_none() {
        lua.set_app_data(TriggerLabels::default());
    }
    if let Some(mut cache) = lua.app_data_mut::<TriggerLabels>() {
        cache.0.insert(fp, label.clone());
    }
    Ok(label)
}

/// Legacy `time.stamp`: the MUD calendar flattened to game hours (legacy
/// divided its seconds-based sum by `SECS_PER_MUD_HOUR`).
fn game_hours(clock: &MudClock) -> i64 {
    let months = i64::from(clock.year) * MONTHS_PER_YEAR + i64::from(clock.month) - 1;
    let days = months * DAYS_PER_MONTH + i64::from(clock.day) - 1;
    days * HOURS_PER_DAY + i64::from(clock.hour)
}

/// Legacy `without_article` (without the `a pair` entry, which turns
/// "a pair of boots" into "of boots").
fn without_article(s: &str) -> &str {
    for article in ["a ", "an ", "some ", "the "] {
        if let Some(head) = s.get(..article.len())
            && head.eq_ignore_ascii_case(article)
        {
            return &s[article.len()..];
        }
    }
    s
}

/// `zone.echo(zone, msg)`: queue `msg` for every room of `zone` that
/// holds a player. The outbox drain fans it out to the connections.
fn zone_echo(lua: &Lua, zone: i32, msg: &str) -> mlua::Result<()> {
    if msg.is_empty() {
        return Ok(());
    }
    world_mut_from_lua(lua, |world| {
        let mut occupied: Vec<Entity> = {
            let mut q = world.query_filtered::<&Located, With<Player>>();
            q.iter(world).map(|l| l.0).collect()
        };
        occupied.sort_unstable();
        occupied.dedup();
        let mut rooms: Vec<Entity> = world
            .get_resource::<WorldKeyIndex>()
            .map(|idx| {
                idx.rooms
                    .iter()
                    .filter(|((z, _), e)| *z == zone && occupied.binary_search(e).is_ok())
                    .map(|(_, e)| *e)
                    .collect()
            })
            .unwrap_or_default();
        if rooms.is_empty() {
            return;
        }
        rooms.sort_unstable();
        if !world.contains_resource::<LuaOutbox>() {
            world.insert_resource(LuaOutbox::default());
        }
        let mut out = world.resource_mut::<LuaOutbox>();
        for room in rooms {
            out.push_room(room, msg, None);
        }
    })
}

/// Minimum gap between two `wizard_notify` lines from one trigger.
const WIZARD_NOTIFY_GAP: std::time::Duration = std::time::Duration::from_secs(60);

/// When each (trigger body fingerprint, entity it runs for) pair last got a
/// `wizard_notify` line through. The fingerprint is 0 when unknown and the
/// entity `None` outside a fire.
#[derive(Default)]
struct WizardNotifyLog(HashMap<(u64, Option<Entity>), std::time::Instant>);

/// Entries kept before the log drops the ones whose minute is over.
const WIZARD_NOTIFY_PRUNE_AT: usize = 512;

/// Whether the trigger with fingerprint `fp` running for `runner` may send a
/// staff line at `now`; records the send when it may.
fn wizard_notify_allowed(
    lua: &Lua,
    fp: u64,
    runner: Option<Entity>,
    now: std::time::Instant,
) -> bool {
    if lua.app_data_ref::<WizardNotifyLog>().is_none() {
        lua.set_app_data(WizardNotifyLog::default());
    }
    let Some(mut log) = lua.app_data_mut::<WizardNotifyLog>() else {
        return true;
    };
    if let Some(last) = log.0.get(&(fp, runner))
        && now.saturating_duration_since(*last) < WIZARD_NOTIFY_GAP
    {
        return false;
    }
    if log.0.len() >= WIZARD_NOTIFY_PRUNE_AT {
        log.0
            .retain(|_, last| now.saturating_duration_since(*last) < WIZARD_NOTIFY_GAP);
    }
    log.0.insert((fp, runner), now);
    true
}

/// `wizard_notify(msg)`: one `[wiznet]` line to every online player of
/// Immortal rank or above.
fn wizard_notify(lua: &Lua, msg: &str) -> mlua::Result<()> {
    if msg.is_empty() {
        return Ok(());
    }
    world_mut_from_lua(lua, |world| {
        let mut staff: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Account), (With<Player>, With<Online>)>();
            q.iter(world)
                .filter(|(_, a)| a.role.at_least(UserRole::Immortal))
                .map(|(e, _)| e)
                .collect()
        };
        if staff.is_empty() {
            return;
        }
        staff.sort_unstable();
        if !world.contains_resource::<LuaOutbox>() {
            world.insert_resource(LuaOutbox::default());
        }
        let mut out = world.resource_mut::<LuaOutbox>();
        for e in staff {
            out.push_direct(e, format!("<b:cyan>[wiznet]</> {msg}"));
        }
    })
}

/// `find_player(name)`: an online player by (case-insensitive) name,
/// else the first mob whose name or keywords match. The corpus passes
/// mob keywords too (legacy `wteleport <name>` resolved any character),
/// so the mob fallback keeps those calls working; an online player
/// always wins over a same-named mob.
fn find_player(lua: &Lua, name: &str) -> mlua::Result<Value> {
    let needle = name.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Ok(Value::Nil);
    }
    let found = world_mut_from_lua(lua, |world| -> Option<Entity> {
        let player = {
            let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
            q.iter(world)
                .find(|(_, n)| n.name.eq_ignore_ascii_case(&needle))
                .map(|(e, _)| e)
        };
        if player.is_some() {
            return player;
        }
        let mut q = world.query_filtered::<(
            Entity,
            &Named,
            Option<&mud_world::components::Keywords>,
        ), With<Mob>>();
        q.iter(world)
            .find(|(_, n, kw)| {
                mud_world::targeting::entity_matches(&needle, &n.name, kw.map(|k| k.0.as_slice()))
            })
            .map(|(e, _, _)| e)
    })?;
    actor_value(lua, found)
}

fn actor_value(lua: &Lua, entity: Option<Entity>) -> mlua::Result<Value> {
    match entity {
        Some(e) => Ok(Value::UserData(
            lua.create_userdata(LuaActor { entity: e })?,
        )),
        None => Ok(Value::Nil),
    }
}

// ---------------------------------------------------------------------------
// Actor methods
// ---------------------------------------------------------------------------

/// Lower-case alphanumerics only, so `NO_DROP`, `NoDrop` and `no-drop`
/// all compare equal.
fn norm(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Legacy flag spellings whose runtime name differs.
fn flag_alias(n: String) -> String {
    match n.as_str() {
        "illusory" => "illusion".to_string(),
        _ => n,
    }
}

/// The room an entity stands in: itself when it is a room (room
/// triggers bind `self` to the room), else its `Located` container.
fn room_of(world: &World, entity: Entity) -> Option<Entity> {
    if world.get::<Room>(entity).is_some() {
        Some(entity)
    } else {
        world.get::<Located>(entity).map(|l| l.0)
    }
}

/// First entity in `room` with prototype key `(zone, id)`, among items
/// (`items == true`) or non-item characters.
fn find_in_room(
    world: &mut World,
    room: Entity,
    zone: i32,
    id: i32,
    items: bool,
) -> Option<Entity> {
    let mut hits: Vec<Entity> = super::contents_of(world, room)
        .into_iter()
        .filter(|&e| {
            world.get::<Item>(e).is_some() == items
                && world
                    .get::<WorldKey>(e)
                    .is_some_and(|wk| wk.zone == zone && wk.id == id)
        })
        .collect();
    hits.sort_unstable();
    hits.first().copied()
}

fn count_world(world: &mut World, zone: i32, id: i32, items: bool) -> i64 {
    let mut q = world.query::<(&WorldKey, Has<Item>, Has<Mob>)>();
    let n = q
        .iter(world)
        .filter(|(wk, is_item, is_mob)| {
            wk.zone == zone && wk.id == id && if items { *is_item } else { *is_mob }
        })
        .count();
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Whether `entity` carries a flag spelled `flag` (case, `_`, `-` and
/// spaces ignored) in any of its flag lists: item flags and
/// restrictions, mob behaviors and traits, player flags.
fn has_flag(world: &World, entity: Entity, flag: &str) -> bool {
    let want = flag_alias(norm(flag));
    if want.is_empty() {
        return false;
    }
    let hit = |names: Vec<String>| names.iter().any(|n| norm(n) == want);
    if let Some(f) = world.get::<ObjectFlags>(entity)
        && hit(f.0.iter().map(|v| format!("{v:?}")).collect())
    {
        return true;
    }
    if let Some(f) = world.get::<ObjectRestrictions>(entity)
        && hit(f.0.iter().map(|v| format!("{v:?}")).collect())
    {
        return true;
    }
    if let Some(f) = world.get::<MobBehaviors>(entity)
        && hit(f.0.iter().map(|v| format!("{v:?}")).collect())
    {
        return true;
    }
    if let Some(f) = world.get::<MobTraits>(entity)
        && hit(f.0.iter().map(|v| format!("{v:?}")).collect())
    {
        return true;
    }
    if let Some(f) = world.get::<PlayerFlags>(entity)
        && hit(f.0.iter().map(|v| format!("{v:?}")).collect())
    {
        return true;
    }
    false
}

/// Whether `entity` has an active effect named `flag`. Like legacy
/// `search_block(.., exact = false)` a prefix of the effect name counts
/// (`sanct` matches `sanctuary`).
fn has_eff_flag(world: &mut World, entity: Entity, flag: &str) -> bool {
    let want = norm(flag);
    if want.is_empty() {
        return false;
    }
    mud_world::any_effect_on(world, entity, |inst| norm(&inst.name).starts_with(&want))
}

/// `actor:set_skill(name, prof)`. Script use only: it refuses to run when
/// the executing `self` is a player (the staff `lua` console), and is not
/// part of the read-only condition-script whitelist. Players only; the
/// ability must exist; `prof` is clamped to `0..=1000`. Returns whether a
/// skill was set.
fn set_skill(lua: &Lua, target: Entity, name: &str, prof: i32) -> mlua::Result<bool> {
    let runner = lua.app_data_ref::<SelfEntity>().map(|s| s.0);
    let from_player = world_from_lua(lua, |w| {
        runner.is_some_and(|e| w.get::<Player>(e).is_some())
    })?;
    if from_player {
        return Err(mlua::Error::external(
            "set_skill is only available to attached trigger scripts",
        ));
    }
    let prof = prof.clamp(0, MAX_SET_SKILL);
    world_mut_from_lua(lua, |world| {
        if world.get::<Player>(target).is_none() {
            return false;
        }
        let ability_id = world
            .get_resource::<AbilityCatalog>()
            .and_then(|c| c.find_by_prefix(name.trim(), None, world.get::<KnownAbilities>(target)))
            .map(|d| d.id);
        let Some(ability_id) = ability_id else {
            return false;
        };
        if world.get::<KnownAbilities>(target).is_none() {
            let Ok(mut em) = world.get_entity_mut(target) else {
                return false;
            };
            em.insert(KnownAbilities::default());
        }
        let Some(mut known) = world.get_mut::<KnownAbilities>(target) else {
            return false;
        };
        if let Some(entry) = known
            .entries
            .iter_mut()
            .find(|(id, _, _)| *id == ability_id)
        {
            entry.1 = prof;
            entry.2 = prof > 0;
        } else {
            known.entries.push((ability_id, prof, prof > 0));
            known.entries.sort_by_key(|(id, _, _)| *id);
        }
        true
    })
}

/// The room the script's `self` is in, following containers upward (a
/// carried item is `Located` in its carrier, who is `Located` in the room).
fn enclosing_room(world: &World, entity: Entity) -> Option<Entity> {
    let mut cur = entity;
    for _ in 0..8 {
        if world.get::<Room>(cur).is_some() {
            return Some(cur);
        }
        cur = world.get::<Located>(cur)?.0;
    }
    None
}

/// Resolve the string form of `world.destroy(name)` the way legacy
/// `mpurge` / `opurge` do: an object `self` carries, then an object lying
/// in `self`'s room, then a non-player character in the room. The name is
/// matched whole first (`object.name` is the item's full short name), then
/// as a keyword. Players are never candidates.
pub(crate) fn resolve_destroy_name(world: &mut World, from: Entity, name: &str) -> Option<Entity> {
    let needle = name.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return None;
    }
    let room = enclosing_room(world, from);
    // (priority, entity): carried item 0, room item 1, room character 2.
    // Candidates come straight off the `Contents` index of `from` and its
    // room, not a scan of every located entity in the world.
    let mut candidates: Vec<(u8, Entity)> = super::contents_of(world, from)
        .into_iter()
        .filter(|&e| world.get::<Item>(e).is_some())
        .map(|e| (0, e))
        .collect();
    if let Some(room) = room {
        candidates.extend(
            super::contents_of(world, room)
                .into_iter()
                .map(|e| (if world.get::<Item>(e).is_some() { 1 } else { 2 }, e)),
        );
    }
    let mut best: Option<(u8, bool, Entity)> = None;
    for (rank, e) in candidates {
        if e == from || world.get::<Player>(e).is_some() {
            continue;
        }
        let Some(n) = world.get::<Named>(e) else {
            continue;
        };
        let kw = world.get::<mud_world::components::Keywords>(e);
        let whole = n.name.eq_ignore_ascii_case(&needle);
        if !whole
            && !mud_world::targeting::entity_matches(&needle, &n.name, kw.map(|k| k.0.as_slice()))
        {
            continue;
        }
        // Lower rank first, whole-name hits before keyword hits, entity
        // order last so the pick is stable.
        let better = best.is_none_or(|(r, w, b)| (rank, !whole, e) < (r, !w, b));
        if better {
            best = Some((rank, whole, e));
        }
    }
    best.map(|(_, _, e)| e)
}

/// Characters (anything that is not an item) standing in `room`, in entity
/// order so every caller sees the same sequence.
fn people_in(world: &mut World, room: Entity) -> Vec<Entity> {
    let mut out = super::characters_in(world, room);
    out.sort_unstable();
    out
}

/// A 1-indexed Lua list of `LuaActor` wrappers.
fn actor_list(lua: &Lua, entities: &[Entity]) -> mlua::Result<Value> {
    let tbl = lua.create_table()?;
    for (i, e) in entities.iter().enumerate() {
        tbl.set(i + 1, lua.create_userdata(LuaActor { entity: *e })?)?;
    }
    Ok(Value::Table(tbl))
}

/// Legacy vnum of prototype `(zone, id)`: zone 1000 is the old zone 0.
fn legacy_vnum(zone: i32, id: i32) -> i64 {
    i64::from(zone % 1000) * 100 + i64::from(id)
}

/// `actor.wearing`: what the actor has equipped, keyed by the item's legacy
/// vnum (`wearing[18880]`, the DG form) with the item as the value, so
/// `if actor.wearing[id] then` and `pairs(actor.wearing)` both work.
fn wearing_table(lua: &Lua, entity: Entity) -> mlua::Result<Value> {
    let worn: Vec<(i64, Entity)> = world_from_lua(lua, |w| {
        let mut v: Vec<(i64, Entity)> = super::carried_items(w, entity)
            .filter(|&e| w.get::<EquippedSlot>(e).is_some())
            .filter_map(|e| {
                let wk = w.get::<WorldKey>(e)?;
                Some((legacy_vnum(wk.zone, wk.id), e))
            })
            .collect();
        v.sort_unstable();
        v
    })?;
    let tbl = lua.create_table()?;
    for (vnum, item) in worn {
        // The first item wins when two worn items share a prototype.
        if tbl.raw_get::<Value>(vnum)?.is_nil() {
            tbl.raw_set(vnum, lua.create_userdata(LuaActor { entity: item })?)?;
        }
    }
    Ok(Value::Table(tbl))
}

/// A JSON scalar as the integer DG scripts compared: numbers truncate,
/// booleans are 0/1, numeric strings parse, anything else is 0.
fn json_int(v: Option<&serde_json::Value>) -> i64 {
    match v {
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or_else(|| {
            #[allow(clippy::cast_possible_truncation)]
            let t = n.as_f64().unwrap_or(0.0) as i64;
            t
        }),
        Some(serde_json::Value::Bool(b)) => i64::from(*b),
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// The `Objects.values` key each of `val0`..`val3` stands for, by object
/// type: the importer's `obj_val` order, under the game key names of
/// muditor `objectTypeValues.ts`. An empty key reads 0.
fn value_keys(t: ObjectType) -> [&'static str; 4] {
    match t {
        ObjectType::Armor | ObjectType::Treasure => ["AC", "", "", ""],
        ObjectType::Light => ["Is_Lit", "Capacity", "Remaining", ""],
        ObjectType::Container => ["Capacity", "Flags", "Key", "IsCorpse"],
        ObjectType::Drinkcontainer | ObjectType::Fountain => {
            ["Capacity", "Remaining", "Liquid", "Poisoned"]
        }
        ObjectType::Food => ["Filling", "Poisoned", "", ""],
        ObjectType::Scroll | ObjectType::Potion => ["Level", "", "", ""],
        ObjectType::Wand | ObjectType::Staff | ObjectType::Instrument => {
            ["Level", "Max_Charges", "Charges_Left", ""]
        }
        ObjectType::Money => ["Platinum", "Gold", "Silver", "Copper"],
        ObjectType::Weapon => ["HitRoll", "", "", ""],
        _ => ["", "", "", ""],
    }
}

/// Legacy number (`val2` of a drink container) of the liquid `name`, from the
/// `Liquids` table loaded into [`LiquidLegacyIndex`]; the built-in legacy order
/// when the resource is absent. 0 when unknown.
fn liquid_index(world: &World, name: &str) -> i64 {
    match world.get_resource::<LiquidLegacyIndex>() {
        Some(index) => index.index_of(name),
        None => LiquidLegacyIndex::default().index_of(name),
    }
}

/// Legacy `GET_OBJ_VAL(obj, n)` of item `item`, or `None` when `item` is
/// not an item. Type-specific, from the proto's `values` JSON; the state
/// that changes in play (fuel, liquid, wand charges, lit) is read live.
fn object_value(world: &World, item: Entity, n: usize) -> Option<i64> {
    world.get::<Item>(item)?;
    let Some(proto) = world.get::<WorldKey>(item).and_then(|wk| {
        world
            .get_resource::<ObjectPrototypes>()
            .and_then(|p| p.by_key.get(&(wk.zone, wk.id)))
    }) else {
        return Some(0);
    };
    let t = proto.r#type;
    // Typed columns the runtime already parsed out of the row.
    match (t, n) {
        (ObjectType::Weapon, 1) => return Some(i64::from(proto.weapon_dice_num)),
        (ObjectType::Weapon, 2) => return Some(i64::from(proto.weapon_dice_size)),
        (ObjectType::Weapon, 3) => {
            return Some(proto.weapon_damage_type.as_deref().map_or(0, |d| {
                i64::try_from(DamageType::legacy_index_of_label(d)).unwrap_or(0)
            }));
        }
        _ => {}
    }
    // Live per-instance state.
    match (t, n) {
        (ObjectType::Light, 0) => return Some(i64::from(mud_world::is_lit(world, item))),
        (ObjectType::Light, 1 | 2) => {
            if let Some(f) = world.get::<LightFuel>(item) {
                return Some(i64::from(if n == 1 { f.capacity } else { f.remaining }));
            }
        }
        (ObjectType::Drinkcontainer | ObjectType::Fountain, _) => {
            if let Some(l) = world.get::<LiquidContainer>(item) {
                return Some(match n {
                    0 => i64::from(l.capacity),
                    1 => i64::from(l.remaining),
                    2 => liquid_index(world, &l.liquid),
                    _ => i64::from(l.poisoned),
                });
            }
        }
        (ObjectType::Wand | ObjectType::Staff | ObjectType::Instrument, 2) => {
            if let Some(c) = world.get::<Charges>(item) {
                return Some(i64::from(c.0));
            }
        }
        _ => {}
    }
    let key = value_keys(t)[n];
    if key.is_empty() {
        return Some(0);
    }
    let v = proto.values.get(key);
    Some(match (key, v) {
        ("Liquid", Some(serde_json::Value::String(s))) => liquid_index(world, s),
        ("Flags", Some(serde_json::Value::Array(flags))) => flags
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter_map(|f| {
                CONTAINER_FLAGS
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case(f.trim()))
            })
            .fold(0, |acc, i| acc | (1 << i)),
        _ => json_int(v),
    })
}

/// Methods added to `LuaActor`.
pub(crate) fn add_actor_methods<M: UserDataMethods<LuaActor>>(methods: &mut M) {
    // `self:get_people(zone, id)`: a character with that prototype in
    // the entity's room (the room itself for room triggers), or nil.
    // `self:get_people()` with no arguments: every character in that room
    // (carried items follow their carrier up to the room), as a list in a
    // stable order, safe to iterate while the loop body moves or kills
    // them. Replaces the DG `self.people` / `person.next_in_room` walk.
    methods.add_method(
        "get_people",
        |lua, this, OptIdArg(key): OptIdArg| -> mlua::Result<Value> {
            if let Some(IdArg { zone, id }) = key {
                let found = world_mut_from_lua(lua, |w| {
                    let room = room_of(w, this.entity)?;
                    find_in_room(w, room, zone, id, false)
                })?;
                actor_value(lua, found)
            } else {
                let people = world_mut_from_lua(lua, |w| {
                    enclosing_room(w, this.entity)
                        .map(|r| people_in(w, r))
                        .unwrap_or_default()
                })?;
                actor_list(lua, &people)
            }
        },
    );
    // `self:get_objects(zone, id)`: an item with that prototype in the
    // entity's room, or nil.
    methods.add_method(
        "get_objects",
        |lua, this, IdArg { zone, id }: IdArg| -> mlua::Result<Value> {
            let found = world_mut_from_lua(lua, |w| {
                let room = room_of(w, this.entity)?;
                find_in_room(w, room, zone, id, true)
            })?;
            actor_value(lua, found)
        },
    );
    // `self:get_mexists(zone, id)` / `get_oexists`: how many mobs /
    // items with that prototype exist in the whole world.
    methods.add_method(
        "get_mexists",
        |lua, _this, IdArg { zone, id }: IdArg| -> mlua::Result<i64> {
            world_mut_from_lua(lua, |w| count_world(w, zone, id, false))
        },
    );
    methods.add_method(
        "get_oexists",
        |lua, _this, IdArg { zone, id }: IdArg| -> mlua::Result<i64> {
            world_mut_from_lua(lua, |w| count_world(w, zone, id, true))
        },
    );
    methods.add_method(
        "get_flagged",
        |lua, this, flag: String| -> mlua::Result<bool> {
            world_from_lua(lua, |w| has_flag(w, this.entity, &flag))
        },
    );
    methods.add_method(
        "get_eff_flagged",
        |lua, this, flag: String| -> mlua::Result<bool> {
            world_mut_from_lua(lua, |w| has_eff_flag(w, this.entity, &flag))
        },
    );
    methods.add_method(
        "set_skill",
        |lua, this, (name, prof): (String, i32)| -> mlua::Result<bool> {
            set_skill(lua, this.entity, &name, prof)
        },
    );
    // `self:cast(spell, target?)`: the method form of `spells.cast(self,
    // spell, target)`.
    methods.add_method(
        "cast",
        |lua, this, (spell, target): (String, Value)| -> mlua::Result<()> {
            let caster = Value::UserData(lua.create_userdata(LuaActor {
                entity: this.entity,
            })?);
            let name = Value::String(lua.create_string(&spell)?);
            super::spells_cast_dispatch(lua, MultiValue::from_vec(vec![caster, name, target]))
        },
    );
    // `actor:has_effect_named(name)`: whether an active effect is called
    // `name` (case, spaces and punctuation ignored; a prefix counts, as for
    // `get_eff_flagged`).
    methods.add_method(
        "has_effect_named",
        |lua, this, name: String| -> mlua::Result<bool> {
            world_mut_from_lua(lua, |w| has_eff_flag(w, this.entity, &name))
        },
    );
}

/// Methods added to `LuaRoom`.
pub(crate) fn add_room_methods<M: UserDataMethods<LuaRoom>>(methods: &mut M) {
    // `room:get_people(zone, id)` / `room:get_people()`: see the actor form.
    methods.add_method(
        "get_people",
        |lua, this, OptIdArg(key): OptIdArg| -> mlua::Result<Value> {
            if let Some(IdArg { zone, id }) = key {
                let found =
                    world_mut_from_lua(lua, |w| find_in_room(w, this.entity, zone, id, false))?;
                actor_value(lua, found)
            } else {
                let people = world_mut_from_lua(lua, |w| people_in(w, this.entity))?;
                actor_list(lua, &people)
            }
        },
    );
    methods.add_method(
        "get_objects",
        |lua, this, IdArg { zone, id }: IdArg| -> mlua::Result<Value> {
            let found = world_mut_from_lua(lua, |w| find_in_room(w, this.entity, zone, id, true))?;
            actor_value(lua, found)
        },
    );
}

/// Fall-through of the actor `__index`: the fields that need more than a
/// component read. Unknown keys stay nil.
pub(crate) fn actor_field(lua: &Lua, entity: Entity, key: &str) -> mlua::Result<Value> {
    match key {
        // `actor.group`: the real group (leader first), `{actor}` when
        // solo, so `for _, p in ipairs(actor.group)` always visits the
        // actor at least.
        "group" => {
            let members = world_mut_from_lua(lua, |w| {
                let root = mud_world::group_root(w, entity);
                mud_world::group_members(w, root)
            })?;
            let tbl = lua.create_table()?;
            for (i, e) in members.into_iter().enumerate() {
                tbl.set(i + 1, lua.create_userdata(LuaActor { entity: e })?)?;
            }
            Ok(Value::Table(tbl))
        }
        // `self.fighting`: the current opponent, nil out of combat.
        "fighting" => {
            let target = world_from_lua(lua, |w| {
                w.get::<Fighting>(entity)
                    .map(|f| f.0)
                    .filter(|t| w.get_entity(*t).is_ok())
            })?;
            actor_value(lua, target)
        }
        // `actor.composition`: the body composition in effect right now,
        // lower case (`"flesh"`, `"air"`, `"bone"`): a live Waterform /
        // Vaporform override, else the mob's own or its race's.
        "composition" => {
            let c = world_from_lua(lua, |w| mud_world::actor_composition(w, entity))?;
            Ok(Value::String(
                lua.create_string(format!("{c:?}").to_ascii_lowercase())?,
            ))
        }
        // `actor.wearing[vnum]`: see `wearing_table`.
        "wearing" => wearing_table(lua, entity),
        // `object.val0`..`object.val3`: see `object_value`.
        "val0" | "val1" | "val2" | "val3" => {
            let n = usize::from(key.as_bytes()[3] - b'0');
            match world_from_lua(lua, |w| object_value(w, entity, n))? {
                Some(v) => Ok(Value::Integer(v)),
                None => Ok(Value::Nil),
            }
        }
        _ => Ok(Value::Nil),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LuaHost;
    use mud_world::components::{AppliedTo, EffectInstance, EffectSource, GroupMember, Health};
    use mud_world::liquid_legacy::LegacyLiquidRow;
    use mud_world::resources::{AbilityDef, ObjectProto, OutLine, TriggerAttach, TriggerDef};
    use std::sync::{Arc, Mutex};

    /// Rooms (5,1) and (5,2) in zone 5, (6,1) in zone 6, a player in
    /// (5,1) and a mob (9,1) running scripts there.
    struct Fixture {
        world: World,
        host: LuaHost,
        room: Entity,
        other_room: Entity,
        far_room: Entity,
        player: Entity,
        mob: Entity,
    }

    fn fixture() -> Fixture {
        let mut world = World::new();
        let mut index = WorldKeyIndex::default();
        let mut room = |zone: i32, id: i32| {
            let e = world
                .spawn((
                    Room,
                    Named {
                        name: format!("room {zone}:{id}"),
                    },
                    WorldKey { zone, id },
                ))
                .id();
            index.rooms.insert((zone, id), e);
            e
        };
        let (r1, r2, r3) = (room(5, 1), room(5, 2), room(6, 1));
        world.insert_resource(index);
        let player = world
            .spawn((
                Player,
                Online,
                Named {
                    name: "Hero".into(),
                },
                Health { hp: 10, max: 10 },
                Located(r1),
            ))
            .id();
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "a script keeper".into(),
                },
                WorldKey { zone: 9, id: 1 },
                Located(r1),
            ))
            .id();
        Fixture {
            world,
            host: LuaHost::new(),
            room: r1,
            other_room: r2,
            far_room: r3,
            player,
            mob,
        }
    }

    impl Fixture {
        /// Run `body` with `self` = the mob and `actor` = the player.
        fn run(&mut self, body: &str) -> Result<String, String> {
            self.host.exec_for_listener_with_extras(
                &mut self.world,
                self.mob,
                self.player,
                body,
                &[],
            )
        }

        fn ok(&mut self, body: &str) -> String {
            self.run(body)
                .unwrap_or_else(|e| panic!("`{body}` failed: {e}"))
        }
    }

    fn proto(zone: i32, id: i32, name: &str) -> ObjectProto {
        ObjectProto {
            zone_id: zone,
            id,
            r#type: mud_db::enums::ObjectType::Other,
            name: name.to_string(),
            keywords: vec![],
            room_description: String::new(),
            examine_description: None,
            weight: 0.0,
            weight_reduction: 0.0,
            recall_rooms: None,
            level: 1,
            wear_flags: vec![],
            weapon_dice_num: 0,
            weapon_dice_size: 0,
            weapon_dice_bonus: 0,
            weapon_damage_type: None,
            weapon_element: None,
            cost: 0,
            portal_destination_vnum: None,
            board_id: None,
            liquid: None,
            light_fuel: None,
            armor_pct: 0,
            restricted_alignments: vec![],
            restricted_class_ids: vec![],
            restricted_races: vec![],
            extras: vec![],
            resistances: vec![],
            granted_effects: vec![],
            flags: vec![],
            restrictions: vec![],
            timer_hours: 0,
            decompose_timer: 0,
            allowed_races: vec![],
            min_size: None,
            max_size: None,
            camp_kit_tier: None,
            concealment: 0,
            food_poisoned: false,
            values: serde_json::Value::Null,
        }
    }

    fn ability(id: i32, name: &str) -> AbilityDef {
        AbilityDef {
            id,
            name: name.to_string(),
            plain_name: name.to_string(),
            description: None,
            kind: mud_db::abilities::AbilityKind::Spell,
            violent: false,
            combat_ok: true,
            in_combat_only: false,
            cast_time_rounds: 0,
            cooldown_ms: 0,
            is_area: false,
            min_position_label: "STANDING".into(),
            min_posture_rank: 9,
            target_scope: "SINGLE".into(),
            is_magical: true,
            sphere: None,
            damage_type: None,
            memorization_time: 0,
            passive: false,
            short_cast: false,
            requires_rigid: false,
            tags: Vec::new(),
            quest_only: false,
        }
    }

    fn effect(world: &mut World, on: Entity, name: &str) {
        world.spawn((
            EffectInstance {
                kind: 1,
                name: name.to_string(),
                strength: 1,
                remaining_secs: -1,
                source: EffectSource::Spell,
                ability_id: None,
            },
            AppliedTo(on),
        ));
    }

    // ----- zone.echo -----

    #[test]
    fn zone_echo_queues_only_occupied_rooms_of_that_zone() {
        let mut f = fixture();
        // A second player in a zone-6 room must not hear a zone-5 echo,
        // and an empty zone-5 room is skipped.
        f.world
            .spawn((Player, Named { name: "Far".into() }, Located(f.far_room)));
        f.ok("zone.echo(5, 'The ground shakes.')");
        let out = f.world.resource::<LuaOutbox>();
        assert_eq!(
            out.lines,
            vec![OutLine::Room {
                room: f.room,
                msg: "The ground shakes.".to_string(),
                except: None
            }]
        );
        // An empty message queues nothing.
        f.world.resource_mut::<LuaOutbox>().lines.clear();
        f.ok("zone.echo(5, '')");
        assert!(f.world.resource::<LuaOutbox>().lines.is_empty());
        let _ = f.other_room;
    }

    // ----- timestamp -----

    #[test]
    fn timestamp_counts_game_hours_and_follows_the_clock() {
        let mut f = fixture();
        f.world.insert_resource(MudClock {
            year: 2,
            month: 3,
            day: 4,
            hour: 5,
            minute: 30,
            stamp: 999_999,
        });
        // ((2*16 + 2) * 30 + 3) * 24 + 5
        let expected = ((2 * 16 + 2) * 30 + 3) * 24 + 5;
        assert_eq!(f.ok("print(timestamp())"), format!("{expected}\r\n"));
        f.world.resource_mut::<MudClock>().hour = 8;
        assert_eq!(f.ok("print(timestamp())"), format!("{}\r\n", expected + 3));
    }

    // ----- trigger_log -----

    /// Minimal subscriber that records every event's `message`.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<String>>>);

    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct V<'a>(&'a mut String);
            impl tracing::field::Visit for V<'_> {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    if f.name() == "message" {
                        *self.0 = format!("{v:?}");
                    }
                }
            }
            let mut msg = String::new();
            event.record(&mut V(&mut msg));
            self.0.lock().unwrap().push(msg);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn trigger_log_line_is_prefixed_with_the_trigger_id() {
        assert_eq!(trigger_log_line("49:10", "boom"), "trigger 49:10: boom");
        let mut f = fixture();
        let body = "trigger_log('TD Error:', 7)";
        let mut catalog = TriggerCatalog::default();
        catalog.by_key.insert(
            (49, 10),
            TriggerDef {
                zone_id: 49,
                id: 10,
                name: "td".into(),
                attach_type: TriggerAttach::Mob,
                commands: body.to_string(),
                flags: vec![],
                arg_list: vec![],
                num_args: 0,
            },
        );
        f.world.insert_resource(catalog);
        let cap = Capture::default();
        tracing::subscriber::with_default(cap.clone(), || {
            f.ok(body);
            // A body the catalog does not know logs with an unknown id.
            f.ok("trigger_log('stray')");
        });
        let lines = cap.0.lock().unwrap().clone();
        assert!(
            lines.contains(&"trigger 49:10: TD Error:\t7".to_string()),
            "{lines:?}"
        );
        assert!(lines.contains(&"trigger ?: stray".to_string()), "{lines:?}");
    }

    // ----- find_player -----

    #[test]
    fn find_player_prefers_online_players_and_falls_back_to_mobs() {
        let mut f = fixture();
        // An offline player and a mob sharing a name with nobody online.
        f.world.spawn((
            Player,
            Named {
                name: "Ghost".into(),
            },
        ));
        f.world.spawn((
            Mob,
            Named {
                name: "the Leading Player".into(),
            },
            mud_world::components::Keywords(vec!["leading-player".into()]),
            Located(f.other_room),
        ));
        assert_eq!(f.ok("print(find_player('hero').name)"), "Hero\r\n");
        assert_eq!(f.ok("print(find_player('HERO').name)"), "Hero\r\n");
        assert_eq!(
            f.ok("print(find_player('leading-player').name)"),
            "the Leading Player\r\n"
        );
        assert_eq!(f.ok("print(find_player('ghost'))"), "nil\r\n");
        assert_eq!(f.ok("print(find_player(''))"), "nil\r\n");
        // A same-named mob never shadows the online player.
        f.world.spawn((
            Mob,
            Named {
                name: "Hero".into(),
            },
            Located(f.other_room),
        ));
        assert_eq!(f.ok("print(find_player('hero').is_player)"), "true\r\n");
    }

    // ----- get_obj_noadesc -----

    #[test]
    fn get_obj_noadesc_strips_the_article() {
        let mut f = fixture();
        let mut protos = ObjectPrototypes::default();
        protos
            .by_key
            .insert((23, 39), proto(23, 39, "a gleaming trident"));
        protos
            .by_key
            .insert((23, 34), proto(23, 34, "The Hell Trident"));
        protos.by_key.insert((23, 35), proto(23, 35, "Anvil"));
        f.world.insert_resource(protos);
        assert_eq!(
            f.ok("print(get_obj_noadesc(23, 39))"),
            "gleaming trident\r\n"
        );
        assert_eq!(f.ok("print(get_obj_noadesc(23, 34))"), "Hell Trident\r\n");
        assert_eq!(f.ok("print(get_obj_noadesc(23, 35))"), "Anvil\r\n");
        assert_eq!(
            f.ok("print(get_obj_noadesc(23, 99))"),
            "[no description for object 23:99]\r\n"
        );
    }

    // ----- get_eff_flagged -----

    #[test]
    fn get_eff_flagged_matches_effect_names_and_prefixes() {
        let mut f = fixture();
        effect(&mut f.world, f.player, "sanctuary");
        effect(&mut f.world, f.player, "fly");
        effect(&mut f.world, f.mob, "silence");
        assert_eq!(f.ok("print(actor:get_eff_flagged('sanct'))"), "true\r\n");
        assert_eq!(f.ok("print(actor:get_eff_flagged('FLY'))"), "true\r\n");
        assert_eq!(f.ok("print(actor:get_eff_flagged('silence'))"), "false\r\n");
        assert_eq!(f.ok("print(self:get_eff_flagged('silence'))"), "true\r\n");
        assert_eq!(f.ok("print(actor:get_eff_flagged(''))"), "false\r\n");
    }

    // ----- get_people / get_objects -----

    #[test]
    fn get_people_and_get_objects_look_in_the_entitys_room() {
        let mut f = fixture();
        let item = f
            .world
            .spawn((
                Item,
                Named {
                    name: "lash".into(),
                },
                WorldKey { zone: 43, id: 51 },
                Located(f.room),
            ))
            .id();
        f.world.spawn((
            Item,
            Named {
                name: "lash".into(),
            },
            WorldKey { zone: 43, id: 11 },
            Located(f.other_room),
        ));
        f.world.spawn((
            Mob,
            Named {
                name: "a guard".into(),
            },
            WorldKey { zone: 30, id: 10 },
            Located(f.room),
        ));
        // On a mob: its own room.
        assert_eq!(f.ok("print(self:get_people(30, 10).name)"), "a guard\r\n");
        assert_eq!(f.ok("print(self:get_people(30, 11))"), "nil\r\n");
        assert_eq!(f.ok("print(self:get_objects(43, 51).name)"), "lash\r\n");
        assert_eq!(f.ok("print(self:get_objects(43, 11))"), "nil\r\n");
        // Items are not people, mobs are not objects.
        assert_eq!(f.ok("print(self:get_people(43, 51))"), "nil\r\n");
        assert_eq!(f.ok("print(self:get_objects(30, 10))"), "nil\r\n");
        // On a room: the room itself.
        let body =
            "local r = get_room(5, 2); print(r:get_objects(43, 11).name, r:get_people(30, 10))";
        assert_eq!(f.ok(body), "lash\tnil\r\n");
        // On a room trigger's `self` (the room entity as an actor).
        let listener = f.room;
        let out = f
            .host
            .exec_for_listener_with_extras(
                &mut f.world,
                listener,
                f.player,
                "print(self:get_objects(43, 51).name)",
                &[],
            )
            .unwrap();
        assert_eq!(out, "lash\r\n");
        let _ = item;
    }

    // ----- get_mexists / get_oexists -----

    #[test]
    fn get_mexists_and_get_oexists_count_the_whole_world() {
        let mut f = fixture();
        for room in [f.room, f.other_room, f.far_room] {
            f.world.spawn((
                Mob,
                Named {
                    name: "a druid".into(),
                },
                WorldKey { zone: 30, id: 55 },
                Located(room),
            ));
        }
        f.world.spawn((
            Item,
            Named {
                name: "girth".into(),
            },
            WorldKey { zone: 11, id: 27 },
        ));
        assert_eq!(f.ok("print(self:get_mexists(30, 55))"), "3\r\n");
        assert_eq!(f.ok("print(self:get_mexists(30, 56))"), "0\r\n");
        assert_eq!(f.ok("print(self:get_oexists(11, 27))"), "1\r\n");
        // Mobs are not counted as objects and vice versa.
        assert_eq!(f.ok("print(self:get_oexists(30, 55))"), "0\r\n");
    }

    // ----- get_flagged -----

    #[test]
    fn get_flagged_reads_item_mob_and_player_flags() {
        let mut f = fixture();
        let cursed = f
            .world
            .spawn((
                Item,
                Named {
                    name: "cursed ring".into(),
                },
                ObjectRestrictions(vec![mud_db::enums::ObjectRestriction::NoDrop]),
            ))
            .id();
        let image = f
            .world
            .spawn((
                Mob,
                Named {
                    name: "an image".into(),
                },
                MobTraits(vec![mud_db::enums::MobTrait::Illusion]),
            ))
            .id();
        let run = |f: &mut Fixture, who: Entity, flag: &str| -> String {
            f.host
                .exec_for_listener_with_extras(
                    &mut f.world,
                    who,
                    f.player,
                    &format!("print(self:get_flagged('{flag}'))"),
                    &[],
                )
                .unwrap()
        };
        assert_eq!(run(&mut f, cursed, "NODROP"), "true\r\n");
        assert_eq!(run(&mut f, cursed, "no_drop"), "true\r\n");
        assert_eq!(run(&mut f, cursed, "NOTAKE"), "false\r\n");
        assert_eq!(run(&mut f, cursed, "not DROP"), "false\r\n");
        assert_eq!(run(&mut f, image, "illusory"), "true\r\n");
        let mob = f.mob;
        assert_eq!(run(&mut f, mob, "illusory"), "false\r\n");
        assert_eq!(run(&mut f, mob, ""), "false\r\n");
    }

    // ----- .group -----

    #[test]
    fn group_lists_real_members_leader_first_and_solo_is_self() {
        let mut f = fixture();
        let friend = f
            .world
            .spawn((
                Player,
                Named {
                    name: "Friend".into(),
                },
                GroupMember(f.player),
            ))
            .id();
        let stalker = f
            .world
            .spawn((
                Player,
                Named {
                    name: "Stalker".into(),
                },
                Located(f.room),
            ))
            .id();
        let body = "local n = {} for _, p in ipairs(actor.group) do n[#n + 1] = p.name end print(#actor.group, table.concat(n, ','))";
        assert_eq!(f.ok(body), "2\tHero,Friend\r\n");
        // Asked from a member, the leader still comes first.
        let out = f
            .host
            .exec_for_listener_with_extras(
                &mut f.world,
                f.mob,
                friend,
                "print(actor.group[1].name, #actor.group)",
                &[],
            )
            .unwrap();
        assert_eq!(out, "Hero\t2\r\n");
        // Ungrouped (following does not count): just the actor.
        let out = f
            .host
            .exec_for_listener_with_extras(
                &mut f.world,
                f.mob,
                stalker,
                "print(#actor.group, actor.group[1].name)",
                &[],
            )
            .unwrap();
        assert_eq!(out, "1\tStalker\r\n");
    }

    // ----- .fighting -----

    #[test]
    fn fighting_is_the_current_opponent_or_nil() {
        let mut f = fixture();
        assert_eq!(f.ok("print(self.fighting)"), "nil\r\n");
        f.world.entity_mut(f.mob).insert(Fighting(f.player));
        assert_eq!(f.ok("print(self.fighting.name)"), "Hero\r\n");
        // A despawned opponent reads as nil, not a dangling handle.
        let gone = f.world.spawn_empty().id();
        f.world.entity_mut(f.mob).insert(Fighting(gone));
        f.world.despawn(gone);
        assert_eq!(f.ok("print(self.fighting)"), "nil\r\n");
    }

    // ----- actor:set_skill -----

    #[test]
    fn set_skill_grants_a_known_ability_from_a_script() {
        let mut f = fixture();
        let mut catalog = AbilityCatalog::default();
        catalog
            .by_name
            .insert("group heal".into(), ability(170, "group heal"));
        f.world.insert_resource(catalog);
        assert_eq!(
            f.ok("print(actor:set_skill('group heal', 100))"),
            "true\r\n"
        );
        let known = f.world.get::<KnownAbilities>(f.player).unwrap();
        assert_eq!(known.entries, vec![(170, 100, true)]);
        // Updating an existing entry, clamped to the ceiling.
        assert_eq!(
            f.ok("print(actor:set_skill('Group Heal', 5000))"),
            "true\r\n"
        );
        let known = f.world.get::<KnownAbilities>(f.player).unwrap();
        assert_eq!(known.entries, vec![(170, MAX_SET_SKILL, true)]);
        // Zero un-learns.
        f.ok("actor:set_skill('group heal', 0)");
        let known = f.world.get::<KnownAbilities>(f.player).unwrap();
        assert_eq!(known.entries, vec![(170, 0, false)]);
        // Unknown ability and non-player targets are refused quietly.
        assert_eq!(
            f.ok("print(actor:set_skill('no such thing', 50))"),
            "false\r\n"
        );
        assert_eq!(f.ok("print(self:set_skill('group heal', 50))"), "false\r\n");
        assert!(f.world.get::<KnownAbilities>(f.mob).is_none());
    }

    #[test]
    fn set_skill_is_refused_when_a_player_runs_the_script() {
        let mut f = fixture();
        let mut catalog = AbilityCatalog::default();
        catalog
            .by_name
            .insert("group heal".into(), ability(170, "group heal"));
        f.world.insert_resource(catalog);
        // The staff `lua` console runs with `self` = the player.
        let err = f
            .host
            .exec_for_actor(&mut f.world, f.player, "actor:set_skill('group heal', 100)")
            .expect_err("a player-run script must not set skills");
        assert!(
            err.contains("only available to attached trigger scripts"),
            "{err}"
        );
        assert!(f.world.get::<KnownAbilities>(f.player).is_none());
    }

    #[test]
    fn set_skill_is_not_reachable_from_condition_scripts() {
        let mut f = fixture();
        let err = f
            .host
            .eval_condition(
                &mut f.world,
                f.player,
                None,
                "actor:set_skill('group heal', 100) return true",
            )
            .expect_err("read-only actor has no set_skill");
        assert!(err.contains("lua error"), "{err}");
    }

    // ----- world.destroy(name) -----

    #[test]
    fn destroy_by_name_prefers_carried_then_room_and_never_players() {
        let mut f = fixture();
        let carried = f
            .world
            .spawn((
                Item,
                Named {
                    name: "a rusty sword".into(),
                },
                Located(f.mob),
            ))
            .id();
        let on_floor = f
            .world
            .spawn((
                Item,
                Named {
                    name: "a rusty sword".into(),
                },
                Located(f.room),
            ))
            .id();
        let elsewhere = f
            .world
            .spawn((
                Item,
                Named {
                    name: "a rusty sword".into(),
                },
                Located(f.other_room),
            ))
            .id();
        // Full short name (what `object.name` yields): the carried one goes.
        f.ok("world.destroy('a rusty sword')");
        assert!(f.world.get_entity(carried).is_err());
        assert!(f.world.get_entity(on_floor).is_ok());
        // Then the one on the floor of the script's room, by keyword.
        f.ok("world.destroy('rusty')");
        assert!(f.world.get_entity(on_floor).is_err());
        // Never an item in another room, and never a player.
        f.ok("world.destroy('rusty'); world.destroy('Hero')");
        assert!(f.world.get_entity(elsewhere).is_ok());
        assert!(f.world.get_entity(f.player).is_ok());
        // A mob in the room can be purged by name; unknown / nil are no-ops.
        let guard = f
            .world
            .spawn((
                Mob,
                Named {
                    name: "a guard".into(),
                },
                Located(f.room),
            ))
            .id();
        f.ok("world.destroy('nothing here'); world.destroy(nil); world.destroy('guard')");
        assert!(f.world.get_entity(guard).is_err());
        // The userdata form still works.
        let coin = f
            .world
            .spawn((
                Item,
                Named {
                    name: "a coin".into(),
                },
                WorldKey { zone: 43, id: 51 },
                Located(f.room),
            ))
            .id();
        f.ok("world.destroy(self:get_objects(43, 51))");
        assert!(f.world.get_entity(coin).is_err());
    }

    // ----- wait_hours -----

    #[test]
    fn wait_hours_parks_the_script_for_game_hours() {
        let mut f = fixture();
        f.host.set_current_tick(0);
        assert_eq!(f.ok("print('a') wait_hours(2) print('b')"), "a\r\n");
        assert_eq!(f.host.yielded_count(), 1);
        // Two game hours: 2 * 75 s at 10 Hz.
        f.host.set_current_tick(1499);
        assert_eq!(f.host.tick_yielded(&mut f.world), 0);
        f.host.set_current_tick(1500);
        assert_eq!(f.host.tick_yielded(&mut f.world), 1);
        assert_eq!(f.host.yielded_count(), 0);
    }

    // ----- wizard_notify -----

    #[test]
    fn wizard_notify_reaches_online_staff_only() {
        let mut f = fixture();
        let account = |role| Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role,
            account_role: role,
            perms: vec![],
        };
        f.world
            .entity_mut(f.player)
            .insert(account(UserRole::Player));
        let god = f
            .world
            .spawn((
                Player,
                Online,
                Named { name: "God".into() },
                account(UserRole::Immortal),
                Located(f.far_room),
            ))
            .id();
        f.ok("wizard_notify('Now starting!')");
        let out = f.world.resource::<LuaOutbox>();
        assert_eq!(
            out.lines,
            vec![OutLine::Direct {
                target: god,
                msg: "<b:cyan>[wiznet]</> Now starting!".to_string()
            }]
        );
    }

    #[test]
    fn wizard_notify_is_limited_to_one_line_a_minute_per_trigger_and_entity() {
        let f = fixture();
        let t0 = std::time::Instant::now();
        let lua = &f.host.lua;
        let secs = std::time::Duration::from_secs;
        let (a, b) = (Some(f.player), Some(f.far_room));
        assert!(wizard_notify_allowed(lua, 7, a, t0));
        assert!(!wizard_notify_allowed(lua, 7, a, t0 + secs(59)));
        // Another trigger has its own allowance.
        assert!(wizard_notify_allowed(lua, 8, a, t0 + secs(59)));
        // So does the same script body running for another entity.
        assert!(wizard_notify_allowed(lua, 7, b, t0 + secs(59)));
        assert!(wizard_notify_allowed(lua, 7, a, t0 + secs(60)));
        assert!(!wizard_notify_allowed(lua, 7, a, t0 + secs(61)));
    }

    #[test]
    fn a_spammy_wizard_notify_script_reaches_staff_once() {
        let mut f = fixture();
        f.world.spawn((
            Player,
            Online,
            Named { name: "God".into() },
            Account {
                user_id: "u".into(),
                character_id: "c".into(),
                role: UserRole::Immortal,
                account_role: UserRole::Immortal,
                perms: vec![],
            },
            Located(f.far_room),
        ));
        f.ok("for i = 1, 5 do wizard_notify('spam ' .. i) end");
        assert_eq!(f.world.resource::<LuaOutbox>().lines.len(), 1);
    }

    // ----- actor:cast -----

    #[derive(Resource, Default)]
    struct CastLog(Vec<(Entity, String)>);

    fn record_cast(world: &mut World, caster: Entity, args: &str) {
        world
            .resource_mut::<CastLog>()
            .0
            .push((caster, args.to_string()));
    }

    #[test]
    fn cast_method_dispatches_like_spells_cast() {
        let mut f = fixture();
        f.world.init_resource::<CastLog>();
        f.world
            .insert_resource(crate::SpellExecutor(Some(record_cast)));
        f.ok("self:cast('inspiration')");
        f.ok("self:cast('sleep', actor)");
        f.ok("self:cast('bless', 'Hero')");
        assert_eq!(
            f.world.resource::<CastLog>().0,
            vec![
                (f.mob, "inspiration".to_string()),
                (f.mob, "sleep Hero".to_string()),
                (f.mob, "bless Hero".to_string()),
            ]
        );
    }

    // ----- actor:has_effect_named -----

    #[test]
    fn has_effect_named_matches_the_active_effect_name() {
        let mut f = fixture();
        effect(&mut f.world, f.player, "comprehend languages");
        assert_eq!(
            f.ok("print(actor:has_effect_named('ComprehendLang'), actor:has_effect_named('sanctuary'), self:has_effect_named('ComprehendLang'))"),
            "true\tfalse\tfalse\r\n"
        );
    }

    // ----- get_people() -----

    #[test]
    fn get_people_without_arguments_lists_the_room() {
        let mut f = fixture();
        let guard = f
            .world
            .spawn((
                Mob,
                Named {
                    name: "a guard".into(),
                },
                WorldKey { zone: 9, id: 2 },
                Located(f.room),
            ))
            .id();
        // Items and people elsewhere are not listed.
        f.world.spawn((
            Item,
            Named {
                name: "a coin".into(),
            },
            Located(f.room),
        ));
        f.world
            .spawn((Mob, Named { name: "far".into() }, Located(f.other_room)));
        let body = "local n, names = 0, {} \
            for _, p in ipairs(self:get_people()) do n = n + 1; names[#names + 1] = p.name end \
            table.sort(names) print(n, table.concat(names, ','))";
        assert_eq!(f.ok(body), "3\tHero,a guard,a script keeper\r\n");
        // The room form and a carried item's view give the same answer.
        assert_eq!(
            f.ok("print(#self.room:get_people(), #get_room(5, 1):get_people(), #get_room(5, 2):get_people())"),
            "3\t3\t1\r\n"
        );
        // A purge during the walk is safe: the list is a snapshot.
        f.ok("for _, p in ipairs(self:get_people()) do if p.id == 2 then world.destroy(p) end end");
        assert!(f.world.get_entity(guard).is_err());
        // The (zone, id) form is unchanged; half an argument list is an error.
        assert_eq!(
            f.ok("print(self:get_people(9, 1) ~= nil, self:get_people(9, 7))"),
            "true\tnil\r\n"
        );
        assert!(f.run("self:get_people(9)").is_err());
    }

    // ----- actor.composition -----

    #[test]
    fn composition_reads_the_effective_composition() {
        let mut f = fixture();
        assert_eq!(
            f.ok("print(self.composition, actor.composition)"),
            "flesh\tflesh\r\n"
        );
        f.world
            .entity_mut(f.mob)
            .insert(mud_world::CompositionOverride(
                mud_db::enums::Composition::Water,
            ));
        assert_eq!(f.ok("print(self.composition)"), "water\r\n");
        f.world
            .entity_mut(f.mob)
            .remove::<mud_world::CompositionOverride>();
        assert_eq!(f.ok("print(self.composition)"), "flesh\r\n");
    }

    // ----- actor.wearing -----

    #[test]
    fn wearing_lists_worn_items_by_legacy_vnum() {
        let mut f = fixture();
        let worn = |f: &mut Fixture, zone, id, slot: Option<mud_world::Slot>| {
            let mut e = f.world.spawn((
                Item,
                Named {
                    name: format!("flag {zone}:{id}"),
                },
                WorldKey { zone, id },
                Located(f.player),
            ));
            if let Some(slot) = slot {
                e.insert(EquippedSlot(slot));
            }
            e.id()
        };
        let flag = worn(&mut f, 188, 80, Some(mud_world::Slot::Head));
        worn(&mut f, 188, 81, None); // carried, not worn
        worn(&mut f, 1000, 7, Some(mud_world::Slot::Neck)); // zone 1000 is the old zone 0
        assert_eq!(
            f.ok("print(actor.wearing[18880] ~= nil, actor.wearing[18881] ~= nil, actor.wearing[7] ~= nil, actor.wearing[18880].name)"),
            "true\tfalse\ttrue\tflag 188:80\r\n"
        );
        assert_eq!(
            f.ok("local n = 0 for _ in pairs(actor.wearing) do n = n + 1 end print(n, self.wearing[18880])"),
            "2\tnil\r\n"
        );
        let _ = flag;
    }

    // ----- object.val0 .. val3 -----

    fn item_with(f: &mut Fixture, id: i32, t: ObjectType, values: serde_json::Value) -> Entity {
        let mut p = proto(70, id, "a thing");
        p.r#type = t;
        p.values = values;
        let mut protos = f
            .world
            .remove_resource::<ObjectPrototypes>()
            .unwrap_or_default();
        protos.by_key.insert((70, id), p);
        f.world.insert_resource(protos);
        f.world
            .spawn((
                Item,
                Named {
                    name: "a thing".into(),
                },
                WorldKey { zone: 70, id },
                Located(f.room),
            ))
            .id()
    }

    #[test]
    fn liquid_val2_reads_the_loaded_liquids_table() {
        let mut f = fixture();
        let vals = "print(self:get_objects(70, 2).val2)";
        let cup = item_with(
            &mut f,
            2,
            ObjectType::Fountain,
            serde_json::json!({"Capacity": 9, "Remaining": 9, "Liquid": "WATER"}),
        );
        // Liquids come from the loaded `Liquids` table: a poured alias resolves, and a
        // liquid added in Muditor numbers after the legacy ones.
        let row = |id: i32, alias: &str, idx: Option<i32>, code: Option<&str>| LegacyLiquidRow {
            id,
            alias: alias.into(),
            name: alias.replace('-', " "),
            legacy_index: idx,
            legacy_code: code.map(str::to_string),
        };
        f.world.insert_resource(LiquidLegacyIndex::from_rows(&[
            row(1, "water", Some(0), Some("WATER")),
            row(12, "black-tea", Some(11), Some("TEA")),
            row(43, "root-beer", None, None),
        ]));
        f.world.entity_mut(cup).insert(LiquidContainer {
            liquid: "black-tea".into(),
            capacity: 9,
            remaining: 4,
            poisoned: false,
        });
        assert_eq!(f.ok(vals), "11\r\n");
        f.world.entity_mut(cup).insert(LiquidContainer {
            liquid: "root-beer".into(),
            capacity: 9,
            remaining: 4,
            poisoned: false,
        });
        assert_eq!(f.ok(vals), "42\r\n");
    }

    #[test]
    fn val_fields_follow_the_object_type() {
        let mut f = fixture();
        let vals = "print(self:get_objects(70, ID).val0, self:get_objects(70, ID).val1, self:get_objects(70, ID).val2, self:get_objects(70, ID).val3)";
        let run = |f: &mut Fixture, id: i32| f.ok(&vals.replace("ID", &id.to_string()));
        // Drink container: capacity, remaining, liquid code, poisoned.
        item_with(
            &mut f,
            1,
            ObjectType::Drinkcontainer,
            serde_json::json!({"Capacity": 20, "Remaining": 0, "Liquid": "WHISKY", "Poisoned": true}),
        );
        assert_eq!(run(&mut f, 1), "20\t0\t5\t1\r\n");
        // Live state wins over the proto once the instance has it.
        let cup = item_with(
            &mut f,
            2,
            ObjectType::Fountain,
            serde_json::json!({"Capacity": 9, "Remaining": 9, "Liquid": "WATER"}),
        );
        f.world.entity_mut(cup).insert(LiquidContainer {
            liquid: "tea".into(),
            capacity: 9,
            remaining: 4,
            poisoned: false,
        });
        assert_eq!(run(&mut f, 2), "9\t4\t11\t0\r\n");
        // Container: capacity, flag bits, key.
        item_with(
            &mut f,
            3,
            ObjectType::Container,
            serde_json::json!({"Capacity": 150, "Flags": ["Closeable", "Locked"], "Key": 4}),
        );
        assert_eq!(run(&mut f, 3), "150\t9\t4\t0\r\n");
        // Light: lit flag, capacity, remaining.
        let lamp = item_with(
            &mut f,
            4,
            ObjectType::Light,
            serde_json::json!({"Capacity": -1, "Remaining": -1}),
        );
        f.world.entity_mut(lamp).insert(LightFuel {
            capacity: -1,
            remaining: -1,
        });
        assert_eq!(run(&mut f, 4), "1\t-1\t-1\t0\r\n");
        // Wand: level, max charges, charges left (live), spell not exposed.
        let wand = item_with(
            &mut f,
            5,
            ObjectType::Wand,
            serde_json::json!({"Level": 30, "Max_Charges": 5, "Charges_Left": 5, "Spell": "BLESS"}),
        );
        assert_eq!(run(&mut f, 5), "30\t5\t5\t0\r\n");
        f.world.entity_mut(wand).insert(Charges(2));
        assert_eq!(run(&mut f, 5), "30\t5\t2\t0\r\n");
        // Food: hours of nourishment, poisoned.
        item_with(
            &mut f,
            6,
            ObjectType::Food,
            serde_json::json!({"Filling": 12, "Poisoned": false}),
        );
        assert_eq!(run(&mut f, 6), "12\t0\t0\t0\r\n");
        // Weapon: the typed dice and damage type columns.
        let mut w = proto(70, 7, "a sword");
        w.r#type = ObjectType::Weapon;
        w.weapon_dice_num = 2;
        w.weapon_dice_size = 6;
        w.weapon_damage_type = Some("Slash".into());
        f.world
            .resource_mut::<ObjectPrototypes>()
            .by_key
            .insert((70, 7), w);
        f.world.spawn((
            Item,
            Named {
                name: "a sword".into(),
            },
            WorldKey { zone: 70, id: 7 },
            Located(f.room),
        ));
        assert_eq!(run(&mut f, 7), "0\t2\t6\t3\r\n");
        // Characters have no values.
        assert_eq!(f.ok("print(actor.val0, self.val2)"), "nil\tnil\r\n");
    }

    // ----- (zone, id) argument forms (IdArg) -----

    /// Every spelling of the key (30, 1) a script may pass.
    const FORMS: [&str; 4] = [
        "30, 1",
        "{30, 1}",
        "{zone = 30, id = 1}",
        "{zone_id = 30, local_id = 1}",
    ];

    /// Calls built from `tmpl` (with `$K` the key) that are not valid keys.
    const BAD_KEYS: [&str; 6] = ["", "30", "{30}", "{zone = 30}", "{30, 1, 2}", "{'a', 'b'}"];

    /// Run `tmpl` once per accepted form and expect `want` every time.
    fn all_forms(f: &mut Fixture, tmpl: &str, want: &str) {
        for form in FORMS {
            let body = tmpl.replace("$K", form);
            assert_eq!(f.ok(&body), want, "{body}");
        }
    }

    /// Every malformed key must be refused with the accepted-forms message.
    fn all_bad(f: &mut Fixture, tmpl: &str) {
        for bad in BAD_KEYS {
            let body = tmpl.replace("$K", bad);
            let err = f.run(&body).expect_err(&body);
            assert!(err.contains("(zone, id)"), "{body}: {err}");
        }
    }

    #[test]
    fn id_forms_for_templates_rooms_and_world_lookups() {
        let mut f = fixture();
        f.world.spawn((
            Mob,
            Named {
                name: "a druid".into(),
            },
            WorldKey { zone: 30, id: 1 },
            Located(f.room),
        ));
        f.world.spawn((
            Item,
            Named {
                name: "a gem".into(),
            },
            WorldKey { zone: 30, id: 1 },
            Located(f.room),
        ));
        let mut index = f.world.remove_resource::<WorldKeyIndex>().unwrap();
        let r = f
            .world
            .spawn((
                Room,
                Named { name: "r".into() },
                WorldKey { zone: 30, id: 1 },
            ))
            .id();
        index.rooms.insert((30, 1), r);
        f.world.insert_resource(index);
        for api in ["mobiles.template", "objects.template"] {
            all_forms(
                &mut f,
                &format!("print({api}($K).zone_id, {api}($K).id)"),
                "30\t1\r\n",
            );
            all_bad(&mut f, &format!("print({api}($K).id)"));
        }
        all_forms(&mut f, "print(get_room($K).id)", "1\r\n");
        all_forms(
            &mut f,
            "print(world.count_mobiles($K), world.count_objects($K))",
            "1\t1\r\n",
        );
        all_forms(&mut f, "print(world.find_mobile($K).name)", "a druid\r\n");
        for api in [
            "get_room",
            "world.count_mobiles",
            "world.count_objects",
            "world.find_mobile",
        ] {
            all_bad(&mut f, &format!("{api}($K)"));
        }
    }

    #[test]
    fn id_forms_for_room_queries_and_world_counts() {
        let mut f = fixture();
        f.world.spawn((
            Mob,
            Named {
                name: "a guard".into(),
            },
            WorldKey { zone: 30, id: 1 },
            Located(f.room),
        ));
        f.world.spawn((
            Item,
            Named {
                name: "lash".into(),
            },
            WorldKey { zone: 30, id: 1 },
            Located(f.room),
        ));
        all_forms(&mut f, "print(self:get_people($K).name)", "a guard\r\n");
        all_forms(&mut f, "print(self:get_objects($K).name)", "lash\r\n");
        all_forms(
            &mut f,
            "print(get_room(5, 1):get_people($K).name, get_room(5, 1):get_objects($K).name)",
            "a guard\tlash\r\n",
        );
        all_forms(
            &mut f,
            "print(self:get_mexists($K), self:get_oexists($K))",
            "1\t1\r\n",
        );
        all_forms(&mut f, "print(actor:get_mexists($K))", "1\r\n");
        for call in [
            "self:get_objects($K)",
            "self:get_mexists($K)",
            "self:get_oexists($K)",
            "get_room(5, 1):get_objects($K)",
        ] {
            all_bad(&mut f, call);
        }
        // A half-given key is still refused; no key at all lists the room.
        for bad in ["30", "{30}", "{zone = 30}"] {
            let err = f.run(&format!("self:get_people({bad})")).unwrap_err();
            assert!(err.contains("(zone, id)"), "{bad}: {err}");
        }
        assert_eq!(f.ok("print(#self:get_people() >= 2)"), "true\r\n");
    }

    #[test]
    fn id_forms_for_get_obj_noadesc() {
        let mut f = fixture();
        let mut protos = ObjectPrototypes::default();
        protos
            .by_key
            .insert((30, 1), proto(30, 1, "a gleaming trident"));
        f.world.insert_resource(protos);
        all_forms(&mut f, "print(get_obj_noadesc($K))", "gleaming trident\r\n");
        all_bad(&mut f, "get_obj_noadesc($K)");
    }

    #[test]
    fn id_forms_for_has_item_and_has_equipped() {
        let mut f = fixture();
        f.world.spawn((
            Item,
            Named {
                name: "carried".into(),
            },
            WorldKey { zone: 30, id: 1 },
            Located(f.player),
        ));
        all_forms(
            &mut f,
            "print(actor:has_item($K), actor:has_equipped($K))",
            "true\tfalse\r\n",
        );
        f.world.spawn((
            Item,
            Named {
                name: "worn".into(),
            },
            WorldKey { zone: 30, id: 2 },
            Located(f.player),
            EquippedSlot(mud_world::Slot::Head),
        ));
        assert_eq!(f.ok("print(actor:has_equipped({30, 2}), actor:has_equipped(30, 2), actor:has_equipped({zone = 30, id = 2}))"), "true\ttrue\ttrue\r\n");
        all_bad(&mut f, "actor:has_item($K)");
        all_bad(&mut f, "actor:has_equipped($K)");
    }

    #[test]
    fn id_forms_for_spawn_object_and_spawn_mobile() {
        let mut f = fixture();
        let mut protos = ObjectPrototypes::default();
        protos.by_key.insert((30, 1), proto(30, 1, "a gem"));
        f.world.insert_resource(protos);
        f.world.insert_resource(mud_world::MobPrototypes::default());
        let count = |f: &mut Fixture| f.ok("print(self:get_oexists(30, 1))");
        let mut n = 0;
        for form in FORMS {
            f.ok(&format!("actor:spawn_object({form})"));
            n += 1;
            assert_eq!(
                count(&mut f),
                format!("{n}\r\n"),
                "actor:spawn_object({form})"
            );
            f.ok(&format!("get_room(5, 1):spawn_object({form})"));
            n += 1;
            assert_eq!(
                count(&mut f),
                format!("{n}\r\n"),
                "room:spawn_object({form})"
            );
        }
        // A key with no prototype is nil in every form, never an error;
        // spawn_mobile has no prototype loaded here, so this covers its parsing.
        all_forms(&mut f, "print(get_room(5, 1):spawn_mobile($K))", "nil\r\n");
        for call in [
            "actor:spawn_object($K)",
            "get_room(5, 1):spawn_object($K)",
            "get_room(5, 1):spawn_mobile($K)",
        ] {
            all_bad(&mut f, call);
        }
    }

    #[test]
    fn id_forms_for_active_quest_and_run_room_trigger() {
        let mut f = fixture();
        f.world.entity_mut(f.player).insert(Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        });
        all_forms(&mut f, "print(actor:active_quest($K) ~= nil)", "true\r\n");
        all_bad(&mut f, "actor:active_quest($K)");
        for form in FORMS {
            f.ok(&format!("run_room_trigger({form})"));
        }
        let queued: Vec<(i32, i32)> = f
            .world
            .resource::<mud_world::DeferredRoomTriggerFires>()
            .queue
            .iter()
            .map(|q| (q.room_zone, q.room_id))
            .collect();
        assert_eq!(queued, vec![(30, 1); FORMS.len()]);
        all_bad(&mut f, "run_room_trigger($K)");
    }

    #[test]
    fn id_forms_for_exit_set_key() {
        use mud_db::enums::{Direction, ExitState};
        let mut f = fixture();
        f.world
            .entity_mut(f.room)
            .insert(mud_world::Exits(std::collections::HashMap::from([(
                Direction::North,
                mud_world::ExitData {
                    to: Some(f.other_room),
                    state: ExitState::Open,
                    key: None,
                    description: None,
                    keywords: vec![],
                    is_hidden: false,
                    is_pickproof: false,
                    is_bashable: false,
                    hit_points: None,
                },
            )])));
        let key = |f: &mut Fixture| {
            f.world.get::<mud_world::Exits>(f.room).unwrap().0[&Direction::North].key
        };
        for form in FORMS {
            f.ok("get_room(5, 1):exit('north'):set_key(nil)");
            assert_eq!(key(&mut f), None);
            f.ok(&format!("get_room(5, 1):exit('north'):set_key({form})"));
            assert_eq!(key(&mut f), Some((30, 1)), "{form}");
        }
        f.ok("get_room(5, 1):exit('north'):set_key()");
        assert_eq!(key(&mut f), None);
        for bad in ["30", "{30}", "{zone = 30}", "{30, 1, 2}", "{'a', 'b'}"] {
            let err = f
                .run(&format!("get_room(5, 1):exit('north'):set_key({bad})"))
                .unwrap_err();
            assert!(err.contains("(zone, id)"), "{bad}: {err}");
        }
    }
}
