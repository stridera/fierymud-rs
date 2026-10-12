//! Glory (legacy "Glory rework", stridera/fierymud#405). Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::MobBehavior;
use mud_world::movement::GloryWitnessQueue;
use mud_world::{
    CombatStats, EffectInstance, EffectSource, Fighting, Glory, Health, Located, Mob, MobBehaviors,
    Named, PeacefulRoom, Posture, PostureKind,
};

use super::*;
use crate::commands::gmcp_tests::{Fx, fixture, player};
use crate::commands::test_support::{Rx, drain, make_aggro_target};

/// Room A: a glorion (`glorion`), an ordinary aggro-target player, and a
/// hostile wolf. Room B is empty.
struct Scene {
    fx: Fx,
    glorion: Entity,
    other: Entity,
    wolf: Entity,
    glorion_rx: Rx,
    other_rx: Rx,
}

fn wolf_in(world: &mut World, room: Entity) -> Entity {
    world
        .spawn((
            Mob,
            Named {
                name: "a wolf".into(),
            },
            Located(room),
            CombatStats {
                alignment: -1000,
                ..CombatStats::default()
            },
            Health { hp: 50, max: 50 },
            Posture(PostureKind::Standing),
        ))
        .id()
}

fn scene() -> Scene {
    let mut fx = fixture();
    fx.world.insert_resource(GloryWitnessQueue::default());
    let a = fx.a;
    let (glorion, glorion_rx) = player(&mut fx.world, a, "Shiny");
    make_aggro_target(&mut fx.world, glorion);
    fx.world.entity_mut(glorion).insert(Glory);
    let (other, other_rx) = player(&mut fx.world, a, "Plain");
    make_aggro_target(&mut fx.world, other);
    let wolf = wolf_in(&mut fx.world, a);
    Scene {
        fx,
        glorion,
        other,
        wolf,
        glorion_rx,
        other_rx,
    }
}

fn pin(see_through: Option<i32>, awe: Option<i32>) {
    FORCED_SEE_THROUGH_ROLL.with(|c| c.set(see_through));
    FORCED_AWE_ROLL.with(|c| c.set(awe));
}

fn fighting(world: &World, e: Entity) -> bool {
    world.get::<Fighting>(e).is_some()
}

fn wait_out(world: &mut World) {
    for _ in 0..WITNESS_DELAY_TICKS {
        glory_tick(world);
    }
}

#[test]
fn a_glorion_in_the_room_cancels_the_aggro_check() {
    let mut s = scene();
    let a = s.fx.a;
    crate::commands::try_engage_aggressive_mob(&mut s.fx.world, s.other, a);
    assert!(
        !fighting(&s.fx.world, s.wolf),
        "the wolf holds back for the whole room"
    );
    // Control: without the glory the same wolf attacks.
    s.fx.world.entity_mut(s.glorion).remove::<Glory>();
    crate::commands::try_engage_aggressive_mob(&mut s.fx.world, s.other, a);
    assert!(fighting(&s.fx.world, s.wolf));
}

#[test]
fn a_glorion_the_mob_cannot_see_does_not_stop_it() {
    let mut s = scene();
    let a = s.fx.a;
    // The glorion hides from the wolf with invisibility.
    mud_world::mob_effects::install_flag_marker(&mut s.fx.world, s.glorion, "invisible");
    assert!(!blocks_aggro(&s.fx.world, s.wolf), "invisible glorion");
    crate::commands::try_engage_aggressive_mob(&mut s.fx.world, s.other, a);
    assert!(fighting(&s.fx.world, s.wolf));
}

#[test]
fn find_glorions_lists_only_visible_glorions_and_never_the_mob() {
    let mut s = scene();
    assert_eq!(find_glorions(&s.fx.world, s.wolf), vec![s.glorion]);
    // A non-mob has no glorions to be distracted by.
    assert!(find_glorions(&s.fx.world, s.other).is_empty());
    // A glorion mob is nobody's ally, so it does not count.
    s.fx.world.entity_mut(s.wolf).insert(Glory);
    let wolf2 = wolf_in(&mut s.fx.world, s.fx.a);
    assert_eq!(find_glorions(&s.fx.world, wolf2), vec![s.glorion]);
}

