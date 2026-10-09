//! Legacy `MAG_GROUP` (`skills.cpp`): a group spell, chant or song cast outside
//! a group is refused ("You can't cast this spell if you're not in a group!",
//! `spell_parser.cpp:974`), spends no slot, and a cast inside one fills the
//! caster and every grouped player in the room. Plus War Cry as data: legacy
//! `CHANT_WAR_CRY` (`magic.cpp:3206`) is hitroll and damroll `skill / 25 + 1` on
//! the group, which is accuracy and attack power at the importer's x2 and x5.
//! The effect rows are the ones in `fierylib/data/abilities.json`. Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_world::{
    CombatStats, Fighting, Health, Located, Mob, Named, Profile, SpellSlotData, SpellSlots,
};

use super::blindness_tests::{MODIFY, cast, caster_with_spells};
use super::dispatch;
use super::gmcp_tests::{Fx, player};
use super::test_support::{Rx, drain};

const CLASS: i32 = 7;
const ABILITY: i32 = 1;
const SPELL_REFUSAL: &str = "You can't cast this spell if you're not in a group!";
const CHANT_REFUSAL: &str = "You can't chant this song if you're not in a group!";

fn grouped_rule(message: &str) -> serde_json::Value {
    serde_json::json!({ "type": "grouped", "message": message })
}

/// A caster who knows one group ability (`name`, of `kind`) with the given
/// effect rows, a `ROOM_ALLIES` non-violent area scope and the `grouped` rule,
/// and one circle-1 slot to spend.
fn group_caster(
    name: &str,
    kind: AbilityKind,
    rows: Vec<(i32, Option<serde_json::Value>)>,
    refusal: &str,
) -> (Fx, Entity, Rx) {
    let (mut fx, p, rx) = caster_with_spells(vec![(ABILITY, name, rows)]);
    {
        let mut catalog = fx.world.resource_mut::<mud_world::AbilityCatalog>();
        let def = catalog.by_name.get_mut(&name.to_ascii_lowercase()).unwrap();
        def.kind = kind;
        def.target_scope = "ROOM_ALLIES".to_string();
        def.is_area = true;
        def.violent = false;
        catalog
            .restriction_rules
            .insert(ABILITY, vec![grouped_rule(refusal)]);
    }
    let mut data = SpellSlotData::default();
    data.ability_circle.insert((CLASS, ABILITY), 1);
    data.progression.insert((10, 1), 1);
    fx.world.insert_resource(data);
    fx.world.entity_mut(p).insert(Profile {
        level: 10,
        class_id: Some(CLASS),
        race: "HUMAN".to_string(),
        experience: 0,
        gender: "neutral".to_string(),
    });
    (fx, p, rx)
}

fn group_armor() -> (Fx, Entity, Rx) {
    group_caster(
        "Group Armor",
        AbilityKind::Spell,
        vec![(
            MODIFY,
            Some(serde_json::json!({
                "target": "accuracy", "amount": 3,
                "duration": "5 + (skill / 10)", "durationUnit": "hours"
            })),
        )],
        SPELL_REFUSAL,
    )
}

fn accuracy(fx: &Fx, e: Entity) -> i32 {
    fx.world.get::<CombatStats>(e).unwrap().accuracy
}

/// A second player in the caster's room with stats, grouped by the real
/// `invite` / `accept` exchange when `join` is set.
fn mate(fx: &mut Fx, leader: Entity, name: &str, join: bool) -> Entity {
    let a = fx.a;
    let (e, rx) = player(&mut fx.world, a, name);
    std::mem::forget(rx);
    fx.world
        .entity_mut(e)
        .insert((Health { hp: 100, max: 100 }, CombatStats::default()));
    if join {
        dispatch(&mut fx.world, leader, &format!("invite {name}"));
        dispatch(&mut fx.world, e, "accept");
        assert!(
            fx.world.get::<mud_world::GroupMember>(e).is_some(),
            "control: {name} joined the group"
        );
    }
    e
}

fn slots_used(fx: &Fx, p: Entity) -> (i32, usize) {
    fx.world
        .get::<SpellSlots>(p)
        .map_or((0, 0), |s| (s.used_in_circle(1), s.in_flight.len()))
}

#[test]
fn a_solo_caster_is_refused_and_no_slot_is_spent() {
    let (mut fx, p, mut rx) = group_armor();
    let before = accuracy(&fx, p);
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'group armor'");
    let said = drain(&mut rx);
    assert!(said.contains(SPELL_REFUSAL), "{said}");
    assert_eq!(accuracy(&fx, p), before, "nothing applied to the caster");
    assert_eq!(slots_used(&fx, p), (0, 0), "slot released, not charged");
    // The same slot is still there to cast with once grouped.
    let friend = mate(&mut fx, p, "Friend", true);
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'group armor'");
    let said = drain(&mut rx);
    assert!(!said.contains(SPELL_REFUSAL), "{said}");
    assert_eq!(accuracy(&fx, friend), before + 3, "{said}");
}

#[test]
fn a_mere_follower_does_not_make_a_group() {
    // `follow` needs no consent; only invite + accept makes a group.
    let (mut fx, p, mut rx) = group_armor();
    let stalker = mate(&mut fx, p, "Stalker", false);
    dispatch(&mut fx.world, stalker, "follow caster");
    assert!(fx.world.get::<mud_world::Follower>(stalker).is_some());
    let before = accuracy(&fx, p);
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'group armor'");
    let said = drain(&mut rx);
    assert!(said.contains(SPELL_REFUSAL), "{said}");
    assert_eq!(accuracy(&fx, p), before);
    assert_eq!(accuracy(&fx, stalker), before);
    assert_eq!(slots_used(&fx, p), (0, 0));
}

