//! End-to-end casts of three abilities whose data was brought in line with
//! legacy: `INVIGORATE` (`mag_point`: full max stamina), `NATURES_EMBRACE`
//! (`mag_point`: hiddenness `skill * 5`, no heal) and `SUNRAY` (`spell_dams`:
//! 30d10 PC-vs-PC, 20d10 otherwise). The effect rows are the ones in
//! `fierylib/data/abilities.json`. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::PlayerFlag;
use mud_world::{
    CombatStats, EffectDef, Health, Located, Mob, ModifyDelta, Named, PlayerFlags, Stamina,
};

use super::blindness_tests::{DAMAGE, MODIFY, cast, caster_with_spells};
use super::gmcp_tests::{Fx, player};
use super::test_support::{Rx, drain};
use crate::hiding;

const HEAL: i32 = 14;

fn register_effect(fx: &mut Fx, id: i32, kind: &str) {
    fx.world
        .resource_mut::<mud_world::EffectCatalog>()
        .by_id
        .insert(
            id,
            EffectDef {
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
            },
        );
}

// ---- Invigorate ---------------------------------------------------------

fn invigorate_caster() -> (Fx, Entity, Rx) {
    let (mut fx, p, rx) = caster_with_spells(vec![(
        1,
        "Invigorate",
        vec![(
            HEAL,
            Some(serde_json::json!({ "resource": "move", "amount": "target_max_stamina" })),
        )],
    )]);
    register_effect(&mut fx, HEAL, "heal");
    (fx, p, rx)
}

#[test]
fn invigorate_restores_a_drained_caster_to_full_stamina() {
    let (mut fx, p, mut rx) = invigorate_caster();
    fx.world.entity_mut(p).insert(Stamina {
        current: 3,
        max: 140,
    });
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'invigorate' caster");
    let said = drain(&mut rx);
    assert_eq!(fx.world.get::<Stamina>(p).unwrap().current, 140, "{said}");
}

#[test]
fn invigorate_fills_the_targets_pool_not_the_casters() {
    let (mut fx, p, mut rx) = invigorate_caster();
    fx.world.entity_mut(p).insert(Stamina {
        current: 10,
        max: 50,
    });
    let a = fx.a;
    let (ally, _arx) = player(&mut fx.world, a, "Ally");
    fx.world.entity_mut(ally).insert((
        Health { hp: 100, max: 100 },
        CombatStats::default(),
        Stamina {
            current: 1,
            max: 400,
        },
    ));
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'invigorate' ally");
    let said = drain(&mut rx);
    assert_eq!(
        fx.world.get::<Stamina>(ally).unwrap().current,
        400,
        "{said}"
    );
    assert_eq!(fx.world.get::<Stamina>(p).unwrap().current, 10, "{said}");
}

/// Legacy `MAG_GROUP` (`skills.cpp:969`): `fierylib/data/abilities.json` carries
/// `targetScope: ROOM_ALLIES` on Invigorate, so a cast fills the caster and
/// every grouped player in the room, and nobody else.
#[test]
fn invigorate_group_scope_fills_the_caster_and_the_grouped_allies_in_the_room() {
    let (mut fx, p, mut rx) = invigorate_caster();
    fx.world
        .resource_mut::<mud_world::AbilityCatalog>()
        .by_name
        .get_mut("invigorate")
        .unwrap()
        .target_scope = "ROOM_ALLIES".to_string();
    let drained = Stamina {
        current: 1,
        max: 300,
    };
    fx.world.entity_mut(p).insert(drained);
    let (a, b) = (fx.a, fx.b);
    let mut members = Vec::new();
    for (name, room, grouped) in [
        ("Ally", a, true),
        ("Stranger", a, false),
        ("Faraway", b, true),
    ] {
        let (e, rx) = player(&mut fx.world, room, name);
        std::mem::forget(rx);
        fx.world.entity_mut(e).insert((
            Health { hp: 100, max: 100 },
            CombatStats::default(),
            drained,
        ));
        if grouped {
            fx.world
                .entity_mut(e)
                .insert((mud_world::Follower(p), mud_world::GroupMember(p)));
        }
        members.push(e);
    }
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'invigorate'");
    let said = drain(&mut rx);
    let current = |fx: &Fx, e: Entity| fx.world.get::<Stamina>(e).unwrap().current;
    assert_eq!(current(&fx, p), 300, "caster: {said}");
    assert_eq!(current(&fx, members[0]), 300, "grouped ally: {said}");
    assert_eq!(current(&fx, members[1]), 1, "ungrouped stranger: {said}");
    assert_eq!(current(&fx, members[2]), 1, "grouped but elsewhere: {said}");
}

