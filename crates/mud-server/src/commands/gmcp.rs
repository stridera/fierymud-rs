//! GMCP payload plumbing shared by the prompt path and the commands
//! that push frames ahead of the prompt (`look`, movement).
//!
//! Every payload built here goes through `serde_json`, so control
//! characters, quotes and CRLF in names or descriptions can never
//! produce an invalid frame. Frames the prompt re-sends on every turn
//! are gated by [`GmcpSent`] so the client only receives a package
//! when its content changed (or after it renegotiated).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use bevy_ecs::prelude::*;
use mud_world::{
    AbilityCatalog, AppliedTo, EffectInstance, EffectSource, Exits, Located, Named, RoomSector,
    WorldKey, WorldKeyIndex,
};
use serde_json::{Value, json};

use super::{
    ColorMode, Connection, Description, direction_name, exit_is_hidden_to, render_color_tags,
    room_composite_num, sector_label,
};

/// Per-player record of the last payload hash sent for each GMCP
/// package. Cleared when the client renegotiates (`Core.Hello`,
/// `Core.Supports.Set`) and whenever a connection binds to the player
/// (login spawn, takeover, linkdead reconnect) so a fresh client gets
/// everything again.
#[derive(Component, Debug, Default)]
pub(crate) struct GmcpSent(HashMap<&'static str, u64>);

/// Forget everything sent to this player so the next prompt re-sends
/// every change-gated package.
pub(crate) fn clear_gmcp_sent(world: &mut World, player: Entity) {
    if let Some(mut sent) = world.get_mut::<GmcpSent>(player) {
        sent.0.clear();
    }
}

fn hash_payload(payload: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    payload.hash(&mut h);
    h.finish()
}

/// Send `package` unless its payload is identical to the last one sent
/// to this player. `force` always sends (and records the hash).
pub(crate) fn send_if_changed(
    world: &mut World,
    target: Entity,
    package: &'static str,
    payload: &str,
    force: bool,
) {
    let hash = hash_payload(payload);
    if world.get::<GmcpSent>(target).is_none() {
        super::try_insert(world, target, GmcpSent::default());
    }
    if let Some(mut sent) = world.get_mut::<GmcpSent>(target) {
        let prev = sent.0.insert(package, hash);
        if !force && prev == Some(hash) {
            return;
        }
    }
    if let Some(conn) = world.get::<Connection>(target) {
        let _ = conn.0.try_send(mud_net::gmcp_packet(package, payload));
    }
}

fn strip(s: &str) -> String {
    render_color_tags(s, ColorMode::Strip)
}

/// Whether `viewer` can make out `room`'s contents. The same predicate
/// `look` uses for its pitch-black gate.
pub(crate) fn viewer_sees_room(world: &mut World, viewer: Entity, room: Entity) -> bool {
    !(super::room_is_dark(world, room)
        && !super::room_has_light(world, room)
        && !super::player_can_see_in_dark(world, viewer))
}

/// How much of another character `viewer` makes out in `room`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Perceived {
    /// Seen properly: real name and details.
    Clear,
    /// Only a warm shape in the dark (infravision): a generic entry.
    Shape,
    /// Not perceived at all: left out of every panel.
    Unseen,
}

/// What `viewer` perceives of `target` standing in a room, given
/// whether the viewer can make out that room (`room_seen`, from
/// [`viewer_sees_room`]). Exactly the `look` rules: [`can_see_player`]
/// (magic invisibility, `WizInvis`; gods and `HOLY_LIGHT` pierce) first,
/// then darkness, where only infravision still gives a red shape.
/// Every GMCP panel that names a character goes through this.
///
/// [`can_see_player`]: super::can_see_player
pub(crate) fn perceives(
    world: &World,
    viewer: Entity,
    target: Entity,
    room_seen: bool,
) -> Perceived {
    if !super::can_see_player(world, viewer, target) {
        Perceived::Unseen
    } else if room_seen {
        Perceived::Clear
    } else if super::senses::has_infravision(world, viewer) {
        Perceived::Shape
    } else {
        Perceived::Unseen
    }
}

/// The name a panel shows for `target` given how it is perceived, or
/// `None` when it must be omitted. Shapes get the generic infravision
/// label, never the real name.
pub(crate) fn perceived_name(world: &World, target: Entity, how: Perceived) -> Option<String> {
    match how {
        Perceived::Clear => Some(
            world
                .get::<Named>(target)
                .map(|n| strip(&n.name))
                .unwrap_or_default(),
        ),
        Perceived::Shape => Some(format!(
            "the {}",
            super::senses::red_shape_label(world, target)
        )),
        Perceived::Unseen => None,
    }
}

