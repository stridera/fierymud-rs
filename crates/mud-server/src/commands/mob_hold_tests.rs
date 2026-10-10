//! Mobs that are paralysed, mesmerized, stunned or asleep take no part in
//! the world (issues #100, #105, #110): they do not open fights, hold a
//! grudge, re-engage from a hate list, follow a leader or assist, and
//! paralysis itself ends the victim's fight and every fight against it.
//! Legacy `mobile_activity` / `mob_attack` skip the same mobs. Test-only.

use bevy_ecs::prelude::*;
use mud_world::{
    CombatStats, Fighting, Follower, Health, Located, Mob, Named, Posture, PostureKind, Room,
    Stunned,
};

use super::blindness_tests::{EffectRows, STATUS, cast, caster_with_spells};
use super::test_support::{Rx, drain, make_aggro_target, player_in};
use super::{engage_combat, recheck_aggro_in_room, try_engage_aggressive_mob};
use crate::combat::{HateList, MobMemory};

/// One way a mob can be unable to act.
#[derive(Clone, Copy, Debug)]
enum Hold {
    /// Paralysis, mesmerize and stun all install this marker.
    Stunned,
    Asleep,
    Sitting,
    Resting,
}

/// Holds that really stop a mob. Sitting and resting do not: the mob gets up
/// and fights (legacy only needs it awake), see the tests at the bottom.
const HOLDS: [Hold; 2] = [Hold::Stunned, Hold::Asleep];

/// Postures a mob stands up from when it engages.
const SEATED: [Hold; 2] = [Hold::Sitting, Hold::Resting];

fn apply(world: &mut World, mob: Entity, hold: Hold) {
    let mut em = world.entity_mut(mob);
    match hold {
        Hold::Stunned => em.insert(Stunned),
        Hold::Asleep => em.insert(Posture(PostureKind::Sleeping)),
        Hold::Sitting => em.insert(Posture(PostureKind::Sitting)),
        Hold::Resting => em.insert(Posture(PostureKind::Resting)),
    };
}

struct Fx {
    world: World,
    room: Entity,
    player: Entity,
    rx: Rx,
}

