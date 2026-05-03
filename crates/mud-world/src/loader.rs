use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::{
    abilities, ability_components, ability_damage_components, ability_effects, ability_messages,
    ability_restrictions, ability_saving_throw, ability_targeting, achievements, boards, classes,
    effects, levels,
    mob_reset_equipment, mob_resets, mobs, object_abilities, object_reset_contents, object_resets,
    objects, room_exits, rooms, shops, socials, spell_slots, sqlx::PgPool, triggers, zones,
};
use tracing::{info, warn};

use crate::components::{
    AttachedTriggers, BoardLink, CombatStats, Description, EquippedSlot, ExitData, Exits,
    FromMobReset, FromObjectReset, Health, Item, Keywords, LiquidContainer, Located, Mob,
    Mountable, Named, Posture,
    PostureKind, Room, RoomSector, Shopkeeper, Slot, WorldKey, Zone, ZoneClimate,
};
use crate::resources::{
    AbilityCatalog, AbilityDef, AbilityMessageSet, BoardCatalog, BoardSummary, ClassCatalog,
    ClassDef, ConsumableEffectBinding, ConsumableEffectCatalog, DamageComponent, EffectCatalog,
    EffectDef, LiquidIndex, LiquidProto, MobProto, MobPrototypes, MobResetCatalog, MobResetEntry,
    ObjectAbilityCatalog, ObjectProto, ObjectPrototypes, ObjectResetCatalog, ObjectResetEntry,
    SavingThrow, ShopAcceptRule, ShopCatalog, ShopDef, ShopOffering, ShopPetOffering, SocialDef,
    SocialRegistry, TargetingRule, TriggerAttach, TriggerCatalog, TriggerDef, TriggerEvent,
    WorldKeyIndex,
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
    /// Lua trigger rows in the `Triggers` table.
    pub triggers_loaded: usize,
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
    let mut weather = crate::resources::WeatherCatalog::default();
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
                ZoneClimate(z.climate),
            ))
            .id();
        zone_index.insert(z.id, entity);
        weather
            .by_zone
            .insert(z.id, default_weather_for_climate(z.climate));
    }
    world.insert_resource(weather);
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
        let is_hidden = e.flags.contains(&mud_db::enums::ExitFlag::Hidden);
        let is_pickproof = e.flags.contains(&mud_db::enums::ExitFlag::Pickproof);
        if let Some(mut exits) = world.get_mut::<Exits>(source) {
            exits.0.insert(
                e.direction,
                ExitData {
                    to: target,
                    state: e.default_state,
                    key,
                    description: e.description,
                    keywords: e.keywords,
                    is_hidden,
                    is_pickproof,
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
                class_id: row.class_id,
                behaviors: row.behaviors,
                protected_kind: row.protected_kind,
                professions: row.professions,
            },
        );
    }
    stats.mobs_listed = mob_prototypes.by_key.len();

    let object_rows = objects::list_objects(pool).await?;
    let mut object_prototypes = ObjectPrototypes::default();
    for row in object_rows {
        let (weapon_dice_num, weapon_dice_size, weapon_dice_bonus) =
            parse_weapon_dice(&row.values);
        let portal_destination_vnum = if matches!(row.r#type, mud_db::enums::ObjectType::Portal) {
            parse_portal_destination(&row.values)
        } else {
            None
        };
        let board_id = if matches!(row.r#type, mud_db::enums::ObjectType::Board) {
            parse_board_id(&row.values)
        } else {
            None
        };
        let liquid = if matches!(
            row.r#type,
            mud_db::enums::ObjectType::Drinkcontainer | mud_db::enums::ObjectType::Fountain
        ) {
            parse_liquid(&row.values)
        } else {
            None
        };
        let light_fuel = if matches!(row.r#type, mud_db::enums::ObjectType::Light) {
            Some(parse_light_fuel(&row.values))
        } else {
            None
        };
        object_prototypes.by_key.insert(
            (row.zone_id, row.id),
            ObjectProto {
                zone_id: row.zone_id,
                id: row.id,
                r#type: row.r#type,
                name: strip_ansi(&row.name),
                keywords: row.keywords,
                room_description: strip_ansi(&row.room_description),
                examine_description: row.examine_description.as_deref().map(strip_ansi),
                weight: row.weight,
                level: row.level,
                wear_flags: row.wear_flags,
                weapon_dice_num,
                weapon_dice_size,
                weapon_dice_bonus,
                cost: row.cost,
                portal_destination_vnum,
                board_id,
                liquid,
                light_fuel,
                restricted_alignments: row.restricted_alignments,
                restricted_class_ids: row.restricted_class_ids,
                restricted_races: row.restricted_races,
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
                prevents_speaking: row.prevents_speaking,
                prevents_casting: row.prevents_casting,
                prevents_movement: row.prevents_movement,
                on_apply: row.on_apply.filter(|s| !s.trim().is_empty()),
                on_tick: row.on_tick.filter(|s| !s.trim().is_empty()),
                on_remove: row.on_remove.filter(|s| !s.trim().is_empty()),
            },
        );
    }
    stats.effects_listed = effect_catalog.by_id.len();

    // Achievement catalog. Hooks reference rows by their stable
    // `code` string so per-row id changes (re-import, schema
    // migration) don't break. `by_id` lookup serves the
    // CharacterAchievement render path (per-player unlocks store
    // ids only).
    let achievement_rows = achievements::list_all(pool).await?;
    let mut achievement_catalog = crate::resources::AchievementCatalog::default();
    for row in achievement_rows {
        let def = crate::resources::AchievementDef {
            id: row.id,
            code: row.code.clone(),
            title: row.title,
            description: row.description,
            category: row.category,
            hidden: row.hidden,
            sort_order: row.sort_order,
        };
        achievement_catalog.by_id.insert(row.id, def.clone());
        achievement_catalog.by_code.insert(row.code, def);
    }
    info!(
        achievements = achievement_catalog.by_id.len(),
        "achievement catalog loaded"
    );
    world.insert_resource(achievement_catalog);

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

    // Material reagent rows. `invoke_ability` checks every
    // `required` row against the caster's carried items before
    // applying effects; on success it removes one instance per
    // `consumed` row.
    let comp_rows = ability_components::list_all(pool).await?;
    for row in comp_rows {
        ability_catalog
            .components
            .entry(row.ability_id)
            .or_default()
            .push(crate::resources::AbilityComponentReq {
                object_id: row.object_id,
                consumed: row.consumed,
                required: row.required,
            });
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

    // Pass 4c.4: level definition table — drives the `level`
    // command's XP-curve readout and is the eventual source of
    // per-level HP/stamina gains.
    let level_rows = levels::list_all(pool).await?;
    let mut level_table = crate::resources::LevelTable::default();
    for r in level_rows {
        level_table.rows.push(crate::resources::LevelRow {
            level: r.level,
            name: r.name,
            exp_required: r.exp_required,
            hp_gain: r.hp_gain,
            stamina_gain: r.stamina_gain,
            is_immortal: r.is_immortal,
        });
    }
    world.insert_resource(level_table);

    // Pass 4c.5: spell slot data. Three tables joined into one
    // resource — `SpellSlotProgression` (level × circle → slots),
    // `ClassAbilityCircles` (which circles each class can access at
    // which min level), and `ClassAbilities` ((class, ability) →
    // circle for memorize/forget gating). Read by the `slots` /
    // `memorize` / `forget` commands.
    let progression_rows = spell_slots::list_slot_progression(pool).await?;
    let class_circle_rows = spell_slots::list_class_circles(pool).await?;
    let class_ability_rows = spell_slots::list_class_abilities(pool).await?;
    let mut spell_slot_data = crate::resources::SpellSlotData::default();
    for r in progression_rows {
        spell_slot_data.progression.insert((r.level, r.circle), r.slots);
    }
    for r in class_circle_rows {
        spell_slot_data
            .class_circles
            .entry(r.class_id)
            .or_default()
            .push((r.circle, r.min_level));
    }
    for r in class_ability_rows {
        spell_slot_data
            .ability_circle
            .insert((r.class_id, r.ability_id), r.circle);
        spell_slot_data
            .ability_cap
            .insert((r.class_id, r.ability_id), r.proficiency_cap);
    }
    world.insert_resource(spell_slot_data);

    // ClassSkills: per-class, per-skill (class_id, ability_id) →
    // (min_level, proficiency_cap). Drives the SKILL invoke gate
    // ("can this class use this skill at this level?"). Loaded
    // alongside the spell-slot triple since they share the same
    // (class_id, ability_id) shape and authoring lifecycle.
    let class_skill_rows = spell_slots::list_class_skills(pool).await?;
    let mut class_skills_data = crate::resources::ClassSkillsData::default();
    for r in class_skill_rows {
        class_skills_data
            .min_level
            .insert((r.class_id, r.ability_id), r.min_level);
        class_skills_data
            .proficiency_cap
            .insert((r.class_id, r.ability_id), r.proficiency_cap);
    }
    world.insert_resource(class_skills_data);

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

    // ConsumableEffects: per-object and per-liquid bindings consumed
    // by `consume_item` (eat / quaff / drink). The schema's row picks
    // exactly one binding shape — split into two maps for O(1)
    // lookup at consume time.
    let mut consumable_catalog = ConsumableEffectCatalog::default();
    {
        let object_bindings_query = sqlx::query!(
            r#"
            SELECT effect_id, chance, level, duration, object_zone_id, object_id
            FROM "ConsumableEffects"
            WHERE object_zone_id IS NOT NULL AND object_id IS NOT NULL
            "#
        )
        .fetch_all(pool)
        .await?;
        for r in object_bindings_query {
            let (Some(zone), Some(id)) = (r.object_zone_id, r.object_id) else {
                continue;
            };
            consumable_catalog
                .by_object
                .entry((zone, id))
                .or_default()
                .push(ConsumableEffectBinding {
                    effect_id: r.effect_id,
                    chance: r.chance,
                    level: r.level,
                    duration_secs: r.duration,
                });
        }
        // Per-liquid bindings: one query, walk all liquid IDs.
        let liquid_bindings_query = sqlx::query!(
            r#"
            SELECT effect_id, chance, level, duration, liquid_id
            FROM "ConsumableEffects"
            WHERE liquid_id IS NOT NULL
            "#
        )
        .fetch_all(pool)
        .await?;
        for r in liquid_bindings_query {
            let Some(lid) = r.liquid_id else { continue };
            consumable_catalog
                .by_liquid
                .entry(lid)
                .or_default()
                .push(ConsumableEffectBinding {
                    effect_id: r.effect_id,
                    chance: r.chance,
                    level: r.level,
                    duration_secs: r.duration,
                });
        }
    }
    info!(
        objects = consumable_catalog.by_object.len(),
        liquids = consumable_catalog.by_liquid.len(),
        "consumable effect catalog loaded"
    );

    // Liquids name → id index for the drink path's per-liquid
    // ConsumableEffects lookup. `LiquidContainer` stores the name;
    // this resolves to the schema's id.
    let mut liquid_index = LiquidIndex::default();
    let liquid_rows = sqlx::query!(
        r#"SELECT id, name, drunk_effect FROM "Liquids""#
    )
    .fetch_all(pool)
    .await?;
    for row in liquid_rows {
        let name_lc = row.name.to_ascii_lowercase();
        liquid_index.by_name.insert(name_lc.clone(), row.id);
        liquid_index.drunk_effect.insert(name_lc, row.drunk_effect);
    }
    info!(
        liquids = liquid_index.by_name.len(),
        "liquid index loaded"
    );

    // RoomEnvironmentalEffect: per-room link to Effect rows. The
    // runtime applies these on arrival (short duration so leaving
    // the room lets them decay).
    let env_rows = mud_db::room_environmental_effects::list_all(pool).await?;
    let mut room_env_effects = crate::resources::RoomEnvironmentalEffects::default();
    for row in &env_rows {
        room_env_effects
            .by_room
            .entry((row.room_zone_id, row.room_id))
            .or_default()
            .push(row.effect_id);
    }
    info!(
        rooms = room_env_effects.by_room.len(),
        bindings = env_rows.len(),
        "room environmental effects loaded"
    );

    // Pass 4.5: load Shop catalog. Each shop maps to a keeper mob via
    // (keeper_zone_id, keeper_id); the spawn pass below attaches a
    // Shopkeeper component to each spawned mob whose proto matches.
    let shop_rows = shops::list_shops(pool).await?;
    let item_rows = shops::list_shop_items(pool).await?;
    let accept_rows = shops::list_shop_accepts(pool).await?;
    let pet_rows = shops::list_shop_mobs(pool).await?;
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
                accepts: Vec::new(),
                pets: Vec::new(),
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
    for row in &accept_rows {
        if let Some(def) = shop_catalog.by_key.get_mut(&(row.shop_zone_id, row.shop_id)) {
            def.accepts.push(ShopAcceptRule {
                object_type: row.object_type.clone(),
                keywords: row.keywords.clone(),
            });
        }
    }
    for row in &pet_rows {
        if let Some(def) = shop_catalog.by_key.get_mut(&(row.shop_zone_id, row.shop_id)) {
            def.pets.push(ShopPetOffering {
                mob_zone_id: row.mob_zone_id,
                mob_id: row.mob_id,
                amount: row.amount,
                price: row.price,
            });
        }
    }
    info!(
        shops = shop_catalog.by_key.len(),
        items = item_rows.len(),
        accepts = accept_rows.len(),
        pets = pet_rows.len(),
        "shop catalog loaded"
    );

    let mut legacy_vnums = HashMap::with_capacity(room_index.len());
    for &(zone, id) in room_index.keys() {
        // CircleMUD vnum encoding: zone * 100 + id. Zones with > 100
        // rooms (zone 30: id 0..499) overflow into the next zone's
        // namespace; with the last-write-wins insert here, the higher
        // zones' rooms 0..99 win the slot. Acceptable for portal
        // resolution since portal targets typically point at low-id
        // rooms in the destination zone.
        legacy_vnums.insert(zone * 100 + id, (zone, id));
    }
    world.insert_resource(WorldKeyIndex {
        zones: zone_index,
        rooms: room_index,
        legacy_vnums,
    });
    world.insert_resource(mob_prototypes);
    world.insert_resource(object_prototypes);
    world.insert_resource(effect_catalog);
    world.insert_resource(social_registry);
    world.insert_resource(ability_catalog);
    world.insert_resource(class_catalog);
    world.insert_resource(object_ability_catalog);
    world.insert_resource(consumable_catalog);
    world.insert_resource(liquid_index);
    world.insert_resource(room_env_effects);
    world.insert_resource(shop_catalog);

    // Pass 4.6: load Board catalog. Just metadata (id/alias/title/lock) —
    // message rows are queried live so post/edit/delete shows up.
    let board_rows = boards::list_boards(pool).await?;
    let mut board_catalog = BoardCatalog::default();
    for row in &board_rows {
        board_catalog.by_id.insert(
            row.id,
            BoardSummary {
                id: row.id,
                alias: row.alias.clone(),
                title: row.title.clone(),
                locked: row.locked,
            },
        );
    }
    info!(boards = board_catalog.by_id.len(), "board catalog loaded");
    world.insert_resource(board_catalog);

    // Pass 4.7: load Lua trigger catalog + per-prototype attachment
    // indexes. Triggers fire from junction tables (MobTriggers /
    // ObjectTriggers / RoomTriggers); spawn paths copy the relevant
    // list onto fresh entities. The dispatcher (not yet wired) reads
    // `commands` lazily by `(zone, id)` when an entity-relevant event
    // fires.
    let trigger_rows = triggers::list_triggers(pool).await?;
    let mut trigger_catalog = TriggerCatalog::default();
    for row in trigger_rows {
        let attach = match row.attach_type {
            triggers::ScriptType::Mob => TriggerAttach::Mob,
            triggers::ScriptType::Object => TriggerAttach::Object,
            triggers::ScriptType::World => TriggerAttach::World,
        };
        let flags = row
            .flags
            .unwrap_or_default()
            .iter()
            .map(|f| match f {
                triggers::TriggerFlag::Global => TriggerEvent::Global,
                triggers::TriggerFlag::Random => TriggerEvent::Random,
                triggers::TriggerFlag::Command => TriggerEvent::Command,
                triggers::TriggerFlag::Load => TriggerEvent::Load,
                triggers::TriggerFlag::Cast => TriggerEvent::Cast,
                triggers::TriggerFlag::Leave => TriggerEvent::Leave,
                triggers::TriggerFlag::Time => TriggerEvent::Time,
                triggers::TriggerFlag::Speech => TriggerEvent::Speech,
                triggers::TriggerFlag::Act => TriggerEvent::Act,
                triggers::TriggerFlag::Death => TriggerEvent::Death,
                triggers::TriggerFlag::Greet => TriggerEvent::Greet,
                triggers::TriggerFlag::GreetAll => TriggerEvent::GreetAll,
                triggers::TriggerFlag::Entry => TriggerEvent::Entry,
                triggers::TriggerFlag::Receive => TriggerEvent::Receive,
                triggers::TriggerFlag::Fight => TriggerEvent::Fight,
                triggers::TriggerFlag::HitPercent => TriggerEvent::HitPercent,
                triggers::TriggerFlag::Bribe => TriggerEvent::Bribe,
                triggers::TriggerFlag::Memory => TriggerEvent::Memory,
                triggers::TriggerFlag::Door => TriggerEvent::Door,
                triggers::TriggerFlag::SpeechTo => TriggerEvent::SpeechTo,
                triggers::TriggerFlag::Look => TriggerEvent::Look,
                triggers::TriggerFlag::Auto => TriggerEvent::Auto,
                triggers::TriggerFlag::Attack => TriggerEvent::Attack,
                triggers::TriggerFlag::Defend => TriggerEvent::Defend,
                triggers::TriggerFlag::Timer => TriggerEvent::Timer,
                triggers::TriggerFlag::Get => TriggerEvent::Get,
                triggers::TriggerFlag::Drop => TriggerEvent::Drop,
                triggers::TriggerFlag::Give => TriggerEvent::Give,
                triggers::TriggerFlag::Wear => TriggerEvent::Wear,
                triggers::TriggerFlag::Remove => TriggerEvent::Remove,
                triggers::TriggerFlag::Use => TriggerEvent::Use,
                triggers::TriggerFlag::Consume => TriggerEvent::Consume,
                triggers::TriggerFlag::Reset => TriggerEvent::Reset,
                triggers::TriggerFlag::Preentry => TriggerEvent::Preentry,
                triggers::TriggerFlag::Postentry => TriggerEvent::Postentry,
            })
            .collect();
        trigger_catalog.by_key.insert(
            (row.zone_id, row.id),
            TriggerDef {
                zone_id: row.zone_id,
                id: row.id,
                name: row.name,
                attach_type: attach,
                commands: row.commands,
                flags,
                arg_list: row.arg_list.unwrap_or_default(),
                num_args: row.num_args,
            },
        );
    }
    for link in triggers::list_mob_triggers(pool).await? {
        trigger_catalog
            .mob_attachments
            .entry((link.mob_zone_id, link.mob_id))
            .or_default()
            .push((link.trigger_zone_id, link.trigger_id));
    }
    for link in triggers::list_object_triggers(pool).await? {
        trigger_catalog
            .object_attachments
            .entry((link.object_zone_id, link.object_id))
            .or_default()
            .push((link.trigger_zone_id, link.trigger_id));
    }
    for link in triggers::list_room_triggers(pool).await? {
        trigger_catalog
            .room_attachments
            .entry((link.room_zone_id, link.room_id))
            .or_default()
            .push((link.trigger_zone_id, link.trigger_id));
    }
    stats.triggers_loaded = trigger_catalog.by_key.len();
    info!(
        triggers = trigger_catalog.by_key.len(),
        mob_links = trigger_catalog.mob_attachments.len(),
        object_links = trigger_catalog.object_attachments.len(),
        room_links = trigger_catalog.room_attachments.len(),
        "trigger catalog loaded"
    );
    // Pass 4.7b: room trigger attachment. Done now (before the mob/
    // object spawn pass) since room entities already exist after
    // Pass 2 — no need to defer. `room_index` was moved into
    // `WorldKeyIndex` above, so re-borrow from the resource.
    {
        let mut room_to_attach: Vec<(Entity, Vec<(i32, i32)>)> = Vec::new();
        let rooms_lookup = &world.resource::<WorldKeyIndex>().rooms;
        for (key, list) in &trigger_catalog.room_attachments {
            if let Some(&room_entity) = rooms_lookup.get(key) {
                room_to_attach.push((room_entity, list.clone()));
            }
        }
        for (room_entity, list) in room_to_attach {
            world.entity_mut(room_entity).insert(AttachedTriggers(list));
        }
    }
    world.insert_resource(trigger_catalog);

    // Pass 5: spawn live entities from MobResets / ObjectResets. Each
    // reset row spawns *exactly one* entity, only when the world's
    // count of that proto is below the row's `max_instances` (the
    // legacy CircleMUD `max_existing` semantic — a global cap, not a
    // per-row count). Several reset rows may reference the same
    // proto across different rooms; the cap applies across all.
    //
    // Track each spawned mob entity by its reset_id so the equipment
    // pass below can attach gear to the right instances.
    let mob_reset_rows = mob_resets::list_all(pool).await?;
    let mut mobs_by_reset: HashMap<i32, Vec<Entity>> =
        HashMap::with_capacity(mob_reset_rows.len());
    let mut reset_catalog_entries: Vec<MobResetEntry> = Vec::with_capacity(mob_reset_rows.len());
    let mut mob_world_count: HashMap<(i32, i32), i32> = HashMap::new();
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
        // Always cache the catalog row so the respawn system can
        // refill this reset later, even if the cap is exhausted now.
        reset_catalog_entries.push(MobResetEntry {
            reset_id: r.id,
            mob_zone_id: r.mob_zone_id,
            mob_id: r.mob_id,
            room_entity,
            max_instances: r.max_instances,
        });
        // Skip the spawn if this proto has already hit its global cap
        // via earlier reset rows. The cap is a max on world-wide
        // count, not per-row count — older code spawned
        // `max_instances` copies per row, which over-populated rooms.
        let proto_key = (proto.zone_id, proto.id);
        let current = *mob_world_count.get(&proto_key).unwrap_or(&0);
        if current >= r.max_instances.max(1) {
            continue;
        }
        let hp = proto.rolled_hp();
        let dmg = proto.avg_damage();
        let shop_key = world
            .resource::<ShopCatalog>()
            .keeper_index
            .get(&proto_key)
            .copied();
        let trigger_keys = world
            .resource::<TriggerCatalog>()
            .mob_attachments
            .get(&proto_key)
            .cloned();
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
        if let Some(ref keys) = trigger_keys {
            em.insert(AttachedTriggers(keys.clone()));
        }
        if !proto.behaviors.is_empty() {
            em.insert(crate::components::MobBehaviors(proto.behaviors.clone()));
        }
        // Mountable inference: keywords containing horse/steed/mount
        // get the marker. Builders can refine via richer mount data
        // later; for now this catches every horse/warhorse/donkey
        // in the imported world.
        if proto.keywords.iter().any(|k| {
            let lc = k.to_ascii_lowercase();
            lc.contains("horse")
                || lc.contains("steed")
                || lc.contains("mount")
                || lc.contains("donkey")
                || lc.contains("mare")
                || lc.contains("nightmare")
        }) {
            em.insert(Mountable);
        }
        let e = em.id();
        mobs_by_reset.insert(r.id, vec![e]);
        *mob_world_count.entry(proto_key).or_insert(0) += 1;
        stats.mob_resets_spawned += 1;
    }
    world.insert_resource(MobResetCatalog { entries: reset_catalog_entries });

    let object_reset_rows = object_resets::list_all(pool).await?;
    // Mirrors mobs_by_reset: for ObjectResetContents to find which container
    // entity belongs to a given reset_id.
    let mut objects_by_reset: HashMap<i32, Vec<Entity>> =
        HashMap::with_capacity(object_reset_rows.len());
    let mut object_world_count: HashMap<(i32, i32), i32> = HashMap::new();
    let mut object_reset_catalog: Vec<ObjectResetEntry> =
        Vec::with_capacity(object_reset_rows.len());
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
        // Catalog the reset row so the respawn tick can refill if
        // the spawn ever despawns. Mirrors the `MobReset` catalog
        // shape — `ObjectResetContents` (nested chest contents)
        // are a separate table and don't refill independently.
        object_reset_catalog.push(ObjectResetEntry {
            reset_id: r.id,
            object_zone_id: r.object_zone_id,
            object_id: r.object_id,
            room_entity,
            max_instances: r.max_instances,
        });
        // Same global-cap semantic as mob resets: spawn at most one
        // per row, only if the world's count of this proto is below
        // the row's `max_instances`.
        let proto_key = (proto.zone_id, proto.id);
        let current = *object_world_count.get(&proto_key).unwrap_or(&0);
        if current >= r.max_instances.max(1) {
            continue;
        }
        let trigger_keys = world
            .resource::<TriggerCatalog>()
            .object_attachments
            .get(&proto_key)
            .cloned();
        let primary_slot = wear_flags_primary_slot(&proto.wear_flags);
        {
            let mut bundle = world.spawn((
                Item,
                Named { name: proto.name.clone() },
                Keywords(proto.keywords.clone()),
                WorldKey { zone: proto.zone_id, id: proto.id },
                Located(room_entity),
                FromObjectReset(r.id),
            ));
            if let Some(desc) = proto.examine_description.clone() {
                bundle.insert(Description(desc));
            }
            if let Some(s) = primary_slot {
                bundle.insert(crate::components::WearableIn(s));
            }
            if let Some(board_id) = proto.board_id {
                bundle.insert(BoardLink(board_id));
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
                bundle.insert(crate::components::LightFuel {
                    capacity: fuel.capacity,
                    remaining: fuel.remaining,
                });
            }
            if let Some(ref keys) = trigger_keys {
                bundle.insert(AttachedTriggers(keys.clone()));
            }
            objects_by_reset.insert(r.id, vec![bundle.id()]);
            stats.object_resets_spawned += 1;
        }
        *object_world_count.entry(proto_key).or_insert(0) += 1;
    }
    world.insert_resource(ObjectResetCatalog {
        entries: object_reset_catalog,
    });

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
        let trigger_keys = world
            .resource::<TriggerCatalog>()
            .object_attachments
            .get(&(proto.zone_id, proto.id))
            .cloned();
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
            if let Some(ref keys) = trigger_keys {
                bundle.insert(AttachedTriggers(keys.clone()));
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
            let trigger_keys = world
                .resource::<TriggerCatalog>()
                .object_attachments
                .get(&(proto.zone_id, proto.id))
                .cloned();
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
                    if let Some(ref keys) = trigger_keys {
                        bundle.insert(AttachedTriggers(keys.clone()));
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

/// Initial weather state for a zone given its `Climate`. Picks a
/// resting-state band; the weather tick will drift it from here.
/// Strip raw ANSI SGR escape sequences (`ESC[...m`) from a
/// string. Legacy `CircleMUD` content baked color codes directly
/// into object names; those leak into XML-Lite-rendered output as
/// literal escape codes for color-off clients and break visible-
/// width math for everyone. Runtime workaround until the
/// fierylib import pass rewrites them to XML-Lite tags.
fn strip_ansi(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // SGR sequence: skip until 'm'.
            i += 2;
            while i < bytes.len() && bytes[i] != b'm' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // consume the 'm'
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Re-query the trigger catalog from the live DB and return a
/// freshly-built `TriggerCatalog`. Caller is responsible for
/// swapping it into the world's resource slot and refreshing any
/// per-entity `AttachedTriggers` components — this just builds
/// the catalog from rows. Pulled out of `load_from_db` so the
/// admin `reload_triggers` endpoint can rebuild without
/// restarting the process.
pub async fn load_trigger_catalog(pool: &PgPool) -> sqlx::Result<TriggerCatalog> {
    let trigger_rows = triggers::list_triggers(pool).await?;
    let mut catalog = TriggerCatalog::default();
    for row in trigger_rows {
        let attach = match row.attach_type {
            triggers::ScriptType::Mob => TriggerAttach::Mob,
            triggers::ScriptType::Object => TriggerAttach::Object,
            triggers::ScriptType::World => TriggerAttach::World,
        };
        let flags = row
            .flags
            .unwrap_or_default()
            .into_iter()
            .map(map_trigger_flag)
            .collect();
        catalog.by_key.insert(
            (row.zone_id, row.id),
            TriggerDef {
                zone_id: row.zone_id,
                id: row.id,
                name: row.name,
                attach_type: attach,
                commands: row.commands,
                flags,
                arg_list: row.arg_list.unwrap_or_default(),
                num_args: row.num_args,
            },
        );
    }
    for link in triggers::list_mob_triggers(pool).await? {
        catalog
            .mob_attachments
            .entry((link.mob_zone_id, link.mob_id))
            .or_default()
            .push((link.trigger_zone_id, link.trigger_id));
    }
    for link in triggers::list_object_triggers(pool).await? {
        catalog
            .object_attachments
            .entry((link.object_zone_id, link.object_id))
            .or_default()
            .push((link.trigger_zone_id, link.trigger_id));
    }
    for link in triggers::list_room_triggers(pool).await? {
        catalog
            .room_attachments
            .entry((link.room_zone_id, link.room_id))
            .or_default()
            .push((link.trigger_zone_id, link.trigger_id));
    }
    Ok(catalog)
}

/// Mapping from the DB enum to the runtime enum. Same shape as the
/// inline match in `load_from_db`'s pass 4.7; extracted so
/// `load_trigger_catalog` can reuse it without churning the loader.
fn map_trigger_flag(flag: triggers::TriggerFlag) -> TriggerEvent {
    match flag {
        triggers::TriggerFlag::Global => TriggerEvent::Global,
        triggers::TriggerFlag::Random => TriggerEvent::Random,
        triggers::TriggerFlag::Command => TriggerEvent::Command,
        triggers::TriggerFlag::Load => TriggerEvent::Load,
        triggers::TriggerFlag::Cast => TriggerEvent::Cast,
        triggers::TriggerFlag::Leave => TriggerEvent::Leave,
        triggers::TriggerFlag::Time => TriggerEvent::Time,
        triggers::TriggerFlag::Speech => TriggerEvent::Speech,
        triggers::TriggerFlag::Act => TriggerEvent::Act,
        triggers::TriggerFlag::Death => TriggerEvent::Death,
        triggers::TriggerFlag::Greet => TriggerEvent::Greet,
        triggers::TriggerFlag::GreetAll => TriggerEvent::GreetAll,
        triggers::TriggerFlag::Entry => TriggerEvent::Entry,
        triggers::TriggerFlag::Receive => TriggerEvent::Receive,
        triggers::TriggerFlag::Fight => TriggerEvent::Fight,
        triggers::TriggerFlag::HitPercent => TriggerEvent::HitPercent,
        triggers::TriggerFlag::Bribe => TriggerEvent::Bribe,
        triggers::TriggerFlag::Memory => TriggerEvent::Memory,
        triggers::TriggerFlag::Door => TriggerEvent::Door,
        triggers::TriggerFlag::SpeechTo => TriggerEvent::SpeechTo,
        triggers::TriggerFlag::Look => TriggerEvent::Look,
        triggers::TriggerFlag::Auto => TriggerEvent::Auto,
        triggers::TriggerFlag::Attack => TriggerEvent::Attack,
        triggers::TriggerFlag::Defend => TriggerEvent::Defend,
        triggers::TriggerFlag::Timer => TriggerEvent::Timer,
        triggers::TriggerFlag::Get => TriggerEvent::Get,
        triggers::TriggerFlag::Drop => TriggerEvent::Drop,
        triggers::TriggerFlag::Give => TriggerEvent::Give,
        triggers::TriggerFlag::Wear => TriggerEvent::Wear,
        triggers::TriggerFlag::Remove => TriggerEvent::Remove,
        triggers::TriggerFlag::Use => TriggerEvent::Use,
        triggers::TriggerFlag::Consume => TriggerEvent::Consume,
        triggers::TriggerFlag::Reset => TriggerEvent::Reset,
        triggers::TriggerFlag::Preentry => TriggerEvent::Preentry,
        triggers::TriggerFlag::Postentry => TriggerEvent::Postentry,
    }
}

/// `NONE` zones are treated as Mild + Clear (indoor / abstract zones
/// like the Void / Limbo never see player-visible weather anyway).
#[must_use]
pub fn default_weather_for_climate(
    climate: mud_db::enums::Climate,
) -> crate::resources::WeatherState {
    use crate::resources::{PrecipKind, TempBand, WeatherState};
    use mud_db::enums::Climate;
    let (temp, precip) = match climate {
        Climate::None => (TempBand::Mild, PrecipKind::Clear),
        Climate::Arid | Climate::Semiarid => (TempBand::Hot, PrecipKind::Clear),
        Climate::Tropical => (TempBand::Hot, PrecipKind::Cloudy),
        Climate::Subtropical => (TempBand::Warm, PrecipKind::Clear),
        // Temperate / Subarctic / Alpine all happen to rest on
        // (Cold|Mild, Cloudy). Different climates, same default
        // start state — drift will diverge them quickly.
        Climate::Temperate => (TempBand::Mild, PrecipKind::Cloudy),
        Climate::Oceanic => (TempBand::Cool, PrecipKind::Drizzle),
        Climate::Subarctic | Climate::Alpine => (TempBand::Cold, PrecipKind::Cloudy),
        Climate::Arctic => (TempBand::Frigid, PrecipKind::Snow),
    };
    WeatherState { temp, precip }
}

/// Pick a single primary `Slot` from a list of `WearFlag`s. Most
/// items in legacy data carry exactly one flag; for the few that
/// have several (e.g. ring on FINGER + neck-pendant on NECK) we
/// prefer the more common-use slot.
#[must_use]
pub fn wear_flags_primary_slot(flags: &[mud_db::enums::WearFlag]) -> Option<crate::components::Slot> {
    use crate::components::Slot;
    use mud_db::enums::WearFlag::{
        About, Arms, Badge, Belt, Body, Disguise, Ear, Eyes, Face, Feet, Finger, Hands, Head,
        Hover, Legs, Mainhand, Neck, Offhand, Tail, Twohand, Waist, Wrist,
    };
    // Priority order: prefer wield/hold/body slots over decorative.
    // Tail and Disguise have no in-game slot equivalent yet — silently
    // ignored. Everything else maps to a Slot variant.
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
            Wrist => Some(Slot::Wrist),
            About => Some(Slot::About),
            Eyes => Some(Slot::Eyes),
            Face => Some(Slot::Face),
            Ear => Some(Slot::Ears),
            Hover => Some(Slot::Hover),
            Badge => Some(Slot::Badge),
            // Tail / Disguise have no anatomical slot in v1.
            Tail | Disguise => None,
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

/// Pull `Destination` from a Portal's `values` JSONB. Stored either
/// as a number or a string in the legacy data; `0` (or missing /
/// non-portal) is treated as "no destination".
fn parse_portal_destination(values: &serde_json::Value) -> Option<i32> {
    let v = values.get("Destination")?;
    let raw = match v {
        serde_json::Value::Number(n) => i32::try_from(n.as_i64().unwrap_or(0)).unwrap_or(0),
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    };
    if raw <= 0 { None } else { Some(raw) }
}

/// Parse a `DrinkContainer`'s liquid state from its `values` JSONB.
/// Schema shape: `{Liquid: "WATER", Capacity: 256, Poisoned: false,
/// Remaining: 256}`. Numbers come as either Number or String; bools
/// come as Bool or String. Missing fields default to safe values
/// (capacity 0 means uninitialized — runtime treats it as empty).
/// Parse a Light's fuel state from its `values` JSONB. Schema
/// shape: `{"Capacity": 240, "Remaining": 240, "Is_Lit:": false}`.
/// Numbers can be int or string; missing fields default to -1
/// (infinite). The `Is_Lit:` flag is loader-time hint only — at
/// runtime, the Lit marker comes from `cmd_light` toggles.
fn parse_light_fuel(values: &serde_json::Value) -> crate::resources::LightFuelProto {
    let parse_int = |v: Option<&serde_json::Value>| -> i32 {
        match v {
            Some(serde_json::Value::Number(n)) => {
                i32::try_from(n.as_i64().unwrap_or(-1)).unwrap_or(-1)
            }
            Some(serde_json::Value::String(s)) => s.parse().unwrap_or(-1),
            _ => -1,
        }
    };
    let capacity = parse_int(values.get("Capacity"));
    let remaining = parse_int(values.get("Remaining"));
    crate::resources::LightFuelProto { capacity, remaining }
}

fn parse_liquid(values: &serde_json::Value) -> Option<LiquidProto> {
    let liquid = values.get("Liquid")?.as_str().unwrap_or("WATER").to_string();
    let parse_int = |v: Option<&serde_json::Value>| -> i32 {
        match v {
            Some(serde_json::Value::Number(n)) => i32::try_from(n.as_i64().unwrap_or(0)).unwrap_or(0),
            Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0),
            _ => 0,
        }
    };
    let parse_bool = |v: Option<&serde_json::Value>| -> bool {
        match v {
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::String(s)) => s.eq_ignore_ascii_case("true"),
            _ => false,
        }
    };
    Some(LiquidProto {
        liquid,
        capacity: parse_int(values.get("Capacity")),
        remaining: parse_int(values.get("Remaining")),
        poisoned: parse_bool(values.get("Poisoned")),
    })
}

/// Pull `Pages` from a Board-typed object's `values` JSONB. Legacy
/// `CircleMUD` convention has `Pages` doubling as the `Board.id` —
/// every imported BOARD row carries this field, and matches
/// 1..=8 for the eight known boards. `0` / missing → None.
fn parse_board_id(values: &serde_json::Value) -> Option<i32> {
    let v = values.get("Pages")?;
    let raw = match v {
        serde_json::Value::Number(n) => i32::try_from(n.as_i64().unwrap_or(0)).unwrap_or(0),
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    };
    if raw <= 0 { None } else { Some(raw) }
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