/// Build the `Room.Players` payload: every other player in `viewer`'s
/// room, as far as `viewer` perceives them (see [`perceives`]).
pub(crate) fn build_room_players(world: &mut World, viewer: Entity) -> String {
    let Some(room) = world.get::<Located>(viewer).map(|l| l.0) else {
        return "[]".to_string();
    };
    let room_seen = viewer_sees_room(world, viewer, room);
    let here: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<mud_world::Player>>();
        q.iter(world)
            .filter(|(e, loc)| loc.0 == room && *e != viewer)
            .map(|(e, _)| e)
            .collect()
    };
    let mut names: Vec<String> = here
        .into_iter()
        .filter_map(|e| {
            let how = perceives(world, viewer, e, room_seen);
            perceived_name(world, e, how)
        })
        .collect();
    // ECS query order shifts whenever an unrelated component is added or
    // removed, which would change the hash and resend an identical list.
    names.sort();
    let entries: Vec<Value> = names
        .into_iter()
        .map(|n| json!({ "name": n, "full_name": n }))
        .collect();
    Value::Array(entries).to_string()
}

/// Send `Room.Players` for `viewer`'s room. `force` always sends; the
/// prompt path passes `false` so it only goes out when what the viewer
/// perceives changed (someone arrives, a light goes out, invisibility
/// fades).
pub(crate) fn send_room_players(world: &mut World, viewer: Entity, force: bool) {
    if world.get::<Connection>(viewer).is_none() {
        return;
    }
    let payload = build_room_players(world, viewer);
    send_if_changed(world, viewer, "Room.Players", &payload, force);
}

/// Build the `Room.Info` payload for `viewer`'s current room, or `{}`
/// when they cannot see it (dark room) or are nowhere.
pub(crate) fn build_room_info(world: &mut World, viewer: Entity) -> String {
    let Some(room) = world.get::<Located>(viewer).map(|l| l.0) else {
        return "{}".to_string();
    };
    if !viewer_sees_room(world, viewer, room) {
        return "{}".to_string();
    }
    let name = world
        .get::<Named>(room)
        .map_or_else(String::new, |n| strip(&n.name));
    let desc = world
        .get::<Description>(room)
        .map_or_else(String::new, |d| strip(d.0.trim_end()));
    let (zone_id, room_id) = world
        .get::<WorldKey>(room)
        .map_or((-1, -1), |k| (k.zone, k.id));
    let num = room_composite_num(zone_id, room_id);
    // God zones are not on any mortal map: no area name for them.
    let area = world
        .get_resource::<WorldKeyIndex>()
        .and_then(|idx| idx.zones.get(&zone_id).copied())
        .filter(|_| crate::room_access::room_visible_to(world, viewer, room))
        .and_then(|zone_e| world.get::<Named>(zone_e).map(|n| strip(&n.name)))
        .unwrap_or_default();
    let environment = world
        .get::<RoomSector>(room)
        .map_or("Unknown", |s| sector_label(s.0));

    let mut exits = serde_json::Map::new();
    let mut doors = serde_json::Map::new();
    let mut exit_details = serde_json::Map::new();
    if let Some(room_exits) = world.get::<Exits>(room) {
        for (dir, data) in &room_exits.0 {
            // Undiscovered hidden exits and exits into a god zone are
            // not mapped; hidden exits the player found via `search` are.
            if exit_is_hidden_to(world, viewer, room, *dir, data) {
                continue;
            }
            let dir_name = direction_name(*dir);
            let dest_num = data
                .to
                .and_then(|e| world.get::<WorldKey>(e))
                .map_or(0, |k| room_composite_num(k.zone, k.id));
            exits.insert(dir_name.to_string(), json!(dest_num));
            let door_state = match data.state {
                mud_db::enums::ExitState::Open => None,
                mud_db::enums::ExitState::Closed => Some("closed"),
                mud_db::enums::ExitState::Locked => Some("locked"),
            };
            if let Some(state) = door_state {
                doors.insert(dir_name.to_string(), json!(state));
            }
            let mut detail = serde_json::Map::new();
            detail.insert("to".into(), json!(dest_num));
            let is_door = door_state.is_some() || !data.keywords.is_empty();
            if is_door {
                detail.insert("door".into(), json!(true));
                if let Some(kw) = data.keywords.first() {
                    detail.insert("door_name".into(), json!(strip(kw)));
                }
            }
            exit_details.insert(dir_name.to_string(), Value::Object(detail));
        }
    }
    let mut payload = json!({
        "num": num,
        "name": name,
        "zone": area,
        "area": area,
        "desc": desc,
        "environment": environment,
        "exits": exits,
        "exit_details": exit_details,
        "doors": doors,
    });
    // Builder-set layout coords let client mappers place rooms exactly
    // instead of compass-walking; absence means "auto-place me".
    if let Some(l) = world.get::<mud_world::RoomLayout>(room) {
        payload["coords"] = json!(format!("{},{},{}", l.x, l.y, l.z));
    }
    payload.to_string()
}

