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
    // Above level 20, so helpers do more than watch.
    fx.world.get_mut::<mud_world::Profile>(p).unwrap().level = 50;
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
    assert!(seen.contains("jumps to the aid of"), "{seen}");
    // Without familiarity the roll never matters.
    let (joined, _) = helper_joins(false, 1);
    assert!(joined, "no Familiar, no back-off");
}

/// Legacy `mob_assist` scene: a level-`level` player attacks a mob
/// cityguard (given `guard_flags`, level 50) while a deputy with
/// `helper_flags` and `helper_alignment` stands by. Returns what the deputy
/// ended up fighting, plus the player and the guard.
fn assist_scene(
    level: i32,
    helper_flags: &[mud_db::enums::MobBehavior],
    guard_flags: &[mud_db::enums::MobBehavior],
    helper_alignment: i32,
    player_alignment: i32,
) -> (Option<Entity>, Entity, Entity) {
    let (mut fx, p, _rx) = caster_with_flag_spell("familiarity");
    fx.world.get_mut::<mud_world::Profile>(p).unwrap().level = level;
    fx.world.get_mut::<CombatStats>(p).unwrap().alignment = player_alignment;
    let guard = mob_in(&mut fx, "a hapless cityguard", 0);
    fx.world.entity_mut(guard).insert((
        mud_world::MobBehaviors(guard_flags.to_vec()),
        mud_world::Profile {
            level: 50,
            class_id: None,
            race: "human".into(),
            experience: 0,
            gender: "male".into(),
        },
    ));
    let deputy = mob_in(&mut fx, "a loyal deputy", helper_alignment);
    fx.world
        .entity_mut(deputy)
        .insert(mud_world::MobBehaviors(helper_flags.to_vec()));
    let room = fx.a;
    super::mob_helpers_engage(&mut fx.world, guard, p, room);
    let fighting = fx.world.get::<mud_world::Fighting>(deputy).map(|f| f.0);
    (fighting, p, guard)
}

#[test]
fn helpers_only_watch_a_low_level_target() {
    use mud_db::enums::MobBehavior::Helper;
    // Level 20 or less: watch only (legacy `GET_LEVEL(FIGHTING(vict)) <= 20`).
    let (fighting, ..) = assist_scene(20, &[Helper], &[], 0, 0);
    assert_eq!(fighting, None, "level 20 target is only watched");
    let (fighting, p, _) = assist_scene(21, &[Helper], &[], 0, 0);
    assert_eq!(fighting, Some(p), "level 21 target is fought");
}

#[test]
fn assist_flags_and_will_assist_rules() {
    use mud_db::enums::MobBehavior::{Helper, Peaceful, Peacekeeper, Protector};
    // MOB_ASSISTER excludes PEACEFUL mobs; a bare mob never joins.
    assert_eq!(assist_scene(50, &[Helper, Peaceful], &[], 0, 0).0, None);
    assert_eq!(assist_scene(50, &[], &[], 0, 0).0, None);
    // A helper backs any mob.
    let (fighting, p, _) = assist_scene(50, &[Helper], &[], 0, 0);
    assert_eq!(fighting, Some(p));
    // A protector backs the player against a plain mob (level 50 foe) ...
    let (fighting, _, guard) = assist_scene(50, &[Protector], &[], 0, 0);
    assert_eq!(fighting, Some(guard), "protector defends the player");
    // ... but against a protector or peacekeeper it sides with them, never
    // with the player.
    let (fighting, p, _) = assist_scene(50, &[Protector], &[Protector], 0, 0);
    assert_eq!(fighting, Some(p), "protector backs the other protector");
    let (fighting, p, _) = assist_scene(50, &[Protector], &[Peacekeeper], 0, 0);
    assert_eq!(fighting, Some(p), "protector backs the peacekeeper");
    // A peacekeeper backs a protector, but not a plain mob ...
    let (fighting, p, _) = assist_scene(50, &[Peacekeeper], &[Protector], 0, 0);
    assert_eq!(fighting, Some(p), "peacekeeper backs a protector");
    assert_eq!(assist_scene(50, &[Peacekeeper], &[], 0, 0).0, None);
    // ... unless the foe's alignment is more than 1350 away from its own.
    let (fighting, p, _) = assist_scene(50, &[Peacekeeper], &[], 1000, -1000);
    assert_eq!(fighting, Some(p), "peacekeeper vs a badly-aligned foe");
}

