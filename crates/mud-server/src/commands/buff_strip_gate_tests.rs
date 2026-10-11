//! Non-violent buff-strips (Extinguish, Douse) aimed at another player take
//! the same PK / consent / peaceful-room gate as hostile magic.

use super::*;
use crate::commands::invoke_ability_with;
use crate::commands::test_support::{Rx, ability_def, drain, player_in};
use mud_db::abilities::AbilityKind;
use mud_db::enums::PlayerFlag;
use mud_world::{
    AbilityCatalog, AppliedTo, EffectCatalog, EffectDef, EffectInstance, EffectSource,
    KnownAbilities, Named, PeacefulRoom, PlayerFlags, SpellSlotData,
};

const DOUSE: i32 = 1;
const ENRAPTURE: i32 = 2;
const DISPEL_FX: i32 = 8;
const BLESS_FX: i32 = 20;
const CURSE_FX: i32 = 21;

fn effect_def(id: i32, name: &str, effect_type: &str, tag: &str) -> EffectDef {
    EffectDef {
        id,
        name: name.to_string(),
        description: None,
        effect_type: effect_type.to_string(),
        tags: vec![tag.to_string()],
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

fn strip_spell(catalog: &mut AbilityCatalog, id: i32, plain: &str, filter: &str) {
    let mut def = ability_def(id, plain, AbilityKind::Spell);
    def.cast_time_rounds = 0;
    catalog.by_name.insert(plain.to_ascii_lowercase(), def);
    catalog.effects_for.insert(
        id,
        vec![(
            DISPEL_FX,
            Some(serde_json::json!({ "filter": filter, "scope": "all" })),
        )],
    );
}

/// Caster and victim (both players, PK off) in one room; the victim wears a
/// buff and a debuff.
fn setup() -> (World, Entity, Entity, Entity, Rx) {
    let mut world = World::new();
    let room = world.spawn_empty().id();
    let mut catalog = AbilityCatalog::default();
    strip_spell(&mut catalog, DOUSE, "douse", "buff");
    strip_spell(&mut catalog, ENRAPTURE, "enrapture", "debuff");
    world.insert_resource(catalog);
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(
        DISPEL_FX,
        effect_def(DISPEL_FX, "dispel", "dispel", "magic"),
    );
    effects
        .by_id
        .insert(BLESS_FX, effect_def(BLESS_FX, "bless", "status", "buff"));
    effects
        .by_id
        .insert(CURSE_FX, effect_def(CURSE_FX, "curse", "status", "debuff"));
    world.insert_resource(effects);
    world.insert_resource(SpellSlotData::default());
    world.insert_resource(mud_world::ClassSkillsData::default());
    let (caster, rx) = player_in(&mut world, room);
    world.entity_mut(caster).insert((
        Health { hp: 100, max: 100 },
        PlayerFlags(vec![]),
        KnownAbilities {
            entries: vec![(DOUSE, 500, true), (ENRAPTURE, 500, true)],
        },
    ));
    let (victim, _vrx) = player_in(&mut world, room);
    world.entity_mut(victim).insert((
        Named {
            name: "Victim".into(),
        },
        Health { hp: 100, max: 100 },
        PlayerFlags(vec![]),
    ));
    for (fx, name) in [(BLESS_FX, "bless"), (CURSE_FX, "curse")] {
        world.spawn((
            EffectInstance {
                kind: fx,
                name: name.into(),
                strength: 1,
                remaining_secs: 300,
                source: EffectSource::Spell,
                ability_id: None,
            },
            AppliedTo(victim),
        ));
    }
    (world, room, caster, victim, rx)
}

fn effects_on(world: &mut World, e: Entity) -> Vec<i32> {
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    let mut kinds: Vec<i32> = q
        .iter(world)
        .filter(|(_, a)| a.0 == e)
        .map(|(i, _)| i.kind)
        .collect();
    kinds.sort_unstable();
    kinds
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

fn pk_on(world: &mut World, e: Entity) {
    world
        .entity_mut(e)
        .insert(PlayerFlags(vec![PlayerFlag::PkEnabled]));
}

#[test]
fn douse_on_a_non_pk_player_is_refused_and_strips_nothing() {
    let (mut world, _room, caster, victim, mut rx) = setup();
    cast(&mut world, caster, "douse", "victim");
    let out = drain(&mut rx);
    assert!(out.contains("PK"), "{out}");
    assert_eq!(effects_on(&mut world, victim), vec![BLESS_FX, CURSE_FX]);
}

#[test]
fn douse_works_once_both_sides_consent_to_pk() {
    let (mut world, _room, caster, victim, mut rx) = setup();
    pk_on(&mut world, caster);
    pk_on(&mut world, victim);
    cast(&mut world, caster, "douse", "victim");
    drain(&mut rx);
    assert_eq!(effects_on(&mut world, victim), vec![CURSE_FX]);
}

#[test]
fn douse_is_refused_in_a_peaceful_room_even_with_pk() {
    let (mut world, room, caster, victim, mut rx) = setup();
    pk_on(&mut world, caster);
    pk_on(&mut world, victim);
    world.entity_mut(room).insert(PeacefulRoom);
    cast(&mut world, caster, "douse", "victim");
    assert!(drain(&mut rx).contains("peace"), "peaceful refusal");
    assert_eq!(effects_on(&mut world, victim), vec![BLESS_FX, CURSE_FX]);
}

#[test]
fn group_mates_may_strip_each_other_without_pk() {
    let (mut world, _room, caster, victim, mut rx) = setup();
    world
        .entity_mut(victim)
        .insert(mud_world::GroupMember(caster));
    cast(&mut world, caster, "douse", "victim");
    drain(&mut rx);
    assert_eq!(effects_on(&mut world, victim), vec![CURSE_FX]);
}

#[test]
fn a_debuff_only_dispel_stays_a_favour_and_needs_no_consent() {
    let (mut world, _room, caster, victim, mut rx) = setup();
    cast(&mut world, caster, "enrapture", "victim");
    drain(&mut rx);
    assert_eq!(effects_on(&mut world, victim), vec![BLESS_FX]);
}
