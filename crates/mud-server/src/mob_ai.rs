//! Mob item AI ported from legacy `mobact.cpp` / `ai_utils.cpp`: what a
//! scavenger wants off the floor (`appraise_item`, `CAN_GET_OBJ`) and
//! which of its carried items it puts on (`mob_attempt_equip`).
//!
//! Legacy `mobile_activity` runs every `PULSE_MOBILE` (10 s = 100 ticks
//! at 10 Hz, the cadence of [`crate::wander::scavenger_tick`]). For each
//! non-illusory scavenger it calls `mob_scavenge` (50% roll, then the
//! single most valuable gettable floor item) and `mob_attempt_equip`
//! (wear anything carried that beats what is worn in its slot) - both
//! before the "not fighting" gate, so a fighting scavenger still does
//! them.
//!
//! Content gaps against the legacy appraisal (the Rust object model
//! carries no value for them, so they add 0): container capacity, food
//! fillingness, the `NORENT` flag.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{ObjectFlag, ObjectRestriction, ObjectType};
use mud_world::{
    AbilityCatalog, AiCondition, AiKind, AiRule, AiTarget, Charges, Contents, DetectInvis,
    EffectCatalog, EquippedSlot, Item, LightFuel, LiquidContainer, Located, MobAi, MobPrototypes,
    ObjectAbilityCatalog, ObjectFlags, ObjectProto, ObjectPrototypes, ObjectRestrictions,
    RuntimeConfig, Slot, SpellSlotData, SpellSlots, WorldKey, components::WeaponDiceSizeAdjust,
};

use crate::commands::{
    WearWhere, carried_weight, carry_capacity, item_wear_positions, item_weight,
};

