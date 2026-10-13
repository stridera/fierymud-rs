//! Ventriloquate (legacy `spell_ventriloquate`): the room hears the target
//! say the caster's words, listeners' saving rolls reveal the trick, the
//! casting is not announced, nothing is applied to the caster. Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_world::{
    AbilityCatalog, Account, CoreStats, EffectCatalog, EffectDef, EffectInstance, Health,
    KnownAbilities, Located, Mob, Named, Profile, Room, WorldKey, WorldKeyIndex,
};

use super::invoke_ability_with;
use super::test_support::{Rx, ability_def, drain, player_in};
use crate::commands::ventriloquate::{is_speak_as, speak_as, split_tail};

const VENT: i32 = 377;
const ALTER: i32 = 7;
const STATUS: i32 = 10;

struct Fx {
    world: World,
    room: Entity,
    caster: Entity,
    rx: Rx,
}

fn effect_def(id: i32, effect_type: &str) -> EffectDef {
    EffectDef {
        id,
        name: effect_type.to_string(),
        description: None,
        effect_type: effect_type.to_string(),
        tags: Vec::new(),
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

/// Ventriloquate wired like the data patch: with `manual` the single
/// `alter_object` row in mode `speak_as`, without it the old `hidden`
/// status row.
fn fx(manual: bool, cast_rounds: i32) -> Fx {
    let mut world = World::new();
    world.insert_resource(WorldKeyIndex::default());
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    world.insert_resource(mud_world::SpellSlotData::default());
    world.insert_resource(mud_world::ClassSkillsData::default());
    world.insert_resource(mud_world::WeatherCatalog::default());
    let room = world
        .spawn((
            Room,
            WorldKey { zone: 30, id: 1 },
            Named {
                name: "Here".into(),
            },
        ))
        .id();
    let mut def = ability_def(VENT, "Ventriloquate", AbilityKind::Spell);
    def.cast_time_rounds = cast_rounds;
    let mut catalog = AbilityCatalog::default();
    catalog.by_name.insert("ventriloquate".into(), def);
    let row = if manual {
        (ALTER, serde_json::json!({"mode": "speak_as"}))
    } else {
        (
            STATUS,
            serde_json::json!({"flag": "hidden", "amount": 20, "duration": "skill", "durationUnit": "hours"}),
        )
    };
    catalog.effects_for.insert(VENT, vec![(row.0, Some(row.1))]);
    world.insert_resource(catalog);
    let mut effects = EffectCatalog::default();
    effects
        .by_id
        .insert(ALTER, effect_def(ALTER, "alter_object"));
    effects.by_id.insert(STATUS, effect_def(STATUS, "status"));
    world.insert_resource(effects);
    let (caster, rx) = player_in(&mut world, room);
    world.entity_mut(caster).insert((
        Profile {
            level: 20,
            class_id: None,
            race: "Human".into(),
            experience: 0,
            gender: "male".into(),
        },
        KnownAbilities {
            entries: vec![(VENT, 1000, true)],
        },
        Health { hp: 100, max: 100 },
    ));
    Fx {
        world,
        room,
        caster,
        rx,
    }
}

impl Fx {
    fn mob(&mut self, name: &str) -> Entity {
        self.world
            .spawn((
                Mob,
                Named { name: name.into() },
                Located(self.room),
                Health { hp: 10, max: 10 },
            ))
            .id()
    }

    fn player(&mut self, name: &str, level: i32, wisdom: i32) -> (Entity, Rx) {
        let (p, rx) = player_in(&mut self.world, self.room);
        self.world.entity_mut(p).insert((
            Named { name: name.into() },
            Profile {
                level,
                class_id: None,
                race: "Human".into(),
                experience: 0,
                gender: "female".into(),
            },
            CoreStats {
                wisdom,
                ..CoreStats::default()
            },
        ));
        (p, rx)
    }

    fn cast(&mut self, args: &str) -> String {
        invoke_ability_with(
            &mut self.world,
            self.caster,
            args,
            AbilityKind::Spell,
            "cast",
            false,
            false,
            false,
            None,
        );
        drain(&mut self.rx)
    }
}

/// A roller that returns the given values in order (clamped into range).
fn rolls(values: &[i32]) -> impl FnMut(i32) -> i32 + '_ {
    let mut it = values.iter().copied();
    move |max| it.next().unwrap_or(max).clamp(1, max)
}

#[test]
fn the_manual_marker_is_the_speak_as_row() {
    let fx_manual = fx(true, 0);
    let def = fx_manual
        .world
        .resource::<AbilityCatalog>()
        .by_name
        .get("ventriloquate")
        .cloned()
        .unwrap();
    assert!(is_speak_as(&fx_manual.world, &def));
    let fx_old = fx(false, 0);
    assert!(!is_speak_as(&fx_old.world, &def));
}

#[test]
fn the_text_after_the_target_is_the_message() {
    assert_eq!(
        split_tail(Some("bob hello  there ")),
        (Some("bob"), Some("hello  there"))
    );
    assert_eq!(split_tail(Some("bob")), (Some("bob"), None));
    assert_eq!(split_tail(Some("  bob   ")), (Some("bob"), None));
    assert_eq!(split_tail(Some("")), (None, None));
    assert_eq!(split_tail(None), (None, None));
}

#[test]
fn a_cast_makes_the_room_hear_the_target_and_tells_the_caster_nothing() {
    let mut fx = fx(true, 0);
    fx.mob("Bob");
    // Level 1, wisdom 0: no roll can beat the thresholds, so Cara is fooled.
    let (_, mut cara) = fx.player("Cara", 1, 0);
    let out = fx.cast("'ventriloquate' bob hello there");
    assert_eq!(out, "", "the caster sees nothing: {out:?}");
    let heard = drain(&mut cara);
    assert_eq!(heard, "Bob says, 'hello there'\r\n");
}

#[test]
fn nothing_is_applied_to_the_caster() {
    let mut fx = fx(true, 0);
    fx.mob("Bob");
    fx.cast("'ventriloquate' bob hi");
    let mut q = fx.world.query::<&EffectInstance>();
    assert_eq!(q.iter(&fx.world).count(), 0);
    assert!(fx.world.get::<mud_world::Stealth>(fx.caster).is_none());
}

#[test]
fn old_data_without_the_marker_still_applies_its_status() {
    let mut fx = fx(false, 0);
    fx.mob("Bob");
    let (_, mut cara) = fx.player("Cara", 1, 0);
    fx.cast("'ventriloquate' bob");
    assert_eq!(drain(&mut cara), "", "no speech without the marker");
    let mut q = fx.world.query::<&EffectInstance>();
    assert_eq!(q.iter(&fx.world).count(), 1, "the hidden status row ran");
}

#[test]
fn a_player_target_is_always_seen_through() {
    let mut fx = fx(true, 0);
    let (_, mut bob) = fx.player("Bob", 1, 0);
    let (_, mut cara) = fx.player("Cara", 1, 0);
    fx.cast("'ventriloquate' bob hello");
    assert_eq!(
        drain(&mut cara),
        "Someone tried to make it sound like Bob said, 'hello'!\r\n"
    );
    assert_eq!(
        drain(&mut bob),
        "Someone tried to make it sound like you said 'hello'!\r\n"
    );
}

#[test]
fn a_listener_who_beats_the_first_roll_sees_through_the_trick() {
    let mut fx = fx(true, 0);
    let bob = fx.mob("Bob");
    // Level 50, wisdom 40: sees through below 50 + 20 = 70.
    let (_, mut cara) = fx.player("Cara", 50, 40);
    let caster = fx.caster;
    speak_as(
        &mut fx.world,
        caster,
        bob,
        80,
        Some("boo"),
        &mut rolls(&[69]),
    );
    assert_eq!(
        drain(&mut cara),
        "Someone tried to make it sound like Bob said, 'boo'!\r\n"
    );
}

#[test]
fn the_second_roll_names_the_caster() {
    let mut fx = fx(true, 0);
    let bob = fx.mob("Bob");
    // Second threshold (50 + 40) / 4 = 22.
    let (_, mut cara) = fx.player("Cara", 50, 40);
    let caster = fx.caster;
    speak_as(
        &mut fx.world,
        caster,
        bob,
        80,
        Some("boo"),
        &mut rolls(&[70, 21]),
    );
    assert_eq!(
        drain(&mut cara),
        "Tester tries to make it sound like Bob says, 'boo'!\r\n"
    );
}

#[test]
fn failing_both_rolls_believes_it() {
    let mut fx = fx(true, 0);
    let bob = fx.mob("Bob");
    let (_, mut cara) = fx.player("Cara", 50, 40);
    let caster = fx.caster;
    speak_as(
        &mut fx.world,
        caster,
        bob,
        80,
        Some("boo"),
        &mut rolls(&[70, 22]),
    );
    assert_eq!(drain(&mut cara), "Bob says, 'boo'\r\n");
}

#[test]
fn the_rolls_run_on_eight_times_skill_plus_24() {
    let mut fx = fx(true, 0);
    let bob = fx.mob("Bob");
    fx.player("Cara", 50, 40);
    let caster = fx.caster;
    let mut seen = Vec::new();
    speak_as(&mut fx.world, caster, bob, 76, Some("x"), &mut |max| {
        seen.push(max);
        max
    });
    // 8 * (76 + 24) = 800, one roll for the first test and one for the second.
    assert_eq!(seen, vec![800, 800]);
}

#[test]
fn each_listener_rolls_for_themselves() {
    let mut fx = fx(true, 0);
    let bob = fx.mob("Bob");
    let (_, mut sharp) = fx.player("Cara", 50, 40);
    let (_, mut dull) = fx.player("Dana", 1, 0);
    let caster = fx.caster;
    // The roll value is 60 for both: it beats Cara's 70, never Dana's 1.
    speak_as(&mut fx.world, caster, bob, 80, Some("hm"), &mut |_| 60);
    assert!(drain(&mut sharp).starts_with("Someone tried"));
    assert_eq!(drain(&mut dull), "Bob says, 'hm'\r\n");
}

#[test]
fn a_blind_listener_cannot_name_the_speaker_but_hears_the_words() {
    let mut fx = fx(true, 0);
    let bob = fx.mob("Bob");
    let (cara, mut crx) = fx.player("Cara", 1, 0);
    fx.world.entity_mut(cara).insert(mud_world::Blinded);
    let caster = fx.caster;
    speak_as(&mut fx.world, caster, bob, 80, Some("hi"), &mut |max| max);
    assert_eq!(drain(&mut crx), "Someone says, 'hi'\r\n");
}

#[test]
fn a_sleeper_hears_nothing() {
    let mut fx = fx(true, 0);
    let bob = fx.mob("Bob");
    let (cara, mut crx) = fx.player("Cara", 1, 0);
    fx.world
        .entity_mut(cara)
        .insert(mud_world::Posture(mud_world::PostureKind::Sleeping));
    let caster = fx.caster;
    speak_as(&mut fx.world, caster, bob, 80, Some("hi"), &mut |max| max);
    assert_eq!(drain(&mut crx), "");
}

#[test]
fn no_message_means_the_caster_forgot_it() {
    let mut fx = fx(true, 0);
    fx.mob("Bob");
    let (_, mut cara) = fx.player("Cara", 1, 0);
    let out = fx.cast("'ventriloquate' bob");
    assert_eq!(
        out,
        "You seem to have forgotten what you wanted it to say!\r\n"
    );
    assert_eq!(drain(&mut cara), "");
}

#[test]
fn the_caster_is_not_a_valid_target() {
    let mut fx = fx(true, 0);
    let out = fx.cast("'ventriloquate' me hello");
    assert!(
        out.contains("You cannot cast this spell upon yourself!"),
        "{out}"
    );
    let out = fx.cast("'ventriloquate'");
    assert!(
        out.contains("Cannot find the target of your spell!"),
        "{out}"
    );
}

#[test]
fn an_unknown_target_ends_the_cast() {
    let mut fx = fx(true, 0);
    let out = fx.cast("'ventriloquate' nobody hello");
    assert!(
        out.contains("Cannot find the target of your spell!"),
        "{out}"
    );
}

#[test]
fn the_cast_is_announced_to_nobody_but_equal_or_higher_staff() {
    let mut fx = fx(true, 2);
    fx.mob("Bob");
    let (_, mut cara) = fx.player("Cara", 1, 0);
    let (god, mut grx) = fx.player("Zed", 105, 0);
    fx.world.entity_mut(god).insert(Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: mud_db::enums::UserRole::Implementor,
        account_role: mud_db::enums::UserRole::Implementor,
        perms: vec![],
    });
    let out = fx.cast("'ventriloquate' bob hello");
    assert!(out.contains("You begin casting"), "{out}");
    assert_eq!(drain(&mut cara), "", "mortals hear no chanting");
    assert!(
        drain(&mut grx).contains("starts casting"),
        "a level-105 god hears it start"
    );
    for _ in 0..100 {
        crate::casting::casting_tick(&mut fx.world);
        if fx.world.get::<mud_world::Casting>(fx.caster).is_none() {
            break;
        }
    }
    let out = drain(&mut fx.rx);
    assert!(!out.contains("You complete"), "{out}");
    assert_eq!(drain(&mut cara), "Bob says, 'hello'\r\n");
    let god_out = drain(&mut grx);
    assert!(!god_out.contains("completes"), "{god_out}");
    assert!(!god_out.contains("utters"), "{god_out}");
}
