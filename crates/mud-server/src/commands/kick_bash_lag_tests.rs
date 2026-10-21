//! Kick and bash (stridera/fierymud-rs#175, #176): they roll to hit when their
//! rows carry `"attackRoll": true`, a miss still costs the use, and both put
//! the user in skill lag (`Ability.cooldown_ms`) so they cannot be spammed.
//! Test-only.

use bevy_ecs::prelude::*;
use mud_world::{
    AbilityCatalog, AbilityMessageSet, Cooldowns, Fighting, Health, Posture, PostureKind, Stamina,
};

use super::dispatch;
use super::gmcp_tests::Fx;
use super::skill_target_tests::arena;
use super::test_support::{Rx, drain};
use crate::combat::force_hit_roll;

const DAMAGE: i32 = 9;
const KICK: i32 = 71;
const BASH: i32 = 74;
const KICK_LAG_MS: i32 = 6_000;
const BASH_LAG_MS: i32 = 8_000;

/// The skill-target arena with kick and bash both rolling to hit and lagging,
/// and a standing rat in the room.
fn fixture() -> (Fx, Entity, Entity, Rx) {
    let (mut fx, p, rx) = arena();
    {
        let mut catalog = fx.world.resource_mut::<AbilityCatalog>();
        catalog.by_name.get_mut("kick").unwrap().cooldown_ms = KICK_LAG_MS;
        let mut bash =
            super::test_support::ability_def(BASH, "Bash", mud_db::abilities::AbilityKind::Skill);
        bash.violent = true;
        bash.cast_time_rounds = 0;
        bash.cooldown_ms = BASH_LAG_MS;
        catalog.by_name.insert("bash".into(), bash);
        for id in [KICK, BASH] {
            catalog.effects_for.insert(
                id,
                vec![(
                    DAMAGE,
                    Some(serde_json::json!({ "amount": 10, "attackRoll": true })),
                )],
            );
        }
        catalog.messages.insert(
            KICK,
            AbilityMessageSet {
                fail_to_caster: Some("You try to kick {target.name} but miss!".into()),
                ..Default::default()
            },
        );
        catalog.messages.insert(
            BASH,
            AbilityMessageSet {
                fail_to_caster: Some("You try to bash {target.name} but fall flat!".into()),
                ..Default::default()
            },
        );
    }
    let a = fx.a;
    let rat = fx
        .world
        .spawn((
            mud_world::Mob,
            mud_world::Named {
                name: "a rat".into(),
            },
            mud_world::Keywords(vec!["rat".into()]),
            mud_world::Located(a),
            Health { hp: 100, max: 100 },
            mud_world::CombatStats::default(),
            Posture(PostureKind::Standing),
        ))
        .id();
    (fx, p, rat, rx)
}

fn hp(world: &World, e: Entity) -> i32 {
    world.get::<Health>(e).unwrap().hp
}

fn stamina(world: &World, e: Entity) -> i32 {
    world.get::<Stamina>(e).unwrap().current
}

/// Pretend the lag of ability `id` has run out.
fn expire_lag(world: &mut World, p: Entity, id: i32) {
    let mut cd = world.get_mut::<Cooldowns>(p).expect("a lag was started");
    let past = std::time::Instant::now();
    cd.ready_at.insert(id, past);
}

#[test]
fn a_kick_that_rolls_too_low_misses_and_still_starts_the_fight() {
    let (mut fx, p, rat, mut rx) = fixture();
    force_hit_roll(Some(100));
    dispatch(&mut fx.world, p, "kick rat");
    force_hit_roll(None);
    let out = drain(&mut rx);
    assert!(out.contains("You try to kick a rat but miss!"), "{out}");
    assert_eq!(hp(&fx.world, rat), 100, "a miss does no damage: {out}");
    assert_eq!(fx.world.get::<Fighting>(p).map(|f| f.0), Some(rat));
}

#[test]
fn a_kick_that_rolls_well_lands() {
    let (mut fx, p, rat, mut rx) = fixture();
    force_hit_roll(Some(1));
    dispatch(&mut fx.world, p, "kick rat");
    force_hit_roll(None);
    let out = drain(&mut rx);
    assert!(hp(&fx.world, rat) < 100, "{out}");
    assert!(!out.contains("but miss"), "{out}");
}

#[test]
fn a_second_kick_is_refused_until_the_lag_expires() {
    let (mut fx, p, rat, mut rx) = fixture();
    force_hit_roll(Some(1));
    dispatch(&mut fx.world, p, "kick rat");
    let after_first = (hp(&fx.world, rat), stamina(&fx.world, p));
    drain(&mut rx);
    dispatch(&mut fx.world, p, "kick");
    let out = drain(&mut rx);
    assert!(out.contains("You can't use Kick yet"), "{out}");
    assert!(out.contains("remaining"), "{out}");
    assert_eq!(
        (hp(&fx.world, rat), stamina(&fx.world, p)),
        after_first,
        "no damage and no stamina spent while lagged: {out}"
    );
    expire_lag(&mut fx.world, p, KICK);
    dispatch(&mut fx.world, p, "kick");
    force_hit_roll(None);
    let out = drain(&mut rx);
    assert!(!out.contains("yet"), "{out}");
    assert!(hp(&fx.world, rat) < after_first.0, "{out}");
}

#[test]
fn a_missed_kick_pays_the_same_lag() {
    let (mut fx, p, rat, mut rx) = fixture();
    force_hit_roll(Some(100));
    dispatch(&mut fx.world, p, "kick rat");
    drain(&mut rx);
    force_hit_roll(Some(1));
    dispatch(&mut fx.world, p, "kick");
    force_hit_roll(None);
    let out = drain(&mut rx);
    assert!(out.contains("You can't use Kick yet"), "{out}");
    assert_eq!(hp(&fx.world, rat), 100, "{out}");
}