#[test]
fn a_grouped_cast_fills_the_caster_and_the_members_and_charges_the_slot() {
    let (mut fx, p, mut rx) = group_armor();
    let member = mate(&mut fx, p, "Member", true);
    let outsider = mate(&mut fx, p, "Outsider", false);
    let before = accuracy(&fx, p);
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'group armor'");
    let said = drain(&mut rx);
    assert!(!said.contains(SPELL_REFUSAL), "{said}");
    assert_eq!(accuracy(&fx, p), before + 3, "caster: {said}");
    assert_eq!(accuracy(&fx, member), before + 3, "member: {said}");
    assert_eq!(accuracy(&fx, outsider), before, "outsider: {said}");
    assert_eq!(slots_used(&fx, p), (1, 1), "slot committed: {said}");
}

#[test]
fn a_group_member_who_is_not_the_leader_can_cast_too() {
    let (mut fx, p, _rx) = group_armor();
    let member = mate(&mut fx, p, "Member", true);
    // Give the member the spell and a slot of their own.
    let known = fx
        .world
        .get::<mud_world::KnownAbilities>(p)
        .unwrap()
        .clone();
    let profile = fx.world.get::<Profile>(p).unwrap().clone();
    fx.world.entity_mut(member).insert((known, profile));
    let before = accuracy(&fx, p);
    cast(&mut fx, member, "cast 'group armor'");
    assert_eq!(accuracy(&fx, member), before + 3);
    assert_eq!(
        accuracy(&fx, p),
        before + 3,
        "the leader is in the fill too"
    );
}

#[test]
fn a_leader_nobody_has_joined_is_not_grouped() {
    // Invited but not yet accepted: still alone.
    let (mut fx, p, mut rx) = group_armor();
    let a = fx.a;
    let (pending, prx) = player(&mut fx.world, a, "Pending");
    std::mem::forget(prx);
    dispatch(&mut fx.world, p, "invite pending");
    assert!(fx.world.get::<mud_world::GroupInvite>(pending).is_some());
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'group armor'");
    assert!(drain(&mut rx).contains(SPELL_REFUSAL));
}

// ---- War Cry ------------------------------------------------------------

/// War Cry as `fierylib/data/abilities.json` has it: a non-violent chant on
/// the group, hitroll and damroll `skill / 25 + 1` as accuracy x2 and attack
/// power x5, for `skill / 25 + 1` hours. No damage row.
fn war_cry() -> (Fx, Entity, Rx) {
    group_caster(
        "War Cry",
        AbilityKind::Chant,
        vec![
            (
                MODIFY,
                Some(serde_json::json!({
                    "target": "accuracy", "amount": "(skill / 25 + 1) * 2",
                    "duration": "skill / 25 + 1", "durationUnit": "hours"
                })),
            ),
            (
                MODIFY,
                Some(serde_json::json!({
                    "target": "attack_power", "amount": "(skill / 25 + 1) * 5",
                    "duration": "skill / 25 + 1", "durationUnit": "hours"
                })),
            ),
        ],
        CHANT_REFUSAL,
    )
}

fn attack_power(fx: &Fx, e: Entity) -> i32 {
    fx.world.get::<CombatStats>(e).unwrap().attack_power
}

#[test]
fn war_cry_buffs_the_group_and_damages_nobody() {
    let (mut fx, p, mut rx) = war_cry();
    let member = mate(&mut fx, p, "Member", true);
    let outsider = mate(&mut fx, p, "Outsider", false);
    let a = fx.a;
    let goblin = fx
        .world
        .spawn((
            Mob,
            Named {
                name: "a goblin".into(),
            },
            Located(a),
            Health { hp: 40, max: 40 },
            CombatStats::default(),
        ))
        .id();
    let (acc0, ap0) = (accuracy(&fx, p), attack_power(&fx, p));
    let _ = drain(&mut rx);
    cast(&mut fx, p, "chant 'war cry'");
    let said = drain(&mut rx);
    assert!(!said.contains(CHANT_REFUSAL), "{said}");
    // skill 50 here: 50 / 25 + 1 = 3 hitroll and damroll, so +6 accuracy, +15 attack power.
    for (who, label) in [(p, "caster"), (member, "member")] {
        assert_eq!(accuracy(&fx, who), acc0 + 6, "{label}: {said}");
        assert_eq!(attack_power(&fx, who), ap0 + 15, "{label}: {said}");
    }
    for (who, label) in [(outsider, "outsider"), (goblin, "goblin")] {
        assert_eq!(accuracy(&fx, who), acc0, "{label}: {said}");
        assert_eq!(attack_power(&fx, who), ap0, "{label}: {said}");
    }
    // Nobody is hurt and nobody is pulled into a fight.
    for who in [p, member, outsider, goblin] {
        let hp = fx.world.get::<Health>(who).unwrap();
        assert_eq!(
            hp.hp,
            if who == goblin {
                40
            } else if who == p {
                50
            } else {
                100
            }
        );
        assert!(fx.world.get::<Fighting>(who).is_none(), "no fight started");
    }
}

#[test]
fn war_cry_alone_is_refused_with_the_chant_line() {
    let (mut fx, p, mut rx) = war_cry();
    let (acc0, ap0) = (accuracy(&fx, p), attack_power(&fx, p));
    let _ = drain(&mut rx);
    cast(&mut fx, p, "chant 'war cry'");
    let said = drain(&mut rx);
    assert!(said.contains(CHANT_REFUSAL), "{said}");
    assert_eq!((accuracy(&fx, p), attack_power(&fx, p)), (acc0, ap0));
}
