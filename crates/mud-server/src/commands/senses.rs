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
/// `HOLY_LIGHT` and staff see regardless, like legacy `CAN_SEE`'s
/// holylight / immortal bypass, so `player_can_see_in_dark` stays the
/// single bypass for every way of not seeing.
#[must_use]
pub(crate) fn is_blind(world: &World, viewer: Entity) -> bool {
    world.get::<Blinded>(viewer).is_some()
        && !super::player_can_see_in_dark(world, viewer)
        && !crate::room_access::is_immortal(world, viewer)
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

/// Legacy `CAN_SEE_OBJ`'s invisibility half (`OBJ_INVIS_TO_CHAR`): an
/// `Invisible` item is hidden from a viewer who cannot pierce invisibility
/// (detect invisible, `HOLY_LIGHT`, or an Immortal+ account). Light and
/// per-viewer hiddenness are not modelled here.
#[must_use]
pub(crate) fn item_visible_to(world: &World, viewer: Entity, item: Entity) -> bool {
    !world
        .get::<ObjectFlags>(item)
        .is_some_and(|f| f.has(ObjectFlag::Invisible))
        || super::pierces_invisibility(world, viewer)
}

/// Every tag `viewer` perceives on `item`, in legacy
/// `print_obj_flags_to_char` order: floating, illuminated, invisible,
/// magic (detect magic), glowing, humming, poisoned (detect poison), then
/// the detect-align aura. Legacy's `(hidden)` / `(hN)` (object
/// hiddenness) and `(hovering)` (`ITEM_NOFALL`) have no Rust counterpart.
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
    if flagged(ObjectFlag::Magic) && has_detect_magic(world, viewer) {
        tags.push("(<b:blue>magic</>)".into());
    }
    if flagged(ObjectFlag::Glow) {
        tags.push("<b:black>(</><magenta>glowing</><b:black>)</>".into());
    }
    if flagged(ObjectFlag::Hum) {
        tags.push("<red>(</><cyan>humming</><red>)</>".into());
    }
    if world
        .get::<LiquidContainer>(item)
        .is_some_and(|l| l.poisoned)
        && has_detect_poison(world, viewer)
    {
        tags.push("(<b:magenta>poisoned</>)".into());
    }
    if let Some(aura) = item_alignment_aura(world, viewer, item) {
        tags.push(aura.into());
    }
    tags
}

/// `line` with the item tags `viewer` perceives appended, the way legacy
/// `print_obj_flags_to_char` trails them after an item's description.
#[must_use]
pub(crate) fn with_item_tags(world: &World, viewer: Entity, item: Entity, line: String) -> String {
    item_tags(world, viewer, item)
        .into_iter()
        .fold(line, |acc, tag| format!("{acc} {tag}"))
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
/// (magically invisible) but senses. Empty without `detect_life`.
pub(crate) fn lit_room_lines(world: &mut World, viewer: Entity, room: Entity) -> String {
    if world.get::<SenseLife>(viewer).is_none() {
        return String::new();
    }
    let sensed = others_in(world, viewer, room)
        .into_iter()
        .filter(|e| hidden_by_magic_from(world, viewer, *e) && is_living(world, *e))
        .count();
    life_sensed_line(sensed).to_string()
}
