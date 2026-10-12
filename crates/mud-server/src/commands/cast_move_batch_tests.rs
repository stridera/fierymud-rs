//! Cast hints and refusals before the slot is spent (#103, #113, #63, #35,
//! #14), `<keyword> <direction>` door arguments (#114), and mounted
//! movement (#118). Test-only.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Direction, ExitState};
use mud_world::{
    AbilityCatalog, EffectCatalog, EffectDef, Exits, Health, KnownAbilities, Mob, MovementPoints,
    Named, Profile, RiddenBy, Room, SpellSlotData, SpellSlots, Stamina,
};

use super::dispatch;
use super::gmcp_tests::{Fx, exit, fixture, player};
use super::test_support::{Rx, ability_def, drain};
use crate::commands::{cmd_move, resolve_exit_arg};

const SPELL: i32 = 700;
const EFFECT: i32 = 701;
const CLASS: i32 = 7;

fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
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

fn effect(effect_type: &str, defaults: serde_json::Value) -> EffectDef {
    EffectDef {
        id: EFFECT,
        name: effect_type.to_string(),
        description: None,
        effect_type: effect_type.to_string(),
        tags: Vec::new(),
        presence_override: None,
        default_params: defaults,
        prevents_speaking: false,
        prevents_casting: false,
        prevents_movement: false,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    }
}

/// A caster who knows one spell, `name` (plain name `plain_name`), whose single
/// effect is `effect_type` with `params`. Circle 1 of class 7 has one slot.
fn spell_fx(
    name: &str,
    plain_name: &str,
    cast_rounds: i32,
    violent: bool,
    effect_type: &str,
    params: serde_json::Value,
) -> (Fx, Entity, Rx) {
    let mut fx = fixture();
    let mut catalog = AbilityCatalog::default();
    let mut def = ability_def(SPELL, name, AbilityKind::Spell);
    def.plain_name = plain_name.to_string();
    def.cast_time_rounds = cast_rounds;
    def.violent = violent;
    catalog.by_name.insert(name.to_ascii_lowercase(), def);
    catalog
        .effects_for
        .insert(SPELL, vec![(EFFECT, Some(params))]);
    fx.world.insert_resource(catalog);
    let mut effects = EffectCatalog::default();
    effects
        .by_id
        .insert(EFFECT, effect(effect_type, serde_json::json!({})));
    fx.world.insert_resource(effects);
    let mut slots = SpellSlotData::default();
    slots.ability_circle.insert((CLASS, SPELL), 1);
    slots.progression.insert((20, 1), 1);
    fx.world.insert_resource(slots);
    fx.world
        .insert_resource(mud_world::ClassSkillsData::default());
    let a = fx.a;
    let (p, rx) = player(&mut fx.world, a, "Caster");
    fx.world.entity_mut(p).insert((
        Health { hp: 50, max: 50 },
        KnownAbilities {
            entries: vec![(SPELL, 500, true)],
        },
    ));
    fx.world.get_mut::<Profile>(p).unwrap().class_id = Some(CLASS);
    (fx, p, rx)
}

fn slots_used(world: &World, p: Entity) -> i32 {
    world
        .get::<SpellSlots>(p)
        .map_or(0, |s| s.used_in_circle(1))
}

// -- #103: the cast hint names the spell, not its schema identifier ---------

#[test]
fn cast_hint_quotes_the_display_name_lowercased() {
    let (mut fx, p, mut rx) = spell_fx(
        "Ray of Enfeeblement",
        "RAY_OF_ENFEEB",
        1,
        true,
        "status",
        serde_json::json!({"flag": "weaken", "duration": 10}),
    );
    dispatch(&mut fx.world, p, "cast 'ray of enfeeblement'");
    let out = plain(&drain(&mut rx));
    assert!(
        out.contains(
            "Ray of Enfeeblement needs a target. Try: cast 'ray of enfeeblement' <target>."
        ),
        "{out}"
    );
    assert!(!out.contains("enfeeb'"), "schema name leaked: {out}");
}

