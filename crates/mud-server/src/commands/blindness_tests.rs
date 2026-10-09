//! Blindness (legacy `EFF_BLIND`): the `Blinded` marker (a `blind` effect
//! instance or the `blinded` mob / race / item flag) makes `can_see_player`
//! and `viewer_sees_room` fail, so text look, room listings, GMCP panels,
//! target resolvers, aggro and per-observer messages all go blind together.
//! Look, exits, read, scan and search refuse; a blind attacker cannot open
//! a fight (but one already under way carries on); a blind mover stumbles;
//! `HOLY_LIGHT` and staff see regardless. Test-only.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Direction, ExitState, PlayerFlag, UserRole};
use mud_world::{
    Account, Blinded, CombatStats, EffectCatalog, EffectDef, EffectInstance, EffectSource, Exits,
    Fighting, Health, KnownAbilities, Located, Mob, MobDefaultEffect, MobDefaultEffectCatalog,
    Named, PlayerFlags, WorldKey,
};

use super::gmcp_tests::{Fx, drain_bytes, fixture, frames, of, player};
use super::test_support::{Rx, ability_def, drain};
use super::{
    can_see_player, dispatch, engage_combat, remove_effect_named, try_engage_aggressive_mob,
};

const BLIND_TEXT: &str = "You can't see a damned thing; you're blind!";

/// Blind `e` the way the status arm does: a `blind` instance plus its marker.
fn blind(world: &mut World, e: Entity) -> Entity {
    mud_world::mob_effects::install_flag_marker(world, e, "blind");
    world
        .spawn((
            EffectInstance {
                kind: 1,
                name: "blind".into(),
                strength: 1,
                remaining_secs: 60,
                source: EffectSource::Spell,
                ability_id: None,
            },
            mud_world::AppliedTo(e),
        ))
        .id()
}

fn mob_in(world: &mut World, room: Entity, name: &str, alignment: i32) -> Entity {
    world
        .spawn((
            Mob,
            Named { name: name.into() },
            Located(room),
            CombatStats {
                alignment,
                ..CombatStats::default()
            },
            Health { hp: 100, max: 100 },
        ))
        .id()
}

/// "Seer" (to be blinded) and a sighted "Bystander" in room A.
fn two_players() -> (Fx, Entity, Rx, Entity, Rx) {
    let mut fx = fixture();
    let a = fx.a;
    let (seer, srx) = player(&mut fx.world, a, "Seer");
    let (other, orx) = player(&mut fx.world, a, "Bystander");
    for p in [seer, other] {
        fx.world
            .entity_mut(p)
            .insert((Health { hp: 100, max: 100 }, CombatStats::default()));
    }
    (fx, seer, srx, other, orx)
}

fn run(fx: &mut Fx, p: Entity, rx: &mut Rx, cmd: &str) -> String {
    let _ = drain(rx);
    dispatch(&mut fx.world, p, cmd);
    drain(rx)
}

#[test]
fn blind_look_says_so_and_lists_nobody() {
    let (mut fx, seer, mut srx, _other, _orx) = two_players();
    let sighted = run(&mut fx, seer, &mut srx, "look");
    assert!(sighted.contains("Bystander"), "control: {sighted}");
    blind(&mut fx.world, seer);
    let out = run(&mut fx, seer, &mut srx, "look");
    assert!(out.contains(BLIND_TEXT), "{out}");
    assert!(!out.contains("Bystander"), "{out}");
    assert!(!out.contains("Room 18"), "no room text: {out}");
    // Looking at something or someone, and examine, refuse the same way.
    for cmd in ["look bystander", "examine bystander", "look north"] {
        let out = run(&mut fx, seer, &mut srx, cmd);
        assert!(out.contains(BLIND_TEXT), "{cmd}: {out}");
    }
}

