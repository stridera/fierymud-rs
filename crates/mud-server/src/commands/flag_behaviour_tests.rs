//! Status flags that used to be display-only and now do something:
//! `infravision`, `detect_life`, `detect_align`, `blur` (see the combat
//! tests) and `familiarity`. Each is applied through the spell path
//! (`status` effect) and checked for its marker and its behaviour.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_world::{
    CombatStats, Health, Invisible, KnownAbilities, Located, Mob, Named, RoomMagicalDarkness,
};

use super::gmcp_tests::{Fx, fixture, player};
use super::test_support::{Rx, ability_def, drain};
use super::{dispatch, mob_will_start_fight};

const SPELL: i32 = 1;
const EFFECT: i32 = 10;

/// A world where `flagspell` is a real spell applying the `status` flag
/// `flag`, and a level-20 caster ("Caster") who knows it.
fn caster_with_flag_spell(flag: &str) -> (Fx, Entity, Rx) {
    let mut fx = fixture();
    let mut catalog = mud_world::AbilityCatalog::default();
    let mut spell = ability_def(SPELL, "Flagspell", AbilityKind::Spell);
    spell.cast_time_rounds = 0;
    catalog.by_name.insert("flagspell".to_string(), spell);
    catalog.effects_for.insert(
        SPELL,
        vec![(
            EFFECT,
            Some(serde_json::json!({ "flag": flag, "duration": 60 })),
        )],
    );
    fx.world.insert_resource(catalog);
    let mut effects = mud_world::EffectCatalog::default();
    effects.by_id.insert(
        EFFECT,
        mud_world::EffectDef {
            id: EFFECT,
            name: "status".to_string(),
            description: None,
            effect_type: "status".to_string(),
            tags: Vec::new(),
            presence_override: None,
            default_params: serde_json::json!({}),
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        },
    );
    fx.world.insert_resource(effects);
    fx.world
        .insert_resource(mud_world::SpellSlotData::default());
    fx.world
        .insert_resource(mud_world::ClassSkillsData::default());
    let a = fx.a;
    let (p, rx) = player(&mut fx.world, a, "Caster");
    fx.world.entity_mut(p).insert((
        Health { hp: 50, max: 50 },
        CombatStats::default(),
        KnownAbilities {
            entries: vec![(SPELL, 500, true)],
        },
    ));
    (fx, p, rx)
}

fn cast(fx: &mut Fx, p: Entity) {
    dispatch(&mut fx.world, p, "cast 'flagspell'");
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
}

fn mob_in(fx: &mut Fx, name: &str, alignment: i32) -> Entity {
    let room = fx.a;
    fx.world
        .spawn((
            Mob,
            Named { name: name.into() },
            mud_world::Description(format!("{name} stands here.")),
            Located(room),
            CombatStats {
                alignment,
                ..CombatStats::default()
            },
            Health { hp: 20, max: 20 },
        ))
        .id()
}

fn look(fx: &mut Fx, p: Entity, rx: &mut Rx) -> String {
    let _ = drain(rx);
    dispatch(&mut fx.world, p, "look");
    drain(rx)
}

#[test]
fn the_spell_path_installs_and_removes_each_marker() {
    type Has = fn(&World, Entity) -> bool;
    let cases: [(&str, Has); 5] = [
        ("infravision", |w, e| {
            w.get::<mud_world::Infravision>(e).is_some()
        }),
        ("detect_life", |w, e| {
            w.get::<mud_world::SenseLife>(e).is_some()
        }),
        ("detect_align", |w, e| {
            w.get::<mud_world::DetectAlign>(e).is_some()
        }),
        ("blur", |w, e| w.get::<mud_world::Blur>(e).is_some()),
        ("familiarity", |w, e| {
            w.get::<mud_world::Familiar>(e).is_some()
        }),
    ];
    for (flag, has) in cases {
        let (mut fx, p, _rx) = caster_with_flag_spell(flag);
        assert!(!has(&fx.world, p), "{flag}: not there before the cast");
        cast(&mut fx, p);
        assert!(has(&fx.world, p), "{flag}: marker installed by the spell");
        super::remove_effect_named(&mut fx.world, p, flag);
        assert!(!has(&fx.world, p), "{flag}: marker gone with the effect");
    }
}

#[test]
fn infravision_shows_warm_bodies_in_a_dark_room_but_not_the_room() {
    let (mut fx, p, mut rx) = caster_with_flag_spell("infravision");
    fx.world.entity_mut(fx.a).insert(RoomMagicalDarkness);
    let _wolf = mob_in(&mut fx, "a grey wolf", 0);
    let blind = look(&mut fx, p, &mut rx);
    assert!(blind.contains("pitch black"), "{blind}");
    assert!(!blind.contains("red shape"), "{blind}");
    cast(&mut fx, p);
    let out = look(&mut fx, p, &mut rx);
    assert!(out.contains("pitch black"), "the room stays dark: {out}");
    assert!(
        out.contains("The red shape of a medium living being is here."),
        "{out}"
    );
    let text = out.split("pitch black").nth(1).unwrap_or_default();
    assert!(!text.contains("grey wolf"), "no names in the dark: {out}");
}