// -- #113 / #63: upcast notice and the chant line ---------------------------

#[test]
fn upcast_notice_is_coloured_and_says_how_to_cancel() {
    let (mut fx, p, mut rx) = spell_fx(
        "Mend",
        "MEND",
        1,
        false,
        "heal",
        serde_json::json!({"amount": 4}),
    );
    fx.world
        .resource_mut::<SpellSlotData>()
        .progression
        .insert((20, 2), 1);
    let mut used = SpellSlots::default();
    used.reserve(1, 30);
    fx.world.entity_mut(p).insert(used);
    dispatch(&mut fx.world, p, "cast 'mend'");
    let raw = drain(&mut rx);
    let out = plain(&raw);
    assert!(
        out.contains("Upcasting a circle 1 spell into circle 2."),
        "{out}"
    );
    assert!(out.contains("(type abort to cancel)"), "{out}");
    let upcast_at = raw.find("Upcasting").expect("notice");
    assert!(raw[..upcast_at].contains('\x1b'), "no colour code: {raw:?}");
}

#[test]
fn quick_chant_adds_no_extra_line_to_the_cast_start() {
    let (mut fx, p, mut rx) = spell_fx(
        "Mend",
        "MEND",
        4,
        false,
        "heal",
        serde_json::json!({"amount": 4}),
    );
    let quick = 702;
    fx.world.resource_mut::<AbilityCatalog>().by_name.insert(
        "quick_chant".to_string(),
        ability_def(quick, "Quick Chant", AbilityKind::Skill),
    );
    fx.world
        .get_mut::<KnownAbilities>(p)
        .unwrap()
        .entries
        .push((quick, 1000, true));
    let full = crate::casting::handler_ticks(16);
    let mut quick_seen = false;
    for _ in 0..40 {
        fx.world.entity_mut(p).remove::<mud_world::Casting>();
        fx.world.entity_mut(p).remove::<SpellSlots>();
        drain(&mut rx);
        dispatch(&mut fx.world, p, "cast 'mend'");
        let out = plain(&drain(&mut rx));
        assert!(!out.contains("hastens"), "{out}");
        assert!(out.contains("You begin casting Mend"), "{out}");
        quick_seen |= fx
            .world
            .get::<mud_world::Casting>(p)
            .is_some_and(|c| c.ticks_total < full);
    }
    assert!(
        quick_seen,
        "quick chant never fired; the test proves nothing"
    );
}

// -- #35: walls ---------------------------------------------------------------

const WALL_PARAMS: &str = r#"{"type": "ice", "traversalRules": "block", "duration": 60}"#;

fn wall_fx(cast_rounds: i32) -> (Fx, Entity, Rx) {
    let (mut fx, p, rx) = spell_fx(
        "Wall of Ice",
        "WALL_OF_ICE",
        cast_rounds,
        false,
        "room",
        serde_json::from_str(WALL_PARAMS).unwrap(),
    );
    let (a, b) = (fx.a, fx.b);
    fx.world
        .get_mut::<Exits>(a)
        .unwrap()
        .0
        .insert(Direction::North, exit(b, ExitState::Open, false, &[]));
    fx.world
        .get_mut::<Exits>(b)
        .unwrap()
        .0
        .insert(Direction::South, exit(a, ExitState::Open, false, &[]));
    (fx, p, rx)
}

#[test]
fn wall_without_a_direction_is_refused_before_the_slot_is_spent() {
    let (mut fx, p, mut rx) = wall_fx(2);
    dispatch(&mut fx.world, p, "cast 'wall of ice'");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("Wall of Ice needs a direction"), "{out}");
    assert!(
        fx.world.get::<mud_world::Casting>(p).is_none(),
        "no wind-up started"
    );
    assert_eq!(slots_used(&fx.world, p), 0, "slot untouched");
}

