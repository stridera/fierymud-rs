//! Perception flags that change what `look` reports: `infravision`
//! (warm bodies show up in a dark room), `detect_life` (hidden
//! lifeforms are counted) and `detect_align` (good and evil auras on
//! actors). Legacy `EFF_INFRAVISION`, `EFF_SENSE_LIFE`, `EFF_DETECT_ALIGN`.

use bevy_ecs::prelude::{Entity, With, World};
use mud_db::enums::{Alignment, LifeForce, ObjectFlag, Sector, Size};
use mud_world::{
    AbilityCatalog, AppliedTo, Blinded, CombatStats, DetectAlign, EffectInstance, Infravision,
    LifeForceTag, LiquidContainer, Located, Mob, ObjectFlags, ObjectPrototypes, Player, RoomSector,
    SenseLife, Sized, WorldKey, is_lit,
};

use super::{can_see_player, hidden_by_magic_from, wiz_hidden_from};

/// Legacy `IS_GOOD` / `IS_EVIL`: alignment at or beyond +/-350.
const AURA_ALIGNMENT: i32 = 350;

/// Legacy `YOU_ARE_BLIND` (act.hpp): what look, exits, read and scan
/// say to a blind character.
pub(crate) const YOU_ARE_BLIND: &str = "You can't see a damned thing; you're blind!\r\n";

/// Legacy `EFF_BLIND`: `viewer` carries the [`Blinded`] marker (a
/// `blinded` flag or a `blind` effect) and nothing overrides it.
/// Only `HOLY_LIGHT` sees regardless (legacy `IMM_CAN_SEE`; `LIGHT_OK`
/// has no staff-rank bypass), so `player_can_see_in_dark` stays the
/// single bypass for every way of not seeing.
#[must_use]
pub(crate) fn is_blind(world: &World, viewer: Entity) -> bool {
    world.get::<Blinded>(viewer).is_some() && !super::player_can_see_in_dark(world, viewer)
}

/// Legacy `MOB_NOBLIND`: the mob proto lists `blind: 0` in its
/// resistances (the fierylib importer turns `NO_BLIND` into that entry,
/// like `NO_SLEEP` / `NO_CHARM`), so blindness can never land on it.
/// Players are never immune.
#[must_use]
pub(crate) fn is_noblind(world: &World, actor: Entity) -> bool {
    if world.get::<Mob>(actor).is_none() {
        return false;
    }
    let Some(key) = world.get::<mud_world::WorldKey>(actor) else {
        return false;
    };
    world
        .get_resource::<mud_world::MobPrototypes>()
        .and_then(|protos| protos.by_key.get(&(key.zone, key.id)))
        .and_then(|p| p.resistances.as_object())
        .is_some_and(|m| {
            m.iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("blind") && v.as_i64() == Some(0))
        })
}

/// True for a viewer that makes out characters in the dark by body heat.
#[must_use]
pub(crate) fn has_infravision(world: &World, viewer: Entity) -> bool {
    world.get::<Infravision>(viewer).is_some()
}

/// The `detect_align` tag `viewer` reads off `target`: a red aura for an
/// evil actor, a gold one for a good actor, nothing for the neutral or
/// for a viewer without the flag.
#[must_use]
pub(crate) fn alignment_aura(
    world: &World,
    viewer: Entity,
    target: Entity,
) -> Option<&'static str> {
    world.get::<DetectAlign>(viewer)?;
    let alignment = world.get::<CombatStats>(target)?.alignment;
    if alignment <= -AURA_ALIGNMENT {
        Some("<red>(Red Aura)</>")
    } else if alignment >= AURA_ALIGNMENT {
        Some("<b:yellow>(Gold Aura)</>")
    } else {
        None
    }
}

/// The `detect_align` tag `viewer` reads off an item, from legacy
/// `print_obj_flags_to_char`: only an item barred to neutrals shows one,
/// red when it also bars the good (and not the evil), gold when it bars
/// the evil (and not the good). An item barring both gets nothing.
#[must_use]
pub(crate) fn item_alignment_aura(
    world: &World,
    viewer: Entity,
    item: Entity,
) -> Option<&'static str> {
    world.get::<DetectAlign>(viewer)?;
    let key = world.get::<WorldKey>(item)?;
    let proto = world
        .get_resource::<ObjectPrototypes>()?
        .by_key
        .get(&(key.zone, key.id))?;
    let bars = |a: Alignment| proto.restricted_alignments.contains(&a);
    if !bars(Alignment::Neutral) {
        return None;
    }
    match (bars(Alignment::Good), bars(Alignment::Evil)) {
        (true, false) => Some("(<red>Red Aura</>)"),
        (false, true) => Some("(<b:yellow>Gold Aura</>)"),
        _ => None,
    }
}

