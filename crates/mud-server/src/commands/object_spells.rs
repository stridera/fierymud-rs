//! Spells that act on an object (legacy `mag_alter_obj`): Curse sets
//! `NO_DROP` (and shrinks a weapon's dice), Remove Curse undoes both.
//!
//! The behaviour is data: an `alter_object` effect row on the ability
//! carries `restriction` (`NO_DROP`), `mode` (`add` | `remove`),
//! `weaponDiceSizeDelta` and the caster / room messages (`{item}` is the
//! object's name). Nothing here knows the spells by name.

use bevy_ecs::prelude::*;
use mud_db::enums::ObjectRestriction;
use mud_world::components::WeaponDiceSizeAdjust;
use mud_world::{
    AbilityDef, EffectCatalog, EquippedSlot, Item, Located, ObjectPrototypes, ObjectRestrictions,
    WorldKey,
};

use super::{AbilityCatalog, EffectSpec, broadcast_room_except_rendered, name_or, send_to};

/// `alter_object` effect specs bound to `def`, in order.
pub(super) fn alter_object_specs(world: &World, def: &AbilityDef) -> Vec<EffectSpec> {
    let Some(mappings) = world.resource::<AbilityCatalog>().effects_for.get(&def.id) else {
        return Vec::new();
    };
    let effects = world.resource::<EffectCatalog>();
    mappings
        .iter()
        .filter_map(|(id, override_params)| {
            let e = effects.by_id.get(id)?;
            (e.effect_type == "alter_object").then(|| EffectSpec {
                id: *id,
                name: e.name.clone(),
                effect_type: e.effect_type.clone(),
                override_params: override_params.clone(),
                default_params: e.default_params.clone(),
            })
        })
        .collect()
}

fn param<'a>(spec: &'a EffectSpec, key: &str) -> Option<&'a serde_json::Value> {
    spec.override_params
        .as_ref()
        .and_then(|p| p.get(key))
        .or_else(|| spec.default_params.get(key))
}

fn param_str<'a>(spec: &'a EffectSpec, key: &str) -> Option<&'a str> {
    param(spec, key).and_then(serde_json::Value::as_str)
}

/// True when the spec removes the restriction rather than adding it.
pub(super) fn removes(spec: &EffectSpec) -> bool {
    param_str(spec, "mode").is_some_and(|m| m.eq_ignore_ascii_case("remove"))
}

/// Run one `alter_object` spec on `item`. Returns true when the object
/// changed (already cursed / nothing to lift reports the spec's
/// `noopMessageToCaster` instead).
pub(super) fn apply_alter_object(
    world: &mut World,
    caster: Entity,
    item: Entity,
    spec: &EffectSpec,
) -> bool {
    let Some(restriction) = param_str(spec, "restriction").and_then(ObjectRestriction::from_db_str)
    else {
        return false;
    };
    let adding = !removes(spec);
    let item_name = name_or(world, item, "it");
    let has = world
        .get::<ObjectRestrictions>(item)
        .is_some_and(|r| r.has(restriction));
    let say = |template: Option<&str>| template.map(|t| t.replace("{item}", &item_name));
    if adding == has {
        if let Some(msg) = say(param_str(spec, "noopMessageToCaster")) {
            send_to(world, caster, format!("{msg}\r\n"));
        }
        return false;
    }
    if adding {
        if let Some(mut r) = world.get_mut::<ObjectRestrictions>(item) {
            r.0.push(restriction);
        } else {
            world
                .entity_mut(item)
                .insert(ObjectRestrictions(vec![restriction]));
        }
    } else if let Some(mut r) = world.get_mut::<ObjectRestrictions>(item) {
        r.0.retain(|x| *x != restriction);
    }
    #[allow(clippy::cast_possible_truncation)]
    let delta = param(spec, "weaponDiceSizeDelta")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0) as i32;
    if delta != 0 && is_weapon(world, item) {
        let current = world.get::<WeaponDiceSizeAdjust>(item).map_or(0, |a| a.0);
        world
            .entity_mut(item)
            .insert(WeaponDiceSizeAdjust(current + delta));
    }
    crate::item_alter::mark_dirty(world, item);
    if let Some(msg) = say(param_str(spec, "messageToCaster")) {
        send_to(world, caster, format!("{msg}\r\n"));
    }
    if let Some(msg) = say(param_str(spec, "messageToRoom"))
        && let Some(room) = containing_room(world, item)
    {
        broadcast_room_except_rendered(world, room, &[caster], &format!("{msg}\r\n"));
    }
    true
}