/// The leak this guards: `follow` needs no consent, so a stalker trailing the
/// leader must not be swept up by a group spell (Group Recall, Invigorate...)
/// that the leader casts; only players who accepted an `invite` are.
#[test]
fn a_group_spell_skips_a_follower_who_never_joined_the_group() {
    let (mut fx, p, mut rx) = invigorate_caster();
    fx.world
        .resource_mut::<mud_world::AbilityCatalog>()
        .by_name
        .get_mut("invigorate")
        .unwrap()
        .target_scope = "ROOM_ALLIES".to_string();
    let drained = Stamina {
        current: 1,
        max: 300,
    };
    let a = fx.a;
    let mut rxs = Vec::new();
    let mut people = Vec::new();
    for name in ["Member", "Mallory"] {
        let (e, mrx) = player(&mut fx.world, a, name);
        rxs.push(mrx);
        fx.world.entity_mut(e).insert((
            Health { hp: 100, max: 100 },
            CombatStats::default(),
            drained,
        ));
        people.push(e);
    }
    let (member, mallory) = (people[0], people[1]);
    // Mallory just follows; Member is invited and accepts.
    super::dispatch(&mut fx.world, mallory, "follow caster");
    super::dispatch(&mut fx.world, p, "invite member");
    super::dispatch(&mut fx.world, member, "accept");
    assert!(
        fx.world.get::<mud_world::Follower>(mallory).is_some(),
        "control: Mallory is following the caster"
    );
    fx.world.entity_mut(p).insert(drained);
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'invigorate'");
    let said = drain(&mut rx);
    let current = |fx: &Fx, e: Entity| fx.world.get::<Stamina>(e).unwrap().current;
    assert_eq!(current(&fx, p), 300, "caster: {said}");
    assert_eq!(current(&fx, member), 300, "grouped member: {said}");
    assert_eq!(current(&fx, mallory), 1, "mere follower untouched: {said}");
}

// ---- Nature's Embrace ---------------------------------------------------

fn embrace_caster() -> (Fx, Entity, Rx) {
    let (fx, p, rx) = caster_with_spells(vec![(
        1,
        "Natures Embrace",
        vec![(
            MODIFY,
            Some(serde_json::json!({
                "target": "hiddenness",
                "amount": "skill * 5",
                "duration": "skill / 3 + 1",
                "durationUnit": "hours"
            })),
        )],
    )]);
    (fx, p, rx)
}

#[test]
fn natures_embrace_raises_hiddenness_by_five_per_skill_point_and_does_not_heal() {
    let (mut fx, p, mut rx) = embrace_caster();
    fx.world.get_mut::<Health>(p).unwrap().hp = 20;
    assert_eq!(hiding::hiddenness(&fx.world, p), 0);
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'natures embrace' caster");
    let said = drain(&mut rx);
    // The fixture caster's skill resolves to 50: 50 * 5.
    assert_eq!(hiding::hiddenness(&fx.world, p), 250, "{said}");
    assert_eq!(fx.world.get::<Health>(p).unwrap().hp, 20, "no heal: {said}");
    let mut q = fx.world.query::<(
        &mud_world::EffectInstance,
        &mud_world::AppliedTo,
        &ModifyDelta,
    )>();
    let rows: Vec<_> = q
        .iter(&fx.world)
        .filter(|(_, a, _)| a.0 == p)
        .map(|(i, _, d)| (i.remaining_secs, d.target.clone(), d.amount))
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].0 > 0, "has a duration: {rows:?}");
    assert_eq!((rows[0].1.as_str(), rows[0].2), ("hiddenness", 250));
}