/// Whether `viewer` has an effect active that goes by one of `labels`
/// (lowercase, spaces for underscores): the effect's own flag name or its
/// originating ability's plain name. Read-only twin of the aura lookup in
/// `look_auras`.
fn viewer_has_effect(world: &World, viewer: Entity, labels: &[&str]) -> bool {
    let norm = |s: &str| s.replace('_', " ").to_ascii_lowercase();
    let Some(mut q) = world.try_query::<(&EffectInstance, &AppliedTo)>() else {
        return false;
    };
    let catalog = world.get_resource::<AbilityCatalog>();
    q.iter(world)
        .filter(|(_, applied)| applied.0 == viewer)
        .any(|(inst, _)| {
            labels.contains(&norm(&inst.name).as_str())
                || inst
                    .ability_id
                    .and_then(|id| catalog?.by_name.values().find(|d| d.id == id))
                    .is_some_and(|d| labels.contains(&norm(&d.plain_name).as_str()))
        })
}

/// Legacy `EFF_DETECT_MAGIC`: the Detect Magic effect, or the Sphere of
/// Divination that grants it.
fn has_detect_magic(world: &World, viewer: Entity) -> bool {
    viewer_has_effect(world, viewer, &["detect magic", "sphere of divination"])
}

/// Legacy `EFF_DETECT_POISON`.
fn has_detect_poison(world: &World, viewer: Entity) -> bool {
    viewer_has_effect(world, viewer, &["detect poison"])
}

/// Legacy `IS_WATER(obj->in_room)`: the item lies directly on the floor
/// of a shallows, water or underwater room. Items carried, worn or inside
/// a container have no room (`in_room == NOWHERE`) and never float.
fn lies_in_water(world: &World, item: Entity) -> bool {
    world
        .get::<Located>(item)
        .and_then(|l| world.get::<RoomSector>(l.0))
        .is_some_and(|s| matches!(s.0, Sector::Shallows | Sector::Water | Sector::Underwater))
}

/// Legacy `OBJ_HIDDEN_TO_CHAR`: `item` carries a [`Hiddenness`] above
/// `viewer`'s perception. Hiddenness only means something for an item
/// lying in a room or inside a container: whoever carries or wears it has
/// it in hand (legacy `unhide_object` clears it on pickup). Legacy also
/// exempts the character who hid it (`last_to_hold`); nothing hides items
/// in Rust yet, so only authored hiddenness exists and that clause has no
/// holder to name.
fn item_hidden_from(world: &World, viewer: Entity, item: Entity) -> bool {
    let hid = crate::hiding::hiddenness(world, item);
    if hid == 0 {
        return false;
    }
    let carried = world
        .get::<Located>(item)
        .is_some_and(|l| world.get::<Player>(l.0).is_some() || world.get::<Mob>(l.0).is_some());
    !carried && hid > crate::hiding::perception_of(world, viewer)
}

/// Legacy `CAN_SEE_OBJ`: `HOLY_LIGHT` sees everything; otherwise an
/// `Invisible` item needs a viewer who pierces invisibility (detect
/// invisible, `HOLY_LIGHT`, or an Immortal+ account), and a hidden one a
/// viewer whose perception reaches its hiddenness (`search` finds it
/// otherwise). Light (`LIGHT_OK`) is not modelled here.
#[must_use]
pub(crate) fn item_visible_to(world: &World, viewer: Entity, item: Entity) -> bool {
    if super::has_flag(world, viewer, mud_db::enums::PlayerFlag::HolyLight) {
        return true;
    }
    (!world
        .get::<ObjectFlags>(item)
        .is_some_and(|f| f.has(ObjectFlag::Invisible))
        || super::pierces_invisibility(world, viewer))
        && !item_hidden_from(world, viewer, item)
}

/// Legacy `IS_POISONED` for a `Food` item: the prototype's `Poisoned`
/// value (drink containers and fountains keep theirs on the
/// [`LiquidContainer`]).
fn food_is_poisoned(world: &World, item: Entity) -> bool {
    let Some(key) = world.get::<WorldKey>(item) else {
        return false;
    };
    world
        .get_resource::<ObjectPrototypes>()
        .and_then(|p| p.by_key.get(&(key.zone, key.id)))
        .is_some_and(|p| p.food_poisoned)
}

