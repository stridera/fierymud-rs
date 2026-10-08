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
use std::path::{Path, PathBuf};

const CORPSE_SNAPSHOT_PATH: &str = "state/corpses.json";

/// Overrides where the corpse snapshot lives (tests point it at a
/// scratch dir); absent means [`CORPSE_SNAPSHOT_PATH`].
#[derive(Resource, Debug, Clone)]
pub(crate) struct CorpseSnapshotPath(pub(crate) PathBuf);

fn snapshot_path(world: &World) -> PathBuf {
    world
        .get_resource::<CorpseSnapshotPath>()
        .map_or_else(|| PathBuf::from(CORPSE_SNAPSHOT_PATH), |p| p.0.clone())
}

/// `<path>.loaded`: where a snapshot is moved once it has been restored.
fn loaded_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".loaded");
    PathBuf::from(name)
}

/// Write `bytes` to `path` atomically: write a sibling temp file, fsync
/// it, then rename over `path`, so a crash leaves either the old or the
/// new snapshot and never a torn one. The temp file is removed on error.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    // Best effort: make the rename itself durable.
    if let Some(parent) = path.parent()
        && let Ok(dir) = std::fs::File::open(if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        })
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Persist the corpse snapshot, then the player's own save. Call after
/// anything that moves items or coin between a player and a player
/// corpse (death, looting). The order matters: the corpse file lands
/// first, so a crash between the two writes can at worst duplicate
/// gear, never lose it.
pub(crate) fn persist_after_change(world: &mut World, player: Entity) {
    save_snapshot(world);
    crate::quest_progress::save_player_soon(world, player);
}

/// `(item count, coin)` held by `container`: lets callers detect
/// whether a looting command actually changed a corpse's contents.
pub(crate) fn contents_fingerprint(world: &mut World, container: Entity) -> (usize, i64) {
    let items = {
        let mut q = world.query_filtered::<&Located, With<Item>>();
        q.iter(world).filter(|l| l.0 == container).count()
    };
    let coin = world
        .get::<mud_world::CoinPile>(container)
        .map_or(0, |p| p.0);
    (items, coin)
}

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
    /// `ANIMATE_DEAD` survive a restart. Defaults to false for
    /// backward-compat with pre-marker snapshots.
    #[serde(default)]
    is_player: bool,
    /// Dead actor's level at spawn time — read by `ANIMATE_DEAD`'s
    /// HP-scaling pass. Defaults to 1 on legacy snapshots (matches
    /// the lowest spawn tier, so old corpses still raise a basic
    /// skeleton without crashing).
    #[serde(default = "default_origin_level")]
    origin_level: i32,
    /// Coin (copper) lying in the corpse's `CoinPile`: a dead player's
    /// purse, or a mob's leftover loot. Defaults to 0 for older snapshots.
    #[serde(default)]
    coins: i64,
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
    let path = snapshot_path(world);
    save_snapshot_to(world, &path);
}

fn save_snapshot_to(world: &mut World, path: &Path) {
    let mut snapshots: Vec<CorpseSnapshot> = Vec::new();
    // Snapshot corpses first, holding their entity IDs so we can
    // do the contents lookup outside the borrow.
    #[allow(clippy::type_complexity)]
    let corpses: Vec<(Entity, String, Vec<String>, Entity, i32, bool, i32)> = {
        let mut q = world.query_filtered::<(
            Entity,
            &Named,
            &Keywords,
            &Located,
            &CorpseDecay,
            Option<&mud_world::PlayerCorpse>,
            Option<&mud_world::CorpseOriginLevel>,
        ), With<Corpse>>();
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
                .map(|(_, wk)| ContentSnapshot {
                    proto_zone: wk.zone,
                    proto_id: wk.id,
                })
                .collect()
        };
        let coins = world
            .get::<mud_world::CoinPile>(corpse)
            .map_or(0, |p| p.0.max(0));
        snapshots.push(CorpseSnapshot {
            name,
            keywords,
            room_zone: room_key.zone,
            room_id: room_key.id,
            decay_secs,
            contents,
            is_player,
            origin_level,
            coins,
        });
    }
    if snapshots.is_empty() {
        // Clear any stale snapshot file — we don't want yesterday's
        // corpses re-spawning the next time someone dies and saves.
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Some(parent) = path.parent()
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
    if let Err(e) = write_atomic(path, &bytes) {
        tracing::warn!(error = %e, "corpse snapshot write failed");
        return;
    }
    tracing::info!(count, path = %path.display(), "corpse snapshot saved");
}