#[test]
fn casting_it_while_hidden_starts_over_from_the_spell_amount() {
    // Casting is a noisy command: it drops the old hiding, then the spell
    // adds skill * 5 (the 0..=1000 clamp is covered where modify keys are).
    let (mut fx, p, _rx) = embrace_caster();
    fx.world.entity_mut(p).insert(mud_world::Hiddenness(900));
    cast(&mut fx, p, "cast 'natures embrace' caster");
    assert_eq!(hiding::hiddenness(&fx.world, p), 250);
}

// ---- Sunray -------------------------------------------------------------

const SUNRAY_AMOUNT: &str =
    "roll_dice(20 + 10 * actor_is_player * target_is_player, 10) + (pow(skill, 2) * 7) / 400";

fn sunray_caster() -> (Fx, Entity, Rx) {
    let (mut fx, p, rx) = caster_with_spells(vec![(
        1,
        "Sunray",
        vec![(
            DAMAGE,
            Some(serde_json::json!({ "type": "fire", "amount": SUNRAY_AMOUNT })),
        )],
    )]);
    fx.world
        .entity_mut(p)
        .insert(PlayerFlags(vec![PlayerFlag::PkEnabled]));
    (fx, p, rx)
}

/// Damage of each of `n` Sunrays cast at `target` (fixture skill 50: +43).
fn sunray_rolls(fx: &mut Fx, p: Entity, target: Entity, line: &str, n: usize) -> Vec<i32> {
    let mut out = Vec::new();
    for _ in 0..n {
        fx.world.get_mut::<Health>(target).unwrap().hp = 100_000;
        cast(fx, p, line);
        out.push(100_000 - fx.world.get::<Health>(target).unwrap().hp);
    }
    out
}

#[test]
fn sunray_rolls_30d10_against_a_player_and_20d10_against_a_mob() {
    const SKILL_BONUS: i32 = 43; // 50^2 * 7 / 400
    const N: usize = 40;
    let (mut fx, p, _rx) = sunray_caster();
    let a = fx.a;
    let rat = fx
        .world
        .spawn((
            Mob,
            Named {
                name: "a sewer rat".into(),
            },
            Located(a),
            CombatStats::default(),
            Health {
                hp: 100_000,
                max: 100_000,
            },
        ))
        .id();
    let (victim, _vrx) = player(&mut fx.world, a, "Victim");
    fx.world.entity_mut(victim).insert((
        Health {
            hp: 100_000,
            max: 100_000,
        },
        CombatStats::default(),
        PlayerFlags(vec![PlayerFlag::PkEnabled]),
    ));

    let vs_mob = sunray_rolls(&mut fx, p, rat, "cast 'sunray' rat", N);
    let vs_player = sunray_rolls(&mut fx, p, victim, "cast 'sunray' victim", N);
    assert!(
        vs_mob.iter().all(|d| *d > 0),
        "every cast lands: {vs_mob:?}"
    );
    assert!(
        vs_player.iter().all(|d| *d > 0),
        "every cast lands: {vs_player:?}"
    );
    // Hard bounds of the dice.
    assert!(
        vs_mob
            .iter()
            .all(|d| (20 + SKILL_BONUS..=200 + SKILL_BONUS).contains(d)),
        "20d10 + bonus: {vs_mob:?}"
    );
    assert!(
        vs_player
            .iter()
            .all(|d| (30 + SKILL_BONUS..=300 + SKILL_BONUS).contains(d)),
        "30d10 + bonus: {vs_player:?}"
    );
    // Means: 110 + 43 against the mob, 165 + 43 against the player.
    let mean = |v: &[i32]| v.iter().sum::<i32>() / i32::try_from(v.len()).unwrap();
    assert!(mean(&vs_mob) < 180, "mob mean {}", mean(&vs_mob));
    assert!(mean(&vs_player) > 180, "player mean {}", mean(&vs_player));
}

// ---- TAR_OUTDOORS (legacy spell_parser.cpp:1559) -------------------------

const TOO_ENCLOSED: &str = "This area is too enclosed to cast that spell!";

