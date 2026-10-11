//! Message lines open with a capital, like legacy `act()`'s `CAP()`:
//! mob short descriptions ("a swamp troll") lead many echoes. Speech bodies
//! and non-message text are left alone. Test-only.

use mud_world::{
    AbilityCatalog, AbilityMessageSet, AppliedTo, EffectInstance, EffectSource, Health, Mob, Named,
};

use super::blindness_tests::{DAMAGE, cast, caster_with_spells};
use super::gmcp_tests::{fixture, player};
use super::test_support::drain;
use super::{
    broadcast_room_except_rendered, cap_sentence_start, dispatch, format_effect_duration,
    render_ability_template, substitute, unknown_template_tokens,
};

#[test]
fn cap_skips_tags_ansi_and_whitespace_and_is_unicode_safe() {
    assert_eq!(cap_sentence_start("a rat arrives."), "A rat arrives.");
    assert_eq!(
        cap_sentence_start("<red>a rat</> arrives."),
        "<red>A rat</> arrives."
    );
    assert_eq!(
        cap_sentence_start("<b:cyan><dim>a rat</></> arrives."),
        "<b:cyan><dim>A rat</></> arrives."
    );
    assert_eq!(
        cap_sentence_start("\x1b[1;31ma rat\x1b[0m"),
        "\x1b[1;31mA rat\x1b[0m"
    );
    assert_eq!(cap_sentence_start("\r\n  a rat"), "\r\n  A rat");
    assert_eq!(cap_sentence_start("élan vital"), "Élan vital");
    // Not a letter first: left alone, the rest is never touched.
    assert_eq!(cap_sentence_start("3 rats arrive"), "3 rats arrive");
    assert_eq!(cap_sentence_start("'a rat' says"), "'a rat' says");
    assert_eq!(cap_sentence_start(""), "");
}

#[test]
fn room_broadcast_capitalises_per_recipient() {
    let mut fx = fixture();
    let room = fx.a;
    let (_w, mut wrx) = player(&mut fx.world, room, "Watcher");
    let _ = drain(&mut wrx);
    broadcast_room_except_rendered(&mut fx.world, room, &[], "a rat arrives from the west.\r\n");
    assert_eq!(drain(&mut wrx), "A rat arrives from the west.\r\n");
    broadcast_room_except_rendered(
        &mut fx.world,
        room,
        &[],
        "<yellow>a rat</> arrives from the west.\r\n",
    );
    let colored = drain(&mut wrx);
    assert!(
        colored.contains("A rat") && !colored.contains("a rat"),
        "{colored:?}"
    );
}

#[test]
fn spell_room_message_capitalises_a_mob_short_description() {
    let (mut fx, p, mut rx) = caster_with_spells(vec![(
        1,
        "Supernova",
        vec![(
            DAMAGE,
            Some(serde_json::json!({"type": "magic", "amount": "7"})),
        )],
    )]);
    fx.world.resource_mut::<AbilityCatalog>().messages.insert(
        1,
        AbilityMessageSet {
            success_to_caster: Some("{target.name} is brutally burnt.".into()),
            success_to_room: Some(
                "{target.name} is brutally burnt and nearly torn to pieces, but somehow managed to survive!"
                    .into(),
            ),
            ..AbilityMessageSet::default()
        },
    );
    let room = fx.a;
    fx.world.spawn((
        Mob,
        Named {
            name: "a swamp troll".into(),
        },
        mud_world::Located(room),
        mud_world::CombatStats::default(),
        Health { hp: 500, max: 500 },
    ));
    let (_w, mut wrx) = player(&mut fx.world, room, "Watcher");
    let _ = drain(&mut rx);
    let _ = drain(&mut wrx);
    cast(&mut fx, p, "cast supernova troll");
    let mine = drain(&mut rx);
    let seen = drain(&mut wrx);
    assert!(mine.contains("A swamp troll is brutally burnt."), "{mine}");
    assert!(
        seen.contains("A swamp troll is brutally burnt and nearly torn to pieces, but somehow managed to survive!"),
        "{seen}"
    );
}

#[test]
fn a_mover_named_like_a_mob_leaves_with_a_capital() {
    let mut fx = fixture();
    let room = fx.a;
    let (rat, _rrx) = player(&mut fx.world, room, "a rat");
    let (_w, mut wrx) = player(&mut fx.world, room, "Watcher");
    fx.world
        .entity_mut(room)
        .insert(mud_world::Exits(std::collections::HashMap::from([(
            mud_db::enums::Direction::East,
            super::blindness_tests::exit_to(fx.b),
        )])));
    let _ = drain(&mut wrx);
    dispatch(&mut fx.world, rat, "east");
    let seen = drain(&mut wrx);
    assert!(seen.contains("A rat leaves east."), "{seen}");
}

