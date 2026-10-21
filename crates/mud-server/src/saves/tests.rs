use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_world::{MobPrototypes, Profile, SavingThrows};

use super::{SaveKind, save_number, saving_throw};

fn victim(level: i32, saves: Option<SavingThrows>) -> (World, Entity) {
    let mut world = World::new();
    world.insert_resource(MobPrototypes::default());
    let e = world
        .spawn(Profile {
            level,
            class_id: None,
            race: "HUMAN".into(),
            experience: 0,
            gender: "neutral".into(),
        })
        .id();
    if let Some(s) = saves {
        world.entity_mut(e).insert(s);
    }
    (world, e)
}

/// Percent of the 100 possible d(0..99) rolls the victim fails.
fn fail_percent(world: &World, e: Entity, kind: SaveKind) -> usize {
    (0..=99)
        .filter(|r| !saving_throw(world, e, kind, *r))
        .count()
}

#[test]
fn spells_roll_the_spell_save_and_breath_weapons_the_breath_save() {
    for t in ["REFLEX", "FORTITUDE", "WILL"] {
        for k in [AbilityKind::Spell, AbilityKind::Chant, AbilityKind::Song] {
            assert_eq!(
                SaveKind::for_ability(t, k, "ICEBALL"),
                Some(SaveKind::Spell)
            );
        }
        assert_eq!(
            SaveKind::for_ability(t, AbilityKind::Spell, "fire_breath"),
            Some(SaveKind::Breath)
        );
    }
}

#[test]
fn skills_keep_the_modern_type_mapping() {
    let skill = |t| SaveKind::for_ability(t, AbilityKind::Skill, "BASH");
    assert_eq!(skill("REFLEX"), Some(SaveKind::Breath));
    assert_eq!(skill("FORTITUDE"), Some(SaveKind::Para));
    assert_eq!(skill("will"), Some(SaveKind::Spell));
    assert_eq!(skill("LUCK"), None);
}

#[test]
fn level_55_default_mob_mostly_fails() {
    let (world, e) = victim(55, None);
    // base - 55/2 = base - 27.
    assert_eq!(save_number(&world, e, SaveKind::Para), 78);
    assert_eq!(save_number(&world, e, SaveKind::Spell), 83);
    assert_eq!(save_number(&world, e, SaveKind::Breath), 83);
    // Saves on rolls above the number: 79..=99 (21) / 84..=99 (16).
    assert_eq!(fail_percent(&world, e, SaveKind::Para), 79);
    assert_eq!(fail_percent(&world, e, SaveKind::Spell), 84);
    assert_eq!(fail_percent(&world, e, SaveKind::Breath), 84);
}

#[test]
fn seeded_rolls_match_the_closed_form() {
    use rand::{RngExt, SeedableRng};
    let (world, e) = victim(55, None);
    let mut rng = rand::rngs::StdRng::seed_from_u64(180);
    let n: u32 = 20_000;
    let fails = (0..n)
        .filter(|_| !saving_throw(&world, e, SaveKind::Para, rng.random_range(0..=99)))
        .count();
    // 79% expected: within 2 points.
    let permille = fails * 1000 / usize::try_from(n).unwrap();
    assert!((770..=810).contains(&permille), "fail rate {permille}/1000");
}

#[test]
fn strongly_negative_modifier_almost_always_saves() {
    let (world, e) = victim(
        55,
        Some(SavingThrows {
            spell: -100,
            ..Default::default()
        }),
    );
    // Number floors at 1: only rolls 0 and 1 fail.
    assert_eq!(fail_percent(&world, e, SaveKind::Spell), 2);
    // The modifier is per category: paralysis is untouched.
    assert_eq!(fail_percent(&world, e, SaveKind::Para), 79);
}

#[test]
fn positive_modifier_never_saves_and_low_levels_rarely_do() {
    let (world, e) = victim(
        1,
        Some(SavingThrows {
            para: 30,
            ..Default::default()
        }),
    );
    assert_eq!(fail_percent(&world, e, SaveKind::Para), 100);
    let (world, e) = victim(1, None);
    assert_eq!(save_number(&world, e, SaveKind::Para), 105);
    assert_eq!(fail_percent(&world, e, SaveKind::Para), 100);
}

#[test]
fn boundary_is_strictly_greater() {
    let (world, e) = victim(55, None);
    // Number 78: roll 78 fails, roll 79 saves.
    assert!(!saving_throw(&world, e, SaveKind::Para, 78));
    assert!(saving_throw(&world, e, SaveKind::Para, 79));
}