#[test]
fn a_mob_that_sees_through_the_glory_attacks_the_glorion() {
    let mut s = scene();
    pin(Some(2), None);
    witness_glory(&mut s.fx.world, s.glorion);
    pin(None, None);
    assert!(fighting(&s.fx.world, s.wolf));
    assert_eq!(s.fx.world.get::<Fighting>(s.wolf).unwrap().0, s.glorion);
    let said = drain(&mut s.glorion_rx);
    assert!(
        said.contains("The look of awe in a wolf's eyes falters, and it attacks!"),
        "{said}"
    );
    let seen = drain(&mut s.other_rx);
    assert!(
        seen.contains(
            "A wolf forgets its appreciation of Shiny's glorious appearance, and attacks!"
        ),
        "{seen}"
    );
}

#[test]
fn a_roll_of_three_or_more_keeps_the_glory_up() {
    let mut s = scene();
    pin(Some(3), Some(5));
    witness_glory(&mut s.fx.world, s.glorion);
    assert!(!fighting(&s.fx.world, s.wolf));
    assert!(drain(&mut s.glorion_rx).is_empty(), "no awe line on a 5");
    // One in eight shows the awe message.
    pin(Some(3), Some(1));
    witness_glory(&mut s.fx.world, s.glorion);
    pin(None, None);
    assert!(!fighting(&s.fx.world, s.wolf));
    let said = drain(&mut s.glorion_rx);
    assert!(said.contains("A wolf gazes at you in wonder."), "{said}");
    let seen = drain(&mut s.other_rx);
    assert!(
        seen.contains("A wolf looks upon Shiny with awe in its eyes."),
        "{seen}"
    );
}

#[test]
fn keepers_of_the_peace_and_staff_are_never_distracted_into_attacking() {
    let mut s = scene();
    for behavior in [
        MobBehavior::Peacekeeper,
        MobBehavior::Helper,
        MobBehavior::Peaceful,
        MobBehavior::Protector,
    ] {
        s.fx.world
            .entity_mut(s.wolf)
            .insert(MobBehaviors(vec![behavior]));
        pin(Some(1), None);
        witness_glory(&mut s.fx.world, s.glorion);
        pin(None, None);
        assert!(!fighting(&s.fx.world, s.wolf), "{behavior:?}");
    }
    // Staff (legacy NOHASSLE) are not picked on either.
    s.fx.world.entity_mut(s.wolf).remove::<MobBehaviors>();
    s.fx.world
        .get_mut::<mud_world::Account>(s.glorion)
        .unwrap()
        .role = mud_db::enums::UserRole::Implementor;
    pin(Some(1), None);
    witness_glory(&mut s.fx.world, s.glorion);
    pin(None, None);
    assert!(!fighting(&s.fx.world, s.wolf));
}

#[test]
fn nobody_is_distracted_in_a_peaceful_room_or_while_asleep() {
    let mut s = scene();
    let a = s.fx.a;
    s.fx.world.entity_mut(a).insert(PeacefulRoom);
    pin(Some(1), None);
    witness_glory(&mut s.fx.world, s.glorion);
    assert!(!fighting(&s.fx.world, s.wolf), "peaceful room");
    s.fx.world.entity_mut(a).remove::<PeacefulRoom>();
    s.fx.world
        .entity_mut(s.wolf)
        .insert(Posture(PostureKind::Sleeping));
    witness_glory(&mut s.fx.world, s.glorion);
    assert!(!fighting(&s.fx.world, s.wolf), "sleeping mob");
    s.fx.world
        .entity_mut(s.wolf)
        .insert(Posture(PostureKind::Standing));
    s.fx.world
        .entity_mut(s.glorion)
        .insert(Posture(PostureKind::Sleeping));
    witness_glory(&mut s.fx.world, s.glorion);
    pin(None, None);
    assert!(!fighting(&s.fx.world, s.wolf), "sleeping glorion");
}

#[test]
fn a_mob_arriving_among_glorions_rolls_against_one_of_them() {
    let mut s = scene();
    let b = s.fx.b;
    let newcomer = wolf_in(&mut s.fx.world, b);
    s.fx.world.entity_mut(s.wolf).despawn();
    crate::combat::relocate(&mut s.fx.world, newcomer, s.fx.a);
    assert_eq!(
        s.fx.world.resource::<GloryWitnessQueue>().entries,
        vec![(newcomer, 0)],
        "the arrival is queued"
    );
    pin(Some(1), None);
    glory_tick(&mut s.fx.world);
    assert!(!fighting(&s.fx.world, newcomer), "not before the delay");
    wait_out(&mut s.fx.world);
    pin(None, None);
    assert!(fighting(&s.fx.world, newcomer));
    assert_eq!(s.fx.world.get::<Fighting>(newcomer).unwrap().0, s.glorion);
    assert!(
        s.fx.world
            .resource::<GloryWitnessQueue>()
            .entries
            .is_empty()
    );
}