fn fixture() -> Fx {
    let mut world = World::new();
    world.insert_resource(mud_world::WeatherCatalog::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(crate::TickCount(1));
    let room = world.spawn(Room).id();
    let (player, rx) = player_in(&mut world, room);
    make_aggro_target(&mut world, player);
    Fx {
        world,
        room,
        player,
        rx,
    }
}

fn wolf(fx: &mut Fx, name: &str) -> Entity {
    fx.world
        .spawn((
            Mob,
            Named { name: name.into() },
            Located(fx.room),
            CombatStats {
                alignment: -1000,
                ..CombatStats::default()
            },
            Health { hp: 50, max: 50 },
            Posture(PostureKind::Standing),
        ))
        .id()
}

#[test]
fn a_mob_that_cannot_act_never_aggros() {
    for hold in HOLDS {
        let mut fx = fixture();
        let wolf = wolf(&mut fx, "a wolf");
        apply(&mut fx.world, wolf, hold);
        let _ = drain(&mut fx.rx);
        crate::commands::aggro_pulse(&mut fx.world);
        assert!(fx.world.get::<Fighting>(wolf).is_none(), "{hold:?}");
        assert!(fx.world.get::<Fighting>(fx.player).is_none(), "{hold:?}");
        let out = drain(&mut fx.rx);
        assert!(!out.contains("attacks"), "{hold:?}: {out}");
    }
}

#[test]
fn an_able_mob_still_aggros_past_a_held_one() {
    let mut fx = fixture();
    let held = wolf(&mut fx, "a sleeping wolf");
    apply(&mut fx.world, held, Hold::Asleep);
    let fresh = wolf(&mut fx, "a hungry wolf");
    crate::commands::aggro_pulse(&mut fx.world);
    assert!(fx.world.get::<Fighting>(held).is_none());
    assert_eq!(
        fx.world.get::<Fighting>(fresh).map(|f| f.0),
        Some(fx.player)
    );
}

#[test]
fn a_held_mob_holds_no_grudge_until_it_can_act() {
    for hold in HOLDS {
        let mut fx = fixture();
        let wolf = wolf(&mut fx, "a wolf");
        // Not aggressive on its own: only its memory of the player drives it.
        fx.world.entity_mut(wolf).insert(CombatStats::default());
        let mut memory = MobMemory::default();
        memory.0.insert(fx.player);
        fx.world.entity_mut(wolf).insert(memory);
        apply(&mut fx.world, wolf, hold);
        recheck_aggro_in_room(&mut fx.world, fx.player);
        assert!(fx.world.get::<Fighting>(wolf).is_none(), "{hold:?}");
        assert!(fx.world.get::<Fighting>(fx.player).is_none(), "{hold:?}");
    }
    // Control: the same grudge fires once the mob is upright and able.
    let mut fx = fixture();
    let wolf = wolf(&mut fx, "a wolf");
    fx.world.entity_mut(wolf).insert(CombatStats::default());
    let mut memory = MobMemory::default();
    memory.0.insert(fx.player);
    fx.world.entity_mut(wolf).insert(memory);
    recheck_aggro_in_room(&mut fx.world, fx.player);
    assert_eq!(fx.world.get::<Fighting>(wolf).map(|f| f.0), Some(fx.player));
}

#[test]
fn a_held_mob_cannot_engage_but_a_player_can_engage_it() {
    for hold in HOLDS {
        let mut fx = fixture();
        let wolf = wolf(&mut fx, "a wolf");
        apply(&mut fx.world, wolf, hold);
        let room = fx.room;
        engage_combat(&mut fx.world, wolf, fx.player, room);
        assert!(fx.world.get::<Fighting>(wolf).is_none(), "{hold:?}");
        assert!(fx.world.get::<Fighting>(fx.player).is_none(), "{hold:?}");
        // The player attacking the held mob (a damaging spell, say) still
        // opens a fight, so the mob answers once it can.
        engage_combat(&mut fx.world, fx.player, wolf, room);
        assert_eq!(
            fx.world.get::<Fighting>(fx.player).map(|f| f.0),
            Some(wolf),
            "{hold:?}"
        );
    }
}

#[test]
fn a_held_mob_does_not_turn_on_its_hate_list() {
    for hold in HOLDS {
        let mut fx = fixture();
        let wolf = wolf(&mut fx, "a wolf");
        fx.world.entity_mut(wolf).insert(CombatStats::default());
        let mut hate = HateList::default();
        hate.push(fx.player);
        fx.world.entity_mut(wolf).insert(hate);
        apply(&mut fx.world, wolf, hold);
        fx.world.insert_resource(crate::TickCount(0));
        crate::combat::combat_tick(&mut fx.world);
        assert!(fx.world.get::<Fighting>(wolf).is_none(), "{hold:?}");
        let out = drain(&mut fx.rx);
        assert!(!out.contains("turns its hate"), "{hold:?}: {out}");
    }
}

#[test]
fn a_held_mob_does_not_follow_its_leader() {
    use mud_db::enums::{Direction, ExitState};
    for (hold, follows) in [
        (None, true),
        (Some(Hold::Stunned), false),
        (Some(Hold::Asleep), false),
    ] {
        let mut fx = fixture();
        let dest = fx.world.spawn(Room).id();
        let mut exits = mud_world::Exits::default();
        exits.0.insert(
            Direction::North,
            mud_world::ExitData {
                to: Some(dest),
                state: ExitState::Open,
                key: None,
                description: None,
                keywords: Vec::new(),
                is_hidden: false,
                is_pickproof: false,
                is_bashable: false,
                hit_points: None,
            },
        );
        fx.world.entity_mut(fx.room).insert(exits);
        let pet = wolf(&mut fx, "a pet wolf");
        fx.world
            .entity_mut(pet)
            .insert((Follower(fx.player), CombatStats::default()));
        if let Some(hold) = hold {
            apply(&mut fx.world, pet, hold);
        }
        super::dispatch(&mut fx.world, fx.player, "north");
        let at = fx.world.get::<Located>(pet).map(|l| l.0);
        assert_eq!(at == Some(dest), follows, "{hold:?}");
    }
}

#[test]
fn a_held_helper_does_not_assist() {
    use mud_db::enums::MobBehavior;
    for hold in HOLDS {
        let mut fx = fixture();
        let room = fx.room;
        let ogre = wolf(&mut fx, "an ogre");
        fx.world.entity_mut(ogre).insert(CombatStats::default());
        let (friend, _frx) = player_in(&mut fx.world, room);
        make_aggro_target(&mut fx.world, friend);
        fx.world.entity_mut(ogre).insert(Fighting(friend));
        fx.world.entity_mut(friend).insert(Fighting(ogre));
        let guard = wolf(&mut fx, "a guard");
        fx.world.entity_mut(guard).insert((
            CombatStats::default(),
            mud_world::MobBehaviors(vec![MobBehavior::Helper]),
        ));
        apply(&mut fx.world, guard, hold);
        super::mob_helpers_engage(&mut fx.world, ogre, friend, room);
        super::mob_assist_pulse(&mut fx.world);
        assert!(fx.world.get::<Fighting>(guard).is_none(), "{hold:?}");
    }
}

fn status_row(flag: &str) -> EffectRows {
    vec![(
        STATUS,
        Some(serde_json::json!({
            "flag": flag,
            "duration": 600,
            "breakOnDamage": true,
        })),
    )]
}

/// Caster "Caster" (knows Minor Paralysis and Mesmerize) and an ogre that
/// is already fighting it.
fn fight_with_spells() -> (super::gmcp_tests::Fx, Entity, Rx, Entity) {
    let (mut fx, caster, rx) = caster_with_spells(vec![
        (1, "Minor Paralysis", status_row("paralyzed")),
        (2, "Mesmerize", status_row("mesmerized")),
    ]);
    let a = fx.a;
    let ogre = fx
        .world
        .spawn((
            Mob,
            Named {
                name: "an ogre".into(),
            },
            Located(a),
            CombatStats::default(),
            Health { hp: 500, max: 500 },
            Posture(PostureKind::Standing),
        ))
        .id();
    fx.world.entity_mut(ogre).insert(Fighting(caster));
    fx.world.entity_mut(caster).insert(Fighting(ogre));
    (fx, caster, rx, ogre)
}

#[test]
fn paralysis_and_mesmerize_end_the_victims_fight_on_both_sides() {
    for spell in ["minor paralysis", "mesmerize"] {
        let (mut fx, caster, mut rx, ogre) = fight_with_spells();
        let _ = drain(&mut rx);
        cast(&mut fx, caster, &format!("cast '{spell}' ogre"));
        let out = drain(&mut rx);
        assert!(
            fx.world.get::<Stunned>(ogre).is_some(),
            "{spell}: held {out}"
        );
        assert!(fx.world.get::<Fighting>(ogre).is_none(), "{spell}: {out}");
        assert!(
            fx.world.get::<Fighting>(caster).is_none(),
            "{spell}: the caster stops attacking it too: {out}"
        );
    }
}

/// The #100 reproduction: an aggro mob caught by Minor Paralysis neither
/// engages nor keeps the player from leaving.
#[test]
fn a_paralysed_aggro_mob_does_not_engage_the_caster() {
    let (mut fx, caster, mut rx, ogre) = fight_with_spells();
    fx.world.entity_mut(ogre).remove::<Fighting>();
    fx.world.entity_mut(caster).remove::<Fighting>();
    fx.world.entity_mut(ogre).insert(CombatStats {
        alignment: -1000,
        ..CombatStats::default()
    });
    make_aggro_target(&mut fx.world, caster);
    cast(&mut fx, caster, "cast 'minor paralysis' ogre");
    let _ = drain(&mut rx);
    assert!(fx.world.get::<Stunned>(ogre).is_some());
    let a = fx.a;
    try_engage_aggressive_mob(&mut fx.world, caster, a);
    crate::commands::aggro_pulse(&mut fx.world);
    assert!(fx.world.get::<Fighting>(ogre).is_none());
    assert!(fx.world.get::<Fighting>(caster).is_none());
    // And no round of combat finds anything to swing at.
    fx.world.insert_resource(crate::TickCount(0));
    crate::combat::combat_tick(&mut fx.world);
    assert!(!drain(&mut rx).contains("ogre"), "no blows");
    let hp = fx.world.get::<Health>(caster).unwrap().hp;
    assert_eq!(hp, 100, "the caster took no damage");
}

// -- the Sleep spell (#110) ----------------------------------------------------

fn sleep_fight() -> (super::gmcp_tests::Fx, Entity, Rx, Entity) {
    let (mut fx, caster, rx) = caster_with_spells(vec![(1, "Sleep", status_row("sleeping"))]);
    let a = fx.a;
    let ogre = fx
        .world
        .spawn((
            Mob,
            Named {
                name: "an ogre".into(),
            },
            Located(a),
            CombatStats {
                alignment: -1000,
                ..CombatStats::default()
            },
            Health { hp: 500, max: 500 },
            Posture(PostureKind::Standing),
        ))
        .id();
    (fx, caster, rx, ogre)
}

fn posture_of(world: &World, e: Entity) -> Option<PostureKind> {
    world.get::<Posture>(e).map(|p| p.0)
}

#[test]
fn the_sleep_spell_puts_a_mob_to_sleep_so_it_neither_aggros_nor_wakes_by_itself() {
    let (mut fx, caster, mut rx, ogre) = sleep_fight();
    make_aggro_target(&mut fx.world, caster);
    cast(&mut fx, caster, "cast 'sleep' ogre");
    let _ = drain(&mut rx);
    assert_eq!(posture_of(&fx.world, ogre), Some(PostureKind::Sleeping));
    // An aggro mob that is asleep opens no fight.
    crate::commands::aggro_pulse(&mut fx.world);
    assert!(fx.world.get::<Fighting>(ogre).is_none());
    assert!(fx.world.get::<Fighting>(caster).is_none());
}

#[test]
fn a_hit_or_the_spell_running_out_wakes_the_sleeper() {
    // Expiry.
    let (mut fx, caster, _rx, ogre) = sleep_fight();
    cast(&mut fx, caster, "cast 'sleep' ogre");
    assert_eq!(posture_of(&fx.world, ogre), Some(PostureKind::Sleeping));
    {
        let mut q = fx
            .world
            .query::<(&mut mud_world::EffectInstance, &mud_world::AppliedTo)>();
        for (mut inst, applied) in q.iter_mut(&mut fx.world) {
            if applied.0 == ogre {
                inst.remaining_secs = 1;
            }
        }
    }
    fx.world.insert_resource(crate::TickCount(0));
    crate::effects::effects_tick(&mut fx.world);
    assert_eq!(posture_of(&fx.world, ogre), Some(PostureKind::Standing));

    // A blow (the spell breaks on damage).
    let (mut fx, caster, _rx, ogre) = sleep_fight();
    cast(&mut fx, caster, "cast 'sleep' ogre");
    assert_eq!(posture_of(&fx.world, ogre), Some(PostureKind::Sleeping));
    crate::commands::apply_attacker_damage(&mut fx.world, ogre, 5, caster);
    assert_eq!(posture_of(&fx.world, ogre), Some(PostureKind::Standing));
    assert!(!crate::effects::has_sleep_effect(&mut fx.world, ogre));
}

#[test]
fn magical_sleep_cannot_be_woken_or_stood_up_from() {
    let (mut fx, caster, mut rx, _ogre) = sleep_fight();
    let a = fx.a;
    let (victim, mut vrx) = super::gmcp_tests::player(&mut fx.world, a, "Sleeper");
    fx.world
        .entity_mut(victim)
        .insert((Health { hp: 50, max: 50 }, CombatStats::default()));
    cast(&mut fx, caster, "cast 'sleep' sleeper");
    assert_eq!(posture_of(&fx.world, victim), Some(PostureKind::Sleeping));
    let _ = (drain(&mut rx), drain(&mut vrx));
    super::dispatch(&mut fx.world, caster, "wake sleeper");
    assert!(drain(&mut rx).contains("You can't wake Sleeper up!"));
    super::dispatch(&mut fx.world, victim, "wake");
    assert!(drain(&mut vrx).contains("You can't wake up!"));
    super::dispatch(&mut fx.world, victim, "stand");
    assert!(drain(&mut vrx).contains("You can't wake up!"));
    assert_eq!(posture_of(&fx.world, victim), Some(PostureKind::Sleeping));
}

#[test]
fn a_seated_aggressive_mob_stands_and_attacks_on_the_pulse() {
    for seat in SEATED {
        let mut fx = fixture();
        let wolf = wolf(&mut fx, "a wolf");
        apply(&mut fx.world, wolf, seat);
        let _ = drain(&mut fx.rx);
        crate::commands::aggro_pulse(&mut fx.world);
        assert_eq!(
            fx.world.get::<Fighting>(wolf).map(|f| f.0),
            Some(fx.player),
            "{seat:?}"
        );
        assert_eq!(
            posture_of(&fx.world, wolf),
            Some(PostureKind::Standing),
            "{seat:?}"
        );
        let out = drain(&mut fx.rx);
        assert!(out.contains("scrambles to its feet"), "{seat:?}: {out}");
        assert!(out.contains("attacks"), "{seat:?}: {out}");
    }
}

#[test]
fn a_seated_mob_holds_a_grudge_and_turns_on_its_hate_list() {
    for seat in SEATED {
        let mut fx = fixture();
        let wolf = wolf(&mut fx, "a wolf");
        fx.world.entity_mut(wolf).insert(CombatStats::default());
        let mut memory = MobMemory::default();
        memory.0.insert(fx.player);
        fx.world.entity_mut(wolf).insert(memory);
        apply(&mut fx.world, wolf, seat);
        recheck_aggro_in_room(&mut fx.world, fx.player);
        assert_eq!(
            fx.world.get::<Fighting>(wolf).map(|f| f.0),
            Some(fx.player),
            "grudge {seat:?}"
        );
        assert_eq!(posture_of(&fx.world, wolf), Some(PostureKind::Standing));

        let mut fx = fixture();
        let hater = self::wolf(&mut fx, "a wolf");
        fx.world.entity_mut(hater).insert(CombatStats::default());
        let mut hate = HateList::default();
        hate.push(fx.player);
        fx.world.entity_mut(hater).insert(hate);
        apply(&mut fx.world, hater, seat);
        fx.world.insert_resource(crate::TickCount(0));
        crate::combat::combat_tick(&mut fx.world);
        assert_eq!(
            fx.world.get::<Fighting>(hater).map(|f| f.0),
            Some(fx.player),
            "hate {seat:?}"
        );
    }
}

#[test]
fn a_seated_mob_that_is_attacked_stands_then_fights_back() {
    for seat in SEATED {
        let mut fx = fixture();
        let wolf = wolf(&mut fx, "a wolf");
        fx.world.entity_mut(wolf).insert(CombatStats::default());
        apply(&mut fx.world, wolf, seat);
        // The player opened the fight; the mob has Fighting but is seated.
        fx.world.entity_mut(fx.player).insert(Fighting(wolf));
        fx.world.entity_mut(wolf).insert(Fighting(fx.player));
        let hp = fx.world.get::<Health>(fx.player).map(|h| h.hp);
        let _ = drain(&mut fx.rx);
        fx.world.insert_resource(crate::TickCount(0));
        // Round one: it gets to its feet and does not swing yet.
        crate::combat::combat_tick(&mut fx.world);
        assert_eq!(
            posture_of(&fx.world, wolf),
            Some(PostureKind::Standing),
            "{seat:?}"
        );
        let out = drain(&mut fx.rx);
        assert!(out.contains("scrambles to its feet"), "{seat:?}: {out}");
        assert_eq!(
            fx.world.get::<Health>(fx.player).map(|h| h.hp),
            hp,
            "{seat:?}"
        );
        // Later rounds: it swings.
        let mut swung = false;
        for t in 1..40 {
            fx.world.insert_resource(crate::TickCount(t * 100));
            crate::combat::combat_tick(&mut fx.world);
            if fx.world.get::<Health>(fx.player).map(|h| h.hp) != hp {
                swung = true;
                break;
            }
        }
        assert!(swung, "{seat:?}: the mob never fought back");
    }
}

#[test]
fn a_sleeping_or_stunned_mob_in_a_fight_stays_down() {
    for hold in HOLDS {
        let mut fx = fixture();
        let wolf = wolf(&mut fx, "a wolf");
        apply(&mut fx.world, wolf, hold);
        fx.world.entity_mut(wolf).insert(Fighting(fx.player));
        fx.world.insert_resource(crate::TickCount(0));
        crate::combat::combat_tick(&mut fx.world);
        let sleeping = matches!(hold, Hold::Asleep);
        assert_eq!(
            posture_of(&fx.world, wolf),
            Some(if sleeping {
                PostureKind::Sleeping
            } else {
                PostureKind::Standing
            }),
            "{hold:?}"
        );
    }
}
