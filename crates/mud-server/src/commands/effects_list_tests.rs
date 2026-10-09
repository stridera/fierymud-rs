//! `effects` listing (issue #38): stat effects spelled out instead of
//! three-letter stubs, durations in MUD hours (75 real seconds each),
//! permanent effects flagged.

use bevy_ecs::prelude::*;
use mud_world::{
    AppliedTo, EffectInstance, EffectSource, GrantedByItem, ModifyDelta, Named, Profile, Room,
};

use super::info::cmd_effects;
use super::test_support::{Rx, drain, player_in};

fn setup() -> (World, Entity, Rx) {
    let mut world = World::new();
    world.init_resource::<mud_world::AbilityCatalog>();
    let room = world.spawn(Room).id();
    let (player, rx) = player_in(&mut world, room);
    (world, player, rx)
}

fn add_effect(
    world: &mut World,
    player: Entity,
    name: &str,
    remaining_secs: i32,
    delta: Option<i32>,
) {
    let mut e = world.spawn((
        EffectInstance {
            kind: 1,
            name: name.to_string(),
            strength: 1,
            remaining_secs,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(player),
    ));
    if let Some(amount) = delta {
        e.insert(ModifyDelta {
            target: name.to_string(),
            amount,
        });
    }
}

/// Output with ANSI escapes removed.
fn plain(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn effects_output(world: &mut World, player: Entity, rx: &mut Rx) -> String {
    cmd_effects(world, player, "");
    plain(&drain(rx))
}

#[test]
fn stat_effects_are_spelled_out_not_abbreviated() {
    let (mut world, player, mut rx) = setup();
    add_effect(&mut world, player, "cha", 1050, Some(14));
    add_effect(&mut world, player, "eva", 1650, None);
    add_effect(&mut world, player, "max_hp", 750, Some(20));
    let out = effects_output(&mut world, player, &mut rx);
    assert!(out.contains("Charisma (+14)"), "{out}");
    assert!(out.contains("Evasion"), "{out}");
    assert!(out.contains("Maximum Hit Points (+20)"), "{out}");
    assert!(!out.contains("Cha "), "{out}");
    assert!(!out.contains("Eva "), "{out}");
}

#[test]
fn long_effect_name_is_not_truncated() {
    let (mut world, player, mut rx) = setup();
    add_effect(&mut world, player, "save_spell", 750, None);
    add_effect(
        &mut world,
        player,
        "a_rather_extremely_long_effect_name_indeed",
        750,
        None,
    );
    let out = effects_output(&mut world, player, &mut rx);
    assert!(
        out.contains("Spell Saving Throw (10 hours remaining)"),
        "{out}"
    );
    assert!(
        out.contains("A Rather Extremely Long Effect Name Indeed (10 hours remaining)"),
        "{out}"
    );
}

#[test]
fn durations_are_shown_in_mud_hours() {
    let (mut world, player, mut rx) = setup();
    add_effect(&mut world, player, "cha", 75 * 14, Some(14));
    add_effect(&mut world, player, "wis", 75 * 3 + 1, None);
    let out = effects_output(&mut world, player, &mut rx);
    assert!(out.contains("(14 hours remaining)"), "{out}");
    // Partial hours round up, like legacy `duration + 1`.
    assert!(out.contains("(4 hours remaining)"), "{out}");
    assert!(!out.contains("1050"), "{out}");
}

#[test]
fn sub_hour_durations_read_one_hour() {
    let (mut world, player, mut rx) = setup();
    add_effect(&mut world, player, "str", 74, None);
    add_effect(&mut world, player, "dex", 75, None);
    add_effect(&mut world, player, "con", 76, None);
    add_effect(&mut world, player, "int", 5, None);
    let out = effects_output(&mut world, player, &mut rx);
    assert_eq!(out.matches("(1 hour remaining)").count(), 3, "{out}");
    assert!(out.contains("Constitution (2 hours remaining)"), "{out}");
    assert!(!out.contains("1 hours"), "{out}");
}

#[test]
fn permanent_effect_is_flagged_not_timed() {
    let (mut world, player, mut rx) = setup();
    add_effect(&mut world, player, "infravision", -1, None);
    let out = effects_output(&mut world, player, &mut rx);
    assert!(out.contains("Infravision (permanent)"), "{out}");
    assert!(!out.contains("remaining"), "{out}");
}

fn add_sourced(world: &mut World, player: Entity, name: &str, tag: &str, item: Option<Entity>) {
    let mut e = world.spawn((
        EffectInstance {
            kind: 1,
            name: name.to_string(),
            strength: 1,
            remaining_secs: -1,
            source: EffectSource::Other(tag.to_string()),
            ability_id: None,
        },
        AppliedTo(player),
    ));
    if let Some(item) = item {
        e.insert(GrantedByItem(item));
    }
}

#[test]
fn permanent_effects_show_their_source() {
    let (mut world, player, mut rx) = setup();
    world.entity_mut(player).insert(Profile {
        level: 5,
        class_id: None,
        race: "HALF_ELF".to_string(),
        experience: 0,
        gender: "male".to_string(),
    });
    let ring = world
        .spawn(Named {
            name: "a <red>glowing</> ring".to_string(),
        })
        .id();
    add_sourced(&mut world, player, "infravision", "race", None);
    add_sourced(&mut world, player, "fly", "worn_item", Some(ring));
    add_sourced(&mut world, player, "sanctuary", "mob_default", None);
    let out = effects_output(&mut world, player, &mut rx);
    assert!(
        out.contains("Infravision (permanent) — racial (Half Elf)"),
        "{out}"
    );
    assert!(
        out.contains("Fly (permanent) — from a glowing ring"),
        "{out}"
    );
    assert!(out.contains("Sanctuary (permanent) — innate"), "{out}");
}

#[test]
fn permanent_effect_without_known_source_has_no_suffix() {
    let (mut world, player, mut rx) = setup();
    add_effect(&mut world, player, "infravision", -1, None);
    let out = effects_output(&mut world, player, &mut rx);
    assert!(!out.contains('—'), "{out}");
}

#[test]
fn score_lists_permanent_effects_tagged_and_timed_ones_plain() {
    let (mut world, player, mut rx) = setup();
    world.init_resource::<mud_world::AchievementCatalog>();
    add_effect(&mut world, player, "infravision", -1, None);
    add_effect(&mut world, player, "fly", -1, None);
    add_effect(&mut world, player, "detect_magic", 750, None);
    super::info::cmd_score(&mut world, player, "");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("Infravision (permanent)"), "{out}");
    assert!(out.contains("Fly (permanent)"), "{out}");
    assert!(out.contains("Detect Magic"), "{out}");
    assert!(!out.contains("Detect Magic (permanent)"), "{out}");
}

// -- friendly labels (#13, #39) ---------------------------------------------

/// An invisibility spell instance the way `invoke_ability` builds it:
/// named for the stat it moves, carrying a +40 evasion delta.
fn add_invisibility_spell(world: &mut World, player: Entity, ability_id: i32) {
    world.spawn((
        EffectInstance {
            kind: 1,
            name: "evasion".to_string(),
            strength: 40,
            remaining_secs: 750,
            source: EffectSource::Spell,
            ability_id: Some(ability_id),
        },
        AppliedTo(player),
        ModifyDelta {
            target: "evasion".to_string(),
            amount: 40,
        },
        mud_world::InvisibleSource,
    ));
}

fn catalog_with(world: &mut World, defs: &[(i32, &str)]) {
    let mut catalog = mud_world::AbilityCatalog::default();
    for (id, name) in defs {
        let def =
            super::test_support::ability_def(*id, name, mud_db::abilities::AbilityKind::Spell);
        catalog.by_name.insert(name.to_ascii_lowercase(), def);
    }
    world.insert_resource(catalog);
}

#[test]
fn invisibility_is_listed_by_spell_name_not_as_evasion() {
    let (mut world, player, mut rx) = setup();
    catalog_with(&mut world, &[(7, "Invisibility")]);
    add_invisibility_spell(&mut world, player, 7);
    let out = effects_output(&mut world, player, &mut rx);
    assert!(out.contains("Invisibility (10 hours remaining)"), "{out}");
    assert!(!out.contains("Evasion"), "{out}");
    assert!(!out.contains("+40"), "{out}");
}

#[test]
fn spell_stat_buffs_are_named_for_the_spell_with_the_stat_spelled_out() {
    let (mut world, player, mut rx) = setup();
    catalog_with(&mut world, &[(8, "Enhance Ability")]);
    world.spawn((
        EffectInstance {
            kind: 1,
            name: "cha".to_string(),
            strength: 4,
            remaining_secs: 750,
            source: EffectSource::Spell,
            ability_id: Some(8),
        },
        AppliedTo(player),
        ModifyDelta {
            target: "cha".to_string(),
            amount: 4,
        },
    ));
    let out = effects_output(&mut world, player, &mut rx);
    assert!(
        out.contains("Enhance Ability (+4 Charisma) (10 hours remaining)"),
        "{out}"
    );
    assert!(!out.contains("from"), "{out}");
}

#[test]
fn ability_display_name_falls_back_to_title_cased_plain_name() {
    let mut def = super::test_support::ability_def(9, "", mud_db::abilities::AbilityKind::Spell);
    def.plain_name = "DETECT_MAGIC".to_string();
    assert_eq!(super::info::ability_label(&def), "Detect Magic");
    def.name = "<b:cyan>Fire</>ball".to_string();
    assert_eq!(super::info::ability_label(&def), "Fireball");
}

#[test]
fn score_and_prompt_use_the_spell_name_for_invisibility() {
    let (mut world, player, mut rx) = setup();
    world.init_resource::<mud_world::AchievementCatalog>();
    catalog_with(&mut world, &[(7, "Invisibility")]);
    add_invisibility_spell(&mut world, player, 7);
    super::info::cmd_score(&mut world, player, "");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("Invisibility"), "{out}");
    assert!(!out.contains("Evasion"), "{out}");
    let inst = world
        .query::<&EffectInstance>()
        .iter(&world)
        .next()
        .unwrap()
        .clone();
    assert_eq!(super::info::effect_label(&world, &inst), "Invisibility");
}