#[test]
fn wall_toward_a_missing_exit_is_refused_before_the_slot_is_spent() {
    let (mut fx, p, mut rx) = wall_fx(2);
    dispatch(&mut fx.world, p, "cast 'wall of ice' west");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("There's no exit west here."), "{out}");
    assert!(fx.world.get::<mud_world::Casting>(p).is_none());
    assert_eq!(slots_used(&fx.world, p), 0);
}

#[test]
fn wall_with_a_direction_starts_the_wind_up() {
    let (mut fx, p, mut rx) = wall_fx(2);
    dispatch(&mut fx.world, p, "cast 'wall of ice' north");
    drain(&mut rx);
    assert!(fx.world.get::<mud_world::Casting>(p).is_some());
    assert_eq!(slots_used(&fx.world, p), 1);
}

#[test]
fn wall_blocks_both_sides_of_the_exit() {
    let (mut fx, p, mut rx) = wall_fx(0);
    dispatch(&mut fx.world, p, "cast 'wall of ice' north");
    let out = plain(&drain(&mut rx));
    let (a, b) = (fx.a, fx.b);
    let near = fx.world.get::<mud_world::RoomBlockedExits>(a);
    assert!(
        near.is_some_and(|w| w.by_direction.contains_key(&Direction::North)),
        "near side: {out}"
    );
    let far = fx.world.get::<mud_world::RoomBlockedExits>(b);
    assert!(
        far.is_some_and(|w| w.by_direction.contains_key(&Direction::South)),
        "far side: {out}"
    );
    // The far side's wall is backed by its own effect, so it expires on its own.
    let backed = fx
        .world
        .get::<mud_world::RoomBlockedExits>(b)
        .unwrap()
        .by_direction[&Direction::South]
        .backed_by;
    assert!(fx.world.get::<mud_world::EffectInstance>(backed).is_some());
}

#[test]
fn wall_without_a_return_exit_blocks_only_the_near_side() {
    let (mut fx, p, mut rx) = wall_fx(0);
    let b = fx.b;
    fx.world.get_mut::<Exits>(b).unwrap().0.clear();
    dispatch(&mut fx.world, p, "cast 'wall of ice' north");
    drain(&mut rx);
    assert!(fx.world.get::<mud_world::RoomBlockedExits>(fx.a).is_some());
    assert!(fx.world.get::<mud_world::RoomBlockedExits>(b).is_none());
}

// -- #35: spells that do not refresh -------------------------------------

fn stone_skin_fx(refresh: Option<bool>) -> (Fx, Entity, Rx) {
    let mut params =
        serde_json::json!({"flag": "resistance", "type": "earth", "amount": 30, "duration": 60});
    if let Some(r) = refresh {
        params["refresh"] = serde_json::json!(r);
    }
    let (mut fx, p, rx) = spell_fx("Stone Skin", "STONE_SKIN", 0, false, "status", params);
    // Room for a recast.
    fx.world
        .resource_mut::<SpellSlotData>()
        .progression
        .insert((20, 1), 2);
    (fx, p, rx)
}

fn earth(world: &World, p: Entity) -> Option<i32> {
    world
        .get::<mud_world::Resistances>(p)
        .and_then(|r| r.0.get(&mud_db::enums::ElementType::Earth).copied())
}

fn effect_count(world: &mut World, p: Entity) -> usize {
    let mut q = world.query::<(&mud_world::EffectInstance, &mud_world::AppliedTo)>();
    q.iter(world).filter(|(_, at)| at.0 == p).count()
}

#[test]
fn a_no_refresh_spell_fails_on_a_target_that_already_has_it() {
    let (mut fx, p, mut rx) = stone_skin_fx(Some(false));
    dispatch(&mut fx.world, p, "cast 'stone skin'");
    assert_eq!(earth(&fx.world, p), Some(30));
    drain(&mut rx);
    dispatch(&mut fx.world, p, "cast 'stone skin'");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("Nothing seems to happen."), "{out}");
    assert_eq!(earth(&fx.world, p), Some(30), "no second bump");
    assert_eq!(effect_count(&mut fx.world, p), 1, "no second instance");
}

