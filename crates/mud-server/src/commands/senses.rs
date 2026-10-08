//! Perception flags that change what `look` reports: `infravision`
//! (warm bodies show up in a dark room), `detect_life` (hidden
//! lifeforms are counted) and `detect_align` (good and evil auras on
//! actors). Legacy `EFF_INFRAVISION`, `EFF_SENSE_LIFE`, `EFF_DETECT_ALIGN`.

use bevy_ecs::prelude::{Entity, With, World};
use mud_db::enums::{LifeForce, Size};
use mud_world::{
    CombatStats, DetectAlign, Infravision, LifeForceTag, Located, Mob, Player, SenseLife, Sized,
};

use super::{can_see_player, hidden_by_magic_from, wiz_hidden_from};

/// Legacy `IS_GOOD` / `IS_EVIL`: alignment at or beyond +/-350.
const AURA_ALIGNMENT: i32 = 350;

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

/// Legacy `print_char_infra_to_char`.
fn red_shape_line(world: &World, target: Entity) -> String {
    let size = world.get::<Sized>(target).map_or(Size::Medium, |s| s.0);
    format!(
        "<red>The red shape of a {} living being is here.</>\r\n",
        format!("{size:?}").to_lowercase()
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

/// In a lit room, the count of living players `viewer` cannot see
/// (magically invisible) but senses. Empty without `detect_life`.
pub(crate) fn lit_room_lines(world: &mut World, viewer: Entity, room: Entity) -> String {
    if world.get::<SenseLife>(viewer).is_none() {
        return String::new();
    }
    let sensed = others_in(world, viewer, room)
        .into_iter()
        .filter(|e| {
            world.get::<Player>(*e).is_some()
                && hidden_by_magic_from(world, viewer, *e)
                && is_living(world, *e)
        })
        .count();
    life_sensed_line(sensed).to_string()
}
