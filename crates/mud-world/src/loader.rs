use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::{
    abilities, ability_restrictions, effects, mob_reset_equipment, mob_resets, mobs,
    object_reset_contents, object_resets, objects, room_exits, rooms, socials, sqlx::PgPool, zones,
};
use tracing::{info, warn};

use crate::components::{
    CombatStats, Description, EquippedSlot, ExitData, Exits, FromMobReset, Health, Item, Keywords,
    Located, Mob, Named, Posture, PostureKind, Room, RoomSector, Slot, WorldKey, Zone,
};
use crate::resources::{
    AbilityCatalog, AbilityDef, EffectCatalog, EffectDef, MobProto, MobPrototypes, MobResetCatalog,
    MobResetEntry, ObjectProto, ObjectPrototypes, SocialDef, SocialRegistry, WorldKeyIndex,
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
    pub abilities_loaded: usize,
    /// Reset rows that successfully spawned a live entity into a room.
    pub mob_resets_spawned: usize,
    /// Reset rows whose target room or mob prototype was missing —
    /// counted so a stat regression flags a broken import.
    pub mob_resets_skipped: usize,
    pub object_resets_spawned: usize,
    pub object_resets_skipped: usize,
    /// Equipment items successfully attached to a reset-spawned mob.
    pub mob_equipment_spawned: usize,
    /// Equipment rows whose proto/slot/parent mob couldn't be resolved.
    pub mob_equipment_skipped: usize,
    /// Nested-content items (chest contains scroll, etc.) materialized.
    pub object_contents_spawned: usize,
    /// Content rows whose parent or proto couldn't be resolved.
    pub object_contents_skipped: usize,
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

    // Pass 4b: ability catalog (every spell / chant / song / skill).
    let ability_rows = abilities::list_all(pool).await?;
    let mut ability_catalog = AbilityCatalog::default();
    for row in ability_rows {
        let key = row.plain_name.to_ascii_lowercase();
        ability_catalog.by_name.insert(
            key,
            AbilityDef {
                id: row.id,
                name: row.name,
                plain_name: row.plain_name,
                description: row.description,
                kind: abilities::AbilityKind::from_label(&row.ability_type),
                violent: row.violent,
                combat_ok: row.combat_ok,
                in_combat_only: row.in_combat_only,
                cast_time_rounds: row.cast_time_rounds,
                cooldown_ms: row.cooldown_ms,
                is_area: row.is_area,
            },
        );
    }
    stats.abilities_loaded = ability_catalog.by_name.len();

    // Restriction messages: only the `message` field per rule, indexed
    // by ability_id. Rule type/parameters parse on demand once real
    // gating lands.
    let restriction_rows = ability_restrictions::list_all(pool).await?;
    for row in restriction_rows {
        let messages: Vec<String> = row
            .requirements
            .iter()
            .filter_map(|v| v.get("message").and_then(serde_json::Value::as_str).map(String::from))
            .collect();
        if !messages.is_empty() {
            ability_catalog.restriction_messages.insert(row.ability_id, messages);
        }
    }

    world.insert_resource(WorldKeyIndex {
        zones: zone_index,
        rooms: room_index,
    });
    world.insert_resource(mob_prototypes);
    world.insert_resource(object_prototypes);
    world.insert_resource(effect_catalog);
    world.insert_resource(social_registry);
    world.insert_resource(ability_catalog);

    // Pass 5: spawn live entities from MobResets / ObjectResets. Resources
    // were inserted above so the spawners can read them. Probability gating
    // and respawn timing belong to a future tick system; for now we always
    // spawn when probability is non-zero, capping at max_instances per
    // reset row.
    //
    // Track each spawned mob entity by its reset_id so the equipment pass
    // below can attach gear to the right instances.
    let mob_reset_rows = mob_resets::list_all(pool).await?;
    let mut mobs_by_reset: HashMap<i32, Vec<Entity>> =
        HashMap::with_capacity(mob_reset_rows.len());
    let mut reset_catalog_entries: Vec<MobResetEntry> = Vec::with_capacity(mob_reset_rows.len());
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
        let mut spawned_for_reset: Vec<Entity> =
            Vec::with_capacity(usize::try_from(r.max_instances.max(1)).unwrap_or(1));
        for _ in 0..r.max_instances.max(1) {
            let e = world
                .spawn((
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
                    FromMobReset(r.id),
                ))
                .id();
            spawned_for_reset.push(e);
            stats.mob_resets_spawned += 1;
        }
        mobs_by_reset.insert(r.id, spawned_for_reset);
        // Cache enough to refill: the respawn system reads this resource
        // and re-uses the existing MobPrototypes / WorldKeyIndex resources
        // to materialize fresh mobs.
        reset_catalog_entries.push(MobResetEntry {
            reset_id: r.id,
            mob_zone_id: r.mob_zone_id,
            mob_id: r.mob_id,
            room_entity,
            max_instances: r.max_instances,
        });
    }
    world.insert_resource(MobResetCatalog { entries: reset_catalog_entries });

    let object_reset_rows = object_resets::list_all(pool).await?;
    // Mirrors mobs_by_reset: for ObjectResetContents to find which container
    // entity belongs to a given reset_id.
    let mut objects_by_reset: HashMap<i32, Vec<Entity>> =
        HashMap::with_capacity(object_reset_rows.len());
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
        let mut spawned_for_reset: Vec<Entity> =
            Vec::with_capacity(usize::try_from(r.max_instances.max(1)).unwrap_or(1));
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
            spawned_for_reset.push(bundle.id());
            stats.object_resets_spawned += 1;
        }
        objects_by_reset.insert(r.id, spawned_for_reset);
    }

    // Pass 6: equip mobs spawned by Pass 5 according to MobResetEquipment.
    // Each row attaches one Item to every mob spawned for its reset_id.
    // Items get Located on the mob and EquippedSlot when wear_location
    // parses; rows whose proto/slot/parent can't be resolved are skipped.
    let equipment_rows = mob_reset_equipment::list_all(pool).await?;
    for eq in &equipment_rows {
        if eq.probability <= 0.0 {
            continue;
        }
        let proto = world
            .resource::<ObjectPrototypes>()
            .by_key
            .get(&(eq.object_zone_id, eq.object_id))
            .cloned();
        let mob_entities = mobs_by_reset.get(&eq.reset_id).cloned();
        let (Some(proto), Some(mob_entities)) = (proto, mob_entities) else {
            stats.mob_equipment_skipped += 1;
            continue;
        };
        let slot = eq.wear_location.as_deref().and_then(Slot::from_label);
        for &mob in &mob_entities {
            let mut bundle = world.spawn((
                Item,
                Named { name: proto.name.clone() },
                Keywords(proto.keywords.clone()),
                WorldKey { zone: proto.zone_id, id: proto.id },
                Located(mob),
            ));
            if let Some(desc) = proto.examine_description.clone() {
                bundle.insert(Description(desc));
            }
            if let Some(s) = slot {
                bundle.insert(EquippedSlot(s));
            }
            stats.mob_equipment_spawned += 1;
        }
    }

    // Pass 7: nested ObjectResetContents. Each row spawns N items
    // (`quantity`) inside their parent — either the container entity
    // from Pass 5 (parent_content_id IS NULL) or another content entity
    // spawned earlier in this pass. We iterate twice to handle the
    // small amount of nested-content nesting that exists today (max
    // depth 2 in fierydev); deeper nesting would just add another
    // pass. Each row's spawned entities are tracked by content id so
    // children can find them.
    let content_rows = object_reset_contents::list_all(pool).await?;
    let mut entities_by_content: HashMap<i32, Vec<Entity>> =
        HashMap::with_capacity(content_rows.len());
    // Two-pass: top-level first (parent_content_id IS NULL), nested
    // second. With only depth-2 in the data this fully resolves.
    for pass in 0..2 {
        for row in &content_rows {
            // Already spawned this row's items? skip.
            if entities_by_content.contains_key(&row.id) {
                continue;
            }
            // Deeper-nested rows wait for their parent in pass 1.
            let want_top_level = pass == 0;
            if row.parent_content_id.is_some() == want_top_level {
                continue;
            }
            // Resolve parent entities.
            let parents: Option<Vec<Entity>> = if let Some(pcid) = row.parent_content_id {
                entities_by_content.get(&pcid).cloned()
            } else {
                objects_by_reset.get(&row.reset_id).cloned()
            };
            let proto = world
                .resource::<ObjectPrototypes>()
                .by_key
                .get(&(row.object_zone_id, row.object_id))
                .cloned();
            let (Some(parents), Some(proto)) = (parents, proto) else {
                stats.object_contents_skipped += 1;
                continue;
            };
            let qty = usize::try_from(row.quantity.max(1)).unwrap_or(1);
            let mut spawned_for_content: Vec<Entity> =
                Vec::with_capacity(parents.len() * qty);
            for parent in parents {
                for _ in 0..qty {
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
                    spawned_for_content.push(bundle.id());
                    stats.object_contents_spawned += 1;
                }
            }
            entities_by_content.insert(row.id, spawned_for_content);
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
        abilities = stats.abilities_loaded,
        mob_resets_spawned = stats.mob_resets_spawned,
        mob_resets_skipped = stats.mob_resets_skipped,
        object_resets_spawned = stats.object_resets_spawned,
        object_resets_skipped = stats.object_resets_skipped,
        mob_equipment_spawned = stats.mob_equipment_spawned,
        mob_equipment_skipped = stats.mob_equipment_skipped,
        object_contents_spawned = stats.object_contents_spawned,
        object_contents_skipped = stats.object_contents_skipped,
        "world loaded"
    );

    Ok(stats)
}