#[test]
fn detect_life_counts_what_the_dark_hides_and_the_invisible() {
    let (mut fx, p, mut rx) = caster_with_flag_spell("detect_life");
    let _wolf = mob_in(&mut fx, "a grey wolf", 0);
    let room = fx.a;
    let (ghost, _grx) = player(&mut fx.world, room, "Ghost");
    fx.world.entity_mut(ghost).insert(Invisible);
    // Lit room: the invisible player is only sensed.
    let before = look(&mut fx, p, &mut rx);
    assert!(!before.contains("hidden lifeform"), "{before}");
    cast(&mut fx, p);
    let lit = look(&mut fx, p, &mut rx);
    assert!(lit.contains("You sense a hidden lifeform."), "{lit}");
    assert!(!lit.contains("Ghost"), "identity stays hidden: {lit}");
    // Dark room: the wolf and the ghost are both sensed.
    fx.world.entity_mut(room).insert(RoomMagicalDarkness);
    let dark = look(&mut fx, p, &mut rx);
    assert!(dark.contains("You sense a few hidden lifeforms."), "{dark}");
}

#[test]
fn detect_life_senses_invisible_mobs_in_a_lit_room() {
    let (mut fx, p, mut rx) = caster_with_flag_spell("detect_life");
    cast(&mut fx, p);
    let imp = mob_in(&mut fx, "a gutter imp", 0);
    fx.world.entity_mut(imp).insert(Invisible);
    let lit = look(&mut fx, p, &mut rx);
    assert!(lit.contains("You sense a hidden lifeform."), "{lit}");
    assert!(!lit.contains("gutter imp"), "identity stays hidden: {lit}");
    // Undead carry no life force to sense.
    fx.world
        .entity_mut(imp)
        .insert(mud_world::LifeForceTag(mud_db::enums::LifeForce::Undead));
    let lit = look(&mut fx, p, &mut rx);
    assert!(!lit.contains("hidden lifeform"), "{lit}");
}

#[test]
fn detect_align_tags_evil_and_good_actors() {
    let (mut fx, p, mut rx) = caster_with_flag_spell("detect_align");
    let _imp = mob_in(&mut fx, "a gutter imp", -600);
    let _dove = mob_in(&mut fx, "a white dove", 600);
    let _rat = mob_in(&mut fx, "a plain rat", 0);
    let before = look(&mut fx, p, &mut rx);
    assert!(!before.contains("Aura"), "{before}");
    cast(&mut fx, p);
    let out = look(&mut fx, p, &mut rx);
    let line = |needle: &str| {
        out.lines()
            .find(|l| l.contains(needle) && l.contains("stands here"))
            .unwrap_or_else(|| panic!("no {needle} line: {out}"))
            .to_string()
    };
    assert!(line("gutter imp").contains("(Red Aura)"), "{out}");
    assert!(line("white dove").contains("(Gold Aura)"), "{out}");
    assert!(!line("plain rat").contains("Aura"), "{out}");
}

#[test]
fn familiarity_keeps_aggressive_mobs_from_starting_a_fight() {
    let (mut fx, p, _rx) = caster_with_flag_spell("familiarity");
    let wolf = mob_in(&mut fx, "a grey wolf", -1000);
    assert!(mob_will_start_fight(&fx.world, wolf, p), "hostile before");
    cast(&mut fx, p);
    assert!(!mob_will_start_fight(&fx.world, wolf, p), "friend after");
    super::remove_effect_named(&mut fx.world, p, "familiarity");
    assert!(mob_will_start_fight(&fx.world, wolf, p), "hostile again");
}

/// Run `mob_helpers_engage` with the familiarity roll pinned to `roll`
/// and report whether the helper joined the fight, plus what `p` saw.
fn helper_joins(familiar: bool, roll: i32) -> (bool, String) {
    let (mut fx, p, mut rx) = caster_with_flag_spell("familiarity");
    if familiar {
        fx.world.entity_mut(p).insert(mud_world::Familiar);
    }
    let victim = mob_in(&mut fx, "a hapless cityguard", 0);
    let helper = mob_in(&mut fx, "a loyal deputy", 0);
    fx.world
        .entity_mut(helper)
        .insert(mud_world::MobBehaviors(vec![
            mud_db::enums::MobBehavior::Helper,
        ]));
    let room = fx.a;
    let _ = drain(&mut rx);
    super::FORCED_FAMILIARITY_ROLL.with(|c| c.set(Some(roll)));
    super::mob_helpers_engage(&mut fx.world, victim, p, room);
    super::FORCED_FAMILIARITY_ROLL.with(|c| c.set(None));
    let joined = fx.world.get::<mud_world::Fighting>(helper).map(|f| f.0) == Some(p);
    (joined, drain(&mut rx))
}

#[test]
fn familiarity_can_make_a_helper_mob_back_off() {
    // Roll under 50: the helper takes the target for a friend.
    let (joined, seen) = helper_joins(true, 49);
    assert!(!joined, "helper stopped");
    assert!(seen.contains("gets a good look at you and stops"), "{seen}");
    // Roll of 50 or more: it assists as usual.
    let (joined, seen) = helper_joins(true, 50);
    assert!(joined, "helper joined");
    assert!(seen.contains("leaps to"), "{seen}");
    // Without familiarity the roll never matters.
    let (joined, _) = helper_joins(false, 1);
    assert!(joined, "no Familiar, no back-off");
}
