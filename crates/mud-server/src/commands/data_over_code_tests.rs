//! Content that used to be hard-coded by database id or name and is now
//! read from the data: class-gated skills (`ClassSkills` /
//! `KnownAbilities`), class roles resolved by name (`CoreClasses`) and
//! the `Effect.prevents_*` flags. Test-only.

use bevy_ecs::prelude::*;
use mud_world::{
    AppliedTo, ClassCatalog, ClassDef, ClassSkillsData, CoreAbilities, CoreClasses, EffectCatalog,
    EffectDef, EffectInstance, EffectSource, Fighting, KnownAbilities, Profile, Room, Stunned,
};

use super::combat_commands::{cmd_claw, cmd_electrify, cmd_steal};
use super::info::cmd_summonmount;
use super::test_support::{Rx, drain, install_core_abilities, player_in};
use super::{Prevent, SkillAccess, check_ability_restrictions, effect_prevents, skill_access};

// Class ids differ per database; these mimic a prod-like numbering that
// disagrees with dev (Thief 3, Assassin 10, Bard 20, Paladin 5, Monk 14).
const THIEF: i32 = 31;
const ASSASSIN: i32 = 7;
const BARD: i32 = 44;
const PALADIN: i32 = 52;
const MONK: i32 = 9;

// Ability ids from `core_ability_catalog`.
const STEAL: i32 = 345;
const CLAW: i32 = 54;
const ELECTRIFY: i32 = 119;
const SUMMON_MOUNT: i32 = 355;

fn class(id: i32, plain_name: &str) -> ClassDef {
    ClassDef {
        id,
        name: plain_name.to_string(),
        plain_name: plain_name.to_string(),
        is_subclass: false,
        parent_class_id: None,
        description: None,
        hit_dice: "1d8".to_string(),
        primary_stat: None,
        hp_per_level: 0,
        exp_gain_factor: 1.0,
        alignment_bias: 0,
        campcraft_bonus: false,
        resistances: std::collections::HashMap::new(),
    }
}

fn class_catalog() -> ClassCatalog {
    let mut catalog = ClassCatalog::default();
    for (id, name) in [
        (THIEF, "Thief"),
        (ASSASSIN, "Assassin"),
        (BARD, "Bard"),
        (PALADIN, "Paladin"),
        (MONK, "Monk"),
    ] {
        catalog.by_id.insert(id, class(id, name));
    }
    catalog
}

/// Legacy `class.cpp` rows: Thief and Bard steal at 10, Paladin summons a
/// mount at 15. Assassin has no Steal row.
fn class_skills() -> ClassSkillsData {
    let mut data = ClassSkillsData::default();
    for (class_id, ability, level) in [
        (THIEF, STEAL, 10),
        (BARD, STEAL, 10),
        (PALADIN, SUMMON_MOUNT, 15),
        // Rows so the Assassin has a kit, just not Steal.
        (ASSASSIN, 999, 1),
    ] {
        data.min_level.insert((class_id, ability), level);
        data.proficiency_cap.insert((class_id, ability), 100);
    }
    data
}

fn world() -> (World, Entity, Rx) {
    let mut world = World::new();
    install_core_abilities(&mut world);
    world.insert_resource(class_skills());
    world.insert_resource(class_catalog());
    world.insert_resource(EffectCatalog::default());
    let room = world.spawn(Room).id();
    let (p, rx) = player_in(&mut world, room);
    (world, p, rx)
}

fn set_class(world: &mut World, p: Entity, class_id: i32, level: i32) {
    world.entity_mut(p).insert(Profile {
        level,
        class_id: Some(class_id),
        race: "HUMAN".into(),
        experience: 0,
        gender: "male".into(),
    });
}

fn learn(world: &mut World, p: Entity, ability: i32) {
    world.entity_mut(p).insert(KnownAbilities {
        entries: vec![(ability, 500, true)],
    });
}

// ---- steal ----

#[test]
fn bard_can_steal_and_assassin_cannot() {
    let (mut world, p, mut rx) = world();
    // Bard teaches Steal at level 10 in the legacy data.
    set_class(&mut world, p, BARD, 20);
    cmd_steal(&mut world, p, "");
    let out = drain(&mut rx);
    assert!(out.contains("Usage: steal"), "bard passes the gate: {out}");

    // Assassin has no Steal row in the legacy data.
    set_class(&mut world, p, ASSASSIN, 40);
    cmd_steal(&mut world, p, "");
    let out = drain(&mut rx);
    assert!(out.contains("You don't know how to steal."), "{out}");
}

