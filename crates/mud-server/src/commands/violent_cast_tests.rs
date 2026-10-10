//! A violent spell starts the fight even when it does no damage (curses,
//! dispels, holds), judged on the victim's state after the spell ran.

use super::*;
use crate::commands::invoke_ability_with;
use crate::commands::test_support::{Rx, ability_def, drain, player_in};
use mud_db::abilities::AbilityKind;
use mud_world::{
    AbilityCatalog, AppliedTo, EffectCatalog, EffectDef, EffectInstance, EffectSource, Keywords,
    KnownAbilities, Mob, Named, SpellSlotData, Stunned,
};

const RAY: i32 = 1;
const DISPEL: i32 = 2;
const HOLD: i32 = 3;
const BUFF: i32 = 4;
const MODIFY: i32 = 7;
const DISPEL_FX: i32 = 8;

fn effect_def(id: i32, name: &str) -> EffectDef {
    EffectDef {
        id,
        name: name.to_string(),
        description: None,
        effect_type: name.to_string(),
        tags: if name == "dispel" {
            vec!["magic".to_string()]
        } else {
            Vec::new()
        },
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

fn spell(catalog: &mut AbilityCatalog, id: i32, plain: &str, violent: bool, fx: i32) {
    let mut def = ability_def(id, plain, AbilityKind::Spell);
    def.violent = violent;
    def.cast_time_rounds = 0;
    catalog.by_name.insert(plain.to_ascii_lowercase(), def);
    catalog.effects_for.insert(
        id,
        vec![(
            fx,
            Some(serde_json::json!({
                "target": "str", "amount": "-5", "duration": 10, "filter": "magic", "scope": "all"
            })),
        )],
    );
}

fn world_with_spells() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    let room = world.spawn_empty().id();
    let mut catalog = AbilityCatalog::default();
    spell(&mut catalog, RAY, "ray", true, MODIFY);
    spell(&mut catalog, DISPEL, "dispel", true, DISPEL_FX);
    spell(&mut catalog, HOLD, "hold", true, MODIFY);
    spell(&mut catalog, BUFF, "buff", false, MODIFY);
    world.insert_resource(catalog);
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(MODIFY, effect_def(MODIFY, "modify"));
    effects
        .by_id
        .insert(DISPEL_FX, effect_def(DISPEL_FX, "dispel"));
    world.insert_resource(effects);
    world.insert_resource(SpellSlotData::default());
    world.insert_resource(mud_world::ClassSkillsData::default());
    let (caster, rx) = player_in(&mut world, room);
    world.entity_mut(caster).insert((
        Health { hp: 100, max: 100 },
        KnownAbilities {
            entries: [RAY, DISPEL, HOLD, BUFF]
                .iter()
                .map(|id| (*id, 500, true))
                .collect(),
        },
    ));
    (world, room, caster, rx)
}

fn goblin(world: &mut World, room: Entity) -> Entity {
    world
        .spawn((
            Mob,
            Named {
                name: "a goblin".into(),
            },
            Keywords(vec!["goblin".into()]),
            Located(room),
            Health { hp: 50, max: 50 },
        ))
        .id()
}

fn cast(world: &mut World, caster: Entity, spell: &str) {
    invoke_ability_with(
        world,
        caster,
        &format!("'{spell}' goblin"),
        AbilityKind::Spell,
        "cast",
        false,
        false,
        false,
        None,
    );
}

fn fighting(world: &World, e: Entity) -> Option<Entity> {
    world.get::<Fighting>(e).map(|f| f.0)
}

#[test]
fn a_non_damaging_violent_spell_starts_combat_both_ways() {
    let (mut world, room, caster, _rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    cast(&mut world, caster, "ray");
    assert_eq!(
        fighting(&world, mob),
        Some(caster),
        "the goblin turns on you"
    );
    assert_eq!(fighting(&world, caster), Some(mob));
}

#[test]
fn a_non_violent_spell_leaves_the_target_calm() {
    let (mut world, room, caster, _rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    cast(&mut world, caster, "buff");
    assert_eq!(fighting(&world, mob), None);
    assert_eq!(fighting(&world, caster), None);
}

#[test]
fn a_victim_already_held_stays_out_of_the_fight() {
    let (mut world, room, caster, _rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    world.entity_mut(mob).insert(Stunned);
    cast(&mut world, caster, "ray");
    assert_eq!(fighting(&world, mob), None);
}

#[test]
fn dispel_that_lifts_a_paralysis_makes_the_victim_engage_at_once() {
    let (mut world, room, caster, _rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    world.entity_mut(mob).insert(Stunned);
    world.spawn((
        EffectInstance {
            kind: MODIFY,
            name: "paralyzed".to_string(),
            strength: 1,
            remaining_secs: 30,
            source: EffectSource::Spell,
            ability_id: Some(HOLD),
        },
        AppliedTo(mob),
    ));
    cast(&mut world, caster, "dispel");
    assert!(world.get::<Stunned>(mob).is_none(), "the hold is lifted");
    assert_eq!(fighting(&world, mob), Some(caster));
}

#[test]
fn a_caster_already_fighting_keeps_their_opponent() {
    let (mut world, room, caster, _rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    let other = world
        .spawn((
            Mob,
            Named {
                name: "an orc".into(),
            },
            Located(room),
            Health { hp: 50, max: 50 },
        ))
        .id();
    world.entity_mut(caster).insert(Fighting(other));
    world.entity_mut(other).insert(Fighting(caster));
    cast(&mut world, caster, "ray");
    assert_eq!(fighting(&world, caster), Some(other), "opponent unchanged");
    assert_eq!(fighting(&world, mob), Some(caster), "the goblin joins in");
}

#[test]
fn a_charmed_or_sleeping_victim_is_not_dragged_into_a_fight() {
    let (mut world, room, caster, mut rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    world
        .entity_mut(mob)
        .insert(mud_world::Posture(mud_world::PostureKind::Sleeping));
    cast(&mut world, caster, "ray");
    assert_eq!(fighting(&world, mob), None);
    drain(&mut rx);
    world
        .entity_mut(mob)
        .insert(mud_world::Posture(mud_world::PostureKind::Standing));
    world.spawn((
        EffectInstance {
            kind: MODIFY,
            name: "charmed".to_string(),
            strength: 1,
            remaining_secs: 30,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(mob),
    ));
    cast(&mut world, caster, "ray");
    assert_eq!(fighting(&world, mob), None);
}

/// Fly cast on someone else read as if the caster were the one rising:
/// the caster line and the room line must name the target.
fn fly_messages() -> mud_world::AbilityMessageSet {
    mud_world::AbilityMessageSet {
        success_to_caster: Some("{target.name} rises into the air and begins to fly.".into()),
        success_to_victim: Some("You rise into the air and begin to fly.".into()),
        success_to_room: Some("{target.name} rises into the air and begins to fly.".into()),
        success_to_self: Some("You rise into the air and begin to fly.".into()),
        success_self_room: Some("{actor.name} rises into the air and begins to fly.".into()),
        ..Default::default()
    }
}

#[test]
fn a_buff_cast_on_another_names_the_target_to_the_caster_and_the_room() {
    let (mut world, room, caster, mut rx) = world_with_spells();
    world
        .resource_mut::<AbilityCatalog>()
        .messages
        .insert(BUFF, fly_messages());
    let (watcher, mut wrx) = player_in(&mut world, room);
    world.entity_mut(watcher).insert(Named {
        name: "Watcher".into(),
    });
    goblin(&mut world, room);
    drain(&mut rx);
    cast(&mut world, caster, "buff");
    let caster_out = drain(&mut rx);
    assert!(
        caster_out.contains("a goblin rises into the air and begins to fly."),
        "{caster_out}"
    );
    assert!(
        !caster_out.contains("You rise into the air"),
        "{caster_out}"
    );
    let room_out = drain(&mut wrx);
    assert!(
        room_out.contains("a goblin rises into the air and begins to fly."),
        "{room_out}"
    );
    assert!(!room_out.contains("Tester rises"), "{room_out}");
}

#[test]
fn a_buff_cast_on_yourself_keeps_the_first_person_lines() {
    let (mut world, room, caster, mut rx) = world_with_spells();
    world
        .resource_mut::<AbilityCatalog>()
        .messages
        .insert(BUFF, fly_messages());
    let (_watcher, mut wrx) = player_in(&mut world, room);
    drain(&mut rx);
    invoke_ability_with(
        &mut world,
        caster,
        "'buff' me",
        AbilityKind::Spell,
        "cast",
        false,
        false,
        false,
        None,
    );
    assert!(drain(&mut rx).contains("You rise into the air and begin to fly."));
    assert!(drain(&mut wrx).contains("Tester rises into the air and begins to fly."));
}