/// Every tag `viewer` perceives on `item`, in legacy
/// `print_obj_flags_to_char` order: floating, illuminated, invisible,
/// hidden (`(hidden)`, or `(hN)` to staff), magic (detect magic), glowing,
/// humming, poisoned (detect poison), the detect-align aura, then
/// hovering (`ITEM_NOFALL`).
#[must_use]
pub(crate) fn item_tags(world: &World, viewer: Entity, item: Entity) -> Vec<String> {
    let flags = world.get::<ObjectFlags>(item);
    let flagged = |f: ObjectFlag| flags.is_some_and(|x| x.has(f));
    let mut tags: Vec<String> = Vec::new();
    if lies_in_water(world, item) {
        tags.push("(<b:blue>floating</>)".into());
    }
    if is_lit(world, item) {
        tags.push("<yellow>(</><b:yellow>illuminated</><yellow>)</>".into());
    }
    if flagged(ObjectFlag::Invisible) {
        tags.push("(invisible)".into());
    }
    let hid = crate::hiding::hiddenness(world, item);
    if hid > 0 {
        if super::info::is_immortal(world, viewer) {
            tags.push(format!("(h{hid})"));
        } else {
            tags.push("(hidden)".into());
        }
    }
    if flagged(ObjectFlag::Magic) && has_detect_magic(world, viewer) {
        tags.push("(<b:blue>magic</>)".into());
    }
    if flagged(ObjectFlag::Glow) {
        tags.push("<b:black>(</><magenta>glowing</><b:black>)</>".into());
    }
    if flagged(ObjectFlag::Hum) {
        tags.push("<red>(</><cyan>humming</><red>)</>".into());
    }
    if (world
        .get::<LiquidContainer>(item)
        .is_some_and(|l| l.poisoned)
        || food_is_poisoned(world, item))
        && has_detect_poison(world, viewer)
    {
        tags.push("(<b:magenta>poisoned</>)".into());
    }
    if let Some(aura) = item_alignment_aura(world, viewer, item) {
        tags.push(aura.into());
    }
    if flagged(ObjectFlag::NoFall) {
        tags.push("<b:cyan>(</><magenta>hovering</><b:cyan>)</>".into());
    }
    tags
}

/// `line` with the item tags `viewer` perceives appended, the way legacy
/// `print_obj_flags_to_char` trails them after an item's description.
/// The tags follow a reset, so a name that leaves a colour open (a builder
/// forgot the `</>`) does not tint them.
#[must_use]
pub(crate) fn with_item_tags(world: &World, viewer: Entity, item: Entity, line: String) -> String {
    let tags = item_tags(world, viewer, item);
    if tags.is_empty() {
        return line;
    }
    format!("{line}</> {}", tags.join(" "))
}

/// Legacy `senses_living`: life force the sense can pick up. Undead,
/// magical and elemental things carry none.
fn is_living(world: &World, e: Entity) -> bool {
    !world.get::<LifeForceTag>(e).is_some_and(|t| {
        matches!(
            t.0,
            LifeForce::Undead | LifeForce::Magic | LifeForce::Elemental
        )
    })
}

thread_local! {
    /// Test hook: pins the `senses_living` roll (per test thread).
    static FORCED_SENSE_ROLL: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

/// Pin the `senses_living` roll for the current thread (tests only).
#[cfg(test)]
pub(crate) fn force_sense_roll(roll: Option<i32>) {
    FORCED_SENSE_ROLL.with(|c| c.set(roll));
}

/// Legacy `senses_living` (act.informative.cpp:71): `ch` picks up the
/// life force of `vict` when it has `SENSE_LIFE`, is awake and `vict` is
/// not wizinvis above its level. Fighting or casting dulls the sense to
/// 67% of `basepct`; undead, magical and elemental things carry no life.
/// `roll` is the legacy `random_number(1, 100)`.
fn senses_living(world: &World, ch: Entity, vict: Entity, basepct: i32, roll: i32) -> bool {
    if world.get::<SenseLife>(ch).is_none()
        || world
            .get::<mud_world::Posture>(ch)
            .is_some_and(|p| p.0 == mud_world::PostureKind::Sleeping)
        || wiz_hidden_from(world, ch, vict)
        || !is_living(world, vict)
    {
        return false;
    }
    let busy = world.get::<mud_world::Fighting>(ch).is_some()
        || world.get::<mud_world::Casting>(ch).is_some();
    let basepct = if busy { 67 * basepct / 100 } else { basepct };
    roll < basepct
}

/// Legacy `try_to_sense_departure` (act.movement.cpp:115), run for each
/// observer of `mover` leaving `room` who did not get the departure line
/// (the mover is hidden or sneaking past their perception, invisible, or
/// the room is too dark): a `SENSE_LIFE` observer has a 50% chance of
/// feeling a living creature depart. `except` are the movers themselves.
pub(crate) fn sense_departure(world: &mut World, room: Entity, mover: Entity, except: &[Entity]) {
    let observers: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), (With<Player>, With<SenseLife>)>();
        q.iter(world)
            .filter(|(e, l)| l.0 == room && *e != mover && !except.contains(e))
            .map(|(e, _)| e)
            .collect()
    };
    if observers.is_empty() {
        return;
    }
    let visible_here = !super::room_is_dark(world, room) || super::room_has_light(world, room);
    for observer in observers {
        let noticed = can_see_player(world, observer, mover)
            && (visible_here || super::sees_characters_in_dark(world, observer));
        let roll = FORCED_SENSE_ROLL
            .with(std::cell::Cell::get)
            .unwrap_or_else(|| rand::random_range(1..=100));
        if !noticed && senses_living(world, observer, mover, 50, roll) {
            super::send_to(
                world,
                observer,
                "You feel that a living creature has departed.\r\n",
            );
        }
    }
}