/// Recreate any persisted corpses after the world has finished
/// loading. Skips corpses whose room or item prototypes have been
/// removed from the schema since the snapshot was written.
///
/// A restored snapshot is renamed to `corpses.json.loaded` so a crash
/// after boot can never reload (and so duplicate) the same corpses;
/// the live world is then written straight back out as a fresh
/// `corpses.json` so those corpses survive that crash too.
pub fn load_snapshot(world: &mut World) {
    let path = snapshot_path(world);
    if load_snapshot_from(world, &path) {
        save_snapshot_to(world, &path);
    }
}

/// Load `path` and retire it to `<path>.loaded`. Returns whether a
/// snapshot was consumed.
fn load_snapshot_from(world: &mut World, path: &Path) -> bool {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(e) => {
            tracing::warn!(error = %e, "corpse snapshot read failed");
            return false;
        }
    };
    let file: SnapshotFile = match serde_json::from_slice(&bytes) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "corpse snapshot parse failed");
            return false;
        }
    };
    let mut restored_corpses = 0;
    let mut restored_items = 0;
    let mut skipped_rooms = 0;
    let mut skipped_protos = 0;
    for snap in file.corpses {
        let Some(room_entity) = world
            .resource::<WorldKeyIndex>()
            .rooms
            .get(&(snap.room_zone, snap.room_id))
            .copied()
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
                CorpseDecay {
                    remaining_secs: snap.decay_secs.max(1),
                },
            ))
            .id();
        if let Ok(mut em) = world.get_entity_mut(corpse) {
            if snap.is_player {
                em.insert(mud_world::PlayerCorpse);
            }
            em.insert(mud_world::CorpseOriginLevel(snap.origin_level.max(1)));
            if snap.coins > 0 {
                em.insert(mud_world::CoinPile(snap.coins));
            }
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
        path = %path.display(),
        "corpse snapshot loaded",
    );
    // Retire the file so it can only ever be loaded once. If the rename
    // fails, delete it: a stale snapshot is worse than a missing one.
    if let Err(e) = std::fs::rename(path, loaded_path(path)) {
        tracing::warn!(error = %e, "couldn't retire corpse snapshot; removing it");
        let _ = std::fs::remove_file(path);
    }
    true
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
        Named {
            name: proto.name.clone(),
        },
        Keywords(proto.keywords.clone()),
        WorldKey {
            zone: proto.zone_id,
            id: proto.id,
        },
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
    crate::item_decay::attach_timer_if_decaying(world, item_entity, &proto);
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mud_world::{PlayerCorpse, Room};

    /// Fresh scratch dir under the workspace `target/` (gitignored).
    pub(crate) fn scratch_dir(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/corpse-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn keyed_room(world: &mut World, zone: i32, id: i32) -> Entity {
        let room = world.spawn((Room, WorldKey { zone, id })).id();
        world
            .resource_mut::<WorldKeyIndex>()
            .rooms
            .insert((zone, id), room);
        room
    }

    fn corpse_world() -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(WorldKeyIndex::default());
        let room = keyed_room(&mut world, 30, 45);
        (world, room)
    }

    fn spawn_player_corpse(world: &mut World, room: Entity, coins: i64) -> Entity {
        let corpse = world
            .spawn((
                Item,
                Corpse,
                PlayerCorpse,
                Named {
                    name: "the corpse of Bob".into(),
                },
                Keywords(vec!["corpse".into(), "bob".into()]),
                Located(room),
                CorpseDecay {
                    remaining_secs: 500,
                },
            ))
            .id();
        if coins > 0 {
            world.entity_mut(corpse).insert(mud_world::CoinPile(coins));
        }
        corpse
    }

    #[test]
    fn save_then_load_restores_coins_and_load_retires_the_file() {
        let dir = scratch_dir("load_once");
        let path = dir.join("corpses.json");
        let (mut world, room) = corpse_world();
        spawn_player_corpse(&mut world, room, 4321);
        save_snapshot_to(&mut world, &path);
        let text = std::fs::read_to_string(&path).expect("snapshot written");
        assert!(text.contains("the corpse of Bob") && text.contains("4321"));

        let (mut fresh, _room) = corpse_world();
        assert!(load_snapshot_from(&mut fresh, &path));
        let pile = fresh
            .query_filtered::<&mud_world::CoinPile, With<PlayerCorpse>>()
            .single(&fresh)
            .map(|p| p.0);
        assert_eq!(pile.ok(), Some(4321));
        assert!(!path.exists(), "snapshot retired after load");
        assert!(loaded_path(&path).exists(), "kept as corpses.json.loaded");

        // Second load finds nothing, so nothing is duplicated.
        let (mut again, _room) = corpse_world();
        assert!(!load_snapshot_from(&mut again, &path));
        assert_eq!(
            again
                .query_filtered::<Entity, With<Corpse>>()
                .iter(&again)
                .count(),
            0
        );
    }

    #[test]
    fn public_load_rewrites_a_fresh_snapshot_of_the_restored_corpses() {
        let dir = scratch_dir("load_rewrites");
        let path = dir.join("corpses.json");
        let (mut world, room) = corpse_world();
        spawn_player_corpse(&mut world, room, 10);
        save_snapshot_to(&mut world, &path);

        let (mut fresh, _room) = corpse_world();
        fresh.insert_resource(CorpseSnapshotPath(path.clone()));
        load_snapshot(&mut fresh);
        let text = std::fs::read_to_string(&path).expect("fresh snapshot");
        assert!(text.contains("the corpse of Bob"));
        assert!(loaded_path(&path).exists());
    }

    #[test]
    fn atomic_write_replaces_the_file_and_leaves_no_temp() {
        let dir = scratch_dir("atomic_ok");
        let path = dir.join("corpses.json");
        write_atomic(&path, b"old").expect("first write");
        write_atomic(&path, b"new").expect("second write");
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("corpses.json")]);
    }

    #[test]
    fn atomic_write_failure_keeps_the_old_file_and_cleans_the_temp() {
        let dir = scratch_dir("atomic_fail");
        // The rename target is a non-empty directory, so the final
        // rename fails after the temp file was fully written.
        let path = dir.join("corpses.json");
        std::fs::create_dir_all(path.join("blocker")).unwrap();
        assert!(write_atomic(&path, b"data").is_err());
        assert!(!dir.join("corpses.json.tmp").exists(), "temp removed");
        assert!(path.is_dir(), "target untouched");
        // A missing parent dir fails at create and leaves nothing either.
        let missing = dir.join("nope/corpses.json");
        assert!(write_atomic(&missing, b"data").is_err());
        assert!(!dir.join("nope").exists());
    }

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
                    ContentSnapshot {
                        proto_zone: 12,
                        proto_id: 7,
                    },
                    ContentSnapshot {
                        proto_zone: 12,
                        proto_id: 8,
                    },
                ],
                is_player: true,
                origin_level: 47,
                coins: 1234,
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
        assert_eq!(c.coins, 1234);
        assert_eq!(c.contents.len(), 2);
        assert_eq!((c.contents[0].proto_zone, c.contents[0].proto_id), (12, 7));
    }

    #[test]
    fn snapshot_without_coins_field_defaults_to_zero() {
        let parsed: SnapshotFile = serde_json::from_str(
            r#"{"corpses":[{"name":"x","keywords":[],"room_zone":1,"room_id":2,
                "decay_secs":5,"contents":[]}]}"#,
        )
        .expect("parse");
        assert_eq!(parsed.corpses[0].coins, 0);
    }

    #[test]
    fn empty_snapshot_parses_clean() {
        // Forward-compat: a fresh boot with an empty array shouldn't
        // crash, and the load path should treat it the same as a
        // missing file.
        let parsed: SnapshotFile = serde_json::from_str(r#"{"corpses":[]}"#).expect("parse");
        assert!(parsed.corpses.is_empty());
    }
}