#[test]
fn a_no_refresh_spell_still_lands_the_first_time() {
    let (mut fx, p, mut rx) = stone_skin_fx(Some(false));
    dispatch(&mut fx.world, p, "cast 'stone skin'");
    let out = plain(&drain(&mut rx));
    assert!(!out.contains("Nothing seems to happen."), "{out}");
    assert_eq!(effect_count(&mut fx.world, p), 1);
}

#[test]
fn a_refreshing_spell_recasts_in_place() {
    let (mut fx, p, mut rx) = stone_skin_fx(None);
    dispatch(&mut fx.world, p, "cast 'stone skin'");
    drain(&mut rx);
    dispatch(&mut fx.world, p, "cast 'stone skin'");
    let out = plain(&drain(&mut rx));
    assert!(!out.contains("Nothing seems to happen."), "{out}");
    assert_eq!(earth(&fx.world, p), Some(30));
    assert_eq!(effect_count(&mut fx.world, p), 1);
}

// -- #114: <keyword> <direction> ------------------------------------------

fn door_fx(keywords: &[&str]) -> (Fx, Entity, Rx) {
    let mut fx = fixture();
    let (a, b) = (fx.a, fx.b);
    fx.world.get_mut::<Exits>(a).unwrap().0.insert(
        Direction::North,
        exit(b, ExitState::Closed, false, keywords),
    );
    fx.world.get_mut::<Exits>(b).unwrap().0.insert(
        Direction::South,
        exit(a, ExitState::Closed, false, keywords),
    );
    let (p, rx) = player(&mut fx.world, a, "Opener");
    (fx, p, rx)
}

#[test]
fn door_arguments_resolve_by_direction_keyword_or_both() {
    let (fx, p, _rx) = door_fx(&["door"]);
    let w = &fx.world;
    assert_eq!(resolve_exit_arg(w, p, "north"), Some(Direction::North));
    assert_eq!(resolve_exit_arg(w, p, "door"), Some(Direction::North));
    assert_eq!(resolve_exit_arg(w, p, "door north"), Some(Direction::North));
    assert_eq!(resolve_exit_arg(w, p, "door n"), Some(Direction::North));
    assert_eq!(resolve_exit_arg(w, p, "DOOR North"), Some(Direction::North));
    assert_eq!(resolve_exit_arg(w, p, "door south"), None, "no exit south");
    assert_eq!(resolve_exit_arg(w, p, "gate north"), None, "wrong keyword");
    assert_eq!(resolve_exit_arg(w, p, "north door"), None);
    assert_eq!(resolve_exit_arg(w, p, ""), None);
}

#[test]
fn a_keywordless_exit_still_answers_to_door() {
    let (fx, p, _rx) = door_fx(&[]);
    assert_eq!(
        resolve_exit_arg(&fx.world, p, "door north"),
        Some(Direction::North)
    );
    assert_eq!(resolve_exit_arg(&fx.world, p, "gate north"), None);
}

#[test]
fn a_named_exit_is_found_by_its_keyword_and_direction() {
    let (fx, p, _rx) = door_fx(&["gate", "iron"]);
    let w = &fx.world;
    assert_eq!(resolve_exit_arg(w, p, "gate"), Some(Direction::North));
    assert_eq!(resolve_exit_arg(w, p, "gate north"), Some(Direction::North));
    assert_eq!(resolve_exit_arg(w, p, "iron n"), Some(Direction::North));
    assert_eq!(resolve_exit_arg(w, p, "door north"), None);
}

#[test]
fn keyword_plus_direction_beats_a_keyword_that_starts_with_the_direction_letter() {
    let mut fx = fixture();
    let (a, b) = (fx.a, fx.b);
    // North answers to "wooden door"; west is a keywordless door.
    fx.world.get_mut::<Exits>(a).unwrap().0.insert(
        Direction::North,
        exit(b, ExitState::Closed, false, &["wooden", "door"]),
    );
    fx.world
        .get_mut::<Exits>(a)
        .unwrap()
        .0
        .insert(Direction::West, exit(b, ExitState::Closed, false, &[]));
    let (p, _rx) = player(&mut fx.world, a, "Opener");
    let w = &fx.world;
    assert_eq!(resolve_exit_arg(w, p, "door w"), Some(Direction::West));
    assert_eq!(resolve_exit_arg(w, p, "door west"), Some(Direction::West));
    assert_eq!(resolve_exit_arg(w, p, "door n"), Some(Direction::North));
    assert_eq!(resolve_exit_arg(w, p, "wooden"), Some(Direction::North));
}