#[test]
fn a_glorion_walking_in_exposes_the_mobs_after_the_delay() {
    let mut s = scene();
    let b = s.fx.b;
    // The glorion walks out and back; only the arrival has mobs to meet.
    crate::combat::relocate(&mut s.fx.world, s.glorion, b);
    crate::combat::relocate(&mut s.fx.world, s.glorion, s.fx.a);
    assert_eq!(
        s.fx.world.resource::<GloryWitnessQueue>().entries,
        vec![(s.glorion, 0)],
        "one queue entry per mover, restarted by the second move"
    );
    pin(Some(2), None);
    wait_out(&mut s.fx.world);
    pin(None, None);
    assert!(fighting(&s.fx.world, s.wolf));
}

#[test]
fn players_without_glory_queue_nothing_when_they_move() {
    let mut s = scene();
    let b = s.fx.b;
    crate::combat::relocate(&mut s.fx.world, s.other, b);
    assert!(
        s.fx.world
            .resource::<GloryWitnessQueue>()
            .entries
            .is_empty()
    );
    // And moving within the same room is not a move.
    let a = s.fx.a;
    crate::combat::relocate(&mut s.fx.world, s.glorion, a);
    assert!(
        s.fx.world
            .resource::<GloryWitnessQueue>()
            .entries
            .is_empty()
    );
}

#[test]
fn the_check_is_once_per_exposure_not_once_per_pulse() {
    let mut s = scene();
    // Glory holds on the one exposure; the pulse that follows then never
    // re-rolls, however many pulses pass.
    pin(Some(50), Some(5));
    witness_glory(&mut s.fx.world, s.glorion);
    pin(Some(1), None);
    let a = s.fx.a;
    for _ in 0..20 {
        crate::commands::try_engage_aggressive_mob(&mut s.fx.world, s.other, a);
    }
    pin(None, None);
    assert!(!fighting(&s.fx.world, s.wolf));
}

#[test]
fn casting_glory_gives_the_marker_and_queues_the_exposure() {
    use crate::commands::blindness_tests::{STATUS, cast, caster_with_spells};
    let (mut fx, p, _rx) = caster_with_spells(vec![(
        1,
        "Glory",
        vec![(
            STATUS,
            Some(serde_json::json!({
                "flag": "glory", "duration": "5 + (skill / 20)", "durationUnit": "hours"
            })),
        )],
    )]);
    fx.world.insert_resource(GloryWitnessQueue::default());
    let a = fx.a;
    let wolf = wolf_in(&mut fx.world, a);
    cast(&mut fx, p, "cast 'glory'");
    assert!(fx.world.get::<Glory>(p).is_some(), "marker installed");
    assert_eq!(
        fx.world.resource::<GloryWitnessQueue>().entries,
        vec![(p, 0)]
    );
    pin(Some(1), None);
    wait_out(&mut fx.world);
    pin(None, None);
    assert!(fighting(&fx.world, wolf), "the wolf sees through it");
}

#[test]
fn attacking_shatters_the_attackers_glory() {
    let mut s = scene();
    s.fx.world.spawn((
        EffectInstance {
            kind: 4,
            name: "glory".into(),
            strength: 1,
            remaining_secs: 600,
            source: EffectSource::Spell,
            ability_id: Some(1),
        },
        mud_world::AppliedTo(s.glorion),
    ));
    let target = s.wolf;
    crate::commands::apply_attacker_damage(&mut s.fx.world, target, 3, s.glorion);
    assert!(s.fx.world.get::<Glory>(s.glorion).is_none());
    let mut q = s.fx.world.query::<&EffectInstance>();
    assert_eq!(q.iter(&s.fx.world).filter(|i| i.name == "glory").count(), 0);
}

#[test]
fn re_queueing_restarts_the_wait() {
    let mut q = GloryWitnessQueue::default();
    let e = Entity::from_raw_u32(7).unwrap();
    q.queue(e);
    q.entries[0].1 = 6;
    q.queue(e);
    assert_eq!(q.entries, vec![(e, 0)]);
}