#[test]
fn say_body_is_left_alone() {
    let mut fx = fixture();
    let room = fx.a;
    let (speaker, _srx) = player(&mut fx.world, room, "Bob");
    let (_w, mut wrx) = player(&mut fx.world, room, "Watcher");
    let _ = drain(&mut wrx);
    dispatch(&mut fx.world, speaker, "say hello there, friend");
    let seen = drain(&mut wrx);
    assert!(seen.contains("Bob says"), "{seen}");
    assert!(
        seen.contains("hello there, friend"),
        "body stays as typed: {seen}"
    );
    assert!(!seen.contains("Hello there"), "{seen}");
}

/// Every placeholder the seeded content uses (`AbilityMessages` and Social
/// columns in fierydev, 2026-10-11) renders away; nothing leaks as `{...}`.
#[test]
fn no_known_template_token_survives_rendering() {
    let ability_tokens = [
        "actor.name",
        "target.name",
        "actor.pos",
        "target.pos",
        "actor.him",
        "target.him",
        "actor.he",
        "actor.she",
        "actor.it",
        "actor.her",
        "actor.his",
        "target.he",
        "target.she",
        "target.it",
        "target.her",
        "target.his",
        "actor.ref",
        "target.ref",
    ];
    for reflexive in [false, true] {
        for t in ability_tokens {
            let out = render_ability_template(&format!("x {{{t}}} y"), "Ann", "Bob", reflexive);
            assert!(unknown_template_tokens(&out).is_empty(), "{t} -> {out}");
        }
    }
    for who in ["actor", "target"] {
        for p in [
            "objective",
            "reflexive",
            "Subjective",
            "subjective",
            "possessive",
        ] {
            let out = substitute(&format!("x {{{who}.pronoun.{p}}} y"), "Ann", Some("Bob"));
            assert!(
                unknown_template_tokens(&out).is_empty(),
                "{who}.pronoun.{p} -> {out}"
            );
        }
    }
}

#[test]
fn unknown_tokens_are_detected() {
    let out = render_ability_template("{actor.bogus} waves {target.name}.", "Ann", "Bob", false);
    assert_eq!(unknown_template_tokens(&out), vec!["{actor.bogus}"]);
    assert!(unknown_template_tokens("plain { text } and 1}{").is_empty());
}

#[test]
fn summon_wearoff_names_the_owner_and_is_capitalised() {
    let mut fx = fixture();
    let room = fx.a;
    let (owner, mut orx) = player(&mut fx.world, room, "Strider");
    let (_w, mut wrx) = player(&mut fx.world, room, "Watcher");
    fx.world.resource_mut::<AbilityCatalog>().messages.insert(
        56,
        AbilityMessageSet {
            wearoff_to_target: Some("Your summoned creature fades away.".into()),
            wearoff_to_room: Some("{actor.name}'s summoned creature fades away.".into()),
            ..AbilityMessageSet::default()
        },
    );
    fx.world.spawn((
        EffectInstance {
            kind: 1,
            name: "summon-test".into(),
            strength: 1,
            remaining_secs: 1,
            source: EffectSource::Spell,
            ability_id: Some(56),
        },
        AppliedTo(owner),
    ));
    let _ = drain(&mut orx);
    let _ = drain(&mut wrx);
    fx.world.insert_resource(crate::TickCount(0));
    crate::effects::effects_tick(&mut fx.world);
    assert!(drain(&mut orx).contains("Your summoned creature fades away."));
    let seen = drain(&mut wrx);
    assert!(
        seen.contains("Strider's summoned creature fades away."),
        "{seen}"
    );
    assert!(!seen.contains('{'), "{seen}");
}

#[test]
fn permanent_and_long_durations_read_naturally() {
    assert_eq!(format_effect_duration(-1), "permanent");
    assert_eq!(format_effect_duration(i32::MIN), "permanent");
    assert_eq!(format_effect_duration(0), "0s");
    assert_eq!(format_effect_duration(45), "45s");
    assert_eq!(format_effect_duration(60), "1m");
    assert_eq!(format_effect_duration(185), "3m 5s");
    assert_eq!(format_effect_duration(2 * 3600 + 15 * 60), "2h 15m");
    assert_eq!(format_effect_duration(2 * 3600), "2h");
    assert_eq!(format_effect_duration(26 * 3600 + 30 * 60), "1d 2h");
    assert!(!format_effect_duration(-1).contains("-1"));
}
