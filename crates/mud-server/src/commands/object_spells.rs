//! Spells that act on an object (legacy `mag_alter_obj`): Curse sets
//! `NO_DROP` (and shrinks a weapon's dice), Remove Curse undoes both.
//! Enchant Weapon (legacy `spell_enchant_weapon`) is the `enchant` mode.
//!
//! The behaviour is data: an `alter_object` effect row on the ability
//! carries `restriction` (`NO_DROP`), `mode` (`add` | `remove` | `enchant`),
//! `weaponDiceSizeDelta` and the caster / room messages (`{item}` is the
//! object's name). Nothing here knows the spells by name.
//!
//! `enchant` params: `requireType` (object type that can be enchanted),
//! `setFlags` (object flags set on success; an item already carrying one,
//! or already carrying any stat apply, is refused), `applies` (list of
//! `{target, amount}`, `amount` a number or formula in `skill` / `level`),
//! `goodCasterBars` / `evilCasterBars` (alignment the enchanted item then
//! refuses) and `messageToCasterGood` / `Evil` / `Neutral`.

use bevy_ecs::prelude::*;
use mud_db::enums::{Alignment, ObjectFlag, ObjectRestriction};
use mud_world::components::{ItemApplies, ItemBarredAlignments, WeaponDiceSizeAdjust};
use mud_world::{
    AbilityDef, CombatStats, EffectCatalog, EquippedSlot, Item, Located, ObjectFlags,
    ObjectPrototypes, ObjectRestrictions, WorldKey,
};