fn level_50_profile() -> mud_world::Profile {
    mud_world::Profile {
        level: 50,
        class_id: None,
        race: "human".into(),
        experience: 0,
        gender: "male".into(),
    }
}

#[test]
fn defended_player_sees_second_person_and_bystanders_third() {
    use mud_db::enums::MobBehavior::Protector;
    let (mut fx, p, mut rx) = caster_with_flag_spell("familiarity");
    let room = fx.a;
    let (_bystander, mut brx) = player(&mut fx.world, room, "Bystander");
    let brute = mob_in(&mut fx, "a brute", 0);
    fx.world.entity_mut(brute).insert(level_50_profile());
    let deputy = mob_in(&mut fx, "a loyal deputy", 0);
    fx.world
        .entity_mut(deputy)
        .insert(mud_world::MobBehaviors(vec![Protector]));
    let _ = drain(&mut rx);
    let _ = drain(&mut brx);
    // The brute attacks the player; the protector deputy jumps in.
    super::mob_helpers_engage(&mut fx.world, p, brute, room);
    assert_eq!(
        fx.world.get::<mud_world::Fighting>(deputy).map(|f| f.0),
        Some(brute)
    );
    let seen = drain(&mut rx);
    assert!(seen.contains("A loyal deputy jumps to your aid!"), "{seen}");
    assert!(!seen.contains("aid of"), "{seen}");
    let seen = drain(&mut brx);
    assert!(
        seen.contains("A loyal deputy jumps to the aid of Caster!"),
        "{seen}"
    );
}

#[test]
fn a_pet_never_assists_against_its_master() {
    use mud_db::enums::MobBehavior::Helper;
    let (mut fx, p, _rx) = caster_with_flag_spell("familiarity");
    let room = fx.a;
    fx.world.get_mut::<mud_world::Profile>(p).unwrap().level = 50;
    let victim = mob_in(&mut fx, "a hapless cityguard", 0);
    let pet = mob_in(&mut fx, "a loyal hound", 0);
    fx.world.entity_mut(pet).insert((
        mud_world::MobBehaviors(vec![Helper]),
        mud_world::Follower(p),
    ));
    // The player attacks the guard: the hound, who follows the player, must
    // not turn on its master on the guard's behalf.
    super::mob_helpers_engage(&mut fx.world, victim, p, room);
    assert!(fx.world.get::<mud_world::Fighting>(pet).is_none());
    // Not following anyone, the same hound does back the guard.
    fx.world.entity_mut(pet).remove::<mud_world::Follower>();
    super::mob_helpers_engage(&mut fx.world, victim, p, room);
    assert_eq!(
        fx.world.get::<mud_world::Fighting>(pet).map(|f| f.0),
        Some(p)
    );
}

#[test]
fn a_charmed_pet_of_the_target_stays_out() {
    use mud_db::enums::MobBehavior::Helper;
    let (mut fx, _p, _rx) = caster_with_flag_spell("familiarity");
    let room = fx.a;
    // A mob master (charmed servants follow mobs too).
    let master = mob_in(&mut fx, "a warlock", 0);
    fx.world.entity_mut(master).insert(level_50_profile());
    let victim = mob_in(&mut fx, "a hapless cityguard", 0);
    let thrall = mob_in(&mut fx, "a thrall", 0);
    fx.world.entity_mut(thrall).insert((
        mud_world::MobBehaviors(vec![Helper]),
        mud_world::Follower(master),
    ));
    super::mob_helpers_engage(&mut fx.world, victim, master, room);
    assert!(fx.world.get::<mud_world::Fighting>(thrall).is_none());
}
