//! More `alter_object` modes (legacy `mag_alter_obj`, `spell_*` object
//! branches), selected by the effect's `mode` param. Like Curse and Enchant
//! Weapon (see [`super::object_spells`]) the behaviour is data on the
//! ability's `alter_object` row; nothing here knows a spell by name.
//!
//! - `flags` (Bless, Invisibility, Dispel Magic): optional gates, then set
//!   and clear object flags and bar an alignment. Params: `requireAnyFlag`
//!   (the object must carry one of them), `requireTypes` +
//!   `wrongTypeMessage`, `maxObjectLevel` / `maxObjectWeight` (formulas in
//!   `skill` / `level`) + `tooPowerfulMessage` / `tooLargeMessage`,
//!   `blockedByFlags` / `blockedByRestrictions` / `blockedByAlignments` +
//!   `blockedMessage`, `repelledIfMagical` + `repelledMessage`,
//!   `powerRoll` (a formula that must beat the object's level, else
//!   `resistMessageToCaster`), `setFlags`, `clearFlags`, `barAlignment`,
//!   `staffBypass` (staff skip every gate but `requireAnyFlag`).
//! - `poison`: `poisoned` true / false on food and drink (`requireTypes`).
//! - `liquid` (Create Water): `liquid`, `mixedLiquid` (used when the
//!   container already holds something else), `amount` (formula), `requireTypes`,
//!   `fullMessage`.
//! - `corpse_timer` (Preserve): `hours` formula, `{hours}` in the messages.
//! - `corpse_feed` (Dark Feast): `hunger` / `thirst` taken off the caster's
//!   gauges.
//! - `light`: `lightAction` `ignite` (refuel and light a burnt-out light;
//!   `capacityMultiplier`, `defaultFuel`, `permanentMessageToCaster`) or
//!   `drain` (burn it out; `extinguishMessageToCaster` / `ToRoom` when it
//!   was lit).
//!
//! Every mode takes `messageToCaster` / `messageToRoom` (success) and
//! `noopMessageToCaster` (nothing to do); `{item}` is the object's name and
//! the usual `{actor.name}` / `{actor.ref}` tokens work.

use bevy_ecs::prelude::*;
use mud_db::enums::{Alignment, ObjectFlag, ObjectRestriction, ObjectType};
use mud_world::components::ItemBarredAlignments;
use mud_world::{
    Corpse, CorpseDecay, Hunger, LightFuel, LiquidContainer, Lit, Located, ObjectFlags,
    ObjectProto, ObjectPrototypes, ObjectRestrictions, SECS_PER_MUD_HOUR, Thirst, WorldKey,
    bypasses_restrictions,
};

use super::object_spells::{param, param_str, param_strs, parse_alignment, parse_object_flag};
use super::{
    EffectSpec, FormulaCtx, Pronouns, Viewer, broadcast_room_except_rendered, name_or,
    numeric_or_formula, render_ability_template, send_msg,
};

/// Run the `mode` of `spec` if it is one of the modes here. `None` when the
/// mode is not handled by this module.
pub(super) fn apply(
    world: &mut World,
    caster: Entity,
    item: Entity,
    spec: &EffectSpec,
    ctx: &FormulaCtx,
) -> Option<bool> {
    let mode = param_str(spec, "mode")?.to_ascii_lowercase();
    Some(match mode.as_str() {
        "flags" => apply_flags(world, caster, item, spec, ctx),
        "poison" => apply_poison(world, caster, item, spec),
        "liquid" => apply_liquid(world, caster, item, spec, ctx),
        "corpse_timer" => apply_corpse_timer(world, caster, item, spec, ctx),
        "corpse_feed" => apply_corpse_feed(world, caster, item, spec),
        "light" => apply_light(world, caster, item, spec, ctx),
        _ => return None,
    })
}

fn param_bool(spec: &EffectSpec, key: &str) -> Option<bool> {
    param(spec, key).and_then(serde_json::Value::as_bool)
}

fn param_i32(spec: &EffectSpec, key: &str, default: i32) -> i32 {
    param(spec, key)
        .and_then(serde_json::Value::as_i64)
        .and_then(|n| i32::try_from(n).ok())
        .unwrap_or(default)
}

fn proto_of(world: &World, item: Entity) -> Option<ObjectProto> {
    let key = world.get::<WorldKey>(item)?;
    world
        .get_resource::<ObjectPrototypes>()?
        .by_key
        .get(&(key.zone, key.id))
        .cloned()
}