#[test]
fn a_bash_that_rolls_too_low_misses_and_leaves_the_basher_sitting() {
    let (mut fx, p, rat, mut rx) = fixture();
    force_hit_roll(Some(100));
    dispatch(&mut fx.world, p, "bash rat");
    force_hit_roll(None);
    let out = drain(&mut rx);
    assert!(
        out.contains("You try to bash a rat but fall flat!"),
        "{out}"
    );
    assert_eq!(hp(&fx.world, rat), 100, "a miss does no damage: {out}");
    assert_eq!(
        fx.world.get::<Posture>(rat).map(|x| x.0),
        Some(PostureKind::Standing),
        "the target stays up: {out}"
    );
    assert_eq!(
        fx.world.get::<Posture>(p).map(|x| x.0),
        Some(PostureKind::Sitting),
        "the basher falls: {out}"
    );
    assert_eq!(fx.world.get::<Fighting>(p).map(|f| f.0), Some(rat));
}

#[test]
fn a_bash_that_rolls_well_knocks_the_target_down() {
    let (mut fx, p, rat, mut rx) = fixture();
    force_hit_roll(Some(1));
    dispatch(&mut fx.world, p, "bash rat");
    force_hit_roll(None);
    let out = drain(&mut rx);
    assert!(out.contains("You bash a rat"), "{out}");
    assert!(hp(&fx.world, rat) < 100, "{out}");
    assert_eq!(
        fx.world.get::<Posture>(rat).map(|x| x.0),
        Some(PostureKind::Sitting),
        "{out}"
    );
    assert_eq!(
        fx.world.get::<Posture>(p).map(|x| x.0),
        Some(PostureKind::Standing)
    );
}

#[test]
fn a_second_bash_is_refused_until_the_lag_expires() {
    let (mut fx, p, rat, mut rx) = fixture();
    force_hit_roll(Some(1));
    dispatch(&mut fx.world, p, "bash rat");
    let after_first = (hp(&fx.world, rat), stamina(&fx.world, p));
    drain(&mut rx);
    dispatch(&mut fx.world, p, "bash rat");
    let out = drain(&mut rx);
    assert!(out.contains("You can't use Bash yet"), "{out}");
    assert_eq!(
        (hp(&fx.world, rat), stamina(&fx.world, p)),
        after_first,
        "no damage and no stamina spent while lagged: {out}"
    );
    expire_lag(&mut fx.world, p, BASH);
    dispatch(&mut fx.world, p, "bash rat");
    force_hit_roll(None);
    let out = drain(&mut rx);
    assert!(!out.contains("yet"), "{out}");
    assert!(hp(&fx.world, rat) < after_first.0, "{out}");
}

#[test]
fn a_missed_bash_pays_the_same_lag() {
    let (mut fx, p, _rat, mut rx) = fixture();
    force_hit_roll(Some(100));
    dispatch(&mut fx.world, p, "bash rat");
    drain(&mut rx);
    // Stand back up so the posture gate does not hide the lag gate.
    fx.world
        .entity_mut(p)
        .insert(Posture(PostureKind::Standing));
    force_hit_roll(Some(1));
    dispatch(&mut fx.world, p, "bash rat");
    force_hit_roll(None);
    let out = drain(&mut rx);
    assert!(out.contains("You can't use Bash yet"), "{out}");
}

#[test]
fn without_the_attack_roll_flag_a_bash_always_lands() {
    let (mut fx, p, rat, mut rx) = fixture();
    fx.world
        .resource_mut::<AbilityCatalog>()
        .effects_for
        .insert(
            BASH,
            vec![(DAMAGE, Some(serde_json::json!({ "amount": 10 })))],
        );
    force_hit_roll(Some(100));
    dispatch(&mut fx.world, p, "bash rat");
    force_hit_roll(None);
    let out = drain(&mut rx);
    assert!(hp(&fx.world, rat) < 100, "{out}");
}

#[test]
fn a_character_who_never_learned_bash_cannot_bash() {
    let (mut fx, p, rat, mut rx) = fixture();
    // Practiced kit that does not include bash.
    fx.world.entity_mut(p).insert(mud_world::KnownAbilities {
        entries: vec![(KICK, 500, true)],
    });
    let before = stamina(&fx.world, p);
    force_hit_roll(Some(1));
    dispatch(&mut fx.world, p, "bash rat");
    force_hit_roll(None);
    let out = drain(&mut rx);
    assert!(out.contains("You don't know how to use Bash"), "{out}");
    assert_eq!(hp(&fx.world, rat), 100, "{out}");
    assert_eq!(stamina(&fx.world, p), before, "no stamina spent: {out}");
    assert!(fx.world.get::<Fighting>(p).is_none(), "no fight started");
}

#[test]
fn a_refused_bash_does_not_stand_a_sitting_player_up() {
    let (mut fx, p, _rat, mut rx) = fixture();
    force_hit_roll(Some(100));
    dispatch(&mut fx.world, p, "bash rat");
    force_hit_roll(None);
    drain(&mut rx);
    assert_eq!(
        fx.world.get::<Posture>(p).map(|x| x.0),
        Some(PostureKind::Sitting)
    );
    dispatch(&mut fx.world, p, "bash rat");
    let out = drain(&mut rx);
    assert!(out.contains("You can't use Bash yet"), "{out}");
    assert_eq!(
        fx.world.get::<Posture>(p).map(|x| x.0),
        Some(PostureKind::Sitting),
        "the lag refusal came before the posture gate: {out}"
    );
}
