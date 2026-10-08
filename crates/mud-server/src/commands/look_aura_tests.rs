//! `look` at another actor (issue #86): the player appearance line carries
//! composition, and spell auras read as legacy flavor lines gated on
//! Detect Magic instead of a flat "affected by" list.

use bevy_ecs::prelude::*;
use mud_db::enums::Composition;
use mud_world::{
    AppliedTo, ClassCatalog, CombatStats, EffectAura, EffectAuraCatalog, EffectInstance,
    EffectSource, Health, Keywords, Located, Named, Player, Profile, RaceCatalog, RaceDef, Room,
};

use super::info::cmd_look;
use super::test_support::{Rx, drain, player_in};

fn profile(race: &str, gender: &str) -> Profile {
    Profile {
        level: 10,
        class_id: None,
        race: race.to_string(),
        experience: 0,
        gender: gender.to_string(),
    }
}

fn aura(keys: &[&str], text: &str) -> EffectAura {
    EffectAura {
        keys: keys.iter().map(|k| (*k).to_string()).collect(),
        text: text.to_string(),
        needs_detect_magic: false,
        exclusive_group: None,
        min_alignment: None,
        max_alignment: None,
    }
}

/// A slice of the `EffectAura` table (what the boot loader would build).
fn aura_catalog() -> EffectAuraCatalog {
    let mut bless = aura(
        &["bless"],
        "The shimmering telltales of a <b:yellow>magical blessing</> flutter about {S} head.",
    );
    bless.needs_detect_magic = true;
    let mut dragon = aura(
        &["dragons health"],
        "The power of <magenta>dragon's blood</> fills {M}!",
    );
    dragon.needs_detect_magic = true;
    dragon.exclusive_group = Some("health_boost".to_string());
    let mut endurance = aura(
        &["endurance"],
        "{^S} health appears to be bolstered by magical power.",
    );
    endurance.needs_detect_magic = true;
    endurance.exclusive_group = Some("health_boost".to_string());
    let mut evil = aura(
        &["sanctuary"],
        "<dim>{^S} body is surrounded by a black aura!</>",
    );
    evil.max_alignment = Some(-350);
    let mut neutral = aura(
        &["sanctuary"],
        "<b:blue>{^S} body is surrounded by a blue aura!</>",
    );
    neutral.min_alignment = Some(-349);
    neutral.max_alignment = Some(349);
    let mut good = aura(
        &["sanctuary"],
        "<b:white>{^S} body is surrounded by a white aura!</>",
    );
    good.min_alignment = Some(350);
    EffectAuraCatalog {
        auras: vec![
            bless,
            dragon,
            endurance,
            aura(&["fireshield"], "<b:red>{^S} body is encased in fire!</>"),
            aura(
                &["blind", "blindness"],
                "{^S} <dim>dull</> eyes suggest {E} is blind!",
            ),
            evil,
            neutral,
            good,
        ],
    }
}

fn setup() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(aura_catalog());
    world.init_resource::<ClassCatalog>();
    let mut races = RaceCatalog::default();
    races.by_race.insert(
        "DRYAD".to_string(),
        RaceDef {
            race: "DRYAD".to_string(),
            default_size: "MEDIUM".to_string(),
            default_lifeforce: "LIFE".to_string(),
            default_composition: Composition::Plant,
            ..RaceDef::default()
        },
    );
    world.insert_resource(races);
    let room = world.spawn(Room).id();
    let (me, rx) = player_in(&mut world, room);
    world
        .entity_mut(me)
        .insert((profile("DRYAD", "female"), Health { hp: 10, max: 10 }));
    let other = world
        .spawn((
            Player,
            Named {
                name: "Bob".to_string(),
            },
            Keywords(vec!["bob".to_string()]),
            Located(room),
            profile("DRYAD", "male"),
            Health { hp: 10, max: 10 },
        ))
        .id();
    (world, me, other, rx)
}