fn outdoors_rule() -> serde_json::Value {
    serde_json::json!({ "type": "outdoors", "message": TOO_ENCLOSED })
}

fn require_outdoors(fx: &mut Fx, ability: &str) {
    let mut catalog = fx.world.resource_mut::<mud_world::AbilityCatalog>();
    let id = catalog.by_name[ability].id;
    catalog.restriction_rules.insert(id, vec![outdoors_rule()]);
}

fn embrace_in(
    sector: Option<mud_db::enums::Sector>,
    flagged_indoors: bool,
) -> (Fx, Entity, String) {
    let (mut fx, p, mut rx) = embrace_caster();
    require_outdoors(&mut fx, "natures embrace");
    let room = fx.a;
    if let Some(sector) = sector {
        fx.world
            .entity_mut(room)
            .insert(mud_world::RoomSector(sector));
    }
    if flagged_indoors {
        fx.world.entity_mut(room).insert(mud_world::IndoorRoom);
    }
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'natures embrace' caster");
    let said = drain(&mut rx);
    (fx, p, said)
}

#[test]
fn outdoors_spell_is_refused_in_an_indoors_flagged_room_with_the_legacy_line() {
    let (fx, p, said) = embrace_in(Some(mud_db::enums::Sector::Forest), true);
    assert!(said.contains(TOO_ENCLOSED), "{said}");
    assert_eq!(hiding::hiddenness(&fx.world, p), 0, "no effect: {said}");
}

#[test]
fn outdoors_spell_is_refused_in_underdark_and_underwater_sectors() {
    for sector in [
        mud_db::enums::Sector::Underdark,
        mud_db::enums::Sector::Underwater,
    ] {
        let (fx, p, said) = embrace_in(Some(sector), false);
        assert!(said.contains(TOO_ENCLOSED), "{sector:?}: {said}");
        assert_eq!(hiding::hiddenness(&fx.world, p), 0, "{sector:?}");
    }
}

#[test]
fn outdoors_spell_works_outside_and_in_an_unflagged_structure_like_legacy() {
    // Legacy INDOORS(): ROOM_INDOORS, underdark, underwater. A STRUCTURE or
    // CAVE room the builder did not flag indoors counts as outdoors.
    for sector in [
        None,
        Some(mud_db::enums::Sector::Forest),
        Some(mud_db::enums::Sector::Structure),
        Some(mud_db::enums::Sector::Cave),
    ] {
        let (fx, p, said) = embrace_in(sector, false);
        assert!(!said.contains(TOO_ENCLOSED), "{sector:?}: {said}");
        assert_eq!(hiding::hiddenness(&fx.world, p), 250, "{sector:?}: {said}");
    }
}

#[test]
fn outdoors_room_wide_spell_is_refused_once_before_anyone_is_touched() {
    let (mut fx, p, mut rx) = invigorate_caster();
    {
        let mut catalog = fx.world.resource_mut::<mud_world::AbilityCatalog>();
        catalog.by_name.get_mut("invigorate").unwrap().target_scope = "ROOM_ALLIES".to_string();
    }
    require_outdoors(&mut fx, "invigorate");
    let a = fx.a;
    fx.world.entity_mut(a).insert(mud_world::IndoorRoom);
    fx.world.entity_mut(p).insert(Stamina {
        current: 2,
        max: 90,
    });
    let (ally, arx) = player(&mut fx.world, a, "Ally");
    std::mem::forget(arx);
    fx.world.entity_mut(ally).insert((
        Health { hp: 100, max: 100 },
        CombatStats::default(),
        Stamina {
            current: 2,
            max: 90,
        },
        mud_world::Follower(p),
        mud_world::GroupMember(p),
    ));
    let _ = drain(&mut rx);
    cast(&mut fx, p, "cast 'invigorate'");
    let said = drain(&mut rx);
    assert_eq!(said.matches(TOO_ENCLOSED).count(), 1, "{said}");
    assert_eq!(fx.world.get::<Stamina>(p).unwrap().current, 2, "{said}");
    assert_eq!(fx.world.get::<Stamina>(ally).unwrap().current, 2, "{said}");
}