#[test]
fn steal_waits_for_the_class_level() {
    let (mut world, p, mut rx) = world();
    set_class(&mut world, p, THIEF, 9);
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("You don't know how to steal."));
    set_class(&mut world, p, THIEF, 10);
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("Usage: steal"));
}

#[test]
fn steal_needs_practice_once_the_character_practices_anything() {
    let (mut world, p, mut rx) = world();
    set_class(&mut world, p, THIEF, 30);
    // Practiced something else only.
    learn(&mut world, p, 12345);
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("You don't know how to steal."));
    learn(&mut world, p, STEAL);
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("Usage: steal"));
}

#[test]
fn steal_is_off_when_the_catalog_has_no_steal_ability() {
    let (mut world, p, mut rx) = world();
    world.insert_resource(CoreAbilities::default());
    set_class(&mut world, p, THIEF, 50);
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("You don't know how to steal."));
}

#[test]
fn steal_still_refused_mid_fight() {
    let (mut world, p, mut rx) = world();
    set_class(&mut world, p, THIEF, 50);
    let foe = world.spawn_empty().id();
    world.entity_mut(p).insert(Fighting(foe));
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("can't steal while fighting"));
}

// ---- claw / electrify ----

#[test]
fn claw_and_electrify_open_only_with_the_ability() {
    let (mut world, p, mut rx) = world();
    // No class used to be enough (Druid, Sorcerer, ...); now none is.
    set_class(&mut world, p, PALADIN, 50);
    cmd_claw(&mut world, p, "");
    assert!(drain(&mut rx).contains("Grow some longer fingernails first."));
    cmd_electrify(&mut world, p, "");
    assert!(drain(&mut rx).contains("You haven't the arcane training"));

    learn(&mut world, p, CLAW);
    cmd_claw(&mut world, p, "");
    assert!(drain(&mut rx).contains("Claw whom?"));
    // Knowing Claw does not open Electrify.
    cmd_electrify(&mut world, p, "");
    assert!(drain(&mut rx).contains("You haven't the arcane training"));

    learn(&mut world, p, ELECTRIFY);
    cmd_electrify(&mut world, p, "");
    assert!(drain(&mut rx).contains("Lightning whom?"));
}

// ---- summon mount ----

#[test]
fn skill_access_reports_level_and_refusal() {
    let (mut world, p, _rx) = world();
    set_class(&mut world, p, PALADIN, 14);
    assert_eq!(
        skill_access(&world, p, Some(SUMMON_MOUNT)),
        SkillAccess::NeedsLevel(15)
    );
    set_class(&mut world, p, PALADIN, 15);
    assert_eq!(
        skill_access(&world, p, Some(SUMMON_MOUNT)),
        SkillAccess::Allowed
    );
    // A class with no row and no grant.
    set_class(&mut world, p, THIEF, 50);
    assert_eq!(
        skill_access(&world, p, Some(SUMMON_MOUNT)),
        SkillAccess::Refused
    );
    // Ability missing from the catalog.
    assert_eq!(skill_access(&world, p, None), SkillAccess::Refused);
}

#[test]
fn summon_mount_follows_class_skills() {
    let (mut world, p, mut rx) = world();
    set_class(&mut world, p, THIEF, 50);
    cmd_summonmount(&mut world, p, "");
    assert!(drain(&mut rx).contains("You have no idea what you're trying to accomplish."));

    set_class(&mut world, p, PALADIN, 14);
    cmd_summonmount(&mut world, p, "");
    assert!(drain(&mut rx).contains("aren't yet deemed worthy"));

    // Level reached: the gate passes and the next refusal is the room's
    // sector (the test room has none).
    set_class(&mut world, p, PALADIN, 15);
    cmd_summonmount(&mut world, p, "");
    let out = drain(&mut rx);
    assert!(
        !out.contains("no idea") && !out.contains("worthy"),
        "paladin 15 passes the skill gate: {out}"
    );
}

// ---- Monk / Thief by name ----

#[test]
fn core_classes_resolve_by_plain_name_not_id() {
    let catalog = class_catalog();
    let core = CoreClasses::resolve_quiet(&catalog);
    assert_eq!(core.monk, Some(MONK));
    assert_eq!(core.thief, Some(THIEF));
    assert!(core.missing().is_empty());

    // Case-insensitive; the lowest id wins a duplicate.
    let mut dup = ClassCatalog::default();
    dup.by_id.insert(40, class(40, "MONK"));
    dup.by_id.insert(12, class(12, "monk"));
    let core = CoreClasses::resolve_quiet(&dup);
    assert_eq!(core.monk, Some(12));
    assert_eq!(core.thief, None);
    assert_eq!(core.missing(), vec!["Thief"]);

    assert_eq!(
        CoreClasses::resolve_quiet(&ClassCatalog::default()),
        CoreClasses::default()
    );
}