fn item_type(world: &World, item: Entity) -> Option<ObjectType> {
    proto_of(world, item).map(|p| p.r#type)
}

fn type_listed(world: &World, item: Entity, spec: &EffectSpec, default: &[&str]) -> bool {
    let mut listed = param_strs(spec, "requireTypes");
    if listed.is_empty() {
        listed = default.iter().map(|s| (*s).to_string()).collect();
    }
    if listed.is_empty() {
        return true;
    }
    item_type(world, item).is_some_and(|t| {
        listed
            .iter()
            .any(|r| format!("{t:?}").eq_ignore_ascii_case(&r.replace('_', "")))
    })
}

fn has_flag(world: &World, item: Entity, flag: ObjectFlag) -> bool {
    world.get::<ObjectFlags>(item).is_some_and(|f| f.has(flag))
}

fn is_corpse(world: &World, item: Entity) -> bool {
    world.get::<Corpse>(item).is_some() || item_type(world, item) == Some(ObjectType::Corpse)
}

fn render(
    world: &World,
    caster: Entity,
    item: Entity,
    template: &str,
    extra: &[(&str, String)],
    viewer: Viewer,
) -> String {
    let mut text = template.replace("{item}", &name_or(world, item, "it"));
    for (token, value) in extra {
        text = text.replace(token, value);
    }
    let actor = name_or(world, caster, "Someone");
    let pronouns = Pronouns::of(world, caster);
    render_ability_template(&text, (&actor, pronouns), (&actor, pronouns), false, viewer)
}

/// A message param: one line, or an array of lines sent in order.
fn lines(spec: &EffectSpec, key: &str) -> Vec<String> {
    match param(spec, key) {
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// Send `keys.0` to the caster and `keys.1` to the rest of their room.
fn emit(
    world: &mut World,
    caster: Entity,
    item: Entity,
    spec: &EffectSpec,
    keys: (&str, &str),
    extra: &[(&str, String)],
) {
    for t in lines(spec, keys.0) {
        let m = render(world, caster, item, &t, extra, Viewer::Actor);
        send_msg(world, caster, format!("{m}\r\n"));
    }
    let Some(room) = world.get::<Located>(caster).map(|l| l.0) else {
        return;
    };
    for t in lines(spec, keys.1) {
        let m = render(world, caster, item, &t, extra, Viewer::Bystander);
        broadcast_room_except_rendered(world, room, &[caster], &format!("{m}\r\n"));
    }
}

fn say(world: &mut World, caster: Entity, item: Entity, spec: &EffectSpec, key: &str) {
    for t in lines(spec, key) {
        let m = render(world, caster, item, &t, &[], Viewer::Actor);
        send_msg(world, caster, format!("{m}\r\n"));
    }
}

/// Refuse with the message under `key`, else the spec's no-effect line.
fn refuse(world: &mut World, caster: Entity, item: Entity, spec: &EffectSpec, key: &str) -> bool {
    let key = if param(spec, key).is_some() {
        key
    } else {
        "noopMessageToCaster"
    };
    say(world, caster, item, spec, key);
    false
}

fn flags_param(spec: &EffectSpec, key: &str) -> Vec<ObjectFlag> {
    param_strs(spec, key)
        .iter()
        .filter_map(|f| parse_object_flag(f))
        .collect()
}

/// Has stat applies, granted effects or an alignment bar already.
fn is_already_magical(world: &World, item: Entity, proto: Option<&ObjectProto>) -> bool {
    world
        .get::<mud_world::components::ItemApplies>(item)
        .is_some_and(|a| !a.0.is_empty())
        || world
            .get::<ItemBarredAlignments>(item)
            .is_some_and(|b| !b.0.is_empty())
        || proto.is_some_and(|p| !p.granted_effects.is_empty())
}

fn apply_flags(
    world: &mut World,
    caster: Entity,
    item: Entity,
    spec: &EffectSpec,
    ctx: &FormulaCtx,
) -> bool {
    let need = flags_param(spec, "requireAnyFlag");
    if !need.is_empty() && !need.iter().any(|f| has_flag(world, item, *f)) {
        return refuse(world, caster, item, spec, "noopMessageToCaster");
    }
    let proto = proto_of(world, item);
    let bypass =
        param_bool(spec, "staffBypass").unwrap_or(false) && bypasses_restrictions(world, caster);
    if !bypass {
        if param(spec, "requireTypes").is_some() && !type_listed(world, item, spec, &[]) {
            return refuse(world, caster, item, spec, "wrongTypeMessage");
        }
        let level = proto.as_ref().map_or(0, |p| p.level);
        if let Some(max) = param(spec, "maxObjectLevel").and_then(|v| numeric_or_formula(v, ctx))
            && level > max
        {
            return refuse(world, caster, item, spec, "tooPowerfulMessage");
        }
        #[allow(clippy::cast_possible_truncation)]
        let weight = proto.as_ref().map_or(0, |p| p.weight.ceil() as i32);
        if let Some(max) = param(spec, "maxObjectWeight").and_then(|v| numeric_or_formula(v, ctx))
            && weight > max
        {
            return refuse(world, caster, item, spec, "tooLargeMessage");
        }
        let blocked_flag = flags_param(spec, "blockedByFlags")
            .iter()
            .any(|f| has_flag(world, item, *f));
        let blocked_restriction = param_strs(spec, "blockedByRestrictions")
            .iter()
            .filter_map(|r| ObjectRestriction::from_db_str(r))
            .any(|r| {
                world
                    .get::<ObjectRestrictions>(item)
                    .is_some_and(|x| x.has(r))
            });
        let blocked_alignment = param_strs(spec, "blockedByAlignments")
            .iter()
            .filter_map(|a| parse_alignment(a))
            .any(|a| {
                world
                    .get::<ItemBarredAlignments>(item)
                    .is_some_and(|b| b.0.contains(&a))
                    || proto
                        .as_ref()
                        .is_some_and(|p| p.restricted_alignments.contains(&a))
            });
        if blocked_flag || blocked_restriction || blocked_alignment {
            return refuse(world, caster, item, spec, "blockedMessage");
        }
        if param_bool(spec, "repelledIfMagical").unwrap_or(false)
            && is_already_magical(world, item, proto.as_ref())
        {
            return refuse(world, caster, item, spec, "repelledMessage");
        }
    }
    if let Some(roll) = param(spec, "powerRoll").and_then(|v| numeric_or_formula(v, ctx))
        && roll <= proto.as_ref().map_or(0, |p| p.level)
    {
        say(world, caster, item, spec, "resistMessageToCaster");
        return false;
    }
    for f in flags_param(spec, "setFlags") {
        crate::item_alter::add_flag(world, item, f);
    }
    for f in flags_param(spec, "clearFlags") {
        crate::item_alter::remove_flag(world, item, f);
    }
    if let Some(bar) = param_str(spec, "barAlignment").and_then(parse_alignment) {
        bar_alignment(world, item, bar);
    }
    crate::item_alter::mark_dirty(world, item);
    emit(
        world,
        caster,
        item,
        spec,
        ("messageToCaster", "messageToRoom"),
        &[],
    );
    true
}

fn bar_alignment(world: &mut World, item: Entity, bar: Alignment) {
    let Ok(mut em) = world.get_entity_mut(item) else {
        return;
    };
    let mut barred = em
        .get::<ItemBarredAlignments>()
        .map(|b| b.0.clone())
        .unwrap_or_default();
    if !barred.contains(&bar) {
        barred.push(bar);
    }
    em.insert(ItemBarredAlignments(barred));
}

fn apply_poison(world: &mut World, caster: Entity, item: Entity, spec: &EffectSpec) -> bool {
    let target = param_bool(spec, "poisoned").unwrap_or(true);
    if !type_listed(world, item, spec, &["DRINKCONTAINER", "FOUNTAIN", "FOOD"])
        || crate::item_alter::is_poisoned(world, item) == target
    {
        return refuse(world, caster, item, spec, "noopMessageToCaster");
    }
    crate::item_alter::set_poisoned(world, item, target);
    crate::item_alter::mark_dirty(world, item);
    emit(
        world,
        caster,
        item,
        spec,
        ("messageToCaster", "messageToRoom"),
        &[],
    );
    true
}

fn apply_liquid(
    world: &mut World,
    caster: Entity,
    item: Entity,
    spec: &EffectSpec,
    ctx: &FormulaCtx,
) -> bool {
    if !type_listed(world, item, spec, &["DRINKCONTAINER"])
        || world.get::<LiquidContainer>(item).is_none()
    {
        return refuse(world, caster, item, spec, "noopMessageToCaster");
    }
    let liquid = param_str(spec, "liquid").unwrap_or("water").to_string();
    let mixed = param_str(spec, "mixedLiquid")
        .unwrap_or(&liquid)
        .to_string();
    let wanted = param(spec, "amount")
        .and_then(|v| numeric_or_formula(v, ctx))
        .unwrap_or(1)
        .max(0);
    let Some(mut lc) = world.get_mut::<LiquidContainer>(item) else {
        return false;
    };
    let amount = wanted.min(lc.capacity - lc.remaining);
    if amount <= 0 {
        return refuse(world, caster, item, spec, "fullMessage");
    }
    let becomes = if lc.remaining != 0 && !lc.liquid.eq_ignore_ascii_case(&liquid) {
        mixed
    } else {
        liquid
    };
    lc.liquid = becomes;
    lc.remaining += amount;
    emit(
        world,
        caster,
        item,
        spec,
        ("messageToCaster", "messageToRoom"),
        &[],
    );
    true
}

fn apply_corpse_timer(
    world: &mut World,
    caster: Entity,
    item: Entity,
    spec: &EffectSpec,
    ctx: &FormulaCtx,
) -> bool {
    if !is_corpse(world, item) {
        return refuse(world, caster, item, spec, "notCorpseMessageToCaster");
    }
    let hours = param(spec, "hours")
        .and_then(|v| numeric_or_formula(v, ctx))
        .unwrap_or(ctx.level)
        .max(0);
    if let Some(mut d) = world.get_mut::<CorpseDecay>(item) {
        d.remaining_secs = d
            .remaining_secs
            .saturating_add(hours.saturating_mul(SECS_PER_MUD_HOUR));
    }
    emit(
        world,
        caster,
        item,
        spec,
        ("messageToCaster", "messageToRoom"),
        &[("{hours}", hours.to_string())],
    );
    true
}

fn apply_corpse_feed(world: &mut World, caster: Entity, item: Entity, spec: &EffectSpec) -> bool {
    if !is_corpse(world, item) {
        return refuse(world, caster, item, spec, "notCorpseMessageToCaster");
    }
    let hunger = param_i32(spec, "hunger", 0);
    let thirst = param_i32(spec, "thirst", 0);
    if let Some(mut h) = world.get_mut::<Hunger>(caster) {
        h.0 = (h.0 - hunger).max(0);
    }
    if let Some(mut t) = world.get_mut::<Thirst>(caster) {
        t.0 = (t.0 - thirst).max(0);
    }
    emit(
        world,
        caster,
        item,
        spec,
        ("messageToCaster", "messageToRoom"),
        &[],
    );
    true
}

fn apply_light(
    world: &mut World,
    caster: Entity,
    item: Entity,
    spec: &EffectSpec,
    ctx: &FormulaCtx,
) -> bool {
    if item_type(world, item) != Some(ObjectType::Light) {
        return refuse(world, caster, item, spec, "noopMessageToCaster");
    }
    let fuel = world.get::<LightFuel>(item).copied().unwrap_or(LightFuel {
        capacity: 0,
        remaining: 0,
    });
    match param_str(spec, "lightAction").map(str::to_ascii_lowercase) {
        Some(a) if a == "ignite" => {
            if fuel.is_permanent() {
                return refuse(world, caster, item, spec, "permanentMessageToCaster");
            }
            let mut base = fuel
                .capacity
                .saturating_mul(param_i32(spec, "capacityMultiplier", 2));
            if base < 1 {
                base = param_i32(spec, "defaultFuel", 50);
            }
            // Half the base for anyone, up to double that at full proficiency.
            let ticks = base / 2 + base * ctx.skill.clamp(0, 100) / 100;
            let remaining = if fuel.capacity < 0 { -1 } else { ticks };
            world.entity_mut(item).insert(LightFuel {
                capacity: fuel.capacity,
                remaining,
            });
            if remaining > 0 && !mud_world::is_lit(world, item) {
                world.entity_mut(item).insert(Lit);
            }
            emit(
                world,
                caster,
                item,
                spec,
                ("messageToCaster", "messageToRoom"),
                &[],
            );
            true
        }
        Some(a) if a == "drain" => {
            if fuel.remaining == 0 {
                return refuse(world, caster, item, spec, "noopMessageToCaster");
            }
            let was_lit = mud_world::is_lit(world, item);
            // Keep the old fuel as the capacity so a relight can restore it
            // (a permanent light comes back permanent).
            let capacity = if fuel.capacity == 0 {
                fuel.remaining
            } else {
                fuel.capacity
            };
            world.entity_mut(item).insert(LightFuel {
                capacity,
                remaining: 0,
            });
            world.entity_mut(item).remove::<Lit>();
            emit(
                world,
                caster,
                item,
                spec,
                ("messageToCaster", "messageToRoom"),
                &[],
            );
            if was_lit {
                emit(
                    world,
                    caster,
                    item,
                    spec,
                    ("extinguishMessageToCaster", "extinguishMessageToRoom"),
                    &[],
                );
            }
            true
        }
        _ => false,
    }
}