#[test]
fn a_keyword_shared_by_several_exits_resolves_in_legacy_direction_order() {
    let mut fx = fixture();
    let (a, b) = (fx.a, fx.b);
    for dir in [
        Direction::Down,
        Direction::Up,
        Direction::West,
        Direction::South,
        Direction::East,
        Direction::North,
    ] {
        fx.world
            .get_mut::<Exits>(a)
            .unwrap()
            .0
            .insert(dir, exit(b, ExitState::Closed, false, &["gate"]));
    }
    let (p, _rx) = player(&mut fx.world, a, "Opener");
    for _ in 0..20 {
        assert_eq!(
            resolve_exit_arg(&fx.world, p, "gate"),
            Some(Direction::North)
        );
    }
    fx.world
        .get_mut::<Exits>(a)
        .unwrap()
        .0
        .remove(&Direction::North);
    assert_eq!(
        resolve_exit_arg(&fx.world, p, "gate"),
        Some(Direction::East)
    );
    fx.world
        .get_mut::<Exits>(a)
        .unwrap()
        .0
        .remove(&Direction::East);
    fx.world
        .get_mut::<Exits>(a)
        .unwrap()
        .0
        .remove(&Direction::South);
    assert_eq!(
        resolve_exit_arg(&fx.world, p, "gate"),
        Some(Direction::West)
    );
}

#[test]
fn a_hidden_exit_cannot_be_probed_with_keyword_and_direction() {
    let mut fx = fixture();
    let (a, b) = (fx.a, fx.b);
    fx.world.get_mut::<Exits>(a).unwrap().0.insert(
        Direction::North,
        exit(b, ExitState::Closed, true, &["door"]),
    );
    let (p, _rx) = player(&mut fx.world, a, "Opener");
    assert_eq!(resolve_exit_arg(&fx.world, p, "door north"), None);
}

#[test]
fn open_and_close_take_keyword_and_direction() {
    let (mut fx, p, mut rx) = door_fx(&["door"]);
    dispatch(&mut fx.world, p, "open door north");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("You open"), "{out}");
    let state = |fx: &Fx| fx.world.get::<Exits>(fx.a).unwrap().0[&Direction::North].state;
    assert_eq!(state(&fx), ExitState::Open);
    dispatch(&mut fx.world, p, "close door n");
    drain(&mut rx);
    assert_eq!(state(&fx), ExitState::Closed);
    dispatch(&mut fx.world, p, "open north");
    drain(&mut rx);
    assert_eq!(state(&fx), ExitState::Open, "plain direction still works");
    dispatch(&mut fx.world, p, "close door");
    drain(&mut rx);
    assert_eq!(state(&fx), ExitState::Closed, "plain keyword still works");
}

// -- #118: mounted movement --------------------------------------------------

fn exit_to(to: Entity) -> mud_world::ExitData {
    exit(to, ExitState::Open, false, &[])
}