#[test]
fn monk_unarmed_dice_follow_the_resolved_class() {
    let core = CoreClasses::resolve_quiet(&class_catalog());
    // The old code keyed on id 14 (dev's Monk); here 14 is no Monk.
    assert!(crate::login::monk_natural_damage(core, Some(14), 30).is_none());
    let nd = crate::login::monk_natural_damage(core, Some(MONK), 30).expect("monk dice");
    assert_eq!((nd.num, nd.size, nd.bonus), (3, 6, 6));
    let low = crate::login::monk_natural_damage(core, Some(MONK), 3).unwrap();
    assert_eq!((low.num, low.bonus), (1, 0));
    assert!(crate::login::monk_natural_damage(core, Some(THIEF), 30).is_none());
    assert!(crate::login::monk_natural_damage(core, None, 30).is_none());
    // No Monk in the catalog: nobody gets the dice, `None` never matches
    // a classless character.
    let none = CoreClasses::default();
    assert!(crate::login::monk_natural_damage(none, None, 30).is_none());
    assert!(crate::login::monk_natural_damage(none, Some(MONK), 30).is_none());
}

#[test]
fn thief_hide_lag_follows_the_resolved_class() {
    let (mut world, p, _rx) = world();
    world.insert_resource(CoreClasses::resolve_quiet(&class_catalog()));
    set_class(&mut world, p, THIEF, 10);
    assert!(crate::hiding::is_thief(&world, p));
    // Assassin is a different class with its own id.
    set_class(&mut world, p, ASSASSIN, 10);
    assert!(!crate::hiding::is_thief(&world, p));
    // Dev's Thief id (3) means nothing here.
    set_class(&mut world, p, 3, 10);
    assert!(!crate::hiding::is_thief(&world, p));
    world.insert_resource(CoreClasses::default());
    set_class(&mut world, p, THIEF, 10);
    assert!(!crate::hiding::is_thief(&world, p));
}

// ---- Effect.prevents_* ----

fn effect_type(id: i32, name: &str, speaking: bool, movement: bool) -> EffectDef {
    EffectDef {
        id,
        name: name.into(),
        description: None,
        effect_type: "status".into(),
        tags: vec![],
        presence_override: None,
        default_params: serde_json::json!({}),
        prevents_speaking: speaking,
        prevents_casting: false,
        prevents_movement: movement,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    }
}

fn apply(world: &mut World, target: Entity, kind: i32, name: &str) {
    world.spawn((
        EffectInstance {
            kind,
            name: name.into(),
            strength: 1,
            remaining_secs: 30,
            source: EffectSource::Other("test".into()),
            ability_id: None,
        },
        AppliedTo(target),
    ));
}

#[test]
fn prevents_movement_flag_blocks_kick_and_trip() {
    let (mut world, p, _rx) = world();
    let target = world.spawn_empty().id();
    {
        let mut catalog = world.resource_mut::<EffectCatalog>();
        catalog
            .by_id
            .insert(70, effect_type(70, "rooted_by_data", false, true));
        catalog
            .by_id
            .insert(71, effect_type(71, "harmless", false, false));
    }
    let rules = vec![serde_json::json!({
        "type": "not_immobilized",
        "message": "You can't move!",
    })];
    // KICK and TRIP_UP both carry `not_immobilized`.
    assert_eq!(
        check_ability_restrictions(&mut world, p, target, &rules),
        None
    );

    // An unflagged effect with an unrecognised name does nothing.
    apply(&mut world, p, 71, "frobnicated");
    assert_eq!(
        check_ability_restrictions(&mut world, p, target, &rules),
        None
    );

    // The same name under a type flagged `prevents_movement` blocks it.
    apply(&mut world, p, 70, "frobnicated");
    assert!(effect_prevents(&mut world, p, Prevent::Movement));
    assert!(!effect_prevents(&mut world, p, Prevent::Speaking));
    assert_eq!(
        check_ability_restrictions(&mut world, p, target, &rules).as_deref(),
        Some("You can't move!")
    );
}

#[test]
fn stunned_marker_still_immobilizes_without_an_effect_catalog() {
    let (mut world, p, _rx) = world();
    world.remove_resource::<EffectCatalog>();
    assert!(!super::is_immobilized(&mut world, p));
    world.entity_mut(p).insert(Stunned);
    assert!(super::is_immobilized(&mut world, p));
}