fn is_weapon(world: &World, item: Entity) -> bool {
    let Some(key) = world.get::<WorldKey>(item) else {
        return false;
    };
    world
        .get_resource::<ObjectPrototypes>()
        .and_then(|p| p.by_key.get(&(key.zone, key.id)))
        .is_some_and(|p| p.weapon_dice_num > 0)
}

/// The room an object is in, directly or via whoever carries it.
fn containing_room(world: &World, item: Entity) -> Option<Entity> {
    let holder = world.get::<Located>(item)?.0;
    if world.get::<Item>(holder).is_none()
        && world.get::<mud_world::Mob>(holder).is_none()
        && world.get::<mud_world::Player>(holder).is_none()
    {
        return Some(holder);
    }
    world.get::<Located>(holder).map(|l| l.0)
}

/// Cast on an object. False when the ability has no `alter_object`
/// effect (the caller falls through to the ordinary effect loop); true
/// when it was handled here (the cast is spent whether or not the object
/// changed, as in legacy).
pub(super) fn cast_on_object(
    world: &mut World,
    caster: Entity,
    def: &AbilityDef,
    item: Entity,
) -> bool {
    let specs = alter_object_specs(world, def);
    for spec in &specs {
        apply_alter_object(world, caster, item, spec);
    }
    !specs.is_empty()
}