/// Rider "Tester" on "a stout mare" in room A (exit north to B), with Bob
/// watching in A and Cara in B. Returns (world, rider, mount, rider rx, bob rx,
/// cara rx).
fn mounted_fx(mount_points: i32) -> (World, Entity, Entity, Rx, Rx, Rx) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    let b = world.spawn((Room, Exits::default())).id();
    let a = world
        .spawn((Room, Exits(HashMap::from([(Direction::North, exit_to(b))]))))
        .id();
    let (rider, rrx) = super::test_support::player_in(&mut world, a);
    let (bob, brx) = super::test_support::player_in(&mut world, a);
    world.entity_mut(bob).insert(Named { name: "Bob".into() });
    let (cara, crx) = super::test_support::player_in(&mut world, b);
    world.entity_mut(cara).insert(Named {
        name: "Cara".into(),
    });
    let mount = world
        .spawn((
            Mob,
            Named {
                name: "a stout mare".into(),
            },
            mud_world::Located(a),
            MovementPoints {
                current: mount_points,
                max: 100,
            },
            RiddenBy(rider),
        ))
        .id();
    world.entity_mut(rider).insert((
        mud_world::Mounted(mount),
        Stamina {
            current: 50,
            max: 50,
        },
    ));
    (world, rider, mount, rrx, brx, crx)
}

#[test]
fn the_mount_pays_the_movement_not_the_rider() {
    let (mut world, rider, mount, _r, _b, _c) = mounted_fx(40);
    cmd_move(&mut world, rider, Direction::North);
    assert_eq!(
        world.get::<Stamina>(rider).unwrap().current,
        50,
        "rider pays nothing"
    );
    assert!(
        world.get::<MovementPoints>(mount).unwrap().current < 40,
        "mount paid"
    );
    let dest = world.get::<mud_world::Located>(rider).unwrap().0;
    assert_eq!(world.get::<mud_world::Located>(mount).unwrap().0, dest);
}

#[test]
fn an_exhausted_mount_will_not_move_and_the_rider_is_not_charged() {
    let (mut world, rider, mount, mut rrx, _b, _c) = mounted_fx(0);
    let start = world.get::<mud_world::Located>(rider).unwrap().0;
    cmd_move(&mut world, rider, Direction::North);
    let out = plain(&drain(&mut rrx));
    assert!(out.contains("Your mount is too exhausted."), "{out}");
    assert_eq!(world.get::<mud_world::Located>(rider).unwrap().0, start);
    assert_eq!(world.get::<mud_world::Located>(mount).unwrap().0, start);
    assert_eq!(world.get::<Stamina>(rider).unwrap().current, 50);
}

#[test]
fn a_rider_with_no_stamina_left_still_rides() {
    let (mut world, rider, _mount, mut rrx, _b, _c) = mounted_fx(40);
    world.get_mut::<Stamina>(rider).unwrap().current = 0;
    let start = world.get::<mud_world::Located>(rider).unwrap().0;
    cmd_move(&mut world, rider, Direction::North);
    assert_ne!(world.get::<mud_world::Located>(rider).unwrap().0, start);
    assert!(!plain(&drain(&mut rrx)).contains("too exhausted"));
}

#[test]
fn riding_reads_as_one_rider_on_a_mount() {
    let (mut world, rider, _mount, mut rrx, mut brx, mut crx) = mounted_fx(40);
    cmd_move(&mut world, rider, Direction::North);
    let mine = plain(&drain(&mut rrx));
    assert!(mine.contains("You ride north on a stout mare."), "{mine}");
    let left = plain(&drain(&mut brx));
    assert!(
        left.contains("Tester rides north on a stout mare.\r\n"),
        "{left:?}"
    );
    assert!(!left.contains("leaves"), "{left:?}");
    let came = plain(&drain(&mut crx));
    assert!(
        came.contains("Tester arrives from the south, riding a stout mare.\r\n"),
        "{came:?}"
    );
}

#[test]
fn an_unmounted_walker_keeps_the_plain_wording() {
    let (mut world, rider, mount, _r, mut brx, mut crx) = mounted_fx(40);
    world.entity_mut(rider).remove::<mud_world::Mounted>();
    world.entity_mut(mount).remove::<RiddenBy>();
    cmd_move(&mut world, rider, Direction::North);
    assert!(plain(&drain(&mut brx)).contains("Tester leaves north.\r\n"));
    assert!(plain(&drain(&mut crx)).contains("Tester arrives from the south.\r\n"));
    assert!(
        world.get::<Stamina>(rider).unwrap().current < 50,
        "a walker pays for its own steps"
    );
}

