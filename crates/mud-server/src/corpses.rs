//! Corpse persistence. Corpse entities are spawned by the death
//! handler with a 600-second `CorpseDecay`; without a snapshot a
//! Ctrl-C between death and looting silently destroyed everything
//! a player was carrying. Round-trip the corpse marker plus the
//! list of contained item proto `WorldKeys`, and recreate them on
//! the next boot using the same prototype-spawn path the world
//! loader and respawn tick share.
//!
//! Lossy by design: per-instance state (charges, light fuel
//! remaining, container contents-of-contents) resets to the
//! prototype's defaults. The visible "corpse with the right loot
//! in it" survives; a worn-down torch comes back fresh. That's a
//! reasonable trade for first-pass persistence.

use bevy_ecs::prelude::*;
use mud_world::{
    AttachedTriggers, Corpse, CorpseDecay, Description, Item, Keywords, LiquidContainer, Located,
    Named, ObjectPrototypes, TriggerCatalog, WorldKey, WorldKeyIndex,
};
use serde::{Deserialize, Serialize};

const CORPSE_SNAPSHOT_PATH: &str = "state/corpses.json";

#[derive(Serialize, Deserialize, Debug, Clone)]
struct CorpseSnapshot {
    name: String,
    keywords: Vec<String>,
    /// Composite key of the room the corpse rests in. Looked up
    /// against `WorldKeyIndex.rooms` on load — corpses in rooms
    /// the new content snapshot has dropped are silently skipped.
    room_zone: i32,
    room_id: i32,
    /// Seconds left on the decay timer. Snapshotted as-is — the
    /// timer pauses while the server is offline, which is the
    /// player-friendly behavior.
    decay_secs: i32,
    /// Each entry is a prototype `(zone_id, id)` for an item that
    /// was inside the corpse. `EquippedSlot` is dropped on death
    /// already, so only the proto identity matters here.
    contents: Vec<ContentSnapshot>,
    /// True for player corpses (the ones carrying a `PlayerCorpse`
    /// marker). Saved/restored so the consent gates on looting and
    /// ANIMATE_DEAD survive a restart. Defaults to false for
    /// backward-compat with pre-marker snapshots.
    #[serde(default)]
    is_player: bool,
    /// Dead actor's level at spawn time — read by ANIMATE_DEAD's
    /// HP-scaling pass. Defaults to 1 on legacy snapshots (matches
    /// the lowest spawn tier, so old corpses still raise a basic
    /// skeleton without crashing).
    #[serde(default = "default_origin_level")]
    origin_level: i32,
}

fn default_origin_level() -> i32 {
    1
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ContentSnapshot {
    proto_zone: i32,
    proto_id: i32,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct SnapshotFile {
    corpses: Vec<CorpseSnapshot>,
}

/// Snapshot every Corpse entity to disk on graceful shutdown.
/// Invoked alongside the weather and clock snapshots in main.
pub fn save_snapshot(world: &mut World) {
    let mut snapshots: Vec<CorpseSnapshot> = Vec::new();
    // Snapshot corpses first, holding their entity IDs so we can
    // do the contents lookup outside the borrow.
    let corpses: Vec<(Entity, String, Vec<String>, Entity, i32, bool, i32)> = {
        let mut q = world
            .query_filtered::<(Entity, &Named, &Keywords, &Located, &CorpseDecay, Option<&mud_world::PlayerCorpse>, Option<&mud_world::CorpseOriginLevel>), With<Corpse>>();
        q.iter(world)
            .map(|(e, n, k, l, d, pc, ol)| {
                (
                    e,
                    n.name.clone(),
                    k.0.clone(),
                    l.0,
                    d.remaining_secs,
                    pc.is_some(),
                    ol.map_or(1, |o| o.0),
                )
            })
            .collect()
    };
    for (corpse, name, keywords, room, decay_secs, is_player, origin_level) in corpses {
        let Some(room_key) = world.get::<WorldKey>(room).copied() else {
            // Unkeyed room (synthetic / housing) — can't round-trip.
            continue;
        };
        let contents: Vec<ContentSnapshot> = {
            let mut q = world.query_filtered::<(&Located, &WorldKey), With<Item>>();
            q.iter(world)
                .filter(|(l, _)| l.0 == corpse)
                .map(|(_, wk)| ContentSnapshot { proto_zone: wk.zone, proto_id: wk.id })
                .collect()
        };
        snapshots.push(CorpseSnapshot {
            name,
            keywords,
            room_zone: room_key.zone,
            room_id: room_key.id,
            decay_secs,
            contents,
            is_player,
            origin_level,
        });
    }
    if snapshots.is_empty() {
        // Clear any stale snapshot file — we don't want yesterday's
        // corpses re-spawning the next time someone dies and saves.
        let _ = std::fs::remove_file(CORPSE_SNAPSHOT_PATH);
        return;
    }
    if let Some(parent) = std::path::Path::new(CORPSE_SNAPSHOT_PATH).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(error = %e, "couldn't create corpse snapshot dir");
        return;
    }
    let count = snapshots.len();
    let file = SnapshotFile { corpses: snapshots };
    let bytes = match serde_json::to_vec_pretty(&file) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "corpse snapshot serialize failed");
            return;
        }
    };
    if let Err(e) = std::fs::write(CORPSE_SNAPSHOT_PATH, bytes) {
        tracing::warn!(error = %e, "corpse snapshot write failed");
        return;
    }
    tracing::info!(count, path = %CORPSE_SNAPSHOT_PATH, "corpse snapshot saved");
}