/// Other players and mobs standing in `room`, minus anyone a `WizInvis`
/// level hides entirely.
fn others_in(world: &mut World, viewer: Entity, room: Entity) -> Vec<Entity> {
    let mut q = world
        .query_filtered::<(Entity, &Located), bevy_ecs::query::Or<(With<Player>, With<Mob>)>>();
    let mut found: Vec<Entity> = q
        .iter(world)
        .filter(|(e, l)| *e != viewer && l.0 == room)
        .map(|(e, _)| e)
        .collect();
    found.retain(|e| !wiz_hidden_from(world, viewer, *e));
    found
}

/// Legacy `print_life_sensed_msg`.
fn life_sensed_line(count: usize) -> &'static str {
    match count {
        0 => "",
        1 => "<b:white>You sense a hidden lifeform.</>\r\n",
        2..=3 => "<b:white>You sense a few hidden lifeforms.</>\r\n",
        4..=10 => "<b:white>You sense several hidden lifeforms.</>\r\n",
        11..=52 => "<b:white>You sense many hidden lifeforms.</>\r\n",
        _ => "<b:white>You sense a great horde of hidden lifeforms!</>\r\n",
    }
}

/// What infravision makes of `target` in the dark: "red shape of a
/// medium living being". Shared by the text `look` and the GMCP room
/// panels so the two never disagree on how much a shape gives away.
#[must_use]
pub(crate) fn red_shape_label(world: &World, target: Entity) -> String {
    let size = world.get::<Sized>(target).map_or(Size::Medium, |s| s.0);
    format!(
        "red shape of a {} living being",
        format!("{size:?}").to_lowercase()
    )
}

/// Legacy `print_char_infra_to_char`.
fn red_shape_line(world: &World, target: Entity) -> String {
    format!(
        "<red>The {} is here.</>\r\n",
        red_shape_label(world, target)
    )
}

/// What a viewer who cannot see the (dark) room still picks up in it:
/// a red shape for every character seen by infravision, then a count
/// of the living things only sensed. Empty without either flag.
pub(crate) fn dark_room_lines(world: &mut World, viewer: Entity, room: Entity) -> String {
    let infra = has_infravision(world, viewer);
    let sense = world.get::<SenseLife>(viewer).is_some();
    if !infra && !sense {
        return String::new();
    }
    let mut out = String::new();
    let mut sensed = 0;
    for other in others_in(world, viewer, room) {
        if infra && can_see_player(world, viewer, other) {
            out.push_str(&red_shape_line(world, other));
        } else if sense && is_living(world, other) {
            sensed += 1;
        }
    }
    out.push_str(life_sensed_line(sensed));
    out
}

/// In a lit room, the count of living characters (players and mobs) `viewer` cannot see
/// (magically invisible or hiding) but senses. Empty without `detect_life`.
pub(crate) fn lit_room_lines(world: &mut World, viewer: Entity, room: Entity) -> String {
    if world.get::<SenseLife>(viewer).is_none() {
        return String::new();
    }
    let sensed = others_in(world, viewer, room)
        .into_iter()
        .filter(|e| {
            (hidden_by_magic_from(world, viewer, *e)
                || crate::hiding::hidden_from(world, viewer, *e))
                && is_living(world, *e)
        })
        .count();
    life_sensed_line(sensed).to_string()
}