#[test]
fn mount_movement_points_regenerate() {
    let (mut world, _rider, mount, _r, _b, _c) = mounted_fx(10);
    world.insert_resource(crate::TickCount(10));
    crate::regen::regen_tick(&mut world);
    assert_eq!(world.get::<MovementPoints>(mount).unwrap().current, 11);
}

// -- playtest round 4: recast feedback, targets refused at the start -----------

#[test]
fn recasting_a_refreshing_spell_on_yourself_says_it_was_refreshed() {
    let (mut fx, p, mut rx) = stone_skin_fx(None);
    dispatch(&mut fx.world, p, "cast 'stone skin'");
    let first = plain(&drain(&mut rx));
    assert!(!first.contains("refresh"), "{first}");
    dispatch(&mut fx.world, p, "cast 'stone skin'");
    let out = plain(&drain(&mut rx));
    assert!(
        out.contains("You refresh stone skin for yourself."),
        "a recast is never silent: {out}"
    );
    assert!(
        !out.contains("You cast"),
        "refresh replaces the success line: {out}"
    );
    assert_eq!(effect_count(&mut fx.world, p), 1, "refreshed in place");
}

#[test]
fn recasting_on_someone_else_tells_both_of_them() {
    let (mut fx, p, mut rx) = stone_skin_fx(None);
    let b = fx.b;
    let (other, mut orx) = player(&mut fx.world, b, "Other");
    fx.world.entity_mut(p).insert(mud_world::Located(b));
    dispatch(&mut fx.world, p, "cast 'stone skin' other");
    drain(&mut rx);
    drain(&mut orx);
    dispatch(&mut fx.world, p, "cast 'stone skin' other");
    let caster = plain(&drain(&mut rx));
    assert!(
        caster.contains("You refresh stone skin for Other."),
        "{caster}"
    );
    let target = plain(&drain(&mut orx));
    assert!(
        target.contains("Caster refreshes stone skin for you."),
        "{target}"
    );
    assert_eq!(effect_count(&mut fx.world, other), 1);
}

#[test]
fn enhance_ability_recast_swaps_the_enhancement_and_keeps_its_success_line() {
    // Legacy `mag_affect`: Enhance Ability removes the old effect and applies
    // the new one with the normal messages, not the generic refresh line.
    let (mut fx, p, mut rx) = spell_fx(
        "Enhance Ability",
        "ENHANCE_ABILITY",
        0,
        false,
        "modify",
        serde_json::json!({"amount": 2, "duration": 60, "target": "cha"}),
    );
    fx.world
        .resource_mut::<SpellSlotData>()
        .progression
        .insert((20, 1), 2);
    fx.world.entity_mut(p).insert(mud_world::CoreStats {
        strength: 50,
        dexterity: 50,
        constitution: 50,
        intelligence: 50,
        wisdom: 50,
        charisma: 50,
    });
    dispatch(&mut fx.world, p, "cast 'enhance ability' dex");
    drain(&mut rx);
    dispatch(&mut fx.world, p, "cast 'enhance ability' dex");
    let out = plain(&drain(&mut rx));
    assert!(!out.contains("You refresh"), "{out}");
    assert!(
        out.contains("Enhance Ability"),
        "still announces the cast: {out}"
    );
    assert_eq!(
        effect_count(&mut fx.world, p),
        1,
        "one enhancement, not two"
    );
    assert_eq!(
        fx.world.get::<mud_world::CoreStats>(p).unwrap().dexterity,
        52
    );
}

#[test]
fn a_named_target_that_is_not_here_is_refused_at_the_start_of_a_wind_up() {
    let (mut fx, p, mut rx) = spell_fx(
        "Identify",
        "IDENTIFY",
        5,
        false,
        "inspect",
        serde_json::json!({}),
    );
    dispatch(&mut fx.world, p, "cast 'identify' pigxyz");
    let out = plain(&drain(&mut rx));
    assert!(
        out.contains("Cannot find the target of your spell!"),
        "{out}"
    );
    assert!(
        fx.world.get::<mud_world::Casting>(p).is_none(),
        "no wind-up"
    );
    assert_eq!(slots_used(&fx.world, p), 0, "slot untouched");
}