thread_local! {
    /// Test hook: pins the scavenge 50% roll (per test thread).
    static FORCED_SCAVENGE_ROLL: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Legacy `!random_number(0, 1)`: a scavenger acts on half its pulses.
pub(crate) fn scavenge_roll() -> bool {
    FORCED_SCAVENGE_ROLL
        .with(Cell::get)
        .unwrap_or_else(|| rand::random_range(0..2) == 1)
}

/// Pin the scavenge roll for the current thread (tests only).
#[cfg(test)]
pub(crate) fn force_scavenge_roll(roll: Option<bool>) {
    FORCED_SCAVENGE_ROLL.with(|c| c.set(roll));
}

/// Legacy `mobile_activity` entry gate: asleep, casting, stunned
/// (paralysed), frozen and waiting (`GET_MOB_WAIT`) mobs do nothing.
pub(crate) fn mob_can_act(world: &World, mob: Entity) -> bool {
    world
        .get::<mud_world::Posture>(mob)
        .is_none_or(|p| p.0 != mud_world::PostureKind::Sleeping)
        && world.get::<mud_world::Casting>(mob).is_none()
        && world.get::<mud_world::Stunned>(mob).is_none()
        && world.get::<mud_world::Frozen>(mob).is_none()
        && world.get::<mud_world::Ghost>(mob).is_none()
        && !crate::commands::input_queue::wait_active(world, mob)
}

/// Whether a mob may walk about under its own power: [`mob_can_act`] and on
/// its feet. Legacy `mobile_activity` only lets a mob wander while
/// `GET_POS >= POS_STANDING`. Starting or joining a fight is gated on
/// [`mob_can_act`] alone (legacy aggro and grudges only need the mob awake);
/// a sitting or resting mob that engages gets up first, see
/// [`stand_up_for_fight`]. A mob with no [`mud_world::Posture`] counts as
/// standing.
pub(crate) fn mob_can_swing(world: &World, mob: Entity) -> bool {
    mob_can_act(world, mob)
        && world
            .get::<mud_world::Posture>(mob)
            .is_none_or(|p| p.0 == mud_world::PostureKind::Standing)
}

/// Legacy `perform_mob_violence` (mobact.cpp:335): a mob below standing that
/// is ready to act gets to its feet (announced to the room) before it
/// swings. Sleeping mobs are left alone (they cannot act). Returns whether
/// the mob stood up.
pub(crate) fn stand_up_for_fight(world: &mut World, mob: Entity) -> bool {
    use mud_world::PostureKind;
    if !matches!(
        world.get::<mud_world::Posture>(mob).map(|p| p.0),
        Some(PostureKind::Sitting | PostureKind::Kneeling | PostureKind::Resting)
    ) || !mob_can_act(world, mob)
    {
        return false;
    }
    // Getting up for a fight ends any meditation (legacy: "Your meditation is
    // rudely interrupted!").
    world
        .entity_mut(mob)
        .remove::<mud_world::Meditating>()
        .insert(mud_world::Posture(PostureKind::Standing));
    if let Some(room) = world.get::<Located>(mob).map(|l| l.0) {
        let name = crate::commands::cap_sentence_start(&crate::commands::name_of(world, mob));
        let their = crate::flight::possessive(world, mob);
        crate::commands::broadcast_room_except_rendered(
            world,
            room,
            &[mob],
            &format!("<yellow>{name} scrambles to {their} feet!</>\r\n"),
        );
    }
    true
}

/// A mob's percent in a skill at `level` once it has it. Legacy
/// `roll_mob_skill` (chars.cpp:247) gives an NPC `random(50,100)` plus
/// `random(5,15)` per level above the first, capped at 1000, and
/// `GET_SKILL` divides by 10. Rust mobs carry no stored skill rows, so
/// this is that roll's mean, `75 + 10 * (level - 1)` tenths, as a
/// percent.
pub(crate) fn mob_skill_percent_at(level: i32) -> i32 {
    let tenths = 75 + 10 * (level.max(1) - 1);
    (tenths / 10).clamp(0, 100)
}

/// A mob's percent in `ability_id`, 0 when it lacks the skill. Legacy
/// `update_skills` (skills.cpp:252) grants a skill when the class
/// learns it at or below the mob's level OR the race grants it
/// (`min_race_level`, races.cpp `assign_race_skills`); both branches
/// give a mob the same `roll_mob_skill(level)` (skills.cpp:271,
/// :292-:299), and everything else is zeroed. Here: the mob prototype's
/// `class_id` against `ClassSkillsData`, or its `race` against
/// `RaceAbilitiesData` (no per-row level in the schema: legacy racial
/// skills are level 1).
pub(crate) fn mob_skill_pct(world: &World, mob: Entity, ability_id: i32) -> i32 {
    let Some(proto) = proto_of_mob(world, mob) else {
        return 0;
    };
    let level = mud_world::effective_level(world, mob);
    let by_class = proto.class_id.is_some_and(|class_id| {
        world
            .get_resource::<mud_world::ClassSkillsData>()
            .and_then(|d| d.min_level_for(class_id, ability_id))
            .is_some_and(|min| min <= level)
    });
    let by_race = world
        .get_resource::<mud_world::RaceAbilitiesData>()
        .is_some_and(|d| d.grants(&proto.race, ability_id));
    if by_class || by_race {
        mob_skill_percent_at(level)
    } else {
        0
    }
}

fn proto_of_mob(world: &World, mob: Entity) -> Option<&mud_world::MobProto> {
    let key = world.get::<WorldKey>(mob)?;
    world
        .get_resource::<MobPrototypes>()?
        .by_key
        .get(&(key.zone, key.id))
}

fn proto_of(world: &World, item: Entity) -> Option<&ObjectProto> {
    let key = world.get::<WorldKey>(item)?;
    world
        .get_resource::<ObjectPrototypes>()?
        .by_key
        .get(&(key.zone, key.id))
}

/// The mob's class id (from its spawn prototype), for `NOWEAR_CLASS`.
pub(crate) fn mob_class_id(world: &World, mob: Entity) -> Option<i32> {
    let key = world.get::<WorldKey>(mob)?;
    world
        .get_resource::<MobPrototypes>()?
        .by_key
        .get(&(key.zone, key.id))?
        .class_id
}

/// Legacy `CAN_SEE_OBJ`, as far as the Rust model goes: an invisible
/// object needs detect invisibility.
fn can_see_obj(world: &World, mob: Entity, item: Entity) -> bool {
    world
        .get::<ObjectFlags>(item)
        .is_none_or(|f| !f.has(ObjectFlag::Invisible))
        || world.get::<DetectInvis>(mob).is_some()
}

/// Legacy `CAN_CARRY_N`: `5 + dex/2 + level/2`. Mobs carry no `CoreStats`,
/// so they count as average dexterity (the same convention the
/// familiarity roll uses for charisma).
fn can_carry_n(world: &World, mob: Entity) -> usize {
    let dex = world
        .get::<mud_world::CoreStats>(mob)
        .map_or(50, |c| c.dexterity);
    let level = mud_world::effective_level(world, mob);
    usize::try_from(5 + dex / 2 + level / 2).unwrap_or(5)
}

/// Legacy `IS_CARRYING_N`: items in the pack (worn gear and the contents
/// of containers do not count).
fn carried_count(world: &World, mob: Entity) -> usize {
    world.get::<Contents>(mob).map_or(0, |c| {
        c.iter()
            .filter(|&e| world.get::<Item>(e).is_some() && world.get::<EquippedSlot>(e).is_none())
            .count()
    })
}

/// Legacy `value_spell`: a spell's lowest class level (approximated as
/// the circle rule of thumb `circle * 2 + 1` used by the spell formulas),
/// negative for a violent spell when aggression is not good.
fn value_spell(world: &World, ability_id: i32, aggro_good: bool) -> i32 {
    let lowest = world
        .get_resource::<SpellSlotData>()
        .and_then(|s| s.min_circle_for_ability(ability_id))
        .map_or(0, |c| c * 2 + 1);
    let violent = world
        .get_resource::<AbilityCatalog>()
        .is_some_and(|c| c.by_name.values().any(|d| d.id == ability_id && d.violent));
    if !aggro_good && violent {
        -lowest
    } else {
        lowest
    }
}

/// Legacy `value_spell_effect`, keyed by the status flag name. The scores live
/// in the `StatusFlagValue` table (loaded into [`mud_world::StatusFlagValues`]);
/// a flag with no row is worth 0.
fn value_status_flag(world: &World, flag: &str) -> i32 {
    world
        .get_resource::<mud_world::StatusFlagValues>()
        .map_or(0, |v| v.value(flag))
}

/// Legacy `value_effect`: what an `APPLY_*` bonus is worth. Keys are the
/// `modify` effect targets (see `apply_modify_delta`).
fn value_apply(target: &str, amount: i32) -> i32 {
    match target {
        "str" | "strength" | "str_bonus" | "dex" | "dexterity" | "dex_bonus" | "int"
        | "intelligence" | "int_bonus" | "wis" | "wisdom" | "wis_bonus" | "con"
        | "constitution" | "con_bonus" | "cha" | "charisma" | "cha_bonus" => 3 * amount,
        // `armor_pct` is legacy AC x 2, so 2 * AC is the amount itself.
        "max_hp" | "max_move" | "max_stamina" | "stamina_max" | "focus" | "armor_pct" => amount,
        "accuracy" | "attack_power" | "weapon_hitroll" | "unarmed_damage" => 5 * amount,
        "saving_para" | "saving_rod" | "saving_petri" | "saving_breath" | "saving_spell" => -amount,
        "size" => 10 * amount,
        "hit_regen" => 2 * amount,
        "perception" | "hiddenness" => amount / 2,
        _ => 0,
    }
}

/// Legacy `appraise_item`: how much `mob` wants `item`. Higher is better;
/// an item worth 0 or less is never picked up. Items without a prototype
/// (synthetic piles) count as level 0, cost 0.
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
pub(crate) fn appraise_item(world: &World, mob: Entity, item: Entity) -> i32 {
    let proto = proto_of(world, item);
    let kind = proto.map(|p| p.r#type);
    let mut value: i32 = 0;
    match kind {
        Some(ObjectType::Light) => {
            let fuel = world.get::<LightFuel>(item).copied().or_else(|| {
                proto.and_then(|p| p.light_fuel).map(|f| LightFuel {
                    capacity: f.capacity,
                    remaining: f.remaining,
                })
            });
            if let Some(f) = fuel {
                value = if f.is_permanent() {
                    50
                } else {
                    f.remaining / 100 + f.capacity / 200
                };
            }
        }
        Some(ObjectType::Weapon) => {
            let p = proto.expect("weapon kind implies proto");
            let adjust = world.get::<WeaponDiceSizeAdjust>(item).map_or(0, |a| a.0);
            // Legacy `WEAPON_AVERAGE`: ((size + 1) / 2.0) * num, truncated.
            let size = p.weapon_dice_size + adjust;
            value = ((f64::from(size + 1) / 2.0) * f64::from(p.weapon_dice_num)) as i32;
        }
        Some(k @ (ObjectType::Scroll | ObjectType::Potion)) => {
            let bindings = bindings_of(world, proto);
            value = bindings.iter().map(|b| b.level).max().unwrap_or(0);
            for b in bindings.iter().take(3) {
                value += value_spell(world, b.ability_id, k == ObjectType::Potion);
            }
        }
        Some(k @ (ObjectType::Wand | ObjectType::Staff | ObjectType::Instrument)) => {
            let bindings = bindings_of(world, proto);
            if let Some(b) = bindings.first() {
                let charges = world
                    .get::<Charges>(item)
                    .map(|c| c.0)
                    .or(b.charges)
                    .unwrap_or(0);
                value = value_spell(world, b.ability_id, true) * charges;
                if k != ObjectType::Wand {
                    value *= 3;
                }
                value += b.level;
            }
        }
        Some(ObjectType::Armor | ObjectType::Treasure) => {
            value = proto.map_or(0, |p| p.armor_pct) / 2;
        }
        Some(ObjectType::Drinkcontainer | ObjectType::Fountain) => {
            if let Some(l) = world.get::<LiquidContainer>(item) {
                value = l.capacity + l.remaining;
                if l.poisoned {
                    value = -value;
                }
            }
        }
        _ => {}
    }

    let wearable = !item_wear_positions(world, item).is_empty();
    if let Some(p) = proto {
        if wearable {
            let effects = world.get_resource::<EffectCatalog>();
            for g in &p.granted_effects {
                let Some(def) = effects.and_then(|c| c.by_id.get(&g.effect_id)) else {
                    continue;
                };
                if def.effect_type == "modify" {
                    let target = g.modifier_data.get("target").and_then(|v| v.as_str());
                    let amount = g
                        .modifier_data
                        .get("amount")
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|n| i32::try_from(n).ok());
                    if let (Some(t), Some(a)) = (target, amount) {
                        value += value_apply(t, a);
                    }
                }
            }
        }
        value += p.cost / 100;
        // Legacy `value_spell_effects`: effect flags the object grants.
        if let Some(effects) = world.get_resource::<EffectCatalog>() {
            for g in &p.granted_effects {
                let Some(def) = effects.by_id.get(&g.effect_id) else {
                    continue;
                };
                match def.effect_type.as_str() {
                    "status" => {
                        for flag in
                            mud_world::mob_effects::row_flags(&g.modifier_data, &def.default_params)
                        {
                            value += value_status_flag(world, &flag);
                        }
                    }
                    "globe" => {
                        value += if crate::equip_apply::globe_circle(g, &def.default_params) > 3 {
                            70
                        } else {
                            20
                        };
                    }
                    _ => {}
                }
            }
        }
        value += value_obj_flags(world, mob, item, p, wearable);
        value += 100 - p.level;
    } else {
        value += 100;
        if let Some(pile) = world.get::<mud_world::CoinPile>(item) {
            value += i32::try_from(pile.0 / 100).unwrap_or(i32::MAX);
        }
    }
    value
}

fn bindings_of(world: &World, proto: Option<&ObjectProto>) -> Vec<mud_world::ObjectAbilityBinding> {
    proto
        .and_then(|p| {
            world
                .get_resource::<ObjectAbilityCatalog>()?
                .by_key
                .get(&(p.zone_id, p.id))
                .cloned()
        })
        .unwrap_or_default()
}

/// Legacy `value_obj_flags`: item flags and anti-alignment / anti-class
/// restrictions, weighed against the mob that would use it.
fn value_obj_flags(
    world: &World,
    mob: Entity,
    item: Entity,
    proto: &ObjectProto,
    wearable: bool,
) -> i32 {
    let is_weapon = proto.r#type == ObjectType::Weapon;
    let flags = world.get::<ObjectFlags>(item);
    let restrictions = world.get::<ObjectRestrictions>(item);
    let has_flag = |f: ObjectFlag| flags.map_or(proto.flags.contains(&f), |c| c.has(f));
    let has_restriction =
        |r: ObjectRestriction| restrictions.map_or(proto.restrictions.contains(&r), |c| c.has(r));
    let mut value = 0;
    if has_flag(ObjectFlag::Magic) {
        value += 1;
    }
    if has_flag(ObjectFlag::Invisible) {
        value += 5;
    }
    if has_flag(ObjectFlag::Float) {
        if is_weapon {
            value += 10;
        }
        value += 3;
    }
    if has_restriction(ObjectRestriction::NoDrop) {
        if is_weapon {
            value += 15;
        }
        value -= 5;
    }
    if has_restriction(ObjectRestriction::NoInvisible) {
        value += 5;
    }
    if has_restriction(ObjectRestriction::NoBurn) && wearable {
        value += 10;
    }
    if has_restriction(ObjectRestriction::NoLocate) {
        value += 3;
    }
    // Anti-alignment: -20 when the mob is that alignment.
    let align = world
        .get::<mud_world::CombatStats>(mob)
        .map_or(0, |c| c.alignment);
    if proto
        .restricted_alignments
        .contains(&mud_db::enums::Alignment::from_score(align))
    {
        value -= 20;
    }
    // Each anti-class flag is -2; one aimed at the mob's own class is
    // another -100 (`NOWEAR_CLASS`).
    let classes = i32::try_from(proto.restricted_class_ids.len()).unwrap_or(0);
    value -= 2 * classes;
    if mob_class_id(world, mob).is_some_and(|c| proto.restricted_class_ids.contains(&c)) {
        value -= 100;
    }
    value
}

/// Everything legacy `CAN_GET_OBJ` asks of a floor item for `mob`:
/// takeable, within the weight and item-count limits, visible, and not
/// above the mob's level. `carried_w` / `carried_n` are the mob's current
/// load (computed once per mob by the caller).
pub(crate) fn can_get_obj(
    world: &World,
    mob: Entity,
    item: Entity,
    carried_w: f64,
    carried_n: usize,
) -> bool {
    if world
        .get::<ObjectRestrictions>(item)
        .is_some_and(|r| r.has(ObjectRestriction::NoTake))
    {
        return false;
    }
    // A loose coin pile goes straight to the purse: no weight, no slot.
    let is_coins = world.get::<mud_world::CoinPile>(item).is_some();
    if !is_coins {
        let cap = carry_capacity(world, mob);
        let w = item_weight(world, item);
        if carried_w + w > cap || w > cap || carried_n + 1 > can_carry_n(world, mob) {
            return false;
        }
    }
    can_see_obj(world, mob, item)
        && proto_of(world, item).map_or(0, |p| p.level) <= mud_world::effective_level(world, mob)
}

/// Legacy `mob_scavenge` after the 50% roll: pick the most valuable
/// gettable item in `floor` (the room's free-floor items; the chosen one
/// is removed from the list) and take it. Returns the taken item's
/// `(entity, name)`.
pub(crate) fn mob_scavenge(
    world: &mut World,
    mob: Entity,
    floor: &mut Vec<Entity>,
) -> Option<(Entity, String)> {
    let carried_w = carried_weight(world, mob);
    let carried_n = carried_count(world, mob);
    let mut best: Option<(usize, i32)> = None;
    for (i, &item) in floor.iter().enumerate() {
        if !can_get_obj(world, mob, item, carried_w, carried_n) {
            continue;
        }
        let value = appraise_item(world, mob, item);
        if value > best.map_or(0, |(_, v)| v) {
            best = Some((i, value));
        }
    }
    let (idx, _) = best?;
    let item = floor.remove(idx);
    let name = world
        .get::<mud_world::Named>(item)
        .map(|n| n.name.clone())
        .unwrap_or_default();
    if world.get::<mud_world::CoinPile>(item).is_some() {
        crate::commands::info::take_loose_coins(world, mob, item);
    } else {
        world.entity_mut(item).insert(Located(mob));
    }
    Some((item, name))
}

/// Pack items of `mob` (not worn), in container order.
fn pack_items(world: &World, mob: Entity) -> Vec<Entity> {
    world
        .get::<Contents>(mob)
        .map(|c| {
            c.iter()
                .filter(|&e| {
                    world.get::<Item>(e).is_some() && world.get::<EquippedSlot>(e).is_none()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The item `mob` has worn in `slot`.
fn worn_in(world: &World, mob: Entity, slot: Slot) -> Option<Entity> {
    world.get::<Contents>(mob)?.iter().find(|&e| {
        world.get::<Item>(e).is_some() && world.get::<EquippedSlot>(e).is_some_and(|s| s.0 == slot)
    })
}

/// Legacy `find_eq_pos(ch, obj, nullptr)`: the highest-priority wear
/// position, never a light or a held item (legacy's keyword-less search
/// does not offer those).
fn auto_wear_slot(world: &World, item: Entity) -> Option<Slot> {
    item_wear_positions(world, item)
        .into_iter()
        .rfind(|s| !matches!(s, Slot::Light | Slot::Hold))
}

/// Legacy `perform_remove` for a mob: take `item` off into the pack.
fn unequip(world: &mut World, mob: Entity, item: Entity) {
    let item_name = crate::commands::name_of(world, item);
    crate::equip_apply::release_gear(world, item);
    if let Ok(mut e) = world.get_entity_mut(item) {
        e.remove::<EquippedSlot>();
    }
    if let Some(room) = world.get::<Located>(mob).map(|l| l.0) {
        let mob_name = crate::commands::name_of(world, mob);
        crate::commands::broadcast_room_visual(
            world,
            room,
            mob,
            &[mob],
            &crate::commands::cap_sentence_start(&format!(
                "{mob_name} stops using {item_name}.\r\n"
            )),
        );
    }
    crate::triggers::fire_item_event(world, item, mob, mud_world::TriggerEvent::Remove);
}

/// Legacy `mob_attempt_equip`: wear every carried item that fits an
/// empty slot, or beats (`appraise_item`) what is worn there. Animals
/// never wear anything. Gear goes on through `wear_item`, so
/// `apply_object_to_wearer` applies its effects and `release_gear`
/// reverses them on removal, death and despawn.
pub(crate) fn mob_attempt_equip(world: &mut World, mob: Entity) {
    let pack = pack_items(world, mob);
    if pack.is_empty() {
        return;
    }
    if mud_world::effective_race(world, mob).eq_ignore_ascii_case("animal") {
        return;
    }
    for obj in pack {
        if world.get::<EquippedSlot>(obj).is_some()
            || world.get::<Located>(obj).map(|l| l.0) != Some(mob)
        {
            continue;
        }
        // The very predicate `wear_item` applies (alignment, class, race,
        // size), checked before anything is taken off: a refused upgrade
        // must not strip the mob and loop every pulse.
        if !can_see_obj(world, mob, obj)
            || crate::commands::wear_refusal(world, mob, obj, "").is_some()
        {
            continue;
        }
        let Some(slot) = auto_wear_slot(world, obj) else {
            continue;
        };
        // Paired anatomy (rings, wrists, necks, ears): any free side takes
        // the item; only when every side is taken is the first compared.
        let group = slot.group();
        let mut current = None;
        if group.iter().all(|&s| worn_in(world, mob, s).is_some()) {
            let Some(cur) = worn_in(world, mob, group[0]) else {
                continue;
            };
            if appraise_item(world, mob, cur) >= appraise_item(world, mob, obj) {
                continue;
            }
            // Legacy `perform_remove` refuses with a full pack.
            if carried_count(world, mob) >= can_carry_n(world, mob) {
                continue;
            }
            unequip(world, mob, cur);
            current = Some(cur);
        }
        let worn = crate::commands::wear_item(world, mob, obj, WearWhere::Position(slot), true);
        if !worn && let Some(cur) = current {
            crate::commands::wear_item(world, mob, cur, WearWhere::Position(slot), true);
        }
    }
}

// ---------------------------------------------------------------------------
// Spell / skill AI (WP-C and WP-D of `ops/plans/mob-ai.md`)
//
// Once per combat round (`combat::combat_tick`, only with `mob_ai.enabled`
// on) every fighting mob that carries a `MobAi` walks its rule list in
// priority order and may replace its melee swing with one ability, the way
// legacy `perform_mob_violence` lets a cast or a special skill stand in for
// the swing. The ability itself goes through `invoke_ability_core` with a
// forced target, so slots, wind-up, interrupts, silence, cooldowns and
// effects all behave exactly as they do for a player.
//
// On the 10 s mob pulse (`wander::mob_idle_ai_tick`) a mob that is not
// fighting and stands in a room with a player runs the same walk over its
// out-of-combat rules (buffs, cures: legacy `check_mob_status`), and a caster
// with spent slots sits down and meditates until they are back
// (`check_spellbank`).
//
// Mobs carry no `Stamina` and abilities carry no stamina cost, so no skill
// cost is charged.

/// Rules a mob may consider per combat round (legacy tries "best 2-3"). Only
/// rules that passed their roll count: a rare rule that fails its roll is free.
const MAX_RULE_EVALS: u32 = 3;
/// Rules one mob may put through the roll-and-conditions step per round, taken
/// in priority order. Rules skipped by the walk, level gate or a running
/// cooldown cost nothing and do not count. A hard bound so a mob with a long
/// custom list cannot make the combat tick pay for all of it every round.
const MAX_RULES_CHECKED: u32 = 16;
/// Rule cooldowns are written in seconds; the server ticks at 10 Hz.
const TICKS_PER_SECOND: u64 = 10;
/// A rule whose last attempt did not take effect (no slot, resisted, refused)
/// rests this long, so a mob out of options does not retry or telegraph every
/// round.
const FAILED_RETRY_TICKS: u64 = 3 * crate::combat::COMBAT_PERIOD_TICKS;

/// The class a caster's spell slots and circles are read from: a player's
/// `Profile.class_id`, or, for a mob running spell AI (so only with
/// `mob_ai.enabled`), its prototype's class. Mobs carry no `Profile`.
pub(crate) fn spell_class_id(world: &World, entity: Entity) -> Option<i32> {
    if let Some(p) = world.get::<mud_world::Profile>(entity) {
        return p.class_id;
    }
    world.get::<MobAi>(entity)?;
    mob_class_id(world, entity)
}

/// Proficiency (0..=100) of a mob running spell AI in an ability: legacy
/// `roll_mob_skill`'s mean for its level ([`mob_skill_percent_at`]). Zero for
/// anything else, so players and AI-less mobs keep their `KnownAbilities`.
pub(crate) fn ai_mob_skill_pct(world: &World, entity: Entity) -> i32 {
    if world.get::<MobAi>(entity).is_some()
        && world.get::<mud_world::KnownAbilities>(entity).is_none()
    {
        mob_skill_percent_at(mud_world::effective_level(world, entity))
    } else {
        0
    }
}

/// `e`'s side in a fight, within `room`: its master (when it serves one), that
/// master's group, and every servant of any of them.
fn side_of(world: &mut World, e: Entity, room: Entity) -> HashSet<Entity> {
    let leader = crate::commands::servant_master(world, e).unwrap_or(e);
    let party = mud_world::group_root(world, leader);
    let mut side: HashSet<Entity> = mud_world::group_members(world, party).into_iter().collect();
    side.insert(leader);
    side.insert(e);
    let here: Vec<Entity> = crate::room_index::contents_of(world, room).collect();
    for m in here {
        if world.get::<mud_world::Mob>(m).is_some()
            && crate::commands::servant_master(world, m)
                .is_some_and(|master| side.contains(&master))
        {
            side.insert(m);
        }
    }
    side
}

/// Who a mob's room-wide hostile ability (breath, area spell) hits: the
/// mob's foes (whoever it fights, and whoever fights it) with their groups
/// and pets. Bystanders who are not in the fight, other mobs beside it, and a
/// charmed caster's own master with the master's group and pets are spared.
pub(crate) fn mob_aoe_enemies(
    world: &mut World,
    caster: Entity,
    room: Entity,
) -> Vec<(Entity, String)> {
    let here: Vec<Entity> = crate::room_index::contents_of(world, room).collect();
    let mut seeds: Vec<Entity> = world
        .get::<mud_world::Fighting>(caster)
        .map(|f| f.0)
        .into_iter()
        .collect();
    seeds.extend(here.iter().copied().filter(|&e| {
        e != caster
            && world
                .get::<mud_world::Fighting>(e)
                .is_some_and(|f| f.0 == caster)
    }));
    let mut hit: HashSet<Entity> = HashSet::new();
    for seed in seeds {
        hit.extend(side_of(world, seed, room));
    }
    if crate::commands::servant_master(world, caster).is_some() {
        for own in side_of(world, caster, room) {
            hit.remove(&own);
        }
    }
    hit.remove(&caster);
    here.into_iter()
        .filter(|e| hit.contains(e) && combat_target_alive(world, *e))
        .filter_map(|e| {
            let named = world.get::<mud_world::Named>(e)?;
            Some((e, named.name.clone()))
        })
        .collect()
}

/// What the engine needs to know about an ability a rule names.
#[derive(Clone)]
struct AbilityInfo {
    /// Lowercase `plain_name`, the key `invoke_ability_core` resolves.
    key: String,
    display: String,
    hostile: bool,
    violent: bool,
    /// Hits everyone in scope rather than one target.
    area: bool,
}

#[derive(Clone, Copy, Default)]
struct EffectTally {
    buffs: u32,
    debuffs: u32,
}

/// The two rolls legacy made once per mob per AI call, shared by every rule
/// of that mob in a round: the class level gate (`random(0, 101) < level`)
/// and the dragon roll (`random(0, 150 - level)`).
#[derive(Default)]
struct MobRolls {
    gate: Option<i32>,
    dragon: Option<i32>,
}

/// Per-round caches, each built on first use: most rounds a mob's rules
/// fail the cheap checks and none of this is ever touched.
#[derive(Default)]
struct RoundCtx {
    abilities: Option<HashMap<i32, AbilityInfo>>,
    /// victim -> everything with `Fighting(victim)`.
    attackers: Option<HashMap<Entity, Vec<Entity>>>,
    /// room -> mobs standing in it.
    room_mobs: Option<HashMap<Entity, Vec<Entity>>>,
    /// entity -> beneficial / harmful spell effects on it.
    tally: Option<HashMap<Entity, EffectTally>>,
    /// `(entity, ability id)` for every ability effect currently applied.
    applied: Option<HashSet<(Entity, i32)>>,
    rolls: MobRolls,
}

impl RoundCtx {
    fn ability(&mut self, world: &World, id: i32) -> Option<AbilityInfo> {
        let map = self.abilities.get_or_insert_with(|| {
            let Some(catalog) = world.get_resource::<mud_world::AbilityCatalog>() else {
                return HashMap::new();
            };
            catalog
                .by_name
                .values()
                .map(|d| {
                    (
                        d.id,
                        AbilityInfo {
                            key: d.plain_name.to_ascii_lowercase(),
                            display: d.name.clone(),
                            hostile: crate::commands::ability_is_hostile(world, d),
                            violent: d.violent,
                            area: d.is_area,
                        },
                    )
                })
                .collect()
        });
        map.get(&id).cloned()
    }

    fn attackers_of(&mut self, world: &mut World, victim: Entity) -> Vec<Entity> {
        let map = self.attackers.get_or_insert_with(|| {
            let mut map: HashMap<Entity, Vec<Entity>> = HashMap::new();
            let mut q = world.query::<(Entity, &mud_world::Fighting)>();
            for (e, f) in q.iter(world) {
                map.entry(f.0).or_default().push(e);
            }
            map
        });
        map.get(&victim).cloned().unwrap_or_default()
    }

    fn mobs_in(&mut self, world: &mut World, room: Entity) -> Vec<Entity> {
        let map = self.room_mobs.get_or_insert_with(|| {
            let mut map: HashMap<Entity, Vec<Entity>> = HashMap::new();
            let mut q = world.query_filtered::<(Entity, &Located), With<mud_world::Mob>>();
            for (e, l) in q.iter(world) {
                map.entry(l.0).or_default().push(e);
            }
            map
        });
        map.get(&room).cloned().unwrap_or_default()
    }

    fn effects_on(&mut self, world: &mut World, entity: Entity) -> EffectTally {
        if self.tally.is_none() {
            // Building the tally needs the ability table; do that first.
            let violent: HashMap<i32, bool> = self
                .abilities
                .get_or_insert_with(HashMap::new)
                .iter()
                .map(|(id, a)| (*id, a.violent))
                .collect();
            let violent = if violent.is_empty() {
                world
                    .get_resource::<mud_world::AbilityCatalog>()
                    .map(|c| c.by_name.values().map(|d| (d.id, d.violent)).collect())
                    .unwrap_or_default()
            } else {
                violent
            };
            let mut map: HashMap<Entity, EffectTally> = HashMap::new();
            let mut q = world.query::<(&mud_world::EffectInstance, &mud_world::AppliedTo)>();
            for (eff, on) in q.iter(world) {
                let Some(&is_violent) = eff.ability_id.and_then(|id| violent.get(&id)) else {
                    continue;
                };
                let t = map.entry(on.0).or_default();
                if is_violent {
                    t.debuffs += 1;
                } else {
                    t.buffs += 1;
                }
            }
            self.tally = Some(map);
        }
        self.tally
            .as_ref()
            .and_then(|m| m.get(&entity).copied())
            .unwrap_or_default()
    }

    /// Whether `entity` already carries an effect from `ability_id`: a buff
    /// the mob already has, or a hindrance the victim is already under, is
    /// not cast again.
    fn has_effect_from(&mut self, world: &mut World, entity: Entity, ability_id: i32) -> bool {
        let set = self.applied.get_or_insert_with(|| {
            let mut q = world.query::<(&mud_world::EffectInstance, &mud_world::AppliedTo)>();
            q.iter(world)
                .filter_map(|(eff, on)| eff.ability_id.map(|id| (on.0, id)))
                .collect()
        });
        set.contains(&(entity, ability_id))
    }
}

/// Alive, in `room`, and a legal thing to fight or heal.
fn in_room_alive(world: &World, e: Entity, room: Entity) -> bool {
    world.get::<Located>(e).map(|l| l.0) == Some(room) && combat_target_alive(world, e)
}

fn combat_target_alive(world: &World, e: Entity) -> bool {
    world.get::<mud_world::Health>(e).is_some_and(|h| h.hp > 0)
        && world.get::<mud_world::Ghost>(e).is_none()
        && world.get::<crate::login::Retiring>(e).is_none()
}

fn hp_pct(world: &World, e: Entity) -> i64 {
    world.get::<mud_world::Health>(e).map_or(100, |h| {
        i64::from(h.hp.max(0)) * 100 / i64::from(h.max.max(1))
    })
}

/// A chance roll: `chance_pct` scaled by `mob_ai.scale`, capped at 100.
fn chance_passes(chance_pct: u8, scale: f64) -> bool {
    let eff = (f64::from(chance_pct) * scale).clamp(0.0, 100.0);
    rand::random_range(0.0..100.0) < eff
}

/// The roll-style conditions of a rule (`level_scaled_chance`, `level_roll`),
/// asked once per rule before it is evaluated. The rolls are shared by all of
/// the mob's rules this round.
fn roll_gates_pass(ctx: &mut RoundCtx, level: i32, scale: f64, rule: &AiRule) -> bool {
    for cond in &rule.when {
        match cond {
            AiCondition::LevelScaledChance(thousandths) => {
                let roll = *ctx
                    .rolls
                    .gate
                    .get_or_insert_with(|| rand::random_range(0..=101));
                let threshold = f64::from(level) * f64::from(*thousandths) / 1000.0 * scale;
                if f64::from(roll) >= threshold {
                    return false;
                }
            }
            AiCondition::LevelRoll { min, max } => {
                let roll = *ctx
                    .rolls
                    .dragon
                    .get_or_insert_with(|| rand::random_range(0..=(150 - level).max(0)));
                if roll < *min || roll >= *max {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// Conditions about the rule's chosen target (as opposed to the mob or room).
fn needs_target(c: &AiCondition) -> bool {
    matches!(
        c,
        AiCondition::TargetHpBelow(_)
            | AiCondition::TargetHpAbove(_)
            | AiCondition::TargetCasting
            | AiCondition::TargetHasBuff
            | AiCondition::TargetLifeforce(_)
            | AiCondition::NotLifeforce(_)
            | AiCondition::Align(_)
            | AiCondition::NotAlign(_)
            | AiCondition::TargetLevelMax(_)
    )
}

/// Everything a rule's conditions read about the mob's surroundings.
struct Scene<'a> {
    mob: Entity,
    room: Entity,
    /// Live enemies: whoever fights the mob, plus the one it fights.
    foes: &'a [Entity],
    /// A combat-round walk (true) or an idle-pulse walk (false).
    in_combat: bool,
}

/// The mob's side: allies a heal or cure may go to. A servant's allies are
/// its master, the master's group and their pets; a free mob's are the other
/// free mobs here that are not fighting it or one of its own side.
fn allies_of(world: &mut World, ctx: &mut RoundCtx, scene: &Scene<'_>) -> Vec<Entity> {
    if let Some(master) = crate::commands::servant_master(world, scene.mob) {
        let side = side_of(world, master, scene.room);
        let here: Vec<Entity> = crate::room_index::contents_of(world, scene.room).collect();
        return here
            .into_iter()
            .filter(|&e| {
                e != scene.mob
                    && side.contains(&e)
                    && !scene.foes.contains(&e)
                    && in_room_alive(world, e, scene.room)
            })
            .collect();
    }
    ctx.mobs_in(world, scene.room)
        .into_iter()
        .filter(|&e| {
            e != scene.mob
                && in_room_alive(world, e, scene.room)
                && !scene.foes.contains(&e)
                && !crate::commands::is_servant(world, e)
                && !fighting_free_mob_side(world, e, scene.mob)
        })
        .collect()
}

/// Whether `e` is in a fight against `mob` or against another free mob: a
/// brawl between free mobs splits them into sides, so neither heals the
/// other's opponent.
fn fighting_free_mob_side(world: &World, e: Entity, mob: Entity) -> bool {
    world
        .get::<mud_world::Fighting>(e)
        .map(|f| f.0)
        .is_some_and(|t| {
            t == mob
                || (world.get::<mud_world::Mob>(t).is_some()
                    && !crate::commands::is_servant(world, t))
        })
}

fn alignment_of(world: &World, e: Entity) -> i32 {
    world
        .get::<mud_world::CombatStats>(e)
        .map_or(0, |c| c.alignment)
}

fn lifeforce_is(world: &World, e: Entity, lf: mud_db::enums::LifeForce) -> bool {
    world.get::<mud_world::LifeForceTag>(e).map(|t| t.0) == Some(lf)
        || (lf == mud_db::enums::LifeForce::Life
            && world.get::<mud_world::LifeForceTag>(e).is_none())
}

/// Whether the mob wears a shield: an armor piece held in the off hand.
fn wears_shield(world: &World, mob: Entity) -> bool {
    worn_in(world, mob, Slot::Hold)
        .and_then(|item| proto_of(world, item))
        .is_some_and(|p| p.r#type == ObjectType::Armor)
}

/// Whether the mob wields a weapon of damage type `wanted` (lowercase). The
/// legacy families `piercing`, `slashing` and `bludgeoning` stand for the
/// damage types that belong to them, as decided by `combat::weapon_family`
/// (the single damage-type to family table).
fn wields_weapon_type(world: &World, mob: Entity, wanted: &str) -> bool {
    [Slot::Wield, Slot::Hold].into_iter().any(|slot| {
        worn_in(world, mob, slot)
            .and_then(|item| proto_of(world, item))
            .filter(|p| p.r#type == ObjectType::Weapon)
            .and_then(|p| p.weapon_damage_type.as_deref())
            .is_some_and(|t| {
                t.eq_ignore_ascii_case(wanted)
                    || crate::combat::weapon_family(t).is_some_and(|f| f == wanted)
            })
    })
}

/// Whether `entity` carries a debuff answering to one of `names`: an effect of
/// that name, or one applied by the ability of that name (`blind` also answers
/// to every blindness flag). The same matching the cleansing spells use.
fn has_named_debuff(world: &mut World, entity: Entity, names: &[String]) -> bool {
    let key = |s: &str| -> String {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    let wanted: Vec<String> = names.iter().map(|n| key(n)).collect();
    let ability_ids: Vec<i32> = world
        .get_resource::<AbilityCatalog>()
        .map(|cat| {
            cat.by_name
                .values()
                .filter(|a| wanted.contains(&key(&a.plain_name)))
                .map(|a| a.id)
                .collect()
        })
        .unwrap_or_default();
    let blind = names
        .iter()
        .any(|n| mud_world::mob_effects::is_blind_flag(&n.to_ascii_lowercase()));
    let mut q = world.query::<(&mud_world::EffectInstance, &mud_world::AppliedTo)>();
    q.iter(world).any(|(eff, applied)| {
        applied.0 == entity
            && !mud_world::mob_effects::is_innate_effect(&eff.source)
            && (names.iter().any(|n| eff.name.eq_ignore_ascii_case(n))
                || (blind && mud_world::mob_effects::is_blind_flag(&eff.name.to_ascii_lowercase()))
                || eff.ability_id.is_some_and(|id| ability_ids.contains(&id)))
    })
}

/// Whether one condition holds. `subject` is the chosen target; the
/// target-side conditions are only asked once there is one.
fn condition_holds(
    world: &mut World,
    ctx: &mut RoundCtx,
    scene: &Scene<'_>,
    subject: Option<Entity>,
    cond: &AiCondition,
) -> bool {
    let target = subject.unwrap_or(scene.mob);
    match cond {
        // Roll-style conditions were settled before the rule was evaluated.
        AiCondition::Telegraph
        | AiCondition::CooldownGroup(_)
        | AiCondition::LevelScaledChance(_)
        | AiCondition::LevelRoll { .. } => true,
        AiCondition::InCombat => scene.in_combat,
        AiCondition::OutOfCombat => !scene.in_combat,
        AiCondition::SelfHpBelow(p) => hp_pct(world, scene.mob) < i64::from(*p),
        AiCondition::TargetHpBelow(p) => hp_pct(world, target) < i64::from(*p),
        AiCondition::TargetHpAbove(p) => hp_pct(world, target) > i64::from(*p),
        AiCondition::AllyHpBelow(p) => allies_of(world, ctx, scene)
            .into_iter()
            .any(|a| hp_pct(world, a) < i64::from(*p)),
        AiCondition::GroupedTargets(n) => scene.foes.len() >= usize::from(*n),
        AiCondition::TargetCasting => world.get::<mud_world::Casting>(target).is_some(),
        AiCondition::TargetHasBuff => ctx.effects_on(world, target).buffs > 0,
        AiCondition::SelfHasDebuff => ctx.effects_on(world, scene.mob).debuffs > 0,
        AiCondition::SelfHasDebuffNamed(names) => has_named_debuff(world, scene.mob, names),
        AiCondition::TargetLifeforce(lf) => lifeforce_is(world, target, *lf),
        AiCondition::NotLifeforce(lf) => !lifeforce_is(world, target, *lf),
        AiCondition::Align(band) => mud_world::AiAlign::of(alignment_of(world, target)) == *band,
        AiCondition::NotAlign(band) => mud_world::AiAlign::of(alignment_of(world, target)) != *band,
        AiCondition::SelfAlign(mask) => {
            mask.contains(mud_world::AiAlign::of(alignment_of(world, scene.mob)))
        }
        AiCondition::TargetLevelMax(n) => {
            mud_world::effective_level(world, target)
                <= mud_world::effective_level(world, scene.mob).saturating_add(*n)
        }
        AiCondition::Composition(list) => {
            list.contains(&mud_world::actor_composition(world, scene.mob))
        }
        AiCondition::NotComposition(list) => {
            !list.contains(&mud_world::actor_composition(world, scene.mob))
        }
        AiCondition::RequiresShield => wears_shield(world, scene.mob),
        AiCondition::RequiresWeaponType(t) => wields_weapon_type(world, scene.mob, t),
        AiCondition::Outdoors => !crate::commands::room_is_indoors(world, scene.room),
    }
}

/// The mob's current opponent among its live foes.
fn tank_of(world: &World, scene: &Scene<'_>) -> Option<Entity> {
    world
        .get::<mud_world::Fighting>(scene.mob)
        .map(|f| f.0)
        .filter(|t| scene.foes.contains(t))
        .or_else(|| scene.foes.first().copied())
}

/// Resolve a rule's selector to `(forced target, subject for conditions)`.
/// `None` when nobody qualifies. A `Room` rule forces no target: the
/// ability fans out over the room itself. `Auto` leaves it to the ability:
/// a hostile area ability fans out, any other hostile one goes at the
/// opponent, and a friendly one at the mob itself.
fn pick_target(
    world: &mut World,
    ctx: &mut RoundCtx,
    scene: &Scene<'_>,
    selector: AiTarget,
    info: &AbilityInfo,
) -> Option<(Option<Entity>, Entity)> {
    let selector = match selector {
        AiTarget::Auto if info.hostile && info.area => AiTarget::Room,
        AiTarget::Auto if info.hostile => AiTarget::Tank,
        AiTarget::Auto => AiTarget::SelfTarget,
        other => other,
    };
    let pick = match selector {
        AiTarget::SelfTarget => return Some((Some(scene.mob), scene.mob)),
        AiTarget::Auto | AiTarget::Tank => tank_of(world, scene)?,
        AiTarget::Room => return tank_of(world, scene).map(|t| (None, t)),
        AiTarget::WeakestAttacker => scene
            .foes
            .iter()
            .copied()
            .min_by_key(|&e| world.get::<mud_world::Health>(e).map_or(i32::MAX, |h| h.hp))?,
        AiTarget::Caster => scene
            .foes
            .iter()
            .copied()
            .find(|&e| world.get::<mud_world::Casting>(e).is_some())?,
        AiTarget::MostBuffed => {
            let best = scene
                .foes
                .iter()
                .copied()
                .map(|e| (ctx.effects_on(world, e).buffs, e))
                .max_by_key(|(n, _)| *n)?;
            if best.0 == 0 {
                return None;
            }
            best.1
        }
        AiTarget::LowestAlly => allies_of(world, ctx, scene)
            .into_iter()
            .filter(|&a| hp_pct(world, a) < 100)
            .min_by_key(|&a| hp_pct(world, a))?,
    };
    Some((Some(pick), pick))
}

fn ability_kind(kind: AiKind) -> (AbilityKind, &'static str) {
    match kind {
        AiKind::Spell => (AbilityKind::Spell, "cast"),
        AiKind::Skill => (AbilityKind::Skill, "use"),
        AiKind::Song => (AbilityKind::Song, "perform"),
        AiKind::Chant => (AbilityKind::Chant, "chant"),
    }
}

/// Live enemies of `mob` in `room`: whatever fights it, plus the one it fights.
fn foes_of(world: &mut World, ctx: &mut RoundCtx, mob: Entity, room: Entity) -> Vec<Entity> {
    let mut v = ctx.attackers_of(world, mob);
    if let Some(f) = world.get::<mud_world::Fighting>(mob).map(|f| f.0)
        && !v.contains(&f)
    {
        v.push(f);
    }
    v.retain(|&e| in_room_alive(world, e, room));
    v
}

/// Whether a rule that passed its roll can actually fire right now: its
/// conditions hold, it has a target, the target is not already under the
/// effect (nobody re-casts a buff they wear, or a hindrance on a victim who
/// has it), and a silenced mob is not asked to speak a spell. Returns the
/// ability and the forced target for the core.
fn plan_rule(
    world: &mut World,
    ctx: &mut RoundCtx,
    scene: &Scene<'_>,
    rule: &AiRule,
) -> Option<(AbilityInfo, Option<Entity>)> {
    if !rule
        .when
        .iter()
        .filter(|c| !needs_target(c))
        .all(|c| condition_holds(world, ctx, scene, None, c))
    {
        return None;
    }
    let info = ctx.ability(world, rule.ability_id)?;
    let (forced, subject) = pick_target(world, ctx, scene, rule.target, &info)?;
    // A rule aimed at the mob itself asks its target-side conditions about
    // the opponent (legacy `victim`); with no opponent they hold vacuously.
    let cond_subject = if subject == scene.mob {
        tank_of(world, scene)
    } else {
        Some(subject)
    };
    if let Some(cs) = cond_subject
        && !rule
            .when
            .iter()
            .filter(|c| needs_target(c))
            .all(|c| condition_holds(world, ctx, scene, Some(cs), c))
    {
        return None;
    }
    if let Some(t) = forced
        && ctx.has_effect_from(world, t, rule.ability_id)
    {
        return None;
    }
    let verbal = !matches!(rule.kind, AiKind::Skill);
    if verbal
        && (crate::commands::effect_prevents(world, scene.mob, crate::commands::Prevent::Speaking)
            || crate::commands::effect_prevents(
                world,
                scene.mob,
                crate::commands::Prevent::Casting,
            ))
    {
        return None;
    }
    Some((info, forced))
}

/// Which walk a mob's rules are being run for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Walk {
    /// A combat round: rules for a fight.
    Combat,
    /// The 10 s mob pulse: rules for a mob at peace.
    Idle,
}

/// Run one mob's rules for this walk. True when it used an ability (a cast
/// begun, or an instant one landed): in combat its melee swing is skipped.
#[allow(clippy::too_many_lines)] // one linear walk down the rule list
fn run_mob_rules(
    world: &mut World,
    ctx: &mut RoundCtx,
    mob: Entity,
    tick: u64,
    scale: f64,
    walk: Walk,
) -> bool {
    use mud_world::Health;

    if !mob_can_swing(world, mob)
        || world.get::<Health>(mob).is_none_or(|h| h.hp <= 0)
        || crate::fear::is_feared(world, mob)
    {
        return false;
    }
    let Some(room) = world.get::<Located>(mob).map(|l| l.0) else {
        return false;
    };
    let Some(rules) = world
        .get::<MobAi>(mob)
        .map(|a| std::sync::Arc::clone(&a.rules))
    else {
        return false;
    };
    ctx.rolls = MobRolls::default();
    let level = mud_world::effective_level(world, mob);
    let no_magic = world.get::<mud_world::NoMagicRoom>(room).is_some();
    let in_combat = walk == Walk::Combat;
    let mut foes: Option<Vec<Entity>> = None;
    let mut evals = 0;
    let mut checked = 0;
    for (idx, rule) in rules.iter().enumerate() {
        if evals >= MAX_RULE_EVALS || checked >= MAX_RULES_CHECKED {
            break;
        }
        // Each walk runs its own rules: a fight does not buff or cure, and
        // the idle pulse neither fights nor warns of a breath. Skills are
        // left to their own pulses (hide) or to a fight.
        let wants_fight = rule.when.contains(&AiCondition::InCombat);
        let wants_peace = rule.when.contains(&AiCondition::OutOfCombat);
        if (in_combat && wants_peace)
            || (!in_combat
                && (wants_fight
                    || matches!(rule.kind, AiKind::Skill)
                    || rule.when.contains(&AiCondition::Telegraph)))
        {
            continue;
        }
        if rule.min_level.is_some_and(|min| level < min) {
            continue;
        }
        let (last, warned_at, failed) = world.get::<MobAi>(mob).map_or((None, None, None), |a| {
            (a.cooldown_started(idx), a.warned_at(idx), a.failed_at(idx))
        });
        if last
            .is_some_and(|t| tick.saturating_sub(t) < u64::from(rule.cooldown_s) * TICKS_PER_SECOND)
            || failed.is_some_and(|t| tick.saturating_sub(t) < FAILED_RETRY_TICKS)
        {
            continue;
        }
        let verbal = !matches!(rule.kind, AiKind::Skill);
        if verbal && no_magic {
            continue;
        }
        checked += 1;
        let telegraphed = rule.when.contains(&AiCondition::Telegraph);
        // A telegraphed rule that warned last round follows through now,
        // without a fresh roll.
        let follow_through = telegraphed
            && tick
                .checked_sub(crate::combat::COMBAT_PERIOD_TICKS)
                .is_some_and(|prev| warned_at == Some(prev));
        let rolled = follow_through
            || (chance_passes(rule.chance_pct, scale) && roll_gates_pass(ctx, level, scale, rule));
        if !rolled {
            continue;
        }
        // A spell with no slot to cast it from is not a candidate: it neither
        // warns the room nor uses up one of the round's evaluations.
        if matches!(rule.kind, AiKind::Spell)
            && !crate::commands::spell_slot_open(world, mob, rule.ability_id)
        {
            if follow_through && let Some(mut ai) = world.get_mut::<MobAi>(mob) {
                ai.clear_warned(idx);
            }
            continue;
        }
        let foes = foes.get_or_insert_with(|| {
            if in_combat {
                foes_of(world, ctx, mob, room)
            } else {
                Vec::new()
            }
        });
        let scene = Scene {
            mob,
            room,
            foes: foes.as_slice(),
            in_combat,
        };
        let Some((info, forced)) = plan_rule(world, ctx, &scene, rule) else {
            if follow_through && let Some(mut ai) = world.get_mut::<MobAi>(mob) {
                ai.clear_warned(idx);
            }
            continue;
        };
        // Only a rule whose conditions passed (an attempt is made) counts
        // against the round's budget; rules that fail their conditions are
        // free, so a healthy cleric's self-heals do not starve its attacks.
        evals += 1;
        if telegraphed && !follow_through {
            telegraph(world, mob, room, &info.display);
            if let Some(mut ai) = world.get_mut::<MobAi>(mob) {
                ai.mark_warned(idx, tick);
            }
            continue;
        }
        if let Some(mut ai) = world.get_mut::<MobAi>(mob) {
            ai.clear_warned(idx);
        }
        if use_rule(world, mob, rule, &info, forced) {
            if let Some(mut ai) = world.get_mut::<MobAi>(mob) {
                ai.mark_used(idx, tick);
            }
            tracing::debug!(
                mob = ?mob,
                rule = idx,
                ability = info.key.as_str(),
                target = ?forced,
                "mob AI used an ability"
            );
            return true;
        }
        // It did not take effect: let the rule rest before it is tried again.
        if let Some(mut ai) = world.get_mut::<MobAi>(mob) {
            ai.mark_failed(idx, tick);
        }
    }
    false
}

/// The room's one-round warning before a telegraphed ability.
fn telegraph(world: &mut World, mob: Entity, room: Entity, ability: &str) {
    let name = crate::commands::cap_sentence_start(&crate::commands::name_of(world, mob));
    crate::commands::broadcast_room_except_rendered(
        world,
        room,
        &[],
        &format!("<red>{name} gathers its power for {ability}!</>\r\n"),
    );
}

/// Invoke the rule's ability through the player pipeline. True when it took
/// effect: it began winding up or it landed.
fn use_rule(
    world: &mut World,
    mob: Entity,
    rule: &AiRule,
    info: &AbilityInfo,
    forced: Option<Entity>,
) -> bool {
    let (kind, verb) = ability_kind(rule.kind);
    crate::commands::reset_cast_landed();
    crate::commands::invoke_ability_core(
        world,
        mob,
        &format!("'{}'", info.key),
        kind,
        verb,
        false,
        false,
        false,
        forced,
        false,
    );
    let began = world
        .get::<mud_world::Casting>(mob)
        .is_some_and(|c| c.ability_id == rule.ability_id);
    began || crate::commands::cast_landed()
}

fn mob_ai_scale(world: &World) -> f64 {
    world
        .get_resource::<RuntimeConfig>()
        .map_or(1.0, |c| c.get_f64("mob_ai", "scale", 1.0))
}

/// One combat round of mob spell / skill AI. Returns the mobs that used an
/// ability and so skip their melee swing this round. Call only when
/// `mob_ai.enabled` is on; it does nothing otherwise.
pub(crate) fn mob_combat_ai_tick(world: &mut World) -> HashSet<Entity> {
    let mut acted = HashSet::new();
    if !mud_world::mob_ai_enabled(world) {
        return acted;
    }
    let mobs: Vec<Entity> = {
        let mut q = world.query_filtered::<
            Entity,
            (
                With<mud_world::Mob>,
                With<MobAi>,
                With<mud_world::Fighting>,
            ),
        >();
        q.iter(world).collect()
    };
    if mobs.is_empty() {
        return acted;
    }
    let tick = world.resource::<crate::TickCount>().0;
    let scale = mob_ai_scale(world);
    let mut ctx = RoundCtx::default();
    for mob in mobs {
        if world.get_entity(mob).is_ok()
            && run_mob_rules(world, &mut ctx, mob, tick, scale, Walk::Combat)
        {
            acted.insert(mob);
        }
    }
    acted
}

// ---------------------------------------------------------------------------
// Idle pulse (WP-D)

/// Every slot is back: nothing recovering and nothing held by a cast.
fn slots_full(world: &World, mob: Entity) -> bool {
    world
        .get::<SpellSlots>(mob)
        .is_none_or(|s| s.in_flight.is_empty() && s.reserved.is_empty())
}

/// Legacy `check_spellbank`: a caster with spent slots sits down and
/// meditates. The slot recovery tick does the rest (sitting doubles the pace,
/// meditating doubles it again).
fn begin_meditation(world: &mut World, mob: Entity, room: Entity) {
    world.entity_mut(mob).insert((
        mud_world::Posture(mud_world::PostureKind::Sitting),
        mud_world::Meditating,
    ));
    let name = crate::commands::cap_sentence_start(&crate::commands::name_of(world, mob));
    let their = crate::flight::possessive(world, mob);
    crate::commands::broadcast_room_except_rendered(
        world,
        room,
        &[mob],
        &format!("{name} sits down and closes {their} eyes, slipping into meditation.\r\n"),
    );
}

/// The slots are full again (`spellslot_restore_event`): the mob ends its
/// trance and gets back on its feet.
fn end_meditation(world: &mut World, mob: Entity, room: Entity) {
    world
        .entity_mut(mob)
        .remove::<mud_world::Meditating>()
        .insert(mud_world::Posture(mud_world::PostureKind::Standing));
    let name = crate::commands::cap_sentence_start(&crate::commands::name_of(world, mob));
    let their = crate::flight::possessive(world, mob);
    crate::commands::broadcast_room_except_rendered(
        world,
        room,
        &[mob],
        &format!("{name} ceases {their} meditative trance and rises.\r\n"),
    );
}

/// `mob_ai.enabled` was flipped off while mobs already carried the AI: take
/// it back so the world matches a build that never had it. A mob loses its
/// [`MobAi`], its [`SpellSlots`] and any meditation (a meditating mob stands
/// up), so nothing keeps sitting, recovering slots or casting. Mobs spawn
/// without these while the flag is off, so on a flag that stays off this
/// finds nothing; turning the flag back on only equips mobs spawned after.
fn strip_mob_ai(world: &mut World) {
    let mobs: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (
            With<mud_world::Mob>,
            Or<(With<MobAi>, With<SpellSlots>, With<mud_world::Meditating>)>,
        )>();
        q.iter(world).collect()
    };
    for mob in mobs {
        // Only a mob the AI sat down is stood back up: `Meditating` on a mob
        // without `MobAi` belongs to something else (for example a switched-in immortal).
        let was_ai = world.get::<MobAi>(mob).is_some();
        if was_ai && world.get::<mud_world::Meditating>(mob).is_some() {
            match world.get::<Located>(mob).map(|l| l.0) {
                Some(room) => end_meditation(world, mob, room),
                None => {
                    world
                        .entity_mut(mob)
                        .remove::<mud_world::Meditating>()
                        .insert(mud_world::Posture(mud_world::PostureKind::Standing));
                }
            }
        }
        world.entity_mut(mob).remove::<(MobAi, SpellSlots)>();
    }
}

/// One 10 s pulse of idle mob AI (legacy `mobile_activity` -> `check_mob_status`
/// and `check_spellbank`). Only mobs that carry a `MobAi`, are not fighting,
/// and stand in a room with a player are considered, so the cost follows the
/// number of occupied rooms and not the size of the world. Each such mob
///   * ends its meditation when its slots are full (and does nothing else
///     while still meditating),
///   * otherwise tries its out-of-combat rules (buffs, cures, ...), one
///     ability per pulse,
///   * and failing that, a caster with spent slots sits down to meditate.
///
/// A mob someone is fighting is left to the combat tick. With
/// `mob_ai.enabled` off it does no AI, only [`strip_mob_ai`].
pub(crate) fn mob_idle_ai_tick(world: &mut World) {
    if !mud_world::mob_ai_enabled(world) {
        strip_mob_ai(world);
        return;
    }
    let player_rooms: HashSet<Entity> = {
        let mut q = world.query_filtered::<&Located, With<mud_world::Player>>();
        q.iter(world).map(|l| l.0).collect()
    };
    if player_rooms.is_empty() {
        return;
    }
    let candidates: Vec<(Entity, Entity)> = player_rooms
        .iter()
        .flat_map(|&room| {
            crate::room_index::contents_of(world, room)
                .filter(|&e| {
                    world.get::<mud_world::Mob>(e).is_some()
                        && world.get::<MobAi>(e).is_some()
                        && world.get::<mud_world::Fighting>(e).is_none()
                })
                .map(move |e| (e, room))
                .collect::<Vec<_>>()
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    let tick = world.resource::<crate::TickCount>().0;
    let scale = mob_ai_scale(world);
    let mut ctx = RoundCtx::default();
    for (mob, room) in candidates {
        if world.get_entity(mob).is_err()
            || !mob_can_act(world, mob)
            || world.get::<mud_world::Health>(mob).is_none_or(|h| h.hp <= 0)
            || crate::fear::is_feared(world, mob)
            // Never act while somebody is fighting it.
            || !ctx.attackers_of(world, mob).is_empty()
        {
            continue;
        }
        if world.get::<mud_world::Meditating>(mob).is_some() {
            if slots_full(world, mob) {
                end_meditation(world, mob, room);
            }
            continue;
        }
        if run_mob_rules(world, &mut ctx, mob, tick, scale, Walk::Idle) {
            continue;
        }
        if world.get::<SpellSlots>(mob).is_some()
            && !slots_full(world, mob)
            && mob_can_swing(world, mob)
            && !has_named_debuff(world, mob, &["insanity".to_string()])
        {
            begin_meditation(world, mob, room);
        }
    }
}

#[cfg(test)]
mod tests {
    //! Scavenger item AI: choice by `appraise_item`, `CAN_GET_OBJ` limits,
    //! and `mob_attempt_equip` through the normal gear path.

    use bevy_ecs::prelude::*;
    use mud_db::enums::{MobBehavior, ObjectRestriction, ObjectType, WearFlag};
    use mud_world::{
        CombatStats, EquippedSlot, Exits, Item, Keywords, Located, Mob, MobBehaviors, Named,
        ObjectPrototypes, ObjectRestrictions, Room, Slot, WorldKey,
    };

    use crate::TickCount;
    use crate::commands::test_support::object_proto;
    use crate::wander::{SCAVENGER_PERIOD_TICKS, scavenger_tick};

    fn setup() -> (World, Entity, Entity) {
        super::force_scavenge_roll(Some(true));
        let mut world = World::new();
        world.insert_resource(ObjectPrototypes::default());
        world.init_resource::<mud_world::ObjectAbilityCatalog>();
        world.insert_resource(TickCount(SCAVENGER_PERIOD_TICKS));
        let room = world
            .spawn((
                Room,
                Named {
                    name: "A hall".into(),
                },
                Exits::default(),
            ))
            .id();
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "a magpie".into(),
                },
                Located(room),
                MobBehaviors(vec![MobBehavior::Scavenger]),
                CombatStats::default(),
            ))
            .id();
        (world, room, mob)
    }

    /// Spawn an item of proto `(1, id)` (built by `tweak`) at `holder`.
    fn item(
        world: &mut World,
        holder: Entity,
        id: i32,
        kind: ObjectType,
        tweak: impl FnOnce(&mut mud_world::ObjectProto),
    ) -> Entity {
        let mut proto = object_proto(1, id, kind);
        proto.name = format!("item {id}");
        proto.keywords = vec![format!("item{id}")];
        tweak(&mut proto);
        let restrictions = proto.restrictions.clone();
        let name = proto.name.clone();
        let keyword = proto.keywords.clone();
        world
            .resource_mut::<ObjectPrototypes>()
            .by_key
            .insert((1, id), proto);
        let e = world
            .spawn((
                Item,
                Named { name },
                Keywords(keyword),
                WorldKey { zone: 1, id },
                Located(holder),
            ))
            .id();
        if !restrictions.is_empty() {
            world.entity_mut(e).insert(ObjectRestrictions(restrictions));
        }
        e
    }

    fn holder(world: &World, e: Entity) -> Entity {
        world.get::<Located>(e).unwrap().0
    }

    #[test]
    fn status_flag_scores_come_from_the_resource() {
        let (mut world, room, mob) = setup();
        let mut catalog = mud_world::EffectCatalog::default();
        catalog.by_id.insert(
            7,
            mud_world::EffectDef {
                id: 7,
                name: "status".into(),
                description: None,
                effect_type: "status".into(),
                tags: Vec::new(),
                presence_override: None,
                default_params: serde_json::json!({}),
                prevents_speaking: false,
                prevents_casting: false,
                prevents_movement: false,
                on_apply: None,
                on_tick: None,
                on_remove: None,
            },
        );
        world.insert_resource(catalog);
        let ring = item(&mut world, room, 1, ObjectType::Other, |p| {
            p.granted_effects.push(mud_world::ObjectGrantedEffect {
                effect_id: 7,
                strength: 1,
                modifier_data: serde_json::json!({"flags": ["sanctuary", "brand_new_flag"]}),
                wear_location: None,
            });
        });
        let base = super::appraise_item(&world, mob, ring);

        // No resource (table missing at boot): every flag scores 0.
        assert_eq!(super::value_status_flag(&world, "sanctuary"), 0);

        let mut values = mud_world::StatusFlagValues::default();
        values.by_flag.insert("sanctuary".into(), 90);
        world.insert_resource(values);
        assert_eq!(super::value_status_flag(&world, "sanctuary"), 90);
        assert_eq!(super::value_status_flag(&world, "brand_new_flag"), 0);
        assert_eq!(
            super::appraise_item(&world, mob, ring),
            base + 90,
            "a flag with a row adds its score; a flag without adds nothing"
        );
    }

    #[test]
    fn scavenger_takes_the_most_valuable_takeable_item() {
        let (mut world, room, mob) = setup();
        let trinket = item(&mut world, room, 1, ObjectType::Other, |p| {
            p.cost = 500;
        });
        let gem = item(&mut world, room, 2, ObjectType::Treasure, |p| {
            p.cost = 90_000;
        });
        // The most valuable thing here is fixed to the floor.
        let statue = item(&mut world, room, 3, ObjectType::Other, |p| {
            p.cost = 9_000_000;
            p.restrictions = vec![ObjectRestriction::NoTake];
        });
        scavenger_tick(&mut world);
        assert_eq!(holder(&world, gem), mob, "best takeable item");
        assert_eq!(holder(&world, trinket), room);
        assert_eq!(holder(&world, statue), room, "!TAKE stays");
    }

    #[test]
    fn scavenger_skips_the_pickup_on_a_losing_roll() {
        let (mut world, room, mob) = setup();
        let gem = item(&mut world, room, 2, ObjectType::Treasure, |p| {
            p.cost = 90_000;
        });
        super::force_scavenge_roll(Some(false));
        scavenger_tick(&mut world);
        assert_eq!(holder(&world, gem), room);
        super::force_scavenge_roll(Some(true));
        scavenger_tick(&mut world);
        assert_eq!(holder(&world, gem), mob);
    }

    #[test]
    fn scavenger_does_not_exceed_its_carry_capacity() {
        let (mut world, room, mob) = setup();
        // Level-1 mob: 105 lb capacity, 20 already in the pack.
        let cap = crate::commands::carry_capacity(&world, mob);
        let _pack = item(&mut world, mob, 9, ObjectType::Other, |p| p.weight = 20.0);
        let anvil = item(&mut world, room, 1, ObjectType::Treasure, |p| {
            p.cost = 90_000;
            p.weight = cap - 10.0;
        });
        let coin = item(&mut world, room, 2, ObjectType::Other, |p| {
            p.cost = 100;
            p.weight = 1.0;
        });
        scavenger_tick(&mut world);
        assert_eq!(holder(&world, anvil), room, "too heavy to lift");
        assert_eq!(holder(&world, coin), mob, "the light one still fits");
        let carried = crate::commands::carried_weight(&mut world, mob);
        assert!(carried <= cap, "{carried} over {cap}");
    }

    #[test]
    fn scavenger_wears_a_pickup_that_beats_an_empty_slot_and_gear_applies() {
        let (mut world, room, mob) = setup();
        let helm = item(&mut world, room, 1, ObjectType::Armor, |p| {
            p.armor_pct = 30;
            p.wear_flags = vec![WearFlag::Head];
        });
        scavenger_tick(&mut world);
        assert_eq!(holder(&world, helm), mob);
        assert_eq!(
            world.get::<EquippedSlot>(helm).map(|e| e.0),
            Some(Slot::Head)
        );
        assert_eq!(world.get::<CombatStats>(mob).unwrap().armor_pct, 30);
        // Death / despawn releases the bonus.
        crate::equip_apply::release_gear(&mut world, helm);
        assert_eq!(world.get::<CombatStats>(mob).unwrap().armor_pct, 0);
    }

    #[test]
    fn scavenger_swaps_in_an_upgrade_and_keeps_a_downgrade_in_the_pack() {
        let (mut world, _room, mob) = setup();
        let old = item(&mut world, mob, 1, ObjectType::Armor, |p| {
            p.armor_pct = 10;
            p.wear_flags = vec![WearFlag::Head];
        });
        let better = item(&mut world, mob, 2, ObjectType::Armor, |p| {
            p.armor_pct = 30;
            p.wear_flags = vec![WearFlag::Head];
        });
        let worse = item(&mut world, mob, 3, ObjectType::Armor, |p| {
            p.armor_pct = 4;
            p.wear_flags = vec![WearFlag::Head];
        });
        // Start with the old helm on.
        assert!(crate::commands::wear_item(
            &mut world,
            mob,
            old,
            crate::commands::WearWhere::Default,
            true,
        ));
        assert_eq!(world.get::<CombatStats>(mob).unwrap().armor_pct, 10);
        scavenger_tick(&mut world);
        let worn = |w: &World, e: Entity| w.get::<EquippedSlot>(e).is_some();
        assert!(worn(&world, better), "upgrade worn");
        assert!(!worn(&world, old), "old helm back in the pack");
        assert!(!worn(&world, worse), "downgrade stays in the pack");
        assert_eq!(
            world.get::<CombatStats>(mob).unwrap().armor_pct,
            30,
            "old bonus released, new one applied"
        );
        // A second pass changes nothing.
        scavenger_tick(&mut world);
        assert!(worn(&world, better));
        assert_eq!(world.get::<CombatStats>(mob).unwrap().armor_pct, 30);
    }

    #[test]
    fn a_refused_upgrade_leaves_the_mob_dressed_and_silent() {
        let (mut world, room, mob) = setup();
        world.get_mut::<CombatStats>(mob).unwrap().alignment = -600;
        let (_watcher, mut rx) = crate::commands::test_support::player_in(&mut world, room);
        let old = item(&mut world, mob, 1, ObjectType::Armor, |p| {
            p.armor_pct = 4;
            p.wear_flags = vec![WearFlag::Head];
        });
        // Far better, but anti-evil: `wear_item` would refuse it.
        let holy = item(&mut world, mob, 2, ObjectType::Armor, |p| {
            p.armor_pct = 100;
            p.wear_flags = vec![WearFlag::Head];
            p.restricted_alignments = vec![mud_db::enums::Alignment::Evil];
        });
        assert!(crate::commands::wear_item(
            &mut world,
            mob,
            old,
            crate::commands::WearWhere::Default,
            true,
        ));
        let _ = crate::commands::test_support::drain(&mut rx);
        for _ in 0..3 {
            scavenger_tick(&mut world);
        }
        assert!(
            world.get::<EquippedSlot>(old).is_some(),
            "old helm stays on"
        );
        assert!(world.get::<EquippedSlot>(holy).is_none());
        assert_eq!(world.get::<CombatStats>(mob).unwrap().armor_pct, 4);
        assert_eq!(crate::commands::test_support::drain(&mut rx), "");
    }

    #[test]
    fn a_free_paired_slot_takes_the_new_piece_without_removing_the_old() {
        let (mut world, _room, mob) = setup();
        let ring = |world: &mut World, id: i32, armor_pct: i32| {
            item(world, mob, id, ObjectType::Armor, |p| {
                p.armor_pct = armor_pct;
                p.wear_flags = vec![WearFlag::Finger];
            })
        };
        let first = ring(&mut world, 1, 2);
        assert!(crate::commands::wear_item(
            &mut world,
            mob,
            first,
            crate::commands::WearWhere::Default,
            true,
        ));
        // Better than the worn ring, and the other finger is bare.
        let second = ring(&mut world, 2, 40);
        scavenger_tick(&mut world);
        let slot = |w: &World, e: Entity| w.get::<EquippedSlot>(e).map(|s| s.0);
        assert!(slot(&world, first).is_some(), "first ring stays on");
        assert!(
            slot(&world, second).is_some(),
            "second ring takes the free finger"
        );
        assert_ne!(slot(&world, first), slot(&world, second));
        assert_eq!(world.get::<CombatStats>(mob).unwrap().armor_pct, 42);
    }

    #[test]
    fn appraisal_follows_the_legacy_formula() {
        let (mut world, room, mob) = setup();
        // Armor: AC (armor_pct / 2) + cost / 100 + 100 - level.
        let plate = item(&mut world, room, 1, ObjectType::Armor, |p| {
            p.armor_pct = 40;
            p.cost = 1_000;
            p.level = 10;
            p.wear_flags = vec![WearFlag::Body];
        });
        assert_eq!(super::appraise_item(&world, mob, plate), 20 + 10 + 90);
        // Weapon: dice average (size + 1) / 2 * num, truncated.
        let sword = item(&mut world, room, 2, ObjectType::Weapon, |p| {
            p.weapon_dice_num = 2;
            p.weapon_dice_size = 6;
            p.level = 1;
        });
        assert_eq!(super::appraise_item(&world, mob, sword), 7 + 99);
    }
}