/// Legacy `spell_remove_curse` on a person with no curse effect: lift the
/// restriction from the first carried (not worn) object that has it.
/// Returns true if an object was changed.
pub(super) fn lift_first_carried(
    world: &mut World,
    caster: Entity,
    holder: Entity,
    spec: &EffectSpec,
) -> bool {
    let Some(restriction) = param_str(spec, "restriction").and_then(ObjectRestriction::from_db_str)
    else {
        return false;
    };
    let mut candidates: Vec<Entity> = {
        let mut q =
            world.query_filtered::<(Entity, &Located), (With<Item>, Without<EquippedSlot>)>();
        q.iter(world)
            .filter(|(_, l)| l.0 == holder)
            .map(|(e, _)| e)
            .collect()
    };
    candidates.sort_by_key(|e| e.index());
    let Some(item) = candidates.into_iter().find(|e| {
        world
            .get::<ObjectRestrictions>(*e)
            .is_some_and(|r| r.has(restriction))
    }) else {
        return false;
    };
    apply_alter_object(world, caster, item, spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::invoke_ability_with;
    use crate::commands::test_support::{Rx, ability_def, drain, object_proto, player_in};
    use mud_db::abilities::AbilityKind;
    use mud_db::enums::ObjectType;
    use mud_world::{
        EffectDef, Keywords, KnownAbilities, Mob, Named, ObjectPrototypes, SpellSlotData,
    };

    const CURSE: i32 = 1;
    const REMOVE_CURSE: i32 = 2;
    const ALTER: i32 = 7;
    const CLEANSE: i32 = 8;

    fn effect_def(id: i32, name: &str, effect_type: &str) -> EffectDef {
        EffectDef {
            id,
            name: name.to_string(),
            description: None,
            effect_type: effect_type.to_string(),
            tags: Vec::new(),
            presence_override: None,
            default_params: serde_json::json!({}),
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        }
    }

    fn targeting(targets: &[&str]) -> mud_world::TargetingRule {
        mud_world::TargetingRule {
            valid_targets: targets.iter().map(|s| (*s).to_string()).collect(),
            scope: "SINGLE".to_string(),
            max_targets: 1,
            require_los: false,
        }
    }

    /// Curse and Remove Curse wired exactly like the data patch: an
    /// `alter_object` effect row each, plus the targeting rows.
    fn world_with_curses() -> (World, Entity, Entity, Rx) {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let mut catalog = AbilityCatalog::default();
        let mut curse = ability_def(CURSE, "Curse", AbilityKind::Spell);
        curse.violent = true;
        curse.cast_time_rounds = 0;
        let mut remove = ability_def(REMOVE_CURSE, "Remove Curse", AbilityKind::Spell);
        remove.cast_time_rounds = 0;
        catalog.by_name.insert("curse".into(), curse);
        catalog.by_name.insert("remove_curse".into(), remove);
        catalog.effects_for.insert(
            CURSE,
            vec![(
                ALTER,
                Some(serde_json::json!({
                    "restriction": "NO_DROP", "mode": "add", "weaponDiceSizeDelta": -1,
                    "messageToCaster": "{item} briefly glows red.",
                    "messageToRoom": "{item} briefly glows red."
                })),
            )],
        );
        catalog.effects_for.insert(
            REMOVE_CURSE,
            vec![
                (
                    CLEANSE,
                    Some(serde_json::json!({"condition": "curse", "scope": "all"})),
                ),
                (
                    ALTER,
                    Some(serde_json::json!({
                        "restriction": "NO_DROP", "mode": "remove", "weaponDiceSizeDelta": 1,
                        "messageToCaster": "{item} briefly glows blue.",
                        "noopMessageToCaster": "You do not sense any foul magicks upon {item}."
                    })),
                ),
            ],
        );
        catalog.targeting.insert(
            CURSE,
            targeting(&["ENEMY_PC", "ENEMY_NPC", "OBJECT_INV", "OBJECT_WORLD"]),
        );
        catalog.targeting.insert(
            REMOVE_CURSE,
            targeting(&["SELF", "ALLY_PC", "ALLY_NPC", "OBJECT_INV", "OBJECT_WORLD"]),
        );
        world.insert_resource(catalog);
        let mut effects = EffectCatalog::default();
        effects
            .by_id
            .insert(ALTER, effect_def(ALTER, "alter_object", "alter_object"));
        effects
            .by_id
            .insert(CLEANSE, effect_def(CLEANSE, "cleanse", "cleanse"));
        world.insert_resource(effects);
        world.insert_resource(SpellSlotData::default());
        world.insert_resource(mud_world::ClassSkillsData::default());
        let (caster, rx) = player_in(&mut world, room);
        world.entity_mut(caster).insert(KnownAbilities {
            entries: vec![(CURSE, 500, true), (REMOVE_CURSE, 500, true)],
        });
        (world, room, caster, rx)
    }

    fn item(world: &mut World, name: &str, keyword: &str, holder: Entity) -> Entity {
        world
            .spawn((
                Item,
                Named {
                    name: name.to_string(),
                },
                Keywords(vec![keyword.to_string()]),
                Located(holder),
            ))
            .id()
    }

    fn cast(world: &mut World, caster: Entity, spell: &str, target: &str) {
        invoke_ability_with(
            world,
            caster,
            &format!("'{spell}' {target}"),
            AbilityKind::Spell,
            "cast",
            false,
            false,
            false,
            None,
        );
    }

    fn cursed(world: &World, e: Entity) -> bool {
        world
            .get::<ObjectRestrictions>(e)
            .is_some_and(|r| r.has(ObjectRestriction::NoDrop))
    }

    #[test]
    fn curse_a_carried_item_makes_it_undroppable() {
        let (mut world, _room, caster, mut rx) = world_with_curses();
        let helm = item(&mut world, "a shiny helm", "helm", caster);
        cast(&mut world, caster, "curse", "helm");
        assert!(cursed(&world, helm));
        assert!(drain(&mut rx).contains("a shiny helm briefly glows red."));
    }

    #[test]
    fn curse_an_item_on_the_floor_targets_the_room() {
        let (mut world, room, caster, _rx) = world_with_curses();
        let rug = item(&mut world, "a rug", "rug", room);
        cast(&mut world, caster, "curse", "rug");
        assert!(cursed(&world, rug));
    }

    #[test]
    fn curse_still_targets_mobs_and_keeps_the_item_untouched() {
        let (mut world, room, caster, _rx) = world_with_curses();
        let bag = item(&mut world, "a goblin's bag", "bag", caster);
        let goblin = world
            .spawn((
                Mob,
                Named {
                    name: "a goblin".into(),
                },
                Keywords(vec!["goblin".into()]),
                Located(room),
            ))
            .id();
        cast(&mut world, caster, "curse", "goblin");
        assert!(!cursed(&world, bag), "the mob was the target, not the bag");
        assert!(world.get_entity(goblin).is_ok());
    }

    #[test]
    fn curse_a_weapon_also_shrinks_its_die_and_remove_curse_restores_it() {
        let (mut world, _room, caster, mut rx) = world_with_curses();
        let mut p = object_proto(30, 3, ObjectType::Weapon);
        p.weapon_dice_num = 2;
        p.weapon_dice_size = 6;
        let mut protos = ObjectPrototypes::default();
        protos.by_key.insert((30, 3), p);
        world.insert_resource(protos);
        let sword = item(&mut world, "a long sword", "sword", caster);
        world.entity_mut(sword).insert(WorldKey { zone: 30, id: 3 });
        cast(&mut world, caster, "curse", "sword");
        assert_eq!(
            world.get::<WeaponDiceSizeAdjust>(sword).map(|a| a.0),
            Some(-1)
        );
        // A second curse changes nothing (already NO_DROP).
        cast(&mut world, caster, "curse", "sword");
        assert_eq!(
            world.get::<WeaponDiceSizeAdjust>(sword).map(|a| a.0),
            Some(-1)
        );
        drain(&mut rx);
        cast(&mut world, caster, "remove curse", "sword");
        assert!(!cursed(&world, sword));
        assert_eq!(
            world.get::<WeaponDiceSizeAdjust>(sword).map(|a| a.0),
            Some(0)
        );
        assert!(drain(&mut rx).contains("a long sword briefly glows blue."));
    }

    #[test]
    fn curse_and_remove_curse_mark_the_item_for_saving() {
        use mud_world::components::ItemAlterDirty;
        let (mut world, _room, caster, _rx) = world_with_curses();
        let helm = item(&mut world, "a helm", "helm", caster);
        assert!(world.get::<ItemAlterDirty>(helm).is_none());
        cast(&mut world, caster, "curse", "helm");
        assert!(world.get::<ItemAlterDirty>(helm).is_some());
        world.entity_mut(helm).remove::<ItemAlterDirty>();
        cast(&mut world, caster, "remove curse", "helm");
        assert!(world.get::<ItemAlterDirty>(helm).is_some());
    }

    #[test]
    fn remove_curse_on_a_clean_item_senses_nothing() {
        let (mut world, _room, caster, mut rx) = world_with_curses();
        let _ring = item(&mut world, "a ring", "ring", caster);
        cast(&mut world, caster, "remove curse", "ring");
        assert!(drain(&mut rx).contains("You do not sense any foul magicks upon a ring."));
    }

    #[test]
    fn remove_curse_on_a_person_lifts_the_first_cursed_carried_item() {
        let (mut world, _room, caster, _rx) = world_with_curses();
        let helm = item(&mut world, "a shiny helm", "helm", caster);
        world
            .entity_mut(helm)
            .insert(ObjectRestrictions(vec![ObjectRestriction::NoDrop]));
        cast(&mut world, caster, "remove curse", "me");
        assert!(!cursed(&world, helm));
    }

    /// Start a real wind-up (`cast_time_rounds` as authored) and tick it
    /// until it resolves or aborts.
    fn wind_up_and_finish(world: &mut World, caster: Entity, spell: &str, target: &str) {
        invoke_ability_with(
            world,
            caster,
            &format!("'{spell}' {target}"),
            AbilityKind::Spell,
            "cast",
            false,
            false,
            false,
            None,
        );
        for _ in 0..200 {
            if world.get::<mud_world::Casting>(caster).is_none() {
                break;
            }
            crate::casting::casting_tick(world);
        }
        assert!(world.get::<mud_world::Casting>(caster).is_none());
    }

    fn real_cast_times(world: &mut World) {
        let mut cat = world.resource_mut::<AbilityCatalog>();
        cat.by_name.get_mut("curse").unwrap().cast_time_rounds = 1;
        cat.by_name
            .get_mut("remove_curse")
            .unwrap()
            .cast_time_rounds = 2;
    }

    #[test]
    fn winding_up_curse_on_a_floor_item_lands() {
        let (mut world, room, caster, mut rx) = world_with_curses();
        real_cast_times(&mut world);
        world
            .entity_mut(caster)
            .insert(mud_world::Health { hp: 50, max: 50 });
        let rug = item(&mut world, "a rug", "rug", room);
        wind_up_and_finish(&mut world, caster, "curse", "rug");
        assert!(cursed(&world, rug), "{}", drain(&mut rx));
    }

    #[test]
    fn winding_up_curse_on_a_carried_item_lands() {
        let (mut world, _room, caster, _rx) = world_with_curses();
        real_cast_times(&mut world);
        world
            .entity_mut(caster)
            .insert(mud_world::Health { hp: 50, max: 50 });
        let helm = item(&mut world, "a helm", "helm", caster);
        wind_up_and_finish(&mut world, caster, "curse", "helm");
        assert!(cursed(&world, helm));
    }

    #[test]
    fn winding_up_remove_curse_on_floor_and_carried_items_lands() {
        let (mut world, room, caster, _rx) = world_with_curses();
        real_cast_times(&mut world);
        world
            .entity_mut(caster)
            .insert(mud_world::Health { hp: 50, max: 50 });
        let rug = item(&mut world, "a rug", "rug", room);
        let helm = item(&mut world, "a helm", "helm", caster);
        for e in [rug, helm] {
            world
                .entity_mut(e)
                .insert(ObjectRestrictions(vec![ObjectRestriction::NoDrop]));
        }
        wind_up_and_finish(&mut world, caster, "remove curse", "rug");
        wind_up_and_finish(&mut world, caster, "remove curse", "helm");
        assert!(!cursed(&world, rug) && !cursed(&world, helm));
    }

    #[test]
    fn floor_item_taken_mid_cast_aborts_the_wind_up() {
        let (mut world, room, caster, mut rx) = world_with_curses();
        real_cast_times(&mut world);
        world
            .entity_mut(caster)
            .insert(mud_world::Health { hp: 50, max: 50 });
        let rug = item(&mut world, "a rug", "rug", room);
        let thief = world
            .spawn((
                Named {
                    name: "Thief".into(),
                },
                Located(room),
            ))
            .id();
        invoke_ability_with(
            &mut world,
            caster,
            "'curse' rug",
            AbilityKind::Spell,
            "cast",
            false,
            false,
            false,
            None,
        );
        assert!(world.get::<mud_world::Casting>(caster).is_some());
        drain(&mut rx);
        world.entity_mut(rug).insert(Located(thief));
        crate::casting::casting_tick(&mut world);
        assert!(world.get::<mud_world::Casting>(caster).is_none());
        assert!(drain(&mut rx).contains("You stop chanting abruptly!"));
        assert!(!cursed(&world, rug), "the curse never landed");
    }

    #[test]
    fn restriction_names_parse_from_db_spelling() {
        assert_eq!(
            ObjectRestriction::from_db_str("NO_DROP"),
            Some(ObjectRestriction::NoDrop)
        );
        assert_eq!(ObjectRestriction::from_db_str("nonsense"), None);
    }
}