/// Recreate any persisted corpses after the world has finished
/// loading. Skips corpses whose room or item prototypes have been
/// removed from the schema since the snapshot was written.
pub fn load_snapshot(world: &mut World) {
    let bytes = match std::fs::read(CORPSE_SNAPSHOT_PATH) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(error = %e, "corpse snapshot read failed");
            return;
        }
    };
    let file: SnapshotFile = match serde_json::from_slice(&bytes) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "corpse snapshot parse failed");
            return;
        }
    };
    let mut restored_corpses = 0;
    let mut restored_items = 0;
    let mut skipped_rooms = 0;
    let mut skipped_protos = 0;
    for snap in file.corpses {
        let Some(room_entity) =
            world.resource::<WorldKeyIndex>().rooms.get(&(snap.room_zone, snap.room_id)).copied()
        else {
            skipped_rooms += 1;
            continue;
        };
        let corpse = world
            .spawn((
                Item,
                Corpse,
                Named { name: snap.name },
                Keywords(snap.keywords),
                Located(room_entity),
                CorpseDecay { remaining_secs: snap.decay_secs.max(1) },
            ))
            .id();
        if let Ok(mut em) = world.get_entity_mut(corpse) {
            if snap.is_player {
                em.insert(mud_world::PlayerCorpse);
            }
            em.insert(mud_world::CorpseOriginLevel(snap.origin_level.max(1)));
        }
        for c in snap.contents {
            if !spawn_item_into(world, c.proto_zone, c.proto_id, corpse) {
                skipped_protos += 1;
                continue;
            }
            restored_items += 1;
        }
        restored_corpses += 1;
    }
    tracing::info!(
        restored_corpses,
        restored_items,
        skipped_rooms,
        skipped_protos,
        path = %CORPSE_SNAPSHOT_PATH,
        "corpse snapshot loaded",
    );
}

/// Spawn an item from its prototype directly into `parent`. Mirrors
/// the bundle the respawn tick assembles, minus `FromObjectReset`
/// (corpse contents aren't tracked by the reset system) and the
/// `WearableIn` slot (looted items get their slot reattached when
/// a player wears them). Returns false if the proto is unknown.
fn spawn_item_into(world: &mut World, proto_zone: i32, proto_id: i32, parent: Entity) -> bool {
    let proto_key = (proto_zone, proto_id);
    let proto = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&proto_key)
        .cloned();
    let Some(proto) = proto else {
        return false;
    };
    let trigger_keys = world
        .resource::<TriggerCatalog>()
        .object_attachments
        .get(&proto_key)
        .cloned();
    let primary_slot = mud_world::wear_flags_primary_slot(&proto.wear_flags);
    let mut bundle = world.spawn((
        Item,
        Named { name: proto.name.clone() },
        Keywords(proto.keywords.clone()),
        WorldKey { zone: proto.zone_id, id: proto.id },
        Located(parent),
    ));
    if let Some(desc) = proto.examine_description.clone() {
        bundle.insert(Description(desc));
    }
    if let Some(s) = primary_slot {
        bundle.insert(mud_world::WearableIn(s));
    }
    if let Some(board_id) = proto.board_id {
        bundle.insert(mud_world::BoardLink(board_id));
    }
    if let Some(liq) = proto.liquid.clone() {
        bundle.insert(LiquidContainer {
            liquid: liq.liquid,
            capacity: liq.capacity,
            remaining: liq.remaining,
            poisoned: liq.poisoned,
        });
    }
    if let Some(fuel) = proto.light_fuel {
        bundle.insert(mud_world::LightFuel {
            capacity: fuel.capacity,
            remaining: fuel.remaining,
        });
    }
    if let Some(keys) = trigger_keys {
        bundle.insert(AttachedTriggers(keys));
    }
    if !proto.flags.is_empty() {
        bundle.insert(mud_world::ObjectFlags(proto.flags.clone()));
    }
    if !proto.restrictions.is_empty() {
        bundle.insert(mud_world::ObjectRestrictions(proto.restrictions.clone()));
    }
    let item_entity = bundle.id();
    drop(bundle);
    crate::item_decay::attach_timer_if_decaying(world, item_entity, &proto);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_through_json() {
        let original = SnapshotFile {
            corpses: vec![CorpseSnapshot {
                name: "the corpse of Strider".into(),
                keywords: vec!["corpse".into(), "strider".into()],
                room_zone: 30,
                room_id: 45,
                decay_secs: 480,
                contents: vec![
                    ContentSnapshot { proto_zone: 12, proto_id: 7 },
                    ContentSnapshot { proto_zone: 12, proto_id: 8 },
                ],
                is_player: true,
                origin_level: 47,
            }],
        };
        let bytes = serde_json::to_vec_pretty(&original).expect("serialize");
        let parsed: SnapshotFile = serde_json::from_slice(&bytes).expect("parse");
        assert_eq!(parsed.corpses.len(), 1);
        let c = &parsed.corpses[0];
        assert_eq!(c.name, "the corpse of Strider");
        assert_eq!(c.keywords, vec!["corpse".to_string(), "strider".into()]);
        assert_eq!((c.room_zone, c.room_id), (30, 45));
        assert_eq!(c.decay_secs, 480);
        assert_eq!(c.contents.len(), 2);
        assert_eq!((c.contents[0].proto_zone, c.contents[0].proto_id), (12, 7));
    }

    #[test]
    fn empty_snapshot_parses_clean() {
        // Forward-compat: a fresh boot with an empty array shouldn't
        // crash, and the load path should treat it the same as a
        // missing file.
        let parsed: SnapshotFile =
            serde_json::from_str(r#"{"corpses":[]}"#).expect("parse");
        assert!(parsed.corpses.is_empty());
    }
}