#[test]
fn any_non_violent_spell_refuses_a_missing_target_before_the_wind_up() {
    // Not special to Identify: a buff aimed at a name nobody answers to.
    let (mut fx, p, mut rx) = spell_fx(
        "Bless",
        "BLESS",
        3,
        false,
        "status",
        serde_json::json!({"flag": "bless", "duration": 60}),
    );
    dispatch(&mut fx.world, p, "cast 'bless' nobody");
    let out = plain(&drain(&mut rx));
    assert!(
        out.contains("Cannot find the target of your spell!"),
        "{out}"
    );
    assert!(fx.world.get::<mud_world::Casting>(p).is_none());
    assert_eq!(slots_used(&fx.world, p), 0);
    assert_eq!(effect_count(&mut fx.world, p), 0, "not cast on yourself");
}

#[test]
fn an_instant_spell_with_a_missing_target_keeps_its_slot_too() {
    let (mut fx, p, mut rx) = stone_skin_fx(None);
    dispatch(&mut fx.world, p, "cast 'stone skin' nobody");
    let out = plain(&drain(&mut rx));
    assert!(
        out.contains("Cannot find the target of your spell!"),
        "{out}"
    );
    assert_eq!(slots_used(&fx.world, p), 0);
}

#[test]
fn a_spell_whose_word_is_content_still_falls_back_to_the_caster() {
    // `cast 'locate object' sword`: the word is a search keyword, not a target.
    let (mut fx, p, mut rx) = spell_fx(
        "Locate Object",
        "LOCATE_OBJECT",
        0,
        false,
        "reveal",
        serde_json::json!({"type": "object", "depth": "world"}),
    );
    dispatch(&mut fx.world, p, "cast 'locate object' sword");
    let out = plain(&drain(&mut rx));
    assert!(!out.contains("Cannot find the target"), "{out}");
}

fn dimension_door_fx() -> (Fx, Entity, Entity, Rx) {
    let (mut fx, p, rx) = spell_fx(
        "Dimension Door",
        "DIMENSION_DOOR",
        4,
        false,
        "teleport",
        serde_json::json!({"destination": "target", "range": "zone"}),
    );
    // A room in another zone, with a friend in it.
    let far = fx
        .world
        .spawn((
            Room,
            mud_world::WorldKey { zone: 551, id: 1 },
            Named {
                name: "Far Room".into(),
            },
            mud_world::Description("far".into()),
            Exits::default(),
        ))
        .id();
    let (friend, _frx) = player(&mut fx.world, far, "Friend");
    (fx, p, friend, rx)
}

#[test]
fn a_cross_zone_dimension_door_is_refused_before_the_wind_up() {
    let (mut fx, p, _friend, mut rx) = dimension_door_fx();
    dispatch(&mut fx.world, p, "cast 'dimension door' friend");
    let out = plain(&drain(&mut rx));
    assert!(
        out.contains("Your magics are not strong enough for such a great journey."),
        "{out}"
    );
    assert!(
        fx.world.get::<mud_world::Casting>(p).is_none(),
        "no wind-up"
    );
    assert_eq!(slots_used(&fx.world, p), 0, "slot untouched");
}

#[test]
fn a_dimension_door_to_a_mob_is_refused_before_the_wind_up() {
    let (mut fx, p, _friend, mut rx) = dimension_door_fx();
    let a = fx.a;
    fx.world.spawn((
        Mob,
        Named {
            name: "a goblin".into(),
        },
        mud_world::Keywords(vec!["goblin".into()]),
        mud_world::Located(a),
    ));
    dispatch(&mut fx.world, p, "cast 'dimension door' goblin");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("You failed."), "{out}");
    assert!(fx.world.get::<mud_world::Casting>(p).is_none());
    assert_eq!(slots_used(&fx.world, p), 0);
}
