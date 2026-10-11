//! Message lines open with a capital, like legacy `act()`'s `CAP()`:
//! mob short descriptions ("a swamp troll") lead many echoes. Speech bodies
//! and non-message text are left alone. Test-only.

use bevy_ecs::prelude::Entity;
use mud_world::{
    AbilityCatalog, AbilityMessageSet, AppliedTo, EffectInstance, EffectSource, Follower, Health,
    Located, Mob, Named,
};

use super::blindness_tests::{DAMAGE, cast, caster_with_spells};
use super::gmcp_tests::{fixture, player};
use super::test_support::drain;
use super::{
    Viewer, broadcast_room_except_rendered, cap_sentence_start, dispatch, format_effect_duration,
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
    for viewer in [Viewer::Actor, Viewer::Target, Viewer::Bystander] {
        for reflexive in [false, true] {
            for t in ability_tokens {
                let out = render_ability_template(
                    &format!("x {{{t}}} y"),
                    "Ann",
                    "Bob",
                    reflexive,
                    viewer,
                );
                assert!(unknown_template_tokens(&out).is_empty(), "{t} -> {out}");
            }
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
    let out = render_ability_template(
        "{actor.bogus} waves {target.name}.",
        "Ann",
        "Bob",
        false,
        Viewer::Bystander,
    );
    assert_eq!(unknown_template_tokens(&out), vec!["{actor.bogus}"]);
    assert!(unknown_template_tokens("plain { text } and 1}{").is_empty());
}

/// Place a `summoned-*` effect on a conjured mob the way the summon spell
/// does (`AppliedTo(mob)`, the mob following `owner`), with the caster-facing
/// wear-off texts the content carries.
fn summoned_mob_with_effect(fx: &mut super::gmcp_tests::Fx, owner: Entity, room: Entity) -> Entity {
    fx.world.resource_mut::<AbilityCatalog>().messages.insert(
        56,
        AbilityMessageSet {
            wearoff_to_target: Some("Your summoned creature fades away.".into()),
            wearoff_to_room: Some("{actor.name}'s summoned creature fades away.".into()),
            ..AbilityMessageSet::default()
        },
    );
    let mob = fx
        .world
        .spawn((
            Mob,
            Named {
                name: "a fire elemental".into(),
            },
            Located(room),
            Health { hp: 50, max: 50 },
            Follower(owner),
        ))
        .id();
    fx.world.spawn((
        EffectInstance {
            kind: 1,
            name: "summoned-fire_elemental".into(),
            strength: 1,
            remaining_secs: 1,
            source: EffectSource::Spell,
            ability_id: Some(56),
        },
        AppliedTo(mob),
    ));
    mob
}

/// The wear-off lines belong to the caster, but the effect lives on the
/// mob: they must not render against it. The despawn line tells the room
/// and the summoner instead.
#[test]
fn summon_wearoff_skips_the_caster_lines_and_tells_the_summoner() {
    let mut fx = fixture();
    let (room, elsewhere) = (fx.a, fx.b);
    let (owner, mut orx) = player(&mut fx.world, elsewhere, "Strider");
    let (_w, mut wrx) = player(&mut fx.world, room, "Watcher");
    let mob = summoned_mob_with_effect(&mut fx, owner, room);
    let _ = drain(&mut orx);
    let _ = drain(&mut wrx);
    fx.world.insert_resource(crate::TickCount(0));
    crate::effects::effects_tick(&mut fx.world);
    let seen = drain(&mut wrx);
    assert!(
        seen.contains("A fire elemental fades back to where it was summoned from."),
        "{seen}"
    );
    assert!(!seen.contains("summoned creature"), "{seen}");
    assert!(!seen.contains('{'), "{seen}");
    let told = drain(&mut orx);
    assert!(
        told.contains("Your fire elemental fades back to where it was summoned from."),
        "{told}"
    );
    assert!(!told.contains("summoned creature"), "{told}");
    assert!(fx.world.get_entity(mob).is_err(), "mob despawned");
}

/// A summoner standing with the mob gets the one first-person line, not the
/// bystander line as well.
#[test]
fn summon_wearoff_in_the_same_room_is_one_line_for_the_summoner() {
    let mut fx = fixture();
    let room = fx.a;
    let (owner, mut orx) = player(&mut fx.world, room, "Strider");
    let (_w, mut wrx) = player(&mut fx.world, room, "Watcher");
    let _mob = summoned_mob_with_effect(&mut fx, owner, room);
    let _ = drain(&mut orx);
    let _ = drain(&mut wrx);
    fx.world.insert_resource(crate::TickCount(0));
    crate::effects::effects_tick(&mut fx.world);
    let told = drain(&mut orx);
    assert_eq!(
        told.matches("fades back to where it was summoned from")
            .count(),
        1,
        "{told}"
    );
    assert!(told.contains("Your fire elemental fades"), "{told}");
    let seen = drain(&mut wrx);
    assert!(seen.contains("A fire elemental fades back"), "{seen}");
}

/// A non-summon wear-off puts the effect on its bearer: they read it in the
/// second person, the room reads their name, and `{actor.pos}` renders per
/// viewer instead of leaking.
#[test]
fn wearoff_renders_per_viewer_including_the_possessive() {
    let mut fx = fixture();
    let room = fx.a;
    let (bearer, mut brx) = player(&mut fx.world, room, "Strider");
    let (_w, mut wrx) = player(&mut fx.world, room, "Watcher");
    fx.world.resource_mut::<AbilityCatalog>().messages.insert(
        57,
        AbilityMessageSet {
            wearoff_to_target: Some("{actor.pos} berserker fury fades.".into()),
            wearoff_to_room: Some("{actor.pos} berserker fury fades from {actor.him}.".into()),
            ..AbilityMessageSet::default()
        },
    );
    fx.world.spawn((
        EffectInstance {
            kind: 1,
            name: "berserk".into(),
            strength: 1,
            remaining_secs: 1,
            source: EffectSource::Spell,
            ability_id: Some(57),
        },
        AppliedTo(bearer),
    ));
    let _ = drain(&mut brx);
    let _ = drain(&mut wrx);
    fx.world.insert_resource(crate::TickCount(0));
    crate::effects::effects_tick(&mut fx.world);
    let own = drain(&mut brx);
    assert!(own.contains("Your berserker fury fades."), "{own}");
    let seen = drain(&mut wrx);
    assert!(
        seen.contains("Strider's berserker fury fades from them."),
        "{seen}"
    );
    assert!(!seen.contains('{') && !own.contains('{'));
}

#[test]
fn possessive_and_pronoun_tokens_render_per_viewer() {
    let r =
        |t: &str, reflexive, viewer| render_ability_template(t, "Ann", "Bob", reflexive, viewer);
    // The actor reads "your" (capitalised at sentence start), others the name.
    assert_eq!(
        r("{actor.pos} aura fades.", false, Viewer::Actor),
        "Your aura fades."
    );
    assert_eq!(
        r("{actor.pos} aura fades.", false, Viewer::Target),
        "Ann's aura fades."
    );
    assert_eq!(
        r("{actor.pos} aura fades.", false, Viewer::Bystander),
        "Ann's aura fades."
    );
    assert_eq!(r("1 {actor.pos} y", false, Viewer::Actor), "1 your y");
    // The target side: "your" for the victim, the name for everyone else.
    assert_eq!(
        r("{target.pos} aura fades.", false, Viewer::Target),
        "Your aura fades."
    );
    assert_eq!(
        r("{target.pos} aura fades.", false, Viewer::Actor),
        "Bob's aura fades."
    );
    assert_eq!(
        r("{target.pos} aura fades.", false, Viewer::Bystander),
        "Bob's aura fades."
    );
    // Self-target as the actor: both sides are "you".
    assert_eq!(
        r("{target.pos} aura fades.", true, Viewer::Actor),
        "Your aura fades."
    );
    // The other actor tokens follow the same you/name split.
    assert_eq!(r("{actor.name} nods.", false, Viewer::Actor), "You nods.");
    assert_eq!(
        r("{actor.name} nods.", false, Viewer::Bystander),
        "Ann nods."
    );
    assert_eq!(
        r("1 hit {actor.ref}", false, Viewer::Actor),
        "1 hit yourself"
    );
    assert_eq!(
        r("1 hit {actor.ref}", false, Viewer::Bystander),
        "1 hit themself"
    );
    assert_eq!(r("{target.name}!", false, Viewer::Target), "You!");
    assert_eq!(r("{target.name}!", false, Viewer::Bystander), "Bob!");
    assert_eq!(r("{target.name}!", true, Viewer::Actor), "Yourself!");
}

/// Observers who cannot see the actor get "Someone's" through the room
/// broadcast, since the possessive is "<Name>'s".
#[test]
fn bystander_possessive_becomes_someones_when_the_actor_is_unseen() {
    let mut fx = fixture();
    let room = fx.a;
    let (actor, _arx) = player(&mut fx.world, room, "Ann");
    let (blind, mut brx) = player(&mut fx.world, room, "Blind");
    fx.world.entity_mut(blind).insert(mud_world::Blinded);
    let line = render_ability_template(
        "{actor.pos} aura fades.",
        "Ann",
        "Ann",
        false,
        Viewer::Bystander,
    );
    let _ = drain(&mut brx);
    super::broadcast_room_anonymised(&mut fx.world, room, &[actor], &[(actor, "Ann")], &line);
    let out = drain(&mut brx);
    assert!(out.contains("Someone's aura fades."), "{out}");
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
