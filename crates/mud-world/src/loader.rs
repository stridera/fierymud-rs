use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::{effects, mobs, objects, room_exits, rooms, sqlx::PgPool, zones};
use tracing::{info, warn};

use crate::components::{ExitData, Exits, Located, Named, Room, RoomSector, WorldKey, Zone};
use crate::resources::{EffectCatalog, EffectDef, ObjectProto, ObjectPrototypes, WorldKeyIndex};

#[derive(Debug, Default, Clone, Copy)]
pub struct LoadStats {
    pub zones: usize,
    pub rooms: usize,
    pub exits_resolved: usize,
    pub exits_dangling: usize,
    pub mobs_listed: usize,
    pub objects_listed: usize,
    pub effects_listed: usize,
}

/// Load the persistent world from the database into the ECS World:
///   pass 1: spawn zone entities
///   pass 2: spawn room entities, attach Located(zone), Sector, empty Exits
///   pass 3: resolve room-to-room exits and populate the Exits component
///   pass 4: just count mobs/objects/effects so we know they loaded — actual
///           prototype caching comes when the spawner needs it.
pub async fn load_from_db(world: &mut World, pool: &PgPool) -> sqlx::Result<LoadStats> {
    let mut stats = LoadStats::default();

    // Pass 1: zones.
    let zone_rows = zones::list_zones(pool).await?;
    let mut zone_index: HashMap<i32, Entity> = HashMap::with_capacity(zone_rows.len());
    for z in &zone_rows {
        let entity = world
            .spawn((
                Zone,
                WorldKey {
                    zone: z.id,
                    id: 0,
                },
                Named {
                    name: z.name.clone(),
                },
            ))
            .id();
        zone_index.insert(z.id, entity);
    }
    stats.zones = zone_index.len();

    // Pass 2: rooms.
    let room_rows = rooms::list_rooms(pool).await?;
    let mut room_index: HashMap<(i32, i32), Entity> = HashMap::with_capacity(room_rows.len());
    for r in &room_rows {
        let zone_entity = match zone_index.get(&r.zone_id) {
            Some(&e) => e,
            None => {
                warn!(
                    zone_id = r.zone_id,
                    room_id = r.id,
                    "room references missing zone; skipping"
                );
                continue;
            }
        };
        let entity = world
            .spawn((
                Room,
                WorldKey {
                    zone: r.zone_id,
                    id: r.id,
                },
                Named {
                    name: r.name.clone(),
                },
                Located(zone_entity),
                RoomSector(r.sector),
                Exits::default(),
            ))
            .id();
        room_index.insert((r.zone_id, r.id), entity);
    }
    stats.rooms = room_index.len();

    // Pass 3: resolve exits and attach to source rooms.
    let exit_rows = room_exits::list_exits(pool).await?;
    for e in exit_rows {
        let Some(&source) = room_index.get(&(e.room_zone_id, e.room_id)) else {
            continue;
        };
        let target = match (e.to_zone_id, e.to_room_id) {
            (Some(tz), Some(tr)) => room_index.get(&(tz, tr)).copied(),
            _ => None,
        };
        if target.is_some() {
            stats.exits_resolved += 1;
        } else {
            stats.exits_dangling += 1;
        }
        if let Some(mut exits) = world.get_mut::<Exits>(source) {
            exits.0.insert(
                e.direction,
                ExitData {
                    to: target,
                    state: e.default_state,
                },
            );
        }
    }

    // Pass 4: catalog data. Mob prototype caching still deferred. Object
    // and Effect catalogs become Resources since commands look them up
    // by name/key.
    stats.mobs_listed = mobs::list_mobs(pool).await?.len();

    let object_rows = objects::list_objects(pool).await?;
    let mut object_prototypes = ObjectPrototypes::default();
    for row in object_rows {
        object_prototypes.by_key.insert(
            (row.zone_id, row.id),
            ObjectProto {
                zone_id: row.zone_id,
                id: row.id,
                r#type: row.r#type,
                name: row.name,
                keywords: row.keywords,
                weight: row.weight,
                level: row.level,
            },
        );
    }
    stats.objects_listed = object_prototypes.by_key.len();

    let effect_rows = effects::list_effects(pool).await?;
    let mut effect_catalog = EffectCatalog::default();
    for row in effect_rows {
        effect_catalog.by_id.insert(
            row.id,
            EffectDef {
                id: row.id,
                name: row.name,
                description: row.description,
                effect_type: row.effect_type,
                tags: row.tags,
                presence_override: row.presence_override,
            },
        );
    }
    stats.effects_listed = effect_catalog.by_id.len();

    world.insert_resource(WorldKeyIndex {
        zones: zone_index,
        rooms: room_index,
    });
    world.insert_resource(object_prototypes);
    world.insert_resource(effect_catalog);

    info!(
        zones = stats.zones,
        rooms = stats.rooms,
        exits_resolved = stats.exits_resolved,
        exits_dangling = stats.exits_dangling,
        mobs = stats.mobs_listed,
        objects = stats.objects_listed,
        effects = stats.effects_listed,
        "world loaded"
    );

    Ok(stats)
}
