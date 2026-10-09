//! Hiding and sneaking (legacy `GET_HIDDENNESS` / `INVIS_OK`): who sees a
//! hidden character, what reveals one, `hide`, `search`, and moving.
//! Test-only.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Direction, ExitState, PlayerFlag, UserRole};
use mud_world::{
    Account, CombatStats, CoreStats, ExitData, Exits, Follower, Health, Hiddenness, KnownAbilities,
    Located, Mob, Named, Perception, PlayerFlags, Room, Sneaking,
};
use serde_json::Value;

use super::gmcp_tests::{Fx, drain_bytes, fixture, frames, of, player};
use super::test_support::{Rx, ability_def, drain, player_in};
use super::{
    can_see_player, cmd_move, dispatch, engage_combat, find_actor_in_room,
    info::{hide_with_roll, search_with_roll},
};
use crate::hiding;

const HIDE_ABILITY: i32 = 183;
const SNEAK_ABILITY: i32 = 319;

/// Seeker (watcher) and Lurker (hider) sharing room A.
fn hiding_pair(hid: i32) -> (Fx, Entity, Rx, Entity, Rx) {
    let mut fx = fixture();
    let a = fx.a;
    let (seeker, srx) = player(&mut fx.world, a, "Seeker");
    let (lurker, lrx) = player(&mut fx.world, a, "Lurker");
    fx.world.entity_mut(lurker).insert(Hiddenness(hid));
    (fx, seeker, srx, lurker, lrx)
}

fn look_frames(fx: &mut Fx, p: Entity, rx: &mut Rx) -> (String, Vec<(String, String)>) {
    drain_bytes(rx);
    dispatch(&mut fx.world, p, "look");
    let bytes = drain_bytes(rx);
    (String::from_utf8_lossy(&bytes).into_owned(), frames(&bytes))
}