use super::{
    AbilityCatalog, EffectSpec, FormulaCtx, broadcast_room_except_rendered, name_or,
    numeric_or_formula, send_to,
};

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
    ctx: &FormulaCtx,
) -> bool {
    if param_str(spec, "mode").is_some_and(|m| m.eq_ignore_ascii_case("enchant")) {
        return apply_enchant(world, caster, item, spec, ctx);
    }
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

/// Parse an alignment bucket spelled `GOOD` / `NEUTRAL` / `EVIL`.
fn parse_alignment(s: &str) -> Option<Alignment> {
    match s.trim().to_ascii_uppercase().as_str() {
        "GOOD" => Some(Alignment::Good),
        "NEUTRAL" => Some(Alignment::Neutral),
        "EVIL" => Some(Alignment::Evil),
        _ => None,
    }
}

fn parse_object_flag(s: &str) -> Option<ObjectFlag> {
    match s.trim().to_ascii_uppercase().as_str() {
        "GLOW" => Some(ObjectFlag::Glow),
        "HUM" => Some(ObjectFlag::Hum),
        "INVISIBLE" => Some(ObjectFlag::Invisible),
        "MAGIC" => Some(ObjectFlag::Magic),
        _ => None,
    }
}

fn param_strs(spec: &EffectSpec, key: &str) -> Vec<String> {
    param(spec, key)
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Legacy `spell_enchant_weapon`: a non-magical weapon with no stat
/// applies gains the spec's applies and the `MAGIC` flag, and refuses the
/// opposite alignment of the caster. Anything else is refused (the cast is
/// still spent, silently unless the spec has `noopMessageToCaster`).
/// Returns true when the object changed.
fn apply_enchant(
    world: &mut World,
    caster: Entity,
    item: Entity,
    spec: &EffectSpec,
    ctx: &FormulaCtx,
) -> bool {
    let item_name = name_or(world, item, "it");
    let say = |template: Option<&str>| template.map(|t| t.replace("{item}", &item_name));
    let proto = world.get::<WorldKey>(item).and_then(|k| {
        world
            .get_resource::<ObjectPrototypes>()
            .and_then(|p| p.by_key.get(&(k.zone, k.id)))
            .cloned()
    });
    let type_ok = match (&proto, param_str(spec, "requireType")) {
        (Some(p), Some(t)) => format!("{:?}", p.r#type).eq_ignore_ascii_case(&t.replace('_', "")),
        (Some(_), None) => true,
        (None, _) => false,
    };
    let set_flags: Vec<ObjectFlag> = param_strs(spec, "setFlags")
        .iter()
        .filter_map(|f| parse_object_flag(f))
        .collect();
    let has_flag = |f: ObjectFlag| {
        world
            .get::<ObjectFlags>(item)
            .is_some_and(|flags| flags.has(f))
    };
    let proto_applies = proto.as_ref().is_some_and(|p| {
        let catalog = world.get_resource::<EffectCatalog>();
        p.granted_effects.iter().any(|g| {
            catalog
                .and_then(|c| c.by_id.get(&g.effect_id))
                .is_some_and(|d| d.effect_type == "modify")
                && g.modifier_data
                    .get("amount")
                    .and_then(serde_json::Value::as_i64)
                    .is_some_and(|a| a != 0)
        })
    });
    let has_applies = proto_applies
        || world
            .get::<ItemApplies>(item)
            .is_some_and(|a| !a.0.is_empty());
    if !type_ok || set_flags.iter().any(|f| has_flag(*f)) || has_applies {
        if let Some(msg) = say(param_str(spec, "noopMessageToCaster")) {
            send_to(world, caster, format!("{msg}\r\n"));
        }
        return false;
    }
    let applies: Vec<(String, i32)> = param(spec, "applies")
        .and_then(serde_json::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let target = row.get("target")?.as_str()?.to_string();
                    let amount = numeric_or_formula(row.get("amount")?, ctx)?;
                    Some((target, amount))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut flags = world
        .get::<ObjectFlags>(item)
        .map(|f| f.0.clone())
        .unwrap_or_default();
    for f in set_flags {
        if !flags.contains(&f) {
            flags.push(f);
        }
    }
    let alignment =
        Alignment::from_score(world.get::<CombatStats>(caster).map_or(0, |c| c.alignment));
    let (bars, message) = match alignment {
        Alignment::Good => (param_str(spec, "goodCasterBars"), "messageToCasterGood"),
        Alignment::Evil => (param_str(spec, "evilCasterBars"), "messageToCasterEvil"),
        Alignment::Neutral => (None, "messageToCasterNeutral"),
    };
    let mut em = world.entity_mut(item);
    em.insert(ObjectFlags(flags));
    if !applies.is_empty() {
        em.insert(ItemApplies(applies.clone()));
    }
    if let Some(bar) = bars.and_then(parse_alignment) {
        let mut barred = em
            .get::<ItemBarredAlignments>()
            .map(|b| b.0.clone())
            .unwrap_or_default();
        if !barred.contains(&bar) {
            barred.push(bar);
        }
        em.insert(ItemBarredAlignments(barred));
    }
    crate::equip_apply::grant_applies_to_worn(world, item, &applies);
    crate::item_alter::mark_dirty(world, item);
    if let Some(msg) = say(param_str(spec, message)) {
        send_to(world, caster, format!("{msg}\r\n"));
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
    let ctx = super::caster_formula_ctx(world, caster, def.id);
    for spec in &specs {
        apply_alter_object(world, caster, item, spec, &ctx);
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
    apply_alter_object(world, caster, item, spec, &FormulaCtx::default())
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

    /// Spawn an effect the way the cast pipeline names it: by what it
    /// modifies, tagged with the ability that put it there.
    fn effect_from(world: &mut World, on: Entity, label: &str, ability: i32) -> Entity {
        world
            .spawn((
                mud_world::EffectInstance {
                    kind: 0,
                    name: label.to_string(),
                    strength: -1,
                    remaining_secs: 600,
                    source: mud_world::EffectSource::Spell,
                    ability_id: Some(ability),
                },
                mud_world::AppliedTo(on),
            ))
            .id()
    }

    #[test]
    fn remove_curse_on_a_person_lifts_the_curse_debuff_whatever_it_is_labelled() {
        let (mut world, _room, caster, mut rx) = world_with_curses();
        // Curse's debuff sits under its stat label (`acc`), not "curse".
        let debuff = effect_from(&mut world, caster, "acc", CURSE);
        let other = effect_from(&mut world, caster, "acc", 999);
        let helm = item(&mut world, "a shiny helm", "helm", caster);
        world
            .entity_mut(helm)
            .insert(ObjectRestrictions(vec![ObjectRestriction::NoDrop]));
        cast(&mut world, caster, "remove curse", "me");
        assert!(world.get_entity(debuff).is_err(), "{}", drain(&mut rx));
        assert!(
            world.get_entity(other).is_ok(),
            "another spell's effect with the same label stays"
        );
        assert!(
            cursed(&world, helm),
            "the person had a curse effect, so the carried item is left alone"
        );
    }

    #[test]
    fn lifting_the_curse_debuff_gives_back_its_stat_penalty() {
        let (mut world, _room, caster, _rx) = world_with_curses();
        world.entity_mut(caster).insert(mud_world::CombatStats {
            accuracy: 49,
            ..Default::default()
        });
        let debuff = effect_from(&mut world, caster, "accuracy", CURSE);
        world.entity_mut(debuff).insert(mud_world::ModifyDelta {
            target: "accuracy".into(),
            amount: -1,
        });
        cast(&mut world, caster, "remove curse", "me");
        assert!(world.get_entity(debuff).is_err());
        assert_eq!(
            world
                .get::<mud_world::CombatStats>(caster)
                .unwrap()
                .accuracy,
            50
        );
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

    const ENCHANT: i32 = 3;

    /// The Enchant Weapon `alter_object` params, as the data patch writes them.
    fn enchant_params() -> serde_json::Value {
        serde_json::json!({
            "mode": "enchant",
            "requireType": "WEAPON",
            "setFlags": ["MAGIC"],
            "applies": [
                {"target": "accuracy", "amount": "2 + 2 * clamp(skill - 17, 0, 1)"},
                {"target": "attack_power", "amount": "5 + 5 * clamp(skill - 19, 0, 1)"}
            ],
            "goodCasterBars": "EVIL",
            "evilCasterBars": "GOOD",
            "messageToCasterGood": "{item} glows blue.",
            "messageToCasterEvil": "{item} glows red.",
            "messageToCasterNeutral": "{item} glows yellow."
        })
    }

    /// Enchant Weapon wired like the data patch, caster at `skill` percent
    /// and `alignment`.
    fn world_with_enchant(skill: i32, alignment: i32) -> (World, Entity, Rx) {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let mut catalog = AbilityCatalog::default();
        let mut enchant = ability_def(ENCHANT, "Enchant Weapon", AbilityKind::Spell);
        enchant.cast_time_rounds = 0;
        catalog.by_name.insert("enchant_weapon".into(), enchant);
        catalog
            .effects_for
            .insert(ENCHANT, vec![(ALTER, Some(enchant_params()))]);
        catalog
            .targeting
            .insert(ENCHANT, targeting(&["OBJECT_INV"]));
        world.insert_resource(catalog);
        let mut effects = EffectCatalog::default();
        effects
            .by_id
            .insert(ALTER, effect_def(ALTER, "alter_object", "alter_object"));
        world.insert_resource(effects);
        world.insert_resource(SpellSlotData::default());
        world.insert_resource(mud_world::ClassSkillsData::default());
        world.insert_resource(ObjectPrototypes::default());
        let (caster, rx) = player_in(&mut world, room);
        world.entity_mut(caster).insert((
            KnownAbilities {
                entries: vec![(ENCHANT, skill * 10, true)],
            },
            CombatStats {
                alignment,
                ..CombatStats::default()
            },
        ));
        (world, caster, rx)
    }

    /// A carried object of `kind` with its proto registered.
    fn proto_item(
        world: &mut World,
        holder: Entity,
        kind: ObjectType,
        flags: Vec<ObjectFlag>,
    ) -> Entity {
        let mut p = object_proto(40, 1, kind);
        p.weapon_dice_num = 2;
        p.weapon_dice_size = 6;
        p.flags = flags.clone();
        world
            .resource_mut::<ObjectPrototypes>()
            .by_key
            .insert((40, 1), p);
        let it = item(world, "a long sword", "sword", holder);
        world
            .entity_mut(it)
            .insert((WorldKey { zone: 40, id: 1 }, ObjectFlags(flags)));
        it
    }

    fn applies_of(world: &World, e: Entity) -> Vec<(String, i32)> {
        world
            .get::<ItemApplies>(e)
            .map(|a| a.0.clone())
            .unwrap_or_default()
    }

    #[test]
    fn enchant_weapon_applies_bonus_flag_and_alignment_bar() {
        let (mut world, caster, mut rx) = world_with_enchant(50, 600);
        let sword = proto_item(&mut world, caster, ObjectType::Weapon, vec![]);
        cast(&mut world, caster, "enchant weapon", "sword");
        // hitroll +2 / damroll +2 in legacy units, scaled x2 / x5 into the
        // modern accuracy / attack_power the importer uses.
        assert_eq!(
            applies_of(&world, sword),
            vec![
                ("accuracy".to_string(), 4),
                ("attack_power".to_string(), 10)
            ]
        );
        assert!(
            world
                .get::<ObjectFlags>(sword)
                .unwrap()
                .has(ObjectFlag::Magic)
        );
        assert_eq!(
            world.get::<ItemBarredAlignments>(sword).unwrap().0,
            vec![Alignment::Evil]
        );
        assert!(
            world
                .get::<mud_world::components::ItemAlterDirty>(sword)
                .is_some()
        );
        let out = drain(&mut rx);
        assert!(out.contains("a long sword glows blue."), "{out}");
    }

    #[test]
    fn enchant_weapon_bonus_steps_with_skill_and_glow_with_alignment() {
        // Legacy: hitroll 1 + (skill >= 18), damroll 1 + (skill >= 20).
        let (mut world, caster, mut rx) = world_with_enchant(18, -600);
        let sword = proto_item(&mut world, caster, ObjectType::Weapon, vec![]);
        cast(&mut world, caster, "enchant weapon", "sword");
        assert_eq!(
            applies_of(&world, sword),
            vec![("accuracy".to_string(), 4), ("attack_power".to_string(), 5)]
        );
        assert_eq!(
            world.get::<ItemBarredAlignments>(sword).unwrap().0,
            vec![Alignment::Good]
        );
        assert!(drain(&mut rx).contains("a long sword glows red."));

        let (mut world, caster, mut rx) = world_with_enchant(17, 0);
        let sword = proto_item(&mut world, caster, ObjectType::Weapon, vec![]);
        cast(&mut world, caster, "enchant weapon", "sword");
        assert_eq!(
            applies_of(&world, sword),
            vec![("accuracy".to_string(), 2), ("attack_power".to_string(), 5)]
        );
        assert!(world.get::<ItemBarredAlignments>(sword).is_none());
        assert!(drain(&mut rx).contains("a long sword glows yellow."));
    }

    #[test]
    fn enchant_weapon_refuses_magic_items_non_weapons_and_modified_weapons() {
        let (mut world, caster, mut rx) = world_with_enchant(50, 600);
        let magic = proto_item(
            &mut world,
            caster,
            ObjectType::Weapon,
            vec![ObjectFlag::Magic],
        );
        cast(&mut world, caster, "enchant weapon", "sword");
        assert!(applies_of(&world, magic).is_empty());
        assert!(world.get::<ItemBarredAlignments>(magic).is_none());
        assert!(
            world
                .get::<mud_world::components::ItemAlterDirty>(magic)
                .is_none()
        );
        assert!(!drain(&mut rx).contains("glows"));

        let (mut world, caster, _rx) = world_with_enchant(50, 600);
        let armor = proto_item(&mut world, caster, ObjectType::Armor, vec![]);
        cast(&mut world, caster, "enchant weapon", "sword");
        assert!(applies_of(&world, armor).is_empty());

        // An already enchanted weapon is magic now: a second cast changes nothing.
        let (mut world, caster, _rx) = world_with_enchant(50, 600);
        let sword = proto_item(&mut world, caster, ObjectType::Weapon, vec![]);
        cast(&mut world, caster, "enchant weapon", "sword");
        let first = applies_of(&world, sword);
        cast(&mut world, caster, "enchant weapon", "sword");
        assert_eq!(applies_of(&world, sword), first);
    }

    #[test]
    fn enchant_bonus_is_granted_on_wield_and_released_on_remove() {
        use crate::equip_apply::{apply_object_to_wearer, unapply_object_from_wearer};
        let (mut world, caster, _rx) = world_with_enchant(50, 600);
        let sword = proto_item(&mut world, caster, ObjectType::Weapon, vec![]);
        world.entity_mut(caster).insert(mud_world::CoreStats {
            strength: 13,
            dexterity: 13,
            constitution: 13,
            intelligence: 13,
            wisdom: 13,
            charisma: 13,
        });
        cast(&mut world, caster, "enchant weapon", "sword");
        // Carried, not wielded: nothing granted yet.
        assert_eq!(world.get::<CombatStats>(caster).unwrap().accuracy, 0);
        world
            .entity_mut(sword)
            .insert(mud_world::EquippedSlot(mud_world::Slot::Wield));
        apply_object_to_wearer(&mut world, sword, caster);
        let cs = world.get::<CombatStats>(caster).unwrap();
        assert_eq!((cs.accuracy, cs.attack_power), (4, 10));
        // Nothing persisted-stat related leaks into the save offsets.
        assert_eq!(
            crate::equip_apply::gear_offsets(&world, caster),
            crate::equip_apply::GearOffsets::default()
        );
        unapply_object_from_wearer(&mut world, sword, caster);
        let cs = world.get::<CombatStats>(caster).unwrap();
        assert_eq!((cs.accuracy, cs.attack_power), (0, 0));
    }

    #[test]
    fn enchanting_a_wielded_weapon_grants_at_once_and_remove_releases() {
        let (mut world, caster, _rx) = world_with_enchant(50, 600);
        let sword = proto_item(&mut world, caster, ObjectType::Weapon, vec![]);
        world
            .entity_mut(sword)
            .insert(mud_world::EquippedSlot(mud_world::Slot::Wield));
        crate::equip_apply::apply_object_to_wearer(&mut world, sword, caster);
        cast(&mut world, caster, "enchant weapon", "sword");
        let cs = world.get::<CombatStats>(caster).unwrap();
        assert_eq!((cs.accuracy, cs.attack_power), (4, 10));
        crate::equip_apply::release_gear(&mut world, sword);
        let cs = world.get::<CombatStats>(caster).unwrap();
        assert_eq!((cs.accuracy, cs.attack_power), (0, 0));
    }

    #[test]
    fn enchantment_survives_a_save_snapshot_and_relog_without_baking_into_stats() {
        let (mut world, caster, _rx) = world_with_enchant(50, 600);
        let sword = proto_item(&mut world, caster, ObjectType::Weapon, vec![]);
        cast(&mut world, caster, "enchant weapon", "sword");
        let saved = crate::item_alter::snapshot(&world, sword);
        // What the row stores: the delta only, JSON round-trippable.
        let json = serde_json::to_value(&saved).unwrap();
        assert_eq!(json["applies"][0]["target"], "accuracy");
        assert_eq!(json["flags_added"], serde_json::json!(["Magic"]));
        assert_eq!(json["alignments_barred"], serde_json::json!(["Evil"]));
        let stored: mud_db::character_items::ItemAlter = serde_json::from_value(json).unwrap();

        // Relog: fresh world, fresh character with base stats, the item
        // respawned from its prototype and wielded again.
        let (mut fresh, who, _rx2) = world_with_enchant(50, 600);
        let sword2 = proto_item(&mut fresh, who, ObjectType::Weapon, vec![]);
        crate::item_alter::restore(&mut fresh, sword2, &stored, false);
        fresh
            .entity_mut(sword2)
            .insert(mud_world::EquippedSlot(mud_world::Slot::Wield));
        crate::equip_apply::recompute_equipped_keeping_vitals(&mut fresh, who);
        // Contents index is maintained by the engine's Located hooks; the
        // test world has none, so apply directly like recompute does.
        if fresh
            .get::<crate::equip_apply::GrantedDeltas>(sword2)
            .is_none()
        {
            crate::equip_apply::apply_object_to_wearer(&mut fresh, sword2, who);
        }
        let cs = fresh.get::<CombatStats>(who).unwrap();
        assert_eq!(
            (cs.accuracy, cs.attack_power),
            (4, 10),
            "granted once, not stacked"
        );
        assert!(applies_of(&fresh, sword2) == applies_of(&world, sword));
        assert!(
            fresh
                .get::<ObjectFlags>(sword2)
                .unwrap()
                .has(ObjectFlag::Magic)
        );
        // The saved character stats carry no enchantment: offsets are empty
        // and the item's own snapshot is unchanged by wearing it.
        assert_eq!(
            crate::equip_apply::gear_offsets(&fresh, who),
            crate::equip_apply::GearOffsets::default()
        );
        assert_eq!(crate::item_alter::snapshot(&fresh, sword2), saved);
        // Wield refusal for the barred alignment survives the relog too.
        fresh.entity_mut(who).insert(CombatStats {
            alignment: -600,
            ..CombatStats::default()
        });
        assert!(
            crate::commands::wear_refusal(&fresh, who, sword2, "a long sword")
                .is_some_and(|m| m.contains("incompatible"))
        );
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
