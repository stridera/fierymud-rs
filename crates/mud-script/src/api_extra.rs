//! Lua API the converted trigger corpus calls but the core bindings in
//! `lib.rs` did not provide: `zone.echo`, `timestamp`, `trigger_log`,
//! `find_player`, `get_obj_noadesc`, the `actor:get_*` world-query methods,
//! `actor:set_skill`, and the `.group` / `.fighting` actor fields.
//!
//! Kept in its own module so `lib.rs` only needs three hook-up lines: the
//! global binding call in `bind_globals`, `add_actor_methods` /
//! `add_room_methods` in the `UserData` impls, and `actor_field` as the
//! fall-through of the actor `__index`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use bevy_ecs::prelude::*;
use mlua::{Lua, Table, UserDataMethods, Value, Variadic};
use mud_world::components::{
    EffectInstance, Fighting, Item, KnownAbilities, Located, Mob, MobBehaviors, MobTraits, Named,
    ObjectFlags, ObjectRestrictions, Online, Player, PlayerFlags, Room, WorldKey,
};
use mud_world::resources::{
    AbilityCatalog, LuaOutbox, MudClock, ObjectPrototypes, TriggerCatalog, WorldKeyIndex,
};

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

/// Fingerprint of the body about to run, stashed by the host right before
/// a fire so `trigger_log` can name the trigger without the dispatcher
/// having to pass its id. Only set for bodies that mention `trigger_log`.
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
    if code.contains("trigger_log") {
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

    // find_player(name)
    globals.set(
        "find_player",
        lua.create_function(|lua, name: String| find_player(lua, &name))?,
    )?;

    // get_obj_noadesc(zone, id)
    globals.set(
        "get_obj_noadesc",
        lua.create_function(|lua, (zone, id): (i32, i32)| -> mlua::Result<String> {
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

    // trigger_log(...): the fingerprint belongs to the trigger, so it
    // lives in the trigger's private environment and survives `wait`.
    let fp = if let Value::Integer(i) = globals.raw_get::<Value>("__trigger_fp")? {
        i.cast_unsigned()
    } else {
        let fp = lua
            .remove_app_data::<TriggerFingerprint>()
            .map_or(0, |f| f.0);
        globals.raw_set("__trigger_fp", fp.cast_signed())?;
        fp
    };
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
            out.messages.push((room, msg.to_string(), None));
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
    let mut q = world.query::<(Entity, &Located, &WorldKey, Has<Item>)>();
    let mut hits: Vec<Entity> = q
        .iter(world)
        .filter(|(_, l, wk, is_item)| {
            l.0 == room && wk.zone == zone && wk.id == id && *is_item == items
        })
        .map(|(e, ..)| e)
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
    let mut q = world.query::<(&EffectInstance, &mud_world::components::AppliedTo)>();
    q.iter(world)
        .filter(|(_, applied)| applied.0 == entity)
        .any(|(inst, _)| norm(&inst.name).starts_with(&want))
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

/// Methods added to `LuaActor`.
pub(crate) fn add_actor_methods<M: UserDataMethods<LuaActor>>(methods: &mut M) {
    // `self:get_people(zone, id)`: a character with that prototype in
    // the entity's room (the room itself for room triggers), or nil.
    methods.add_method(
        "get_people",
        |lua, this, (zone, id): (i32, i32)| -> mlua::Result<Value> {
            let found = world_mut_from_lua(lua, |w| {
                let room = room_of(w, this.entity)?;
                find_in_room(w, room, zone, id, false)
            })?;
            actor_value(lua, found)
        },
    );
    // `self:get_objects(zone, id)`: an item with that prototype in the
    // entity's room, or nil.
    methods.add_method(
        "get_objects",
        |lua, this, (zone, id): (i32, i32)| -> mlua::Result<Value> {
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
        |lua, _this, (zone, id): (i32, i32)| -> mlua::Result<i64> {
            world_mut_from_lua(lua, |w| count_world(w, zone, id, false))
        },
    );
    methods.add_method(
        "get_oexists",
        |lua, _this, (zone, id): (i32, i32)| -> mlua::Result<i64> {
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
}

/// Methods added to `LuaRoom`.
pub(crate) fn add_room_methods<M: UserDataMethods<LuaRoom>>(methods: &mut M) {
    methods.add_method(
        "get_people",
        |lua, this, (zone, id): (i32, i32)| -> mlua::Result<Value> {
            let found = world_mut_from_lua(lua, |w| find_in_room(w, this.entity, zone, id, false))?;
            actor_value(lua, found)
        },
    );
    methods.add_method(
        "get_objects",
        |lua, this, (zone, id): (i32, i32)| -> mlua::Result<Value> {
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
        _ => Ok(Value::Nil),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LuaHost;
    use mud_world::components::{AppliedTo, EffectSource, GroupMember, Health};
    use mud_world::resources::{AbilityDef, ObjectProto, TriggerAttach, TriggerDef};
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
            out.messages,
            vec![(f.room, "The ground shakes.".to_string(), None)]
        );
        // An empty message queues nothing.
        f.world.resource_mut::<LuaOutbox>().messages.clear();
        f.ok("zone.echo(5, '')");
        assert!(f.world.resource::<LuaOutbox>().messages.is_empty());
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
}