#[test]
fn blind_exits_read_scan_and_search_refuse() {
    let (mut fx, seer, mut srx, _other, _orx) = two_players();
    let (a, b) = (fx.a, fx.b);
    fx.world.get_mut::<Exits>(a).unwrap().0.insert(
        Direction::East,
        mud_world::ExitData {
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
    );
    let sighted = run(&mut fx, seer, &mut srx, "exits");
    assert!(
        sighted.to_lowercase().contains("east"),
        "control: {sighted}"
    );
    blind(&mut fx.world, seer);
    for cmd in ["exits", "read sign", "scan"] {
        let out = run(&mut fx, seer, &mut srx, cmd);
        assert!(out.contains(BLIND_TEXT), "{cmd}: {out}");
        assert!(!out.to_lowercase().contains("east"), "{cmd}: {out}");
    }
    let out = run(&mut fx, seer, &mut srx, "search");
    assert!(out.contains("You're blind and can't see a thing!"), "{out}");
    assert!(!out.contains("You search"), "{out}");
}

#[test]
fn blind_kill_refuses_and_starts_no_fight() {
    let (mut fx, seer, mut srx, _other, _orx) = two_players();
    let a = fx.a;
    let ogre = mob_in(&mut fx.world, a, "an ogre", 0);
    blind(&mut fx.world, seer);
    for cmd in ["kill ogre", "hit ogre", "backstab ogre", "bash ogre"] {
        let out = run(&mut fx, seer, &mut srx, cmd);
        assert!(out.contains("You can't see a thing!"), "{cmd}: {out}");
    }
    assert!(fx.world.get::<Fighting>(seer).is_none());
    assert!(fx.world.get::<Fighting>(ogre).is_none());
    // Control: once sight returns the same command opens the fight.
    remove_effect_named(&mut fx.world, seer, "blind");
    let out = run(&mut fx, seer, &mut srx, "kill ogre");
    assert!(!out.contains("can't see"), "{out}");
    assert_eq!(fx.world.get::<Fighting>(seer).map(|f| f.0), Some(ogre));
}

#[test]
fn an_ongoing_fight_keeps_swinging_while_blind() {
    use mud_world::{NaturalDamage, Posture, PostureKind};
    let mut fx = fixture();
    let a = fx.a;
    let target = fx
        .world
        .spawn((
            Named {
                name: "Target".into(),
            },
            Located(a),
            Health {
                hp: 1000,
                max: 1000,
            },
        ))
        .id();
    let attacker = fx
        .world
        .spawn((
            Named {
                name: "Attacker".into(),
            },
            Located(a),
            Fighting(target),
            CombatStats {
                accuracy: 200,
                ..CombatStats::default()
            },
            NaturalDamage {
                num: 1,
                size: 1,
                bonus: 9,
            },
            Posture(PostureKind::Standing),
        ))
        .id();
    blind(&mut fx.world, attacker);
    assert!(!can_see_player(&fx.world, attacker, target));
    for round in 1..=20 {
        fx.world.insert_resource(crate::TickCount(round * 40));
        crate::combat::combat_tick(&mut fx.world);
    }
    assert_eq!(
        fx.world.get::<Fighting>(attacker).map(|f| f.0),
        Some(target),
        "the fight carries on"
    );
    assert!(
        fx.world.get::<Health>(target).unwrap().hp < 1000,
        "a blind attacker still lands blows"
    );
}

#[test]
fn a_blind_observer_reads_someone_in_combat_messages() {
    let (mut fx, seer, mut srx, other, _orx) = two_players();
    let a = fx.a;
    let ogre = mob_in(&mut fx.world, a, "an ogre", 0);
    blind(&mut fx.world, seer);
    let _ = drain(&mut srx);
    engage_combat(&mut fx.world, other, ogre, a);
    let out = drain(&mut srx);
    assert!(out.contains("Someone sees someone and attacks!"), "{out}");
    assert!(!out.contains("Bystander") && !out.contains("ogre"), "{out}");
    // Attacked by an unseen mob: the victim is told "Someone".
    let _ = drain(&mut srx);
    engage_combat(&mut fx.world, ogre, seer, a);
    let out = drain(&mut srx);
    assert!(out.contains("Someone sees you and attacks!"), "{out}");
}

#[test]
fn blind_viewers_see_no_players_in_gmcp_and_cure_brings_them_back() {
    let (mut fx, seer, mut srx, _other, _orx) = two_players();
    let players = |fx: &mut Fx, rx: &mut Rx| -> serde_json::Value {
        let _ = drain_bytes(rx);
        crate::commands::gmcp::send_room_players(&mut fx.world, seer, true);
        let fr = frames(&drain_bytes(rx));
        let all = of(&fr, "Room.Players");
        serde_json::from_str(all.last().unwrap_or_else(|| panic!("no frame: {fr:?}"))).unwrap()
    };
    assert_eq!(players(&mut fx, &mut srx).as_array().unwrap().len(), 1);
    blind(&mut fx.world, seer);
    assert_eq!(players(&mut fx, &mut srx), serde_json::json!([]));
    // Room.Info is `{}` too: a blind viewer maps nothing.
    assert_eq!(
        crate::commands::gmcp::build_room_info(&mut fx.world, seer),
        "{}"
    );
    assert_eq!(remove_effect_named(&mut fx.world, seer, "blind"), 1);
    assert!(
        fx.world.get::<Blinded>(seer).is_none(),
        "marker follows the effect"
    );
    assert_eq!(players(&mut fx, &mut srx).as_array().unwrap().len(), 1);
    let look = run(&mut fx, seer, &mut srx, "look");
    assert!(look.contains("Bystander"), "sight restored: {look}");
}

#[test]
fn a_blind_player_still_sees_themselves_and_hears_the_channels() {
    let (mut fx, seer, mut srx, other, _orx) = two_players();
    blind(&mut fx.world, seer);
    assert!(can_see_player(&fx.world, seer, seer));
    assert!(!can_see_player(&fx.world, seer, other));
    assert!(
        can_see_player(&fx.world, other, seer),
        "others still see the blind"
    );
    let _ = drain(&mut srx);
    dispatch(&mut fx.world, other, "gossip hello there");
    let out = drain(&mut srx);
    assert!(
        out.contains("hello there"),
        "blindness does not silence gossip: {out}"
    );
}

#[test]
fn holylight_and_staff_see_through_blindness() {
    let (mut fx, seer, mut srx, other, _orx) = two_players();
    blind(&mut fx.world, seer);
    assert!(!can_see_player(&fx.world, seer, other));
    fx.world
        .entity_mut(seer)
        .insert(PlayerFlags(vec![PlayerFlag::HolyLight]));
    assert!(can_see_player(&fx.world, seer, other));
    let out = run(&mut fx, seer, &mut srx, "look");
    assert!(
        out.contains("Bystander") && !out.contains(BLIND_TEXT),
        "{out}"
    );
    // Staff, without the flag.
    fx.world.entity_mut(seer).remove::<PlayerFlags>();
    assert!(!can_see_player(&fx.world, seer, other));
    fx.world.entity_mut(seer).insert(Account {
        user_id: String::new(),
        character_id: "c".into(),
        role: UserRole::Immortal,
        account_role: UserRole::Immortal,
        perms: vec![],
    });
    assert!(can_see_player(&fx.world, seer, other));
    let out = run(&mut fx, seer, &mut srx, "exits");
    assert!(!out.contains(BLIND_TEXT), "{out}");
}

#[test]
fn blind_mobs_do_not_aggro_and_sighted_ones_do() {
    let (mut fx, seer, _srx, _other, _orx) = two_players();
    let a = fx.a;
    let wolf = mob_in(&mut fx.world, a, "a wolf", -1000);
    blind(&mut fx.world, wolf);
    try_engage_aggressive_mob(&mut fx.world, seer, a);
    assert!(fx.world.get::<Fighting>(wolf).is_none());
    remove_effect_named(&mut fx.world, wolf, "blind");
    try_engage_aggressive_mob(&mut fx.world, seer, a);
    assert_eq!(fx.world.get::<Fighting>(wolf).map(|f| f.0), Some(seer));
}

#[test]
fn a_blind_helper_mob_does_not_join_a_fight() {
    use mud_db::enums::MobBehavior;
    use mud_world::MobBehaviors;
    let (mut fx, seer, _srx, other, _orx) = two_players();
    let a = fx.a;
    let ogre = mob_in(&mut fx.world, a, "an ogre", 0);
    let helper = mob_in(&mut fx.world, a, "a guard", 0);
    fx.world
        .entity_mut(helper)
        .insert(MobBehaviors(vec![MobBehavior::Helper]));
    fx.world.entity_mut(ogre).insert(Fighting(other));
    fx.world.entity_mut(other).insert(Fighting(ogre));
    blind(&mut fx.world, helper);
    crate::commands::mob_helpers_engage(&mut fx.world, ogre, other, a);
    assert!(fx.world.get::<Fighting>(helper).is_none());
    crate::commands::mob_assist_pulse(&mut fx.world);
    assert!(fx.world.get::<Fighting>(helper).is_none());
    let _ = seer;
}

#[test]
fn a_permanent_blinded_default_gives_the_marker_and_noblind_is_a_resistance() {
    let mut fx = fixture();
    let a = fx.a;
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(
        4,
        EffectDef {
            id: 4,
            name: "status".into(),
            description: None,
            effect_type: "status".into(),
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
    let mut defaults = MobDefaultEffectCatalog::default();
    defaults.by_key.insert(
        (30, 1),
        vec![MobDefaultEffect {
            effect_id: 4,
            strength: 1,
            modifier_data: serde_json::json!({"flags": ["blinded"]}),
        }],
    );
    fx.world.insert_resource(defaults);
    let eyeless = mob_in(&mut fx.world, a, "an eyeless worm", 0);
    mud_world::mob_effects::apply_mob_default_effects(&mut fx.world, eyeless, (30, 1));
    assert!(fx.world.get::<Blinded>(eyeless).is_some());
    let (seer, _srx) = player(&mut fx.world, a, "Seer");
    assert!(
        !can_see_player(&fx.world, eyeless, seer),
        "the worm sees nobody"
    );
    // Expiry / dispel of the permanent default would drop the marker.
    assert_eq!(remove_effect_named(&mut fx.world, eyeless, "blinded"), 1);
    assert!(fx.world.get::<Blinded>(eyeless).is_none());
}

/// A real spell `flagspell` applying the status flag `flag`, a spell
/// `curespell` that cleanses the `blind` condition, and a level-20 caster
/// who knows both.
fn caster_with_blind_spells() -> (Fx, Entity, Rx) {
    const STATUS: i32 = 10;
    const CLEANSE: i32 = 11;
    caster_with_spells(vec![
        (
            1,
            "Flagspell",
            vec![(
                STATUS,
                Some(serde_json::json!({ "flag": "blind", "duration": 60 })),
            )],
        ),
        (
            2,
            "Curespell",
            vec![(CLEANSE, Some(serde_json::json!({ "condition": "blind" })))],
        ),
    ])
}

const STATUS: i32 = 10;
const CLEANSE: i32 = 11;
const MODIFY: i32 = 12;

/// `(effect id, override params)` rows of one ability.
type EffectRows = Vec<(i32, Option<serde_json::Value>)>;

/// A level-20 caster who knows `spells` (id, name, effect rows), with the
/// `status` / `cleanse` / `modify` effect kinds registered.
fn caster_with_spells(spells: Vec<(i32, &str, EffectRows)>) -> (Fx, Entity, Rx) {
    let mut fx = fixture();
    let mut catalog = mud_world::AbilityCatalog::default();
    let mut known = Vec::new();
    for (id, name, rows) in spells {
        let mut spell = ability_def(id, name, AbilityKind::Spell);
        spell.cast_time_rounds = 0;
        catalog.by_name.insert(name.to_ascii_lowercase(), spell);
        catalog.effects_for.insert(id, rows);
        known.push((id, 500, true));
    }
    fx.world.insert_resource(catalog);
    let def = |id: i32, kind: &str| EffectDef {
        id,
        name: kind.to_string(),
        description: None,
        effect_type: kind.to_string(),
        tags: Vec::new(),
        presence_override: None,
        default_params: serde_json::json!({}),
        prevents_speaking: false,
        prevents_casting: false,
        prevents_movement: false,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    };
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(STATUS, def(STATUS, "status"));
    effects.by_id.insert(CLEANSE, def(CLEANSE, "cleanse"));
    effects.by_id.insert(MODIFY, def(MODIFY, "modify"));
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
        KnownAbilities { entries: known },
    ));
    (fx, p, rx)
}

fn cast(fx: &mut Fx, p: Entity, line: &str) {
    dispatch(&mut fx.world, p, line);
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
}

#[test]
fn a_blind_spell_blinds_and_a_self_cast_cure_restores_sight() {
    let (mut fx, p, mut rx) = caster_with_blind_spells();
    let a = fx.a;
    let (_other, _orx) = player(&mut fx.world, a, "Bystander");
    cast(&mut fx, p, "cast 'flagspell'");
    assert!(fx.world.get::<Blinded>(p).is_some(), "the spell blinds");
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(out.contains(BLIND_TEXT), "{out}");
    // A blind caster can still target themselves, by default and by name.
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'curespell' self");
    let out = drain(&mut rx);
    assert!(fx.world.get::<Blinded>(p).is_none(), "cure worked: {out}");
    let out = run(&mut fx, p, &mut rx, "look");
    assert!(out.contains("Bystander"), "{out}");
    cast(&mut fx, p, "cast 'flagspell'");
    assert!(fx.world.get::<Blinded>(p).is_some());
    cast(&mut fx, p, "cast 'curespell'");
    assert!(
        fx.world.get::<Blinded>(p).is_none(),
        "default target is self"
    );
}

#[test]
fn noblind_mobs_shrug_off_blindness_and_others_do_not() {
    let (mut fx, p, mut rx) = caster_with_blind_spells();
    let a = fx.a;
    let mut protos = mud_world::MobPrototypes::default();
    for (id, resist) in [
        (1, serde_json::json!({"blind": 0})),
        (2, serde_json::json!({})),
    ] {
        let mut proto =
            super::test_support::mob_proto(30, id, mud_db::enums::MobProfession::Banker);
        proto.resistances = resist;
        protos.by_key.insert((30, id), proto);
    }
    fx.world.insert_resource(protos);
    let spawn = |fx: &mut Fx, name: &str, id: i32| {
        let e = mob_in(&mut fx.world, a, name, 0);
        fx.world.entity_mut(e).insert(WorldKey { zone: 30, id });
        e
    };
    let golem = spawn(&mut fx, "a stone golem", 1);
    let rat = spawn(&mut fx, "a sewer rat", 2);
    assert!(super::senses::is_noblind(&fx.world, golem));
    assert!(!super::senses::is_noblind(&fx.world, rat));
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'flagspell' golem");
    let said = drain(&mut rx);
    assert!(fx.world.get::<Blinded>(golem).is_none(), "NOBLIND: {said}");
    assert!(
        said.contains("a stone golem is immune to blindness."),
        "{said}"
    );
    cast(&mut fx, p, "cast 'flagspell' rat");
    assert!(fx.world.get::<Blinded>(rat).is_some());
}

fn exit_to(to: Entity) -> mud_world::ExitData {
    mud_world::ExitData {
        to: Some(to),
        state: ExitState::Open,
        key: None,
        description: None,
        keywords: vec![],
        is_hidden: false,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    }
}

#[test]
fn a_blind_mover_stumbles_and_the_new_room_stays_dark() {
    let (mut fx, seer, mut srx, other, mut orx) = two_players();
    let (a, b) = (fx.a, fx.b);
    fx.world
        .entity_mut(a)
        .insert(Exits(HashMap::from([(Direction::East, exit_to(b))])));
    let (watcher, mut wrx) = player(&mut fx.world, b, "Watcher");
    let _ = (other, watcher);
    blind(&mut fx.world, seer);
    let _ = (drain(&mut orx), drain(&mut srx));
    dispatch(&mut fx.world, seer, "east");
    let left = drain(&mut orx);
    assert!(left.contains("Seer stumbles east."), "{left:?}");
    let came = drain(&mut wrx);
    assert!(came.contains("Seer stumbles in from the west."), "{came:?}");
    let mine = drain(&mut srx);
    assert!(
        mine.contains("You see nothing but infinite darkness..."),
        "{mine:?}"
    );
    assert!(!mine.contains("Watcher"), "{mine:?}");
}

#[test]
fn a_sighted_mover_still_leaves_normally() {
    let (mut fx, seer, _srx, _other, mut orx) = two_players();
    let (a, b) = (fx.a, fx.b);
    fx.world
        .entity_mut(a)
        .insert(Exits(HashMap::from([(Direction::East, exit_to(b))])));
    let _ = drain(&mut orx);
    dispatch(&mut fx.world, seer, "east");
    assert!(drain(&mut orx).contains("Seer leaves east."));
}

/// The seeded Blindness / Cure Blind rows (fierylib `abilities.json`): a
/// `blind` status plus the legacy -4 hitroll / -40 AC as accuracy / evasion,
/// and a `cleanse` of the `blind` condition.
fn seeded_blind_spells() -> (Fx, Entity, Rx) {
    caster_with_spells(vec![
        (
            1,
            "Blindness",
            vec![
                (
                    STATUS,
                    Some(serde_json::json!({
                        "flag": "blind", "duration": 2, "durationUnit": "hours"
                    })),
                ),
                (
                    MODIFY,
                    Some(serde_json::json!({
                        "target": "accuracy", "amount": "-4",
                        "duration": 2, "durationUnit": "hours"
                    })),
                ),
                (
                    MODIFY,
                    Some(serde_json::json!({
                        "target": "evasion", "amount": "-40",
                        "duration": 2, "durationUnit": "hours"
                    })),
                ),
            ],
        ),
        (
            2,
            "Cure Blind",
            vec![(
                CLEANSE,
                Some(serde_json::json!({ "condition": "blind", "scope": "all" })),
            )],
        ),
    ])
}

fn target_mob(fx: &mut Fx, name: &str) -> Entity {
    let a = fx.a;
    let e = mob_in(&mut fx.world, a, name, 0);
    fx.world.entity_mut(e).insert(CombatStats {
        accuracy: 50,
        evasion: 50,
        ..CombatStats::default()
    });
    e
}

fn combat_pair(world: &World, e: Entity) -> (i32, i32) {
    let cs = world.get::<CombatStats>(e).unwrap();
    (cs.accuracy, cs.evasion)
}

fn effects_on(world: &mut World, e: Entity) -> usize {
    let mut q = world.query::<&mud_world::AppliedTo>();
    q.iter(world).filter(|a| a.0 == e).count()
}

#[test]
fn blindness_blinds_a_mob_and_cure_blind_lifts_the_whole_spell() {
    let (mut fx, p, mut rx) = seeded_blind_spells();
    let rat = target_mob(&mut fx, "a sewer rat");
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'blindness' rat");
    let said = drain(&mut rx);
    assert!(fx.world.get::<Blinded>(rat).is_some(), "blinded: {said}");
    assert_eq!(combat_pair(&fx.world, rat), (46, 10), "legacy -4 / -40");
    assert_eq!(effects_on(&mut fx.world, rat), 3);

    cast(&mut fx, p, "cast 'cure blind' rat");
    let said = drain(&mut rx);
    assert!(fx.world.get::<Blinded>(rat).is_none(), "cured: {said}");
    assert_eq!(combat_pair(&fx.world, rat), (50, 50), "penalties lifted");
    assert_eq!(effects_on(&mut fx.world, rat), 0);
}

#[test]
fn cure_blind_leaves_unrelated_effects_alone() {
    let (mut fx, p, _rx) = seeded_blind_spells();
    let rat = target_mob(&mut fx, "a sewer rat");
    let other = fx
        .world
        .spawn((
            EffectInstance {
                kind: STATUS,
                name: "sleeping".into(),
                strength: 1,
                remaining_secs: 60,
                source: EffectSource::Spell,
                ability_id: Some(77),
            },
            mud_world::AppliedTo(rat),
        ))
        .id();
    cast(&mut fx, p, "cast 'blindness' rat");
    cast(&mut fx, p, "cast 'cure blind' rat");
    assert!(fx.world.get_entity(other).is_ok(), "sleep survives");
    assert!(fx.world.get::<Blinded>(rat).is_none());
}

#[test]
fn blindness_on_a_noblind_mob_changes_nothing() {
    let (mut fx, p, mut rx) = seeded_blind_spells();
    let mut protos = mud_world::MobPrototypes::default();
    let mut proto = super::test_support::mob_proto(30, 1, mud_db::enums::MobProfession::Banker);
    proto.resistances = serde_json::json!({"blind": 0});
    protos.by_key.insert((30, 1), proto);
    fx.world.insert_resource(protos);
    let golem = target_mob(&mut fx, "a stone golem");
    fx.world
        .entity_mut(golem)
        .insert(WorldKey { zone: 30, id: 1 });
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'blindness' golem");
    let said = drain(&mut rx);
    assert!(fx.world.get::<Blinded>(golem).is_none(), "NOBLIND: {said}");
    assert_eq!(combat_pair(&fx.world, golem), (50, 50), "no penalties");
    assert_eq!(effects_on(&mut fx.world, golem), 0);
    assert!(said.contains("immune to blindness"), "{said}");
}

#[test]
fn eye_gouge_formula_blinds_with_a_skill_scaled_accuracy_penalty() {
    // Legacy eye gouge: -(2 + skill/10) hitroll for one tick, plus EFF_BLIND.
    let (mut fx, p, _rx) = caster_with_spells(vec![(
        1,
        "Gouge",
        vec![
            (
                STATUS,
                Some(serde_json::json!({
                    "flag": "blind", "duration": 1, "durationUnit": "hours"
                })),
            ),
            (
                MODIFY,
                Some(serde_json::json!({
                    "target": "accuracy", "amount": "-2 - skill / 10",
                    "duration": 1, "durationUnit": "hours"
                })),
            ),
        ],
    )]);
    let rat = target_mob(&mut fx, "a sewer rat");
    cast(&mut fx, p, "cast 'gouge' rat");
    assert!(fx.world.get::<Blinded>(rat).is_some());
    // The fixture caster's skill resolves to 50: -(2 + 50 / 10) = -7.
    assert_eq!(combat_pair(&fx.world, rat).0, 43);
}
