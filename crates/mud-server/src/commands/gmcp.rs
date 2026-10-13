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
    AccountWealth, BankWealth, EffectInstance, EffectSource, Exits, Fighting, Located, Named,
    Profile, RoomSector, SpellSlotData, SpellSlots, Wealth, WorldKey, WorldKeyIndex,
};
use serde_json::{Value, json};

use super::{
    ColorMode, Connection, Description, direction_name, exit_is_hidden_to, render_color_tags,
    room_composite_num, room_line, sector_label,
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
/// `look` uses for its pitch-black gate; a blind viewer sees no room.
pub(crate) fn viewer_sees_room(world: &mut World, viewer: Entity, room: Entity) -> bool {
    if super::senses::is_blind(world, viewer) {
        return false;
    }
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
/// (magic invisibility, `WizInvis`; `HOLY_LIGHT` pierces) first,
/// then darkness, where only infravision still gives a red shape.
/// A group mate is always `Clear`, in the dark or blind (legacy
/// `CAN_SEE` passes `IS_IN_GROUP` before light and blindness).
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
    } else if room_seen || (viewer != target && crate::hiding::same_group(world, viewer, target)) {
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
    let mut here = crate::room_index::players_in(world, room);
    here.retain(|&e| e != viewer);
    // Posture and hold ride along for a clearly seen player, from the same
    // rules as the text room line (`room_line`); a shape in the dark gets
    // the generic label only.
    let mut rows: Vec<(String, Value)> = here
        .into_iter()
        .filter_map(|e| {
            let how = perceives(world, viewer, e, room_seen);
            let name = perceived_name(world, e, how)?;
            let mut entry = json!({ "name": name, "full_name": name });
            if how == Perceived::Clear {
                entry["posture"] = json!(room_line::posture_word(world, e));
                if let Some(hold) = room_line::hold_of(world, e) {
                    entry["status"] = json!("stunned");
                    entry["hold"] = json!(hold.status());
                }
            }
            Some((name, entry))
        })
        .collect();
    // ECS query order shifts whenever an unrelated component is added or
    // removed, which would change the hash and resend an identical list.
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Value::Array(rows.into_iter().map(|(_, v)| v).collect()).to_string()
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
                // Full state, open doors included: `doors` above only lists
                // the ones that block you.
                detail.insert("door_state".into(), json!(door_state.unwrap_or("open")));
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

/// The `Char.Effects` array for `target`: one entry per active effect
/// attached to the player. `id` is the stable machine key
/// ([`super::info::effect_id`], lowercased `plain_name`, e.g. `stone_skin`),
/// `name` the effect's player-facing label
/// ([`super::info::effect_label`]), `ability` the originating spell's
/// display name ("" when none), `duration` seconds remaining
/// (-1 = permanent), `source` the high-level origin tag.
pub(crate) fn build_char_effects(world: &mut World, target: Entity) -> String {
    let rows: Vec<EffectInstance> = mud_world::effects_on(world, target)
        .into_iter()
        .filter_map(|e| world.get::<EffectInstance>(e).cloned())
        .collect();
    // One entry per effect, as the `effects` command lists them: instances
    // of the same ability under the same label (Statue's several parts) fold
    // into the first, which keeps its place and lasts as long as the longest
    // part (permanent if any part is).
    let mut grouped: Vec<EffectInstance> = Vec::with_capacity(rows.len());
    for inst in rows {
        let same = inst.ability_id.and_then(|id| {
            grouped.iter().position(|g| {
                g.ability_id == Some(id)
                    && super::info::effect_id(world, g) == super::info::effect_id(world, &inst)
                    && super::info::effect_label(world, g)
                        == super::info::effect_label(world, &inst)
            })
        });
        match same {
            Some(i) => {
                let into = &mut grouped[i];
                into.remaining_secs = if into.remaining_secs < 0 || inst.remaining_secs < 0 {
                    -1
                } else {
                    into.remaining_secs.max(inst.remaining_secs)
                };
            }
            None => grouped.push(inst),
        }
    }
    let entries: Vec<Value> = grouped
        .into_iter()
        .map(|inst| {
            let ability = inst
                .ability_id
                .and_then(|id| super::info::ability_label_by_id(world, id))
                .unwrap_or_default();
            let source = match &inst.source {
                EffectSource::Spell => "spell",
                EffectSource::Item => "item",
                EffectSource::Room => "room",
                EffectSource::Admin => "admin",
                EffectSource::Other(_) => "other",
            };
            json!({
                "id": super::info::effect_id(world, &inst),
                "name": super::info::effect_label(world, &inst),
                "ability": ability,
                // Seconds left; -1 marks a permanent effect (kept numeric
                // for the client, which tests `< 0`).
                "duration": inst.remaining_secs,
                "source": source,
                "strength": inst.strength,
            })
        })
        .collect();
    Value::Array(entries).to_string()
}

type EffectKey = (i32, Option<i32>, i32, u8);

fn effect_key(inst: &EffectInstance) -> EffectKey {
    let source = match &inst.source {
        EffectSource::Spell => 0u8,
        EffectSource::Item => 1,
        EffectSource::Room => 2,
        EffectSource::Admin => 3,
        EffectSource::Other(_) => 4,
    };
    (inst.kind, inst.ability_id, inst.strength, source)
}

/// Keys of the effects currently on `target`, via the bearer index (cost is
/// the target's own effects, not the world's).
fn effect_keys_of(world: &mut World, target: Entity) -> Vec<EffectKey> {
    mud_world::effects_on(world, target)
        .into_iter()
        .filter_map(|e| world.get::<EffectInstance>(e).map(effect_key))
        .collect()
}

fn hash_effect_keys(mut keys: Vec<EffectKey>) -> u64 {
    keys.sort_unstable();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    keys.hash(&mut h);
    h.finish()
}

/// Send `Char.Effects` (change-gated on the payload) and remember which
/// effect *set* it showed, so [`push_effect_changes`] has a baseline to
/// compare against. Without the baseline, an effect added or removed
/// (`cancel`, dispel, a strip) between this send and the sweep's first look
/// at the player would go unnoticed: the sweep would record the new set as
/// its starting point and the client would keep showing the old one.
pub(crate) fn send_char_effects(world: &mut World, target: Entity) {
    let payload = build_char_effects(world, target);
    send_if_changed(world, target, "Char.Effects", &payload, false);
    let hash = hash_effect_keys(effect_keys_of(world, target));
    if let Some(mut sent) = world.get_mut::<GmcpSent>(target) {
        sent.0.insert(EFFECT_SET_KEY, hash);
    }
}

/// Push `Char.Effects` the moment a connected player's effect set changes
/// (an effect added, removed or restacked), rather than waiting for their
/// next prompt: silent buffs and expiries would otherwise leave a client's
/// icon bar stale until the player happens to receive output. Every removal
/// path (natural expiry, `cancel`, dispel, a combat break, a strip) is
/// covered because they all change the instance set this looks at. Duration
/// countdowns are deliberately not "changes" here; the prompt path still
/// refreshes those. Players that have not yet been sent a full
/// `Char.Effects` (login in progress) are only tracked, never pushed to.
pub(crate) fn push_effect_changes(world: &mut World) {
    let mut players = world.query_filtered::<Entity, (With<Connection>, With<GmcpSent>)>();
    let players: Vec<Entity> = players.iter(world).collect();
    let mut todo: Vec<Entity> = Vec::new();
    for p in players {
        let hash = hash_effect_keys(effect_keys_of(world, p));
        let Some(mut sent) = world.get_mut::<GmcpSent>(p) else {
            continue;
        };
        if !sent.0.contains_key("Char.Effects") {
            continue;
        }
        let prev = sent.0.insert(EFFECT_SET_KEY, hash);
        if prev.is_some_and(|prev| prev != hash) {
            todo.push(p);
        }
    }
    for p in todo {
        send_char_effects(world, p);
    }
}

/// Clear the client's `Char.Combat` panel the moment a connected player stops
/// fighting, whatever ended the fight (paralysis or mesmerize on the target, a
/// groupmate's spell, the opponent fleeing or being called away). Many of
/// those paths remove `Fighting` silently and the player may receive no
/// output, hence no prompt, so without this sweep the panel would linger.
/// Players still fighting are left to the prompt path; the frame is
/// change-gated, so an already-cleared panel costs nothing.
pub(crate) fn push_combat_ends(world: &mut World) {
    let mut q =
        world.query_filtered::<(Entity, &GmcpSent), (With<Connection>, Without<Fighting>)>();
    let todo: Vec<Entity> = q
        .iter(world)
        .filter(|(_, sent)| sent.0.contains_key("Char.Combat"))
        .map(|(e, _)| e)
        .collect();
    for p in todo {
        super::send_char_combat(world, p);
    }
}

/// Copper split into the four denominations, as legacy `Char.Worth` did.
fn coins_json(copper: i64) -> Value {
    let c = crate::prompt::Coins::from_copper(copper);
    json!({
        "platinum": c.platinum,
        "gold": c.gold,
        "silver": c.silver,
        "copper": c.copper,
    })
}

/// The `Char.Worth` payload, legacy shape (`comm.cpp` `send_gmcp_prompt`):
/// `Carried` is the coin on hand, `Bank` the character's own bank balance,
/// each split into platinum/gold/silver/copper. `Account` (not in legacy)
/// is the account-shared bank pool, present only for characters on an
/// account; it is a separate balance, so it is not folded into `Bank`.
pub(crate) fn build_char_worth(world: &World, target: Entity) -> String {
    let mut worth = json!({
        "Carried": coins_json(world.get::<Wealth>(target).map_or(0, |w| w.0)),
        "Bank": coins_json(world.get::<BankWealth>(target).map_or(0, |w| w.0)),
    });
    if let Some(account) = world.get::<AccountWealth>(target) {
        worth["Account"] = coins_json(account.0);
    }
    worth.to_string()
}

/// Highest spell circle reported in `Char.SpellSlots` (legacy
/// `NUM_SPELL_CIRCLES`).
const SPELL_CIRCLES: usize = 14;

/// The `Char.SpellSlots` payload, legacy shape (`get_spell_slots_available`):
/// a flat array of free slots, index 0 = circle 1 through index 13 =
/// circle 14. A circle the class cannot reach yet reads 0.
pub(crate) fn build_char_spell_slots(world: &World, target: Entity) -> String {
    let mut free = [0i32; SPELL_CIRCLES];
    let class_id = world.get::<Profile>(target).and_then(|p| p.class_id);
    if let (Some(class_id), Some(data)) = (class_id, world.get_resource::<SpellSlotData>()) {
        let level = mud_world::effective_level(world, target);
        let slots = world.get::<SpellSlots>(target);
        for (circle, max) in data.slots_for(class_id, level) {
            let Some(i) = usize::try_from(circle - 1)
                .ok()
                .filter(|i| *i < SPELL_CIRCLES)
            else {
                continue;
            };
            let used = slots.map_or(0, |s| s.used_in_circle(circle));
            free[i] = (max - used).max(0);
        }
    }
    json!(free).to_string()
}

/// Send `Char.Worth` and `Char.SpellSlots` for a player, each only when
/// its content changed since the last send. Classless players still
/// get an all-zero `SpellSlots`.
pub(crate) fn send_char_worth_and_slots(world: &mut World, target: Entity) {
    let worth = build_char_worth(world, target);
    send_if_changed(world, target, "Char.Worth", &worth, false);
    if world.get::<Profile>(target).is_some() {
        let slots = build_char_spell_slots(world, target);
        send_if_changed(world, target, "Char.SpellSlots", &slots, false);
    }
}

/// `GmcpSent` key holding the hash of the effect *set* (not durations).
const EFFECT_SET_KEY: &str = "Char.Effects.set";

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