fn room_players(fr: &[(String, String)]) -> Vec<String> {
    let all = of(fr, "Room.Players");
    let v: Value = serde_json::from_str(all.last().expect("Room.Players frame")).unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_hidden_character_is_absent_from_look_gmcp_and_name_resolution() {
    let (mut fx, seeker, mut srx, lurker, _lrx) = hiding_pair(300);
    let a = fx.a;

    let (text, fr) = look_frames(&mut fx, seeker, &mut srx);
    assert!(!text.contains("Lurker"), "{text}");
    assert!(room_players(&fr).is_empty(), "{fr:?}");
    assert_eq!(find_actor_in_room(&mut fx.world, "lurker", a, seeker), None);
    assert!(!can_see_player(&fx.world, seeker, lurker));

    // The hider still sees itself and the watcher.
    assert!(can_see_player(&fx.world, lurker, lurker));
    assert!(can_see_player(&fx.world, lurker, seeker));

    // Once revealed, every channel shows the same character.
    hiding::reveal(&mut fx.world, lurker);
    let (text, fr) = look_frames(&mut fx, seeker, &mut srx);
    assert!(text.contains("Lurker"), "{text}");
    assert_eq!(room_players(&fr), vec!["Lurker".to_string()]);
    assert_eq!(
        find_actor_in_room(&mut fx.world, "lurker", a, seeker),
        Some(lurker)
    );
}

#[test]
fn perception_group_staff_and_holy_light_see_a_hidden_character() {
    let (mut fx, seeker, _srx, lurker, _lrx) = hiding_pair(300);
    assert!(!can_see_player(&fx.world, seeker, lurker));

    // Perception below the hiding: still hidden. At it: seen.
    fx.world.entity_mut(seeker).insert(Perception(299));
    assert!(!can_see_player(&fx.world, seeker, lurker));
    fx.world.entity_mut(seeker).insert(Perception(300));
    assert!(can_see_player(&fx.world, seeker, lurker));
    fx.world.entity_mut(seeker).insert(Perception(0));

    // A group mate (same leader) sees through the hiding.
    let a = fx.a;
    let (mate, _mrx) = player(&mut fx.world, a, "Mate");
    assert!(!can_see_player(&fx.world, mate, lurker));
    fx.world.entity_mut(mate).insert(Follower(lurker));
    assert!(can_see_player(&fx.world, mate, lurker));
    assert!(can_see_player(&fx.world, lurker, mate));

    // Staff rank alone is no bypass (legacy `IMM_CAN_SEE`); HOLY_LIGHT is.
    let (god, _grx) = player(&mut fx.world, a, "God");
    fx.world.entity_mut(god).insert(Account {
        user_id: String::new(),
        character_id: "g".into(),
        role: UserRole::Immortal,
        account_role: UserRole::Immortal,
        perms: vec![],
    });
    fx.world.entity_mut(god).insert(Perception(0));
    assert!(!can_see_player(&fx.world, god, lurker));
    fx.world
        .entity_mut(god)
        .insert(PlayerFlags(vec![PlayerFlag::HolyLight]));
    assert!(can_see_player(&fx.world, god, lurker));
    fx.world
        .entity_mut(seeker)
        .insert(PlayerFlags(vec![PlayerFlag::HolyLight]));
    assert!(can_see_player(&fx.world, seeker, lurker));
}

#[test]
fn a_players_base_perception_grows_with_level_and_wits() {
    let (mut fx, seeker, _srx, _lurker, _lrx) = hiding_pair(1);
    fx.world.entity_mut(seeker).insert(CoreStats {
        intelligence: 60,
        wisdom: 60,
        ..CoreStats::default()
    });
    // Level 20 * ((60 + 60) / 30) = 80, plus the gear bonus.
    assert_eq!(hiding::perception_of(&fx.world, seeker), 80);
    fx.world.entity_mut(seeker).insert(Perception(15));
    assert_eq!(hiding::perception_of(&fx.world, seeker), 95);
    fx.world.entity_mut(seeker).insert(Perception(5000));
    assert_eq!(hiding::perception_of(&fx.world, seeker), 1000);
    // A mob has no base: just its prototype value.
    let mob = fx
        .world
        .spawn((
            Mob,
            Named {
                name: "a wolf".into(),
            },
            Perception(40),
        ))
        .id();
    assert_eq!(hiding::perception_of(&fx.world, mob), 40);
}

fn advance(world: &mut World, ticks: u64) {
    world.resource_mut::<crate::TickCount>().0 += ticks;
}

fn searcher_world(perception: i32, hid: i32) -> (Fx, Entity, Rx, Entity, Rx) {
    let (mut fx, seeker, srx, lurker, lrx) = hiding_pair(hid);
    fx.world.insert_resource(crate::TickCount(100));
    fx.world.entity_mut(seeker).insert(Perception(perception));
    (fx, seeker, srx, lurker, lrx)
}

#[test]
fn search_cuts_hiding_by_perception_and_finds_the_hider_with_a_forced_roll() {
    // Perception 200 against hiding 350: the cut is 100..=200, so the best
    // case leaves 150 (found) and the worst 250 (not found).
    let (mut fx, seeker, mut srx, lurker, mut lrx) = searcher_world(200, 350);
    let a = fx.a;
    fx.world.entity_mut(lurker).insert(Perception(50));
    search_with_roll(&mut fx.world, seeker, "", &mut |hi| hi);
    let out = drain(&mut srx);
    assert!(out.contains("You find Lurker lurking here!"), "{out}");
    assert!(!hiding::is_hidden(&fx.world, lurker));
    assert!(can_see_player(&fx.world, seeker, lurker));
    assert_eq!(
        find_actor_in_room(&mut fx.world, "lurker", a, seeker),
        Some(lurker)
    );
    // The lurker's 50 + roll(200) beats the searcher's 200: it noticed.
    let told = drain(&mut lrx);
    assert!(told.contains("You think Seeker has spotted you!"), "{told}");
}

#[test]
fn a_failed_search_still_wears_the_hiding_down() {
    let (mut fx, seeker, mut srx, lurker, _lrx) = searcher_world(200, 500);
    // The weakest cut (roll 0) is 100, leaving 400 > 200.
    search_with_roll(&mut fx.world, seeker, "", &mut |_| 0);
    let out = drain(&mut srx);
    assert!(!out.contains("lurking here"), "{out}");
    assert!(out.contains("You find nothing of interest."), "{out}");
    assert_eq!(hiding::hiddenness(&fx.world, lurker), 400);
    // Searching again (after the lag) cuts further and finds it.
    advance(&mut fx.world, 100);
    search_with_roll(&mut fx.world, seeker, "", &mut |hi| hi);
    let out = drain(&mut srx);
    assert!(out.contains("You find Lurker lurking here!"), "{out}");
    assert!(!hiding::is_hidden(&fx.world, lurker));
}

#[test]
fn search_points_out_a_hider_the_searcher_already_out_perceives() {
    let (mut fx, seeker, mut srx, lurker, _lrx) = searcher_world(400, 300);
    search_with_roll(&mut fx.world, seeker, "", &mut |_| 0);
    let out = drain(&mut srx);
    assert!(out.contains("You point out Lurker lurking here!"), "{out}");
    assert!(!hiding::is_hidden(&fx.world, lurker));
}

#[test]
fn search_skips_group_mates_and_staff_find_everyone() {
    let (mut fx, seeker, mut srx, lurker, _lrx) = searcher_world(10, 900);
    let a = fx.a;
    let (other, _orx) = player(&mut fx.world, a, "Other");
    fx.world.entity_mut(other).insert(Hiddenness(800));
    fx.world.entity_mut(lurker).insert(Follower(seeker));
    search_with_roll(&mut fx.world, seeker, "", &mut |_| 0);
    let out = drain(&mut srx);
    assert!(
        !out.contains("lurking here"),
        "group mate left alone: {out}"
    );
    // Group mates keep their hiding value.
    assert_eq!(hiding::hiddenness(&fx.world, lurker), 900);

    fx.world.entity_mut(seeker).insert(Account {
        user_id: String::new(),
        character_id: "g".into(),
        role: UserRole::Immortal,
        account_role: UserRole::Immortal,
        perms: vec![],
    });
    search_with_roll(&mut fx.world, seeker, "", &mut |_| 0);
    let out = drain(&mut srx);
    assert!(out.contains("You find Other lurking here!"), "{out}");
    assert!(!hiding::is_hidden(&fx.world, other));
}

#[test]
fn commands_outside_the_hide_safe_list_reveal_and_the_safe_ones_do_not() {
    for safe in ["look", "inventory", "exits", "hide", "tell nobody hi"] {
        let (mut fx, _seeker, _srx, lurker, _lrx) = hiding_pair(300);
        dispatch(&mut fx.world, lurker, safe);
        assert!(
            hiding::is_hidden(&fx.world, lurker),
            "`{safe}` must not reveal"
        );
    }
    for noisy in [
        "say hello",
        "get all",
        "search",
        "drop sword",
        "emote waves",
    ] {
        let (mut fx, _seeker, _srx, lurker, _lrx) = hiding_pair(300);
        fx.world.insert_resource(crate::TickCount(0));
        dispatch(&mut fx.world, lurker, noisy);
        assert!(
            !hiding::is_hidden(&fx.world, lurker),
            "`{noisy}` must reveal"
        );
    }
}

#[test]
fn saying_something_reveals_the_speaker_to_the_room() {
    let (mut fx, seeker, mut srx, lurker, _lrx) = hiding_pair(300);
    dispatch(&mut fx.world, lurker, "say psst");
    let out = drain(&mut srx);
    assert!(out.contains("Lurker says"), "{out}");
    assert!(can_see_player(&fx.world, seeker, lurker));
}

#[test]
fn attacking_reveals_the_attacker() {
    let (mut fx, _seeker, _srx, lurker, _lrx) = hiding_pair(300);
    let a = fx.a;
    fx.world
        .entity_mut(lurker)
        .insert((CombatStats::default(), Health { hp: 100, max: 100 }));
    fx.world.spawn((
        Mob,
        Named {
            name: "a rat".into(),
        },
        Located(a),
        CombatStats::default(),
        Health { hp: 100, max: 100 },
    ));
    dispatch(&mut fx.world, lurker, "kill rat");
    assert!(!hiding::is_hidden(&fx.world, lurker));
}

/// Backstab (hide-safe) reads the hiding for its bonus, then reveals.
#[test]
fn backstab_keeps_the_hidden_bonus_and_then_reveals() {
    let damage_with = |hid: i32| -> (i32, bool) {
        let (mut fx, _seeker, _srx, lurker, _lrx) = hiding_pair(hid);
        let a = fx.a;
        fx.world.insert_resource(crate::TickCount(0));
        let victim = fx
            .world
            .spawn((
                Mob,
                Named {
                    name: "a rat".into(),
                },
                mud_world::Keywords(vec!["rat".into()]),
                Located(a),
                CombatStats::default(),
                Health {
                    hp: 1000,
                    max: 1000,
                },
            ))
            .id();
        fx.world
            .entity_mut(lurker)
            .insert((CombatStats::default(), Health { hp: 100, max: 100 }));
        let mut abilities = mud_world::AbilityCatalog::default();
        let mut def = ability_def(77, "Backstab", AbilityKind::Skill);
        def.violent = true;
        def.cast_time_rounds = 0;
        abilities.by_name.insert("backstab".to_string(), def);
        abilities.effects_for.insert(
            77,
            vec![(
                9,
                Some(serde_json::json!({
                    "type": "pierce",
                    "amount": "10",
                    "bonusIfHidden": "40 * hidden",
                })),
            )],
        );
        fx.world.insert_resource(abilities);
        let mut effects = mud_world::EffectCatalog::default();
        effects.by_id.insert(
            9,
            mud_world::EffectDef {
                id: 9,
                name: "damage".into(),
                description: None,
                effect_type: "damage".into(),
                tags: vec![],
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
        fx.world.entity_mut(lurker).insert(KnownAbilities {
            entries: vec![(77, 1000, true)],
        });
        // `backstab` is on the hide-safe list, so the dispatcher leaves the
        // hiding for the ability itself to read.
        assert!(hiding::is_hide_safe("backstab", "backstab", false));
        super::invoke_ability_with(
            &mut fx.world,
            lurker,
            "backstab rat",
            AbilityKind::Skill,
            "use",
            false,
            true,
            true,
            None,
        );
        let hp = fx.world.get::<Health>(victim).unwrap().hp;
        (1000 - hp, hiding::is_hidden(&fx.world, lurker))
    };
    let (hidden_dmg, still_hidden) = damage_with(300);
    let (open_dmg, _) = damage_with(0);
    assert!(!still_hidden, "the attack reveals");
    assert!(
        hidden_dmg >= open_dmg + 30,
        "hidden backstab {hidden_dmg} vs open {open_dmg}"
    );
}

fn bare_world() -> World {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    world.insert_resource(crate::TickCount(100));
    world
}

fn account() -> Account {
    Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    }
}

/// Mover "Tester" in A with a hiding of `hid`, a low-perception watcher
/// "Bob" and a high-perception watcher "Hawk" in A, "Cara" in B.
#[allow(clippy::type_complexity)]
fn walkers(hid: i32, sneaking: bool) -> (World, Entity, Rx, Rx, Rx, Rx) {
    let mut world = bare_world();
    let a = world.spawn((Room, Exits::default())).id();
    let b = world.spawn((Room, Exits::default())).id();
    world.entity_mut(a).insert(Exits(HashMap::from([(
        Direction::North,
        ExitData {
            to: Some(b),
            state: ExitState::Open,
            key: None,
            description: None,
            keywords: vec![],
            is_hidden: false,
            is_pickproof: false,
            is_bashable: false,
            hit_points: None,
        },
    )])));
    let (mover, mrx) = player_in(&mut world, a);
    world.entity_mut(mover).insert((account(), Hiddenness(hid)));
    if sneaking {
        world.entity_mut(mover).insert(Sneaking);
    }
    let (bob, brx) = player_in(&mut world, a);
    world
        .entity_mut(bob)
        .insert((Named { name: "Bob".into() }, account()));
    let (hawk, hrx) = player_in(&mut world, a);
    world.entity_mut(hawk).insert((
        Named {
            name: "Hawk".into(),
        },
        account(),
        Perception(900),
    ));
    let (cara, crx) = player_in(&mut world, b);
    world.entity_mut(cara).insert((
        Named {
            name: "Cara".into(),
        },
        account(),
    ));
    let _ = (bob, hawk, cara);
    (world, mover, mrx, brx, hrx, crx)
}

#[test]
fn sneaking_movement_is_silent_to_low_perception_observers() {
    let (mut world, mover, _mrx, mut bob, mut hawk, mut cara) = walkers(500, true);
    cmd_move(&mut world, mover, Direction::North);
    // Bob (perception 0) and Cara (the arrival room) hear nothing.
    assert_eq!(drain(&mut bob), "");
    assert_eq!(drain(&mut cara), "");
    // Hawk's perception clears the hiding, so it sees the departure.
    assert!(
        drain(&mut hawk).contains("Tester leaves north."),
        "high perception notices"
    );
}

#[test]
fn an_unhidden_walker_is_announced_to_everyone() {
    let (mut world, mover, _mrx, mut bob, _hawk, mut cara) = walkers(0, false);
    cmd_move(&mut world, mover, Direction::North);
    assert!(drain(&mut bob).contains("Tester leaves north."));
    assert!(drain(&mut cara).contains("Tester arrives from the south."));
}

#[test]
fn a_sneaker_loses_two_to_five_hiding_per_move() {
    let mut world = bare_world();
    let room = world.spawn((Room, Exits::default())).id();
    let (mover, _rx) = player_in(&mut world, room);
    world.entity_mut(mover).insert(Sneaking);
    for step in 1..=20 {
        hiding::set_hiddenness(&mut world, mover, 500);
        let left = hiding::decay_on_move(&mut world, mover, &mut |lo, hi| {
            assert_eq!((lo, hi), (2, 5));
            lo + step % 4
        });
        assert_eq!(left, 500 - (2 + step % 4), "step {step}");
    }
}

#[test]
fn a_walker_without_sneak_wears_down_by_half_its_level_when_the_roll_beats_its_skill() {
    let mut world = bare_world();
    let room = world.spawn((Room, Exits::default())).id();
    let (mover, _rx) = player_in(&mut world, room);
    world.entity_mut(mover).insert(mud_world::Profile {
        level: 30,
        class_id: None,
        race: "Human".into(),
        experience: 0,
        gender: "neutral".into(),
    });
    hiding::set_hiddenness(&mut world, mover, 100);
    // No sneak skill, DEX 0: keeps hiding only on a roll of 1..=(-99 + 15)
    // (never), so a roll of 1 already wears it down.
    let left = hiding::decay_on_move(&mut world, mover, &mut |lo, hi| {
        assert_eq!((lo, hi), (1, 101));
        1
    });
    assert_eq!(left, 100 - 15);
    // A great sneaker (skill 100, DEX 80: 100 + 13 + 15 = 128) keeps it.
    let mut abilities = mud_world::AbilityCatalog::default();
    abilities.by_name.insert(
        "sneak".into(),
        ability_def(SNEAK_ABILITY, "Sneak", AbilityKind::Skill),
    );
    world.insert_resource(abilities);
    world.entity_mut(mover).insert((
        KnownAbilities {
            entries: vec![(SNEAK_ABILITY, 1000, true)],
        },
        CoreStats {
            dexterity: 80,
            ..CoreStats::default()
        },
    ));
    let left = hiding::decay_on_move(&mut world, mover, &mut |_, _| 101);
    assert_eq!(left, 85);
}

#[test]
fn a_walk_that_wears_the_hiding_to_nothing_says_so() {
    let (mut world, mover, mut mrx, _bob, _hawk, _cara) = walkers(1, true);
    // A sneaker with 1 left loses at least 2: out of hiding at the end.
    cmd_move(&mut world, mover, Direction::North);
    assert!(!hiding::is_hidden(&world, mover));
    assert!(drain(&mut mrx).contains("Your footsteps give you away."));
}

#[test]
fn a_hidden_mob_announces_no_aggro_but_a_plain_one_does() {
    for (hidden, sneaking, announced) in [
        (false, false, true),
        (true, false, false),
        (false, true, false),
    ] {
        let mut fx = fixture();
        let a = fx.a;
        let (victim, mut vrx) = player(&mut fx.world, a, "Victim");
        fx.world
            .entity_mut(victim)
            .insert((CombatStats::default(), Health { hp: 100, max: 100 }));
        let mob = fx
            .world
            .spawn((
                Mob,
                Named {
                    name: "a lurker".into(),
                },
                Located(a),
                CombatStats::default(),
                Health { hp: 100, max: 100 },
            ))
            .id();
        if hidden {
            fx.world.entity_mut(mob).insert(Hiddenness(400));
        }
        if sneaking {
            fx.world.entity_mut(mob).insert(Sneaking);
        }
        engage_combat(&mut fx.world, mob, victim, a);
        let out = drain(&mut vrx);
        assert_eq!(
            out.contains("sees you and attacks!"),
            announced,
            "hidden={hidden} sneaking={sneaking}: {out}"
        );
        // The fight itself still starts.
        assert!(fx.world.get::<mud_world::Fighting>(mob).is_some());
    }
}

/// `hide`: a catalog with the Hide ability, and a hider who knows it.
fn hide_world(skill_raw: i32) -> (World, Entity, Rx) {
    let mut world = bare_world();
    let room = world.spawn((Room, Exits::default())).id();
    let (hider, rx) = player_in(&mut world, room);
    let mut abilities = mud_world::AbilityCatalog::default();
    abilities.by_name.insert(
        "hide".into(),
        ability_def(HIDE_ABILITY, "Hide", AbilityKind::Skill),
    );
    world.insert_resource(abilities);
    world.entity_mut(hider).insert((
        account(),
        KnownAbilities {
            entries: vec![(HIDE_ABILITY, skill_raw, true)],
        },
        CoreStats {
            dexterity: 80,
            intelligence: 60,
            ..CoreStats::default()
        },
    ));
    (world, hider, rx)
}

#[test]
fn hide_rolls_hiddenness_from_skill_dex_and_int_and_lags_the_hider() {
    let (mut world, hider, mut rx) = hide_world(1000);
    let mut bounds = None;
    hide_with_roll(&mut world, hider, &mut |lo, hi| {
        bounds = Some((lo, hi));
        hi
    });
    // Skill 100, DEX 80, INT 60: random(545, 750) + DEX bonus 13.
    assert_eq!(bounds, Some((545, 750)));
    assert_eq!(hiding::hiddenness(&world, hider), 763);
    assert!(drain(&mut rx).contains("You attempt to hide yourself."));
    // Hiding again right away is refused until the lag passes ...
    hide_with_roll(&mut world, hider, &mut |lo, _| lo);
    assert!(drain(&mut rx).contains("still recovering"));
    assert_eq!(hiding::hiddenness(&world, hider), 763);
    // ... then it is a better hiding spot, and the new roll replaces the old.
    advance(&mut world, 40);
    hide_with_roll(&mut world, hider, &mut |lo, _| lo);
    assert!(drain(&mut rx).contains("You try to find a better hiding spot."));
    assert_eq!(hiding::hiddenness(&world, hider), 558);
}

#[test]
fn hide_takes_its_constants_from_the_ability_row() {
    let (mut world, hider, _rx) = hide_world(1000);
    world
        .resource_mut::<mud_world::AbilityCatalog>()
        .effects_for
        .insert(
            HIDE_ABILITY,
            vec![(
                4,
                Some(serde_json::json!({
                    "flag": "hidden",
                    "hide": {"divisor": 80, "waitTicks": 99},
                })),
            )],
        );
    let p = hiding::hide_params(&world);
    assert_eq!(p.wait_ticks, 99);
    assert!((p.divisor - 80.0).abs() < f64::EPSILON);
    assert_eq!(
        p.thief_wait_ticks, 20,
        "unlisted keys keep the legacy value"
    );
    let mut bounds = None;
    hide_with_roll(&mut world, hider, &mut |lo, hi| {
        bounds = Some((lo, hi));
        lo
    });
    // upper = 100 * (3*80 + 60) / 80 = 375; lower unchanged at 545 -> swapped.
    assert_eq!(bounds, Some((375, 545)));
    advance(&mut world, 98);
    hide_with_roll(&mut world, hider, &mut |lo, _| lo);
    // Still within waitTicks 99.
    assert_eq!(hiding::hiddenness(&world, hider), 375 + 13);
}

#[test]
fn only_those_who_know_hide_can_use_it_and_not_while_mounted() {
    let (mut world, hider, mut rx) = hide_world(0);
    hide_with_roll(&mut world, hider, &mut |lo, _| lo);
    assert!(drain(&mut rx).contains("leave that art to the rogues"));
    assert!(!hiding::is_hidden(&world, hider));

    let (mut world, hider, mut rx) = hide_world(1000);
    let mount = world.spawn_empty().id();
    world.entity_mut(hider).insert(mud_world::Mounted(mount));
    hide_with_roll(&mut world, hider, &mut |lo, _| lo);
    assert!(drain(&mut rx).contains("While mounted?"));
    assert!(!hiding::is_hidden(&world, hider));
}

#[test]
fn a_halfling_hiding_in_a_group_multiplies_its_dex_bonus() {
    let (mut world, hider, _rx) = hide_world(1000);
    world.entity_mut(hider).insert(mud_world::Profile {
        level: 65,
        class_id: None,
        race: "Halfling".into(),
        experience: 0,
        gender: "neutral".into(),
    });
    let room = world.get::<Located>(hider).unwrap().0;
    hide_with_roll(&mut world, hider, &mut |lo, _| lo);
    // Alone: bonus 13 -> 545 + 13.
    assert_eq!(hiding::hiddenness(&world, hider), 558);
    let (friend, _frx) = player_in(&mut world, room);
    world
        .entity_mut(friend)
        .insert((account(), Follower(hider)));
    advance(&mut world, 40);
    hide_with_roll(&mut world, hider, &mut |lo, _| lo);
    // Grouped: bonus 13 * (65 / 30 + 1) = 39.
    assert_eq!(hiding::hiddenness(&world, hider), 545 + 39);
}

#[test]
fn visible_drops_the_hiding() {
    let (mut fx, _seeker, _srx, lurker, mut lrx) = hiding_pair(300);
    dispatch(&mut fx.world, lurker, "visible");
    assert!(drain(&mut lrx).contains("You stop hiding."));
    assert!(!hiding::is_hidden(&fx.world, lurker));
    dispatch(&mut fx.world, lurker, "visible");
    assert!(drain(&mut lrx).contains("already visible"));
}

#[test]
fn pointing_out_a_hider_the_pointer_can_see_reveals_it() {
    let (mut fx, seeker, mut srx, lurker, mut lrx) = hiding_pair(100);
    fx.world.entity_mut(seeker).insert(Perception(500));
    dispatch(&mut fx.world, seeker, "point lurker");
    assert!(drain(&mut srx).contains("You point out Lurker's hiding place!"));
    assert!(drain(&mut lrx).contains("points out your hiding place!"));
    assert!(!hiding::is_hidden(&fx.world, lurker));
}

#[test]
fn hiddenness_from_gear_clamps_to_the_legacy_range() {
    let mut world = bare_world();
    let room = world.spawn((Room, Exits::default())).id();
    let (p, _rx) = player_in(&mut world, room);
    assert!(super::apply_modify_delta(&mut world, p, "hiddenness", 40));
    assert_eq!(hiding::hiddenness(&world, p), 40);
    assert!(super::apply_modify_delta(&mut world, p, "hiddenness", 5000));
    assert_eq!(hiding::hiddenness(&world, p), 1000);
    super::reverse_modify_delta(&mut world, p, "hiddenness", 5000);
    super::reverse_modify_delta(&mut world, p, "hiddenness", 40);
    assert_eq!(hiding::hiddenness(&world, p), 0);
    assert!(
        world.get::<Hiddenness>(p).is_none(),
        "zero drops the component"
    );
}

#[test]
fn a_mob_spawns_with_its_prototype_hiding_and_perception() {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    let room = world.spawn((Room, Exits::default())).id();
    let mut proto = super::test_support::mob_proto(30, 1, mud_db::enums::MobProfession::Trainer);
    proto.concealment = 250;
    proto.perception = 120;
    let mob = mud_world::mob_spawn::spawn_mob_from_proto(&mut world, &proto, room, None);
    assert_eq!(hiding::hiddenness(&world, mob), 250);
    assert_eq!(hiding::perception_of(&world, mob), 120);
    // An ordinary mob carries neither component.
    let plain = super::test_support::mob_proto(30, 2, mud_db::enums::MobProfession::Trainer);
    let mob = mud_world::mob_spawn::spawn_mob_from_proto(&mut world, &plain, room, None);
    assert!(world.get::<Hiddenness>(mob).is_none());
    assert!(world.get::<Perception>(mob).is_none());
}

/// Issue #96: legacy `score` is `CMD_ANY` (which includes `CMD_HIDE`), so a
/// hider can read their sheet without giving themself away.
#[test]
fn score_keeps_a_hider_hidden_like_legacy() {
    let (mut fx, _seeker, _srx, lurker, mut lrx) = hiding_pair(300);
    fx.world.init_resource::<mud_world::LevelTable>();
    fx.world.init_resource::<mud_world::AchievementCatalog>();
    for typed in ["score", "sc"] {
        drain(&mut lrx);
        dispatch(&mut fx.world, lurker, typed);
        assert!(drain(&mut lrx).contains("Stealth: hidden"), "{typed}");
        assert!(
            hiding::is_hidden(&fx.world, lurker),
            "`{typed}` must not reveal"
        );
    }
}

/// Issue #96, the reading that matters: a class without the Hide skill (the
/// reporter's Cryomancer) must not be able to hide at all, and `score` then
/// shows no hidden stealth.
#[test]
fn a_character_without_the_hide_skill_cannot_hide_and_score_says_so() {
    let (mut fx, _seeker, _srx, lurker, mut lrx) = hiding_pair(0);
    fx.world.init_resource::<mud_world::LevelTable>();
    fx.world.init_resource::<mud_world::AchievementCatalog>();
    drain(&mut lrx);
    dispatch(&mut fx.world, lurker, "hide");
    let out = drain(&mut lrx);
    assert!(out.contains("leave that art to the rogues"), "{out}");
    assert!(!hiding::is_hidden(&fx.world, lurker));
    dispatch(&mut fx.world, lurker, "sc");
    assert!(!drain(&mut lrx).contains("Stealth: hidden"));
}

// ---- mob auto-hide (legacy mobact.cpp:181), the hide wait state, and
// ---- sensing a hidden mover's departure (act.movement.cpp:115)

const THIEF: i32 = 4;
const MAGE: i32 = 5;
const NOVICE: i32 = 6;

/// A catalog where THIEF and MAGE (but not NOVICE) learn `hide` at level 1.
fn mob_hide_world() -> (World, Entity) {
    let mut world = bare_world();
    let room = world.spawn((Room, Exits::default())).id();
    let mut abilities = mud_world::AbilityCatalog::default();
    abilities.by_name.insert(
        "hide".into(),
        ability_def(HIDE_ABILITY, "Hide", AbilityKind::Skill),
    );
    world.insert_resource(abilities);
    let mut skills = mud_world::ClassSkillsData::default();
    for class in [THIEF, MAGE] {
        skills.min_level.insert((class, HIDE_ABILITY), 1);
    }
    world.insert_resource(skills);
    let mut classes = mud_world::ClassCatalog::default();
    for (id, name) in [(THIEF, "Thief"), (MAGE, "Mage"), (NOVICE, "Novice")] {
        classes.by_id.insert(
            id,
            mud_world::ClassDef {
                id,
                name: name.into(),
                plain_name: name.into(),
                is_subclass: false,
                parent_class_id: None,
                description: None,
                hit_dice: "1d8".into(),
                primary_stat: None,
                hp_per_level: 0,
                exp_gain_factor: 1.0,
                alignment_bias: 0,
                resistances: HashMap::new(),
                campcraft_bonus: false,
            },
        );
    }
    world.insert_resource(classes);
    world.insert_resource(mud_world::MobPrototypes::default());
    (world, room)
}

/// A prototype-backed mob of `class` and `level` in `room`.
fn class_mob(world: &mut World, room: Entity, id: i32, class: i32, level: i32) -> Entity {
    let mut proto = super::test_support::mob_proto(30, id, mud_db::enums::MobProfession::Trainer);
    proto.class_id = Some(class);
    proto.level = level;
    world
        .resource_mut::<mud_world::MobPrototypes>()
        .by_key
        .insert((30, id), proto);
    world
        .spawn((
            Mob,
            Named {
                name: "a cutpurse".into(),
            },
            Located(room),
            mud_world::WorldKey { zone: 30, id },
            CoreStats {
                dexterity: 80,
                intelligence: 60,
                ..CoreStats::default()
            },
        ))
        .id()
}

#[test]
fn a_mob_that_knows_hide_hides_on_the_mob_pulse() {
    let (mut world, room) = mob_hide_world();
    let thief = class_mob(&mut world, room, 1, THIEF, 20);
    let novice = class_mob(&mut world, room, 2, NOVICE, 20);
    // Off the pulse nothing happens, on it only the mob with the skill hides.
    world.insert_resource(crate::TickCount(101));
    crate::wander::mob_hide_tick(&mut world);
    assert!(!hiding::is_hidden(&world, thief));
    world.insert_resource(crate::TickCount(100));
    crate::wander::mob_hide_tick(&mut world);
    // Skill 26 (the legacy mob roll's mean at level 20), DEX 80 / INT 60:
    // random(.., 195) + DEX bonus 13, so a roll can floor at 0; try a few.
    for _ in 0..50 {
        if hiding::is_hidden(&world, thief) {
            break;
        }
        advance(&mut world, 100);
        crate::wander::mob_hide_tick(&mut world);
    }
    assert!(hiding::is_hidden(&world, thief), "the thief hides");
    assert!(
        !hiding::is_hidden(&world, novice),
        "no hide skill, no hiding"
    );
}

#[test]
fn mob_auto_hide_skips_the_mobs_legacy_skips() {
    let (mut world, room) = mob_hide_world();
    let fighting = class_mob(&mut world, room, 1, THIEF, 20);
    let target = class_mob(&mut world, room, 2, NOVICE, 20);
    world
        .entity_mut(fighting)
        .insert(mud_world::Fighting(target));
    let no_ai = class_mob(&mut world, room, 3, THIEF, 20);
    world.entity_mut(no_ai).insert(mud_world::MobBehaviors(vec![
        mud_db::enums::MobBehavior::NoClassAi,
    ]));
    let god = class_mob(&mut world, room, 4, THIEF, 100);
    let asleep = class_mob(&mut world, room, 5, THIEF, 20);
    world
        .entity_mut(asleep)
        .insert(mud_world::Posture(mud_world::PostureKind::Sleeping));
    let elsewhere = world.spawn(Room).id();
    let owner = world.spawn((mud_world::Player, Located(elsewhere))).id();
    let pet = class_mob(&mut world, room, 6, THIEF, 20);
    world.entity_mut(pet).insert(Follower(owner));
    world.insert_resource(crate::TickCount(100));
    for _ in 0..60 {
        crate::wander::mob_hide_tick(&mut world);
        advance(&mut world, 100);
    }
    for (mob, why) in [
        (fighting, "fighting"),
        (no_ai, "NO_CLASS_AI"),
        (god, "immortal level"),
        (asleep, "asleep"),
        (pet, "charmed, master elsewhere"),
    ] {
        assert!(!hiding::is_hidden(&world, mob), "{why} mob hid");
    }
}

#[test]
fn mob_skill_and_class_come_from_the_prototype() {
    let (mut world, room) = mob_hide_world();
    let thief = class_mob(&mut world, room, 1, THIEF, 20);
    let novice = class_mob(&mut world, room, 2, NOVICE, 20);
    assert_eq!(hiding::skill_pct(&world, thief, "hide"), 26);
    assert_eq!(hiding::skill_pct(&world, novice, "hide"), 0);
    assert_eq!(
        hiding::class_plain_name(&world, thief).as_deref(),
        Some("thief")
    );
}

#[test]
fn a_thief_mob_is_lagged_half_as_long_after_hiding() {
    let (mut world, room) = mob_hide_world();
    let thief = class_mob(&mut world, room, 1, THIEF, 20);
    let mage = class_mob(&mut world, room, 2, MAGE, 20);
    for mob in [thief, mage] {
        hide_with_roll(&mut world, mob, &mut |_, hi| hi);
        assert!(hiding::is_hidden(&world, mob));
        hiding::set_hiddenness(&mut world, mob, 0);
    }
    // 2 s on: the thief's 20-tick lag is over, the mage's 40 is not.
    advance(&mut world, 20);
    for mob in [thief, mage] {
        hide_with_roll(&mut world, mob, &mut |_, hi| hi);
    }
    assert!(
        hiding::is_hidden(&world, thief),
        "thief free after 20 ticks"
    );
    assert!(!hiding::is_hidden(&world, mage), "mage still lagged");
    advance(&mut world, 20);
    hide_with_roll(&mut world, mage, &mut |_, hi| hi);
    assert!(hiding::is_hidden(&world, mage), "mage free after 40 ticks");
}

#[test]
fn a_hide_wait_state_binds_a_thief_player_for_half_a_round() {
    let (mut world, hider, mut rx) = hide_world(1000);
    let mut classes = mud_world::ClassCatalog::default();
    classes.by_id.insert(
        THIEF,
        mud_world::ClassDef {
            id: THIEF,
            name: "Thief".into(),
            plain_name: "Thief".into(),
            is_subclass: false,
            parent_class_id: None,
            description: None,
            hit_dice: "1d8".into(),
            primary_stat: None,
            hp_per_level: 0,
            exp_gain_factor: 1.0,
            alignment_bias: 0,
            resistances: HashMap::new(),
            campcraft_bonus: false,
        },
    );
    world.insert_resource(classes);
    world.entity_mut(hider).insert(mud_world::Profile {
        level: 20,
        class_id: Some(THIEF),
        race: "human".into(),
        experience: 0,
        gender: "male".into(),
    });
    hide_with_roll(&mut world, hider, &mut |lo, _| lo);
    let _ = drain(&mut rx);
    advance(&mut world, 19);
    hide_with_roll(&mut world, hider, &mut |lo, _| lo);
    assert!(drain(&mut rx).contains("still recovering"), "lagged at 19");
    advance(&mut world, 1);
    hide_with_roll(&mut world, hider, &mut |lo, _| lo);
    assert!(drain(&mut rx).contains("better hiding spot"), "free at 20");
}

fn named_actor(world: &mut World, name: &str) -> Entity {
    let mut q = world.query::<(Entity, &Named)>();
    q.iter(world)
        .find(|(_, n)| n.name == name)
        .map(|(e, _)| e)
        .expect("named actor")
}

/// `walkers` with Bob (perception 0) sensing life.
fn sensing_walkers(hid: i32) -> (World, Entity, Rx, Rx, Rx, Rx) {
    let (mut world, mover, mrx, brx, hrx, crx) = walkers(hid, true);
    let bob = named_actor(&mut world, "Bob");
    world.entity_mut(bob).insert(mud_world::SenseLife);
    (world, mover, mrx, brx, hrx, crx)
}

const FELT: &str = "You feel that a living creature has departed.";

#[test]
fn a_sensing_observer_feels_a_sneaker_it_missed_depart() {
    let (mut world, mover, _m, mut bob, mut hawk, mut cara) = sensing_walkers(500);
    super::senses::force_sense_roll(Some(49));
    cmd_move(&mut world, mover, Direction::North);
    super::senses::force_sense_roll(None);
    // Bob missed the departure but senses it; Hawk simply saw it.
    let got = drain(&mut bob);
    assert!(got.contains(FELT) && !got.contains("leaves"), "{got:?}");
    let got = drain(&mut hawk);
    assert!(got.contains("Tester leaves north.") && !got.contains(FELT));
    assert_eq!(drain(&mut cara), "");
}

#[test]
fn sensing_a_departure_is_a_fifty_percent_roll_that_fighting_dulls() {
    let (mut world, mover, _m, mut bob, ..) = sensing_walkers(500);
    super::senses::force_sense_roll(Some(50));
    cmd_move(&mut world, mover, Direction::North);
    assert!(!drain(&mut bob).contains(FELT), "50 is not below 50");

    // Busy with a fight: 67% of 50 is 33.
    let (mut world, mover, _m, mut bob, ..) = sensing_walkers(500);
    let b = named_actor(&mut world, "Bob");
    let foe = world
        .spawn((Mob, Located(world.get::<Located>(b).unwrap().0)))
        .id();
    world.entity_mut(b).insert(mud_world::Fighting(foe));
    super::senses::force_sense_roll(Some(34));
    cmd_move(&mut world, mover, Direction::North);
    assert!(!drain(&mut bob).contains(FELT), "34 misses while fighting");
    let (mut world, mover, _m, mut bob, ..) = sensing_walkers(500);
    let b = named_actor(&mut world, "Bob");
    let foe = world
        .spawn((Mob, Located(world.get::<Located>(b).unwrap().0)))
        .id();
    world.entity_mut(b).insert(mud_world::Fighting(foe));
    super::senses::force_sense_roll(Some(32));
    cmd_move(&mut world, mover, Direction::North);
    super::senses::force_sense_roll(None);
    assert!(drain(&mut bob).contains(FELT), "32 lands while fighting");
}

#[test]
fn nothing_is_felt_without_the_sense_or_from_the_lifeless_or_the_unhidden() {
    // No SenseLife: silence, as before.
    let (mut world, mover, _m, mut bob, ..) = walkers(500, true);
    super::senses::force_sense_roll(Some(1));
    cmd_move(&mut world, mover, Direction::North);
    assert_eq!(drain(&mut bob), "");
    // An undead mover carries no life to feel.
    let (mut world, mover, _m, mut bob, ..) = sensing_walkers(500);
    world
        .entity_mut(mover)
        .insert(mud_world::LifeForceTag(mud_db::enums::LifeForce::Undead));
    cmd_move(&mut world, mover, Direction::North);
    assert_eq!(drain(&mut bob), "");
    // An unhidden mover is seen, not sensed.
    let (mut world, mover, _m, mut bob, ..) = sensing_walkers(0);
    world.entity_mut(mover).remove::<Sneaking>();
    cmd_move(&mut world, mover, Direction::North);
    super::senses::force_sense_roll(None);
    let got = drain(&mut bob);
    assert!(
        got.contains("Tester leaves north.") && !got.contains(FELT),
        "{got:?}"
    );
}

#[test]
fn a_hidden_wandering_mob_is_felt_leaving_too() {
    let (mut world, a, b) = {
        let mut world = bare_world();
        let a = world.spawn((Room, Exits::default())).id();
        let b = world.spawn((Room, Exits::default())).id();
        (world, a, b)
    };
    world.entity_mut(a).insert(Exits(HashMap::from([(
        Direction::North,
        ExitData {
            to: Some(b),
            state: ExitState::Open,
            key: None,
            description: None,
            keywords: vec![],
            is_hidden: false,
            is_pickproof: false,
            is_bashable: false,
            hit_points: None,
        },
    )])));
    let (watcher, mut rx) = player_in(&mut world, a);
    world
        .entity_mut(watcher)
        .insert((account(), mud_world::SenseLife));
    let mob = world
        .spawn((
            Mob,
            Named {
                name: "a cutpurse".into(),
            },
            Located(a),
            Hiddenness(900),
        ))
        .id();
    super::senses::force_sense_roll(Some(1));
    world.insert_resource(crate::TickCount(300));
    for _ in 0..200 {
        crate::wander::wander_tick(&mut world);
        if world.get::<Located>(mob).is_some_and(|l| l.0 == b) {
            break;
        }
    }
    super::senses::force_sense_roll(None);
    assert_eq!(world.get::<Located>(mob).map(|l| l.0), Some(b));
    let got = drain(&mut rx);
    assert!(got.contains(FELT) && !got.contains("leaves"), "{got:?}");
}

/// A hidden item named `name` lying in `at` (room or container).
fn hidden_item(fx: &mut Fx, name: &str, at: Entity, hid: i32) -> Entity {
    fx.world
        .spawn((
            mud_world::Item,
            Named { name: name.into() },
            mud_world::Keywords(vec![name.rsplit(' ').next().unwrap_or("x").to_string()]),
            Located(at),
            Hiddenness(hid),
        ))
        .id()
}

#[test]
fn search_cuts_an_objects_hiding_and_reveals_it_with_a_forced_roll() {
    // Perception 200 against hiding 350: the cut is 100..=200, so the best
    // case leaves 150 (found) and the worst 250 (not found).
    let (mut fx, seeker, mut srx, _lurker, _lrx) = searcher_world(200, 0);
    let a = fx.a;
    let cache = hidden_item(&mut fx, "a mossy cache", a, 350);
    assert!(!super::senses::item_visible_to(&fx.world, seeker, cache));
    search_with_roll(&mut fx.world, seeker, "", &mut |_| 0);
    let out = drain(&mut srx);
    assert!(out.contains("You find nothing of interest."), "{out}");
    assert_eq!(hiding::hiddenness(&fx.world, cache), 250);
    advance(&mut fx.world, 100);
    search_with_roll(&mut fx.world, seeker, "", &mut |hi| hi);
    let out = drain(&mut srx);
    assert!(out.contains("You find a mossy cache!"), "{out}");
    assert_eq!(hiding::hiddenness(&fx.world, cache), 0);
    assert!(super::senses::item_visible_to(&fx.world, seeker, cache));
}

#[test]
fn search_reveals_an_object_the_searcher_already_out_perceives() {
    let (mut fx, seeker, mut srx, lurker, mut lrx) = searcher_world(400, 0);
    let a = fx.a;
    hidden_item(&mut fx, "a mossy cache", a, 300);
    let _ = drain(&mut lrx);
    search_with_roll(&mut fx.world, seeker, "", &mut |_| 0);
    let out = drain(&mut srx);
    assert!(out.contains("You reveal a mossy cache!"), "{out}");
    let told = drain(&mut lrx);
    assert!(told.contains("Seeker reveals a mossy cache!"), "{told}");
    let _ = lurker;
}

#[test]
fn a_found_object_ends_the_search_before_doors_and_characters() {
    let (mut fx, seeker, mut srx, lurker, _lrx) = searcher_world(200, 150);
    let a = fx.a;
    hidden_item(&mut fx, "a mossy cache", a, 100);
    search_with_roll(&mut fx.world, seeker, "", &mut |_| 0);
    let out = drain(&mut srx);
    assert!(out.contains("You reveal a mossy cache!"), "{out}");
    assert!(!out.contains("lurking here"), "{out}");
    assert_eq!(hiding::hiddenness(&fx.world, lurker), 150);
}

#[test]
fn staff_find_every_hidden_object() {
    let (mut fx, seeker, mut srx, _lurker, _lrx) = searcher_world(10, 0);
    let a = fx.a;
    let one = hidden_item(&mut fx, "a mossy cache", a, 900);
    let two = hidden_item(&mut fx, "a rusty hinge", a, 800);
    fx.world.entity_mut(seeker).insert(Account {
        user_id: String::new(),
        character_id: "g".into(),
        role: UserRole::Immortal,
        account_role: UserRole::Immortal,
        perms: vec![],
    });
    search_with_roll(&mut fx.world, seeker, "", &mut |_| 0);
    let out = drain(&mut srx);
    assert!(
        out.contains("a mossy cache!") && out.contains("a rusty hinge!"),
        "{out}"
    );
    assert_eq!(hiding::hiddenness(&fx.world, one), 0);
    assert_eq!(hiding::hiddenness(&fx.world, two), 0);
}

#[test]
fn search_with_a_container_argument_looks_only_inside_it() {
    let (mut fx, seeker, mut srx, _lurker, _lrx) = searcher_world(200, 0);
    let a = fx.a;
    let mut protos = mud_world::ObjectPrototypes::default();
    protos.by_key.insert(
        (551, 2),
        super::test_support::object_proto(551, 2, mud_db::enums::ObjectType::Container),
    );
    fx.world.insert_resource(protos);
    let chest = fx
        .world
        .spawn((
            mud_world::Item,
            Named {
                name: "an oak chest".into(),
            },
            mud_world::Keywords(vec!["chest".into()]),
            mud_world::WorldKey { zone: 551, id: 2 },
            Located(a),
        ))
        .id();
    let coin = hidden_item(&mut fx, "a false coin", chest, 100);
    let floor = hidden_item(&mut fx, "a mossy cache", a, 100);
    search_with_roll(&mut fx.world, seeker, "chest", &mut |_| 0);
    let out = drain(&mut srx);
    assert!(out.contains("You reveal a false coin!"), "{out}");
    assert_eq!(hiding::hiddenness(&fx.world, coin), 0);
    assert_eq!(hiding::hiddenness(&fx.world, floor), 100);
    // Nothing hidden inside: no door or character search follows.
    advance(&mut fx.world, 100);
    search_with_roll(&mut fx.world, seeker, "chest", &mut |_| 0);
    let out = drain(&mut srx);
    assert!(out.contains("You find nothing of interest."), "{out}");
}