/// Send `Room.Info` for `viewer`'s room. `force` (used by `look`, so
/// movement delivers it ahead of the room text) always sends; the
/// prompt path passes `false` and only sends on change.
pub(crate) fn send_room_info(world: &mut World, viewer: Entity, force: bool) {
    if world.get::<Connection>(viewer).is_none() {
        return;
    }
    let payload = build_room_info(world, viewer);
    send_if_changed(world, viewer, "Room.Info", &payload, force);
}

/// Cached `Ability.id` -> plain name map for `Char.Effects`, rebuilt
/// only when the catalog's size changes (catalog reload) rather than
/// on every prompt.
#[derive(Resource, Default)]
struct GmcpAbilityNames {
    source_len: usize,
    names: HashMap<i32, String>,
}

fn ability_name(world: &mut World, id: i32) -> Option<String> {
    let len = world
        .get_resource::<AbilityCatalog>()
        .map(|c| c.by_name.len())?;
    let stale = world
        .get_resource::<GmcpAbilityNames>()
        .is_none_or(|c| c.source_len != len);
    if stale {
        let names = world
            .resource::<AbilityCatalog>()
            .by_name
            .values()
            .map(|d| (d.id, d.plain_name.clone()))
            .collect();
        world.insert_resource(GmcpAbilityNames {
            source_len: len,
            names,
        });
    }
    world.resource::<GmcpAbilityNames>().names.get(&id).cloned()
}

/// The `Char.Effects` array for `target`: one entry per active effect
/// attached to the player. `name` is the effect's own label, `ability`
/// the originating spell ("" when none), `duration` seconds remaining
/// (-1 = permanent), `source` the high-level origin tag.
pub(crate) fn build_char_effects(world: &mut World, target: Entity) -> String {
    let mut rows: Vec<EffectInstance> = Vec::new();
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    for (inst, applied) in q.iter(world) {
        if applied.0 == target {
            rows.push(inst.clone());
        }
    }
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|inst| {
            let ability = inst
                .ability_id
                .and_then(|id| ability_name(world, id))
                .map(|s| strip(&s))
                .unwrap_or_default();
            let source = match &inst.source {
                EffectSource::Spell => "spell",
                EffectSource::Item => "item",
                EffectSource::Room => "room",
                EffectSource::Admin => "admin",
                EffectSource::Other(_) => "other",
            };
            json!({
                "name": inst.name,
                "ability": ability,
                "duration": inst.remaining_secs,
                "source": source,
                "strength": inst.strength,
            })
        })
        .collect();
    Value::Array(entries).to_string()
}

/// The `Char.Aggro` payload: mobs that have `target` on their hate list
/// (`hating`) or in their memory (`remembering`), anywhere in the world.
/// Only mobs that carry a `HateList` / `MobMemory` are visited, so the
/// cost tracks the number of mobs that ever fought someone, not the
/// size of the world.
pub(crate) fn build_char_aggro(world: &mut World, target: Entity) -> String {
    use crate::combat::{HateList, MobMemory};
    let mut hating: Vec<String> = Vec::new();
    let mut remembering: Vec<String> = Vec::new();
    let mut q = world.query_filtered::<
        (Entity, &Named, Option<&HateList>, Option<&MobMemory>),
        (With<mud_world::Mob>, Or<(With<HateList>, With<MobMemory>)>),
    >();
    for (e, n, hate, mem) in q.iter(world) {
        // An invisible (or wizinvis) mob stays nameless to the viewer.
        if !super::can_see_player(world, target, e) {
            continue;
        }
        if hate.is_some_and(|h| h.0.contains(&target)) {
            hating.push(strip(&n.name));
        } else if mem.is_some_and(|m| m.0.contains(&target)) {
            remembering.push(strip(&n.name));
        }
    }
    // Stable order so an unchanged set hashes identically.
    hating.sort();
    remembering.sort();
    json!({ "hating": hating, "remembering": remembering }).to_string()
}
