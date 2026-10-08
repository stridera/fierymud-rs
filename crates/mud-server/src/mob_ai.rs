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

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectFlag, ObjectRestriction, ObjectType};
use mud_world::{
    AbilityCatalog, Charges, Contents, DetectInvis, EffectCatalog, EquippedSlot, Item, LightFuel,
    LiquidContainer, Located, MobPrototypes, ObjectAbilityCatalog, ObjectFlags, ObjectProto,
    ObjectPrototypes, ObjectRestrictions, Slot, SpellSlotData, WorldKey,
    components::WeaponDiceSizeAdjust,
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
/// (paralysed) and frozen mobs do nothing.
pub(crate) fn mob_can_act(world: &World, mob: Entity) -> bool {
    world
        .get::<mud_world::Posture>(mob)
        .is_none_or(|p| p.0 != mud_world::PostureKind::Sleeping)
        && world.get::<mud_world::Casting>(mob).is_none()
        && world.get::<mud_world::Stunned>(mob).is_none()
        && world.get::<mud_world::Frozen>(mob).is_none()
        && world.get::<mud_world::Ghost>(mob).is_none()
}

fn proto_of(world: &World, item: Entity) -> Option<&ObjectProto> {
    let key = world.get::<WorldKey>(item)?;
    world
        .get_resource::<ObjectPrototypes>()?
        .by_key
        .get(&(key.zone, key.id))
}

/// The mob's class id (from its spawn prototype), for `NOWEAR_CLASS`.
fn class_of(world: &World, mob: Entity) -> Option<i32> {
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

/// Legacy `value_spell_effect`, keyed by the status flag name.
fn value_status_flag(flag: &str) -> i32 {
    match flag {
        "major_paralysis" => -50,
        "blind" | "blindness" => -40,
        "silence" | "insanity" | "hurt_throat" => -30,
        "poison" | "poisoned" | "sleep" | "minor_paralysis" | "on_fire" | "disease"
        | "animated" | "exposed" => -20,
        "charm" | "tamed" | "fear" => -10,
        "vitality" => -5,
        "curse" => 3,
        "enlarge" | "reduce" | "bless" => 10,
        "tongues" | "feather_fall" | "camouflaged" | "ray_of_enfeeblement" => 15,
        "farsee" | "detect_align" | "detect_poison" | "detect_magic" | "waterbreath"
        | "minor_globe" | "shadowing" => 20,
        "waterwalk" | "invisible" | "notrack" | "light" => 25,
        "sneak" | "sense_life" | "detect_life" | "infravision" => 35,
        "detect_invis" => 40,
        "protect_evil" | "protect_good" | "fly" | "soulshield" | "prot_fire" | "prot_cold"
        | "prot_air" | "prot_earth" | "fireshield" | "coldshield" | "ultravision" | "aware"
        | "vamp_touch" => 50,
        "haste" | "displacement" | "nimble" | "acid_weapon" | "fire_weapon" | "ice_weapon"
        | "radiant_weapon" | "poison_weapon" | "shock_weapon" => 60,
        "major_globe"
        | "harness"
        | "negate_heat"
        | "negate_cold"
        | "negate_air"
        | "negate_earth"
        | "greater_displacement" => 70,
        "blur" => 80,
        "sanctuary" => 90,
        "stone_skin" | "stoneskin" => 100,
        _ => 0,
    }
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
        "accuracy" | "attack_power" => 5 * amount,
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
                            value += value_status_flag(&flag);
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
    if class_of(world, mob).is_some_and(|c| proto.restricted_class_ids.contains(&c)) {
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

/// A mob never puts on gear built for another class or race.
fn mob_may_wear(world: &World, mob: Entity, item: Entity) -> bool {
    let Some(p) = proto_of(world, item) else {
        return true;
    };
    if class_of(world, mob).is_some_and(|c| p.restricted_class_ids.contains(&c)) {
        return false;
    }
    let race = mud_world::effective_race(world, mob);
    if p.restricted_races
        .iter()
        .any(|r| r.eq_ignore_ascii_case(&race))
    {
        return false;
    }
    p.allowed_races.is_empty()
        || p.allowed_races
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&race))
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
        if !can_see_obj(world, mob, obj) || !mob_may_wear(world, mob, obj) {
            continue;
        }
        let Some(slot) = auto_wear_slot(world, obj) else {
            continue;
        };
        let current = worn_in(world, mob, slot);
        if let Some(cur) = current {
            if appraise_item(world, mob, cur) >= appraise_item(world, mob, obj) {
                continue;
            }
            // Legacy `perform_remove` refuses with a full pack.
            if carried_count(world, mob) >= can_carry_n(world, mob) {
                continue;
            }
            unequip(world, mob, cur);
        }
        let worn = crate::commands::wear_item(world, mob, obj, WearWhere::Position(slot), true);
        if !worn && let Some(cur) = current {
            // The upgrade was refused (alignment, size, ...): put the
            // old piece back rather than strip the mob.
            crate::commands::wear_item(world, mob, cur, WearWhere::Position(slot), true);
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
