use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::{
    effects, mob_resets, mobs, object_resets, objects, room_exits, rooms, socials, sqlx::PgPool,
    zones,
};
use tracing::{info, warn};

use crate::components::{
    CombatStats, Description, ExitData, Exits, Health, Item, Keywords, Located, Mob, Named,
    Posture, PostureKind, Room, RoomSector, WorldKey, Zone,
};
use crate::resources::{
    EffectCatalog, EffectDef, MobProto, MobPrototypes, ObjectProto, ObjectPrototypes, SocialDef,
    SocialRegistry, WorldKeyIndex,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct LoadStats {
    pub zones: usize,
    pub rooms: usize,
    pub exits_resolved: usize,
    pub exits_dangling: usize,
    pub mobs_listed: usize,
    pub objects_listed: usize,
    pub effects_listed: usize,
    pub socials_listed: usize,
    /// Reset rows that successfully spawned a live entity into a room.
    pub mob_resets_spawned: usize,
    /// Reset rows whose target room or mob prototype was missing —
    /// counted so a stat regression flags a broken import.
    pub mob_resets_skipped: usize,
    pub object_resets_spawned: usize,
    pub object_resets_skipped: usize,
}

/// Load the persistent world from the database into the ECS World:
///   pass 1: spawn zone entities
///   pass 2: spawn room entities, attach Located(zone), Sector, empty Exits
///   pass 3: resolve room-to-room exits and populate the Exits component
///   pass 4: just count mobs/objects/effects so we know they loaded — actual
///           prototype caching comes when the spawner needs it.
// The structure is one long pass-by-pass loader; splitting into helpers would
// just hide the order. Long but linear.
#[allow(clippy::too_many_lines)]
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
        let Some(&zone_entity) = zone_index.get(&r.zone_id) else {
            warn!(
                zone_id = r.zone_id,
                room_id = r.id,
                "room references missing zone; skipping"
            );
            continue;
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
                Description(r.room_description.clone()),
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

    // Pass 4: catalog data. Mob, Object, Effect, and Social catalogs all
    // become Resources since commands look them up by name/key.
    let mob_rows = mobs::list_mobs(pool).await?;
    let mut mob_prototypes = MobPrototypes::default();
    for row in mob_rows {
        mob_prototypes.by_key.insert(
            (row.zone_id, row.id),
            MobProto {
                zone_id: row.zone_id,
                id: row.id,
                name: row.name,
                keywords: row.keywords,
                room_description: row.room_description,
                level: row.level,
                alignment: row.alignment,
                role: row.role,
                hp_dice_num: row.hp_dice_num,
                hp_dice_size: row.hp_dice_size,
                hp_dice_bonus: row.hp_dice_bonus,
                damage_dice_num: row.damage_dice_num,
                damage_dice_size: row.damage_dice_size,
                damage_dice_bonus: row.damage_dice_bonus,
                hit_roll: row.hit_roll,
                armor_class: row.armor_class,
            },
        );
    }
    stats.mobs_listed = mob_prototypes.by_key.len();

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
                room_description: row.room_description,
                examine_description: row.examine_description,
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

    let social_rows = socials::list_all(pool).await?;
    let mut social_registry = SocialRegistry::default();
    for row in social_rows {
        social_registry.by_name.insert(
            row.name.to_ascii_lowercase(),
            SocialDef {
                name: row.name,
                hide: row.hide,
                char_no_arg: row.char_no_arg,
                others_no_arg: row.others_no_arg,
                char_found: row.char_found,
                others_found: row.others_found,
                vict_found: row.vict_found,
                not_found: row.not_found,
                char_auto: row.char_auto,
                others_auto: row.others_auto,
            },
        );
    }
    stats.socials_listed = social_registry.by_name.len();

    world.insert_resource(WorldKeyIndex {
        zones: zone_index,
        rooms: room_index,
    });
    world.insert_resource(mob_prototypes);
    world.insert_resource(object_prototypes);
    world.insert_resource(effect_catalog);
    world.insert_resource(social_registry);

    // Pass 5: spawn live entities from MobResets / ObjectResets. Resources
    // were inserted above so the spawners can read them. Probability gating
    // and respawn timing belong to a future tick system; for now we always
    // spawn when probability is non-zero, capping at max_instances per
    // reset row (which today means 1 — multi-instance resets are rare).
    let mob_reset_rows = mob_resets::list_all(pool).await?;
    for r in &mob_reset_rows {
        if r.probability <= 0.0 {
            continue;
        }
        let proto = world
            .resource::<MobPrototypes>()
            .by_key
            .get(&(r.mob_zone_id, r.mob_id))
            .cloned();
        let room_entity = world
            .resource::<WorldKeyIndex>()
            .rooms
            .get(&(r.room_zone_id, r.room_id))
            .copied();
        let (Some(proto), Some(room_entity)) = (proto, room_entity) else {
            stats.mob_resets_skipped += 1;
            continue;
        };
        let hp = proto.rolled_hp();
        let dmg = proto.avg_damage();
        for _ in 0..r.max_instances.max(1) {
            world.spawn((
                Mob,
                Named { name: proto.name.clone() },
                Keywords(proto.keywords.clone()),
                Description(proto.room_description.clone()),
                WorldKey { zone: proto.zone_id, id: proto.id },
                Located(room_entity),
                Health { hp, max: hp },
                CombatStats {
                    hit_roll: proto.hit_roll,
                    dmg_roll: dmg,
                    ac: proto.armor_class,
                    alignment: proto.alignment,
                },
                Posture(PostureKind::Standing),
            ));
            stats.mob_resets_spawned += 1;
        }
    }

    let object_reset_rows = object_resets::list_all(pool).await?;
    for r in &object_reset_rows {
        if r.probability <= 0.0 {
            continue;
        }
        let proto = world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(r.object_zone_id, r.object_id))
            .cloned();
        let room_entity = world
            .resource::<WorldKeyIndex>()
            .rooms
            .get(&(r.room_zone_id, r.room_id))
            .copied();
        let (Some(proto), Some(room_entity)) = (proto, room_entity) else {
            stats.object_resets_skipped += 1;
            continue;
        };
        for _ in 0..r.max_instances.max(1) {
            let mut bundle = world.spawn((
                Item,
                Named { name: proto.name.clone() },
                Keywords(proto.keywords.clone()),
                WorldKey { zone: proto.zone_id, id: proto.id },
                Located(room_entity),
            ));
            if let Some(desc) = proto.examine_description.clone() {
                bundle.insert(Description(desc));
            }
            stats.object_resets_spawned += 1;
        }
    }

    info!(
        zones = stats.zones,
        rooms = stats.rooms,
        exits_resolved = stats.exits_resolved,
        exits_dangling = stats.exits_dangling,
        mobs = stats.mobs_listed,
        objects = stats.objects_listed,
        effects = stats.effects_listed,
        socials = stats.socials_listed,
        mob_resets_spawned = stats.mob_resets_spawned,
        mob_resets_skipped = stats.mob_resets_skipped,
        object_resets_spawned = stats.object_resets_spawned,
        object_resets_skipped = stats.object_resets_skipped,
        "world loaded"
    );

    Ok(stats)
}
