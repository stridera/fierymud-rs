use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::{
    abilities, ability_damage_components, ability_effects, ability_messages, ability_restrictions, ability_saving_throw, ability_targeting, classes, effects, mob_reset_equipment,
    mob_resets, mobs, object_abilities, object_reset_contents, object_resets, objects, room_exits,
    rooms, shops, socials, sqlx::PgPool, zones,
};
use tracing::{info, warn};

use crate::components::{
    CombatStats, Description, EquippedSlot, ExitData, Exits, FromMobReset, Health, Item, Keywords,
    Located, Mob, Named, Posture, PostureKind, Room, RoomSector, Shopkeeper, Slot, WorldKey, Zone,
};
use crate::resources::{
    AbilityCatalog, AbilityDef, AbilityMessageSet, ClassCatalog, ClassDef, DamageComponent,
    EffectCatalog, EffectDef, MobProto, MobPrototypes, MobResetCatalog, MobResetEntry,
    ObjectAbilityCatalog, ObjectProto, ObjectPrototypes, SavingThrow, ShopCatalog, ShopDef,
    ShopOffering, SocialDef, SocialRegistry, TargetingRule, WorldKeyIndex,
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
        // Composite `key_zone_id` + `key_id` columns. Both NULL means
        // no key required; both Some means the (zone, id) of the
        // required Object proto.
        let key = match (e.key_zone_id, e.key_id) {
            (Some(z), Some(i)) => Some((z, i)),
            _ => None,
        };
        if let Some(mut exits) = world.get_mut::<Exits>(source) {
            exits.0.insert(
                e.direction,
                ExitData {
                    to: target,
                    state: e.default_state,
                    key,
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
                wealth: row.wealth,
            },
        );
    }
    stats.mobs_listed = mob_prototypes.by_key.len();

    let object_rows = objects::list_objects(pool).await?;
    let mut object_prototypes = ObjectPrototypes::default();
    for row in object_rows {
        let (weapon_dice_num, weapon_dice_size, weapon_dice_bonus) =
            parse_weapon_dice(&row.values);
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
                wear_flags: row.wear_flags,
                weapon_dice_num,
                weapon_dice_size,
                weapon_dice_bonus,
                cost: row.cost,
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
                default_params: row.default_params,
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
        let min_posture_rank = position_rank(&row.min_position);
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
                min_position_label: row.min_position,
                min_posture_rank,
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
            ability_catalog
                .restriction_messages
                .insert(row.ability_id, messages);
        }
        // Stash the full rule list so the runtime evaluator
        // (commands::check_ability_restrictions) can interpret
        // type-specific rules at cast time.
        if !row.requirements.is_empty() {
            ability_catalog
                .restriction_rules
                .insert(row.ability_id, row.requirements);
        }
    }

    // Effect mappings: ordered list of (effect_id, override_params)
    // per ability_id. Rows are returned ORDER BY ability_id, "order"
    // so push order matches schema order.
    let ability_effect_rows = ability_effects::list_all(pool).await?;
    for row in ability_effect_rows {
        ability_catalog
            .effects_for
            .entry(row.ability_id)
            .or_default()
            .push((row.effect_id, row.override_params));
    }

    // Per-ability targeting rules (valid target types, scope,
    // range, LOS). Keyed by ability_id; UNIQUE per ability.
    let targeting_rows = ability_targeting::list_all(pool).await?;
    for row in targeting_rows {
        ability_catalog.targeting.insert(
            row.ability_id,
            TargetingRule {
                valid_targets: row.valid_targets,
                scope: row.scope,
                max_targets: row.max_targets,
                require_los: row.require_los,
            },
        );
    }

    // Per-ability saving-throw rules. UNIQUE per ability_id.
    let save_rows = ability_saving_throw::list_all(pool).await?;
    for row in save_rows {
        ability_catalog.saves.insert(
            row.ability_id,
            SavingThrow {
                save_type: row.save_type,
                dc_formula: row.dc_formula,
                on_save_action: row.on_save_action,
            },
        );
    }

    // Multi-element damage components — append per-ability
    // ordered by sequence. The damage arm sums these when present
    // and otherwise falls back to override_params.amount.
    let dc_rows = ability_damage_components::list_all(pool).await?;
    for row in dc_rows {
        ability_catalog
            .damage_components
            .entry(row.ability_id)
            .or_default()
            .push(DamageComponent {
                element: row.element,
                damage_formula: row.damage_formula,
                percentage: row.percentage,
                sequence: row.sequence,
            });
    }

    // Templated message strings (success/fail/wearoff text). Keyed by
    // ability_id; per-row UNIQUE in the schema so each ability has
    // at most one message set.
    let message_rows = ability_messages::list_all(pool).await?;
    for row in message_rows {
        ability_catalog.messages.insert(
            row.ability_id,
            AbilityMessageSet {
                start_to_caster: row.start_to_caster,
                start_to_victim: row.start_to_victim,
                start_to_room: row.start_to_room,
                success_to_caster: row.success_to_caster,
                success_to_victim: row.success_to_victim,
                success_to_room: row.success_to_room,
                success_to_self: row.success_to_self,
                success_self_room: row.success_self_room,
                fail_to_caster: row.fail_to_caster,
                fail_to_victim: row.fail_to_victim,
                fail_to_room: row.fail_to_room,
                wearoff_to_target: row.wearoff_to_target,
                wearoff_to_room: row.wearoff_to_room,
                look_message: row.look_message,
            },
        );
    }

    // Pass 4c: class catalog. Just identity for the score / who readouts;
    // full mechanics (proficiencies, stat bonuses, allowed abilities) load
    // on demand once the systems that need them land.
    let class_rows = classes::list_all(pool).await?;
    let mut class_catalog = ClassCatalog::default();
    for row in class_rows {
        class_catalog.by_id.insert(
            row.id,
            ClassDef {
                id: row.id,
                name: row.name,
                plain_name: row.plain_name,
                is_subclass: row.is_subclass,
                parent_class_id: row.parent_class_id,
            },
        );
    }

    // Pass 4d: object-ability catalog (scrolls / wands / staves
    // bound abilities). Lookups are by `(zone, id)` matching an item
    // entity's `WorldKey`.
    let oa_rows = object_abilities::list_all(pool).await?;
    let mut object_ability_catalog = ObjectAbilityCatalog::default();
    for row in oa_rows {
        object_ability_catalog
            .by_key
            .entry((row.object_zone_id, row.object_id))
            .or_default()
            .push(crate::resources::ObjectAbilityBinding {
                ability_id: row.ability_id,
                level: row.level,
                charges: row.charges,
            });
    }

    // Pass 4.5: load Shop catalog. Each shop maps to a keeper mob via
    // (keeper_zone_id, keeper_id); the spawn pass below attaches a
    // Shopkeeper component to each spawned mob whose proto matches.
    let shop_rows = shops::list_shops(pool).await?;
    let item_rows = shops::list_shop_items(pool).await?;
    let mut shop_catalog = ShopCatalog::default();
    for row in &shop_rows {
        shop_catalog
            .keeper_index
            .insert((row.keeper_zone_id, row.keeper_id), (row.zone_id, row.id));
        shop_catalog.by_key.insert(
            (row.zone_id, row.id),
            ShopDef {
                zone_id: row.zone_id,
                id: row.id,
                keeper_zone_id: row.keeper_zone_id,
                keeper_id: row.keeper_id,
                buy_profit: row.buy_profit,
                sell_profit: row.sell_profit,
                items: Vec::new(),
            },
        );
    }
    for row in &item_rows {
        if let Some(def) = shop_catalog.by_key.get_mut(&(row.shop_zone_id, row.shop_id)) {
            def.items.push(ShopOffering {
                object_zone_id: row.object_zone_id,
                object_id: row.object_id,
                amount: row.amount,
                price: row.price,
            });
        }
    }
    info!(
        shops = shop_catalog.by_key.len(),
        items = item_rows.len(),
        "shop catalog loaded"
    );

    world.insert_resource(WorldKeyIndex {
        zones: zone_index,
        rooms: room_index,
    });
    world.insert_resource(mob_prototypes);
    world.insert_resource(object_prototypes);
    world.insert_resource(effect_catalog);
    world.insert_resource(social_registry);
    world.insert_resource(ability_catalog);
    world.insert_resource(class_catalog);
    world.insert_resource(object_ability_catalog);
    world.insert_resource(shop_catalog);

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
        let shop_key = world
            .resource::<ShopCatalog>()
            .keeper_index
            .get(&(proto.zone_id, proto.id))
            .copied();
        let mut spawned_for_reset: Vec<Entity> =
            Vec::with_capacity(usize::try_from(r.max_instances.max(1)).unwrap_or(1));
        for _ in 0..r.max_instances.max(1) {
            let mut em = world.spawn((
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
            ));
            if let Some((shop_zone_id, shop_id)) = shop_key {
                em.insert(Shopkeeper { shop_zone_id, shop_id });
            }
            let e = em.id();
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
        let primary_slot = wear_flags_primary_slot(&proto.wear_flags);
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
            if let Some(s) = primary_slot {
                bundle.insert(crate::components::WearableIn(s));
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

/// Pick a single primary `Slot` from a list of `WearFlag`s. Most
/// items in legacy data carry exactly one flag; for the few that
/// have several (e.g. ring on FINGER + neck-pendant on NECK) we
/// prefer the more common-use slot. The runtime `Slot` enum is
/// smaller than the schema's wear-flag set; flags without a runtime
/// equivalent (Tail, Hover, Disguise, Badge) return None.
#[must_use]
pub fn wear_flags_primary_slot(flags: &[mud_db::enums::WearFlag]) -> Option<crate::components::Slot> {
    use crate::components::Slot;
    use mud_db::enums::WearFlag::{
        About, Arms, Badge, Belt, Body, Disguise, Ear, Eyes, Face, Feet, Finger, Hands, Head,
        Hover, Legs, Mainhand, Neck, Offhand, Tail, Twohand, Waist, Wrist,
    };
    // Priority order: prefer wield/hold/body slots over decorative.
    for f in flags {
        let slot = match f {
            Mainhand | Twohand => Some(Slot::Wield),
            Offhand => Some(Slot::Hold),
            Body => Some(Slot::Body),
            Head => Some(Slot::Head),
            Arms => Some(Slot::Arms),
            Legs => Some(Slot::Legs),
            Feet => Some(Slot::Feet),
            Hands => Some(Slot::Hands),
            Waist | Belt => Some(Slot::Waist),
            Neck => Some(Slot::Neck),
            Finger => Some(Slot::LeftFinger),
            // Schema flags with no runtime slot: ignored.
            Wrist | About | Eyes | Face | Ear | Tail | Hover | Disguise | Badge => None,
        };
        if slot.is_some() {
            return slot;
        }
    }
    None
}

/// Pull weapon dice out of an Object's `values` JSONB blob. Schema
/// shape: `{"Hit Dice": {"num": "N", "size": "M", "bonus": B}, ...}`
/// where num/size are typed as strings (not numbers) in the legacy
/// data — we accept either form. Returns `(num, size, bonus)` zeros
/// for non-weapons or malformed rows so the formula evaluator falls
/// through cleanly.
fn parse_weapon_dice(values: &serde_json::Value) -> (i32, i32, i32) {
    let Some(dice) = values.get("Hit Dice") else {
        return (0, 0, 0);
    };
    let parse = |v: Option<&serde_json::Value>| -> i32 {
        match v {
            Some(serde_json::Value::Number(n)) => i32::try_from(n.as_i64().unwrap_or(0)).unwrap_or(0),
            Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0),
            _ => 0,
        }
    };
    (
        parse(dice.get("num")),
        parse(dice.get("size")),
        parse(dice.get("bonus")),
    )
}

/// Map the schema `Position` enum label to the rank used by ability
/// gating. Order matches `pg_enum.enumsortorder`. Unknown labels return
/// 0 so they never gate a runtime posture (fail-open: a malformed row
/// shouldn't make a spell uncastable).
fn position_rank(label: &str) -> i32 {
    match label {
        "DEAD" => 1,
        "GHOST" => 2,
        "MORTALLY_WOUNDED" => 3,
        "INCAPACITATED" => 4,
        "STUNNED" => 5,
        "SLEEPING" => 6,
        "RESTING" => 7,
        "SITTING" => 8,
        "STANDING" => 9,
        _ => 0,
    }
}