fn plain(rx: &mut Rx) -> String {
    let raw = drain(rx);
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn add_effect(world: &mut World, on: Entity, name: &str) {
    world.spawn((
        EffectInstance {
            kind: 1,
            name: name.to_string(),
            strength: 1,
            remaining_secs: 600,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(on),
    ));
}

#[test]
fn player_look_shows_size_and_race_composition() {
    let (mut world, me, _other, mut rx) = setup();
    cmd_look(&mut world, me, "bob");
    let out = plain(&mut rx);
    assert!(out.contains("Lifeforce: Life."), "{out}");
    assert!(
        out.contains("He is medium in size, and is composed of plant material."),
        "{out}"
    );
}

#[test]
fn magical_auras_need_detect_magic() {
    let (mut world, me, other, mut rx) = setup();
    add_effect(&mut world, other, "bless");
    cmd_look(&mut world, me, "bob");
    let blind = plain(&mut rx);
    assert!(!blind.contains("blessing"), "{blind}");
    assert!(!blind.contains("affected by"), "{blind}");

    add_effect(&mut world, me, "detect_magic");
    cmd_look(&mut world, me, "bob");
    let seen = plain(&mut rx);
    assert!(
        seen.contains("The shimmering telltales of a magical blessing flutter about his head."),
        "{seen}"
    );
    assert!(!seen.contains("affected by"), "{seen}");
}

#[test]
fn visible_auras_show_without_detect_magic_and_follow_gender() {
    let (mut world, me, other, mut rx) = setup();
    add_effect(&mut world, other, "fireshield");
    add_effect(&mut world, other, "blind");
    cmd_look(&mut world, me, "bob");
    let out = plain(&mut rx);
    assert!(out.contains("His body is encased in fire!"), "{out}");
    assert!(out.contains("His dull eyes suggest he is blind!"), "{out}");
}

#[test]
fn worn_gear_lists_one_piece_per_line() {
    let (mut world, me, other, mut rx) = setup();
    for (name, slot) in [
        ("a steel helm", mud_world::Slot::Head),
        ("a pair of boots", mud_world::Slot::Feet),
    ] {
        world.spawn((
            mud_world::Item,
            Named {
                name: name.to_string(),
            },
            Located(other),
            mud_world::EquippedSlot(slot),
        ));
    }
    cmd_look(&mut world, me, "bob");
    let out = plain(&mut rx);
    let lines: Vec<&str> = out.lines().collect();
    let using = lines.iter().position(|l| l.contains("is using:")).unwrap();
    assert!(lines[using + 1].contains("a steel helm"), "{out}");
    assert!(lines[using + 2].contains("a pair of boots"), "{out}");
}

#[test]
fn sanctuary_line_follows_bearer_alignment() {
    for (alignment, expect) in [
        (-500, "black aura"),
        (-349, "blue aura"),
        (0, "blue aura"),
        (349, "blue aura"),
        (350, "white aura"),
    ] {
        let (mut world, me, other, mut rx) = setup();
        world.entity_mut(other).insert(CombatStats {
            alignment,
            ..CombatStats::default()
        });
        add_effect(&mut world, other, "sanctuary");
        cmd_look(&mut world, me, "bob");
        let out = plain(&mut rx);
        assert!(out.contains(expect), "{alignment}: {out}");
        assert_eq!(out.matches("surrounded by a").count(), 1, "{out}");
    }
}

#[test]
fn dragons_health_supersedes_plain_endurance() {
    let (mut world, me, other, mut rx) = setup();
    add_effect(&mut world, me, "detect_magic");
    add_effect(&mut world, other, "endurance");
    add_effect(&mut world, other, "dragons_health");
    cmd_look(&mut world, me, "bob");
    let out = plain(&mut rx);
    assert!(out.contains("dragon's blood"), "{out}");
    assert!(!out.contains("bolstered by magical power"), "{out}");
}
