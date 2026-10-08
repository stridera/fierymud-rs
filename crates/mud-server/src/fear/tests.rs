//! Fear: spell flee, immunity, roar, markers, no re-engage, players.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Direction, ExitState, MobBehavior};
use mud_world::{
    AppliedTo, CombatStats, EffectInstance, EffectSource, EquippedSlot, Exits, Feared, Fighting,
    Health, Item, Keywords, Located, Mob, MobPrototypes, Named, ObjectRestrictions, Player,
    Posture, PostureKind, Profile, Room, Slot, Stunned, WorldKey, WorldKeyIndex,
};

use super::{
    ChantRolls, FearOutcome, FearRolls, Panic, PinnedFearRolls, RoarOutcome, RoarRolls,
    chant_gates_pass, inflict_fear, is_feared, on_fear_applied, panic_flee, roar_target,
    sync_markers,
};
use crate::combat::MobMemory;
use crate::commands::test_support::{Rx, drain, mob_proto};
use crate::commands::{Connection, try_insert, try_remove};

/// Rolls that fail the freeze and drop branches and pass the flee branch.
const FLEE: FearRolls = FearRolls {
    freeze: 100,
    drop: 100,
    flee: 0,
    falter: 100,
};

const ABILITY: i32 = 141;
const EFFECT: i32 = 30;

struct Fx {
    world: World,
    here: Entity,
    away: Entity,
    caster: Entity,
    rx: Rx,
}

fn open_exit(to: Entity) -> mud_world::ExitData {
    mud_world::ExitData {
        to: Some(to),
        state: ExitState::Open,
        key: None,
        description: None,
        keywords: Vec::new(),
        is_hidden: false,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    }
}

fn fx() -> Fx {
    let mut world = World::new();
    world.insert_resource(WorldKeyIndex::default());
    world.insert_resource(MobPrototypes::default());
    world.insert_resource(mud_script::LuaHost::default());
    world.insert_resource(mud_world::WeatherCatalog::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    world.insert_resource(crate::TickCount(0));
    world.insert_resource(mud_world::EffectCatalog::default());
    // Spell-path tests want the flee branch; cascade tests call
    // `inflict_fear` with their own rolls.
    world.insert_resource(PinnedFearRolls(FLEE));
    let away = world
        .spawn((
            Room,
            Named {
                name: "Away".into(),
            },
            Exits::default(),
        ))
        .id();
    let here = world
        .spawn((
            Room,
            Named {
                name: "Here".into(),
            },
            Exits::default(),
        ))
        .id();
    world
        .get_mut::<Exits>(here)
        .unwrap()
        .0
        .insert(Direction::North, open_exit(away));
    world
        .get_mut::<Exits>(away)
        .unwrap()
        .0
        .insert(Direction::South, open_exit(here));
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let caster = world
        .spawn((
            Player,
            Named {
                name: "Caster".into(),
            },
            Located(here),
            Connection(tx),
            Profile {
                level: 50,
                class_id: None,
                race: "Human".into(),
                experience: 0,
                gender: "neutral".into(),
            },
            Health { hp: 500, max: 500 },
            CombatStats::default(),
            Posture(PostureKind::Standing),
        ))
        .id();
    Fx {
        world,
        here,
        away,
        caster,
        rx,
    }
}

impl Fx {
    /// A standing mob of `level` named "jackal" in `here`, optionally
    /// carrying `resistances` on its proto.
    fn mob(&mut self, level: i32, resistances: serde_json::Value) -> Entity {
        let n = i32::try_from(self.world.resource::<MobPrototypes>().by_key.len()).unwrap();
        let mut proto = mob_proto(77, n, mud_db::enums::MobProfession::Banker);
        proto.level = level;
        proto.resistances = resistances;
        self.world
            .resource_mut::<MobPrototypes>()
            .by_key
            .insert((77, n), proto);
        self.world
            .spawn((
                Mob,
                Named {
                    name: "a jackal".into(),
                },
                Keywords(vec!["jackal".into()]),
                WorldKey { zone: 77, id: n },
                Located(self.here),
                Health { hp: 100, max: 100 },
                CombatStats::default(),
                Posture(PostureKind::Standing),
            ))
            .id()
    }

    fn at(&self, e: Entity) -> Option<Entity> {
        self.world.get::<Located>(e).map(|l| l.0)
    }

    /// Install FEAR (a `feared` status with an optional area flag).
    fn fear_spell(&mut self, area: bool) {
        self.fear_spell_with(area, true);
    }

    /// `override_flag`: the `feared` flag sits in the ability's override
    /// params (live data); otherwise only in the status effect's defaults.
    fn fear_spell_with(&mut self, area: bool, override_flag: bool) {
        let mut abilities = mud_world::AbilityCatalog::default();
        let mut def =
            crate::commands::test_support::ability_def(ABILITY, "Fear", AbilityKind::Spell);
        def.cast_time_rounds = 0;
        def.violent = true;
        def.is_area = area;
        abilities.by_name.insert("fear".to_string(), def);
        abilities.effects_for.insert(
            ABILITY,
            vec![(
                EFFECT,
                Some(if override_flag {
                    serde_json::json!({"flag": "feared", "duration": "60"})
                } else {
                    serde_json::json!({"duration": "60"})
                }),
            )],
        );
        self.world.insert_resource(abilities);
        let mut effects = mud_world::EffectCatalog::default();
        effects.by_id.insert(
            EFFECT,
            mud_world::EffectDef {
                id: EFFECT,
                name: "status".into(),
                description: None,
                effect_type: "status".into(),
                tags: vec![],
                presence_override: None,
                default_params: if override_flag {
                    serde_json::json!({})
                } else {
                    serde_json::json!({"flag": "feared"})
                },
                prevents_speaking: false,
                prevents_casting: false,
                prevents_movement: false,
                on_apply: None,
                on_tick: None,
                on_remove: None,
            },
        );
        self.world.insert_resource(effects);
        self.world
            .entity_mut(self.caster)
            .insert(mud_world::KnownAbilities {
                entries: vec![(ABILITY, 1000, true)],
            });
    }

    fn cast_fear(&mut self, target: &str) {
        crate::commands::invoke_ability_with(
            &mut self.world,
            self.caster,
            &format!("fear {target}"),
            AbilityKind::Spell,
            "cast",
            false,
            true,
            true,
            None,
        );
    }
}

/// Rolls that never save and always pass the chant's skill check.
const NO_SAVE: RoarRolls = RoarRolls {
    save: 0,
    sentinel_save: 0,
    wake: true,
    trip: 0,
};

// -- the fear spell ---------------------------------------------------------

#[test]
fn fear_spell_makes_a_mob_flee_and_marks_it_feared() {
    let mut f = fx();
    f.fear_spell(false);
    let mob = f.mob(10, serde_json::json!({}));
    f.world.entity_mut(mob).insert(Fighting(f.caster));
    f.world.entity_mut(f.caster).insert(Fighting(mob));
    f.cast_fear("jackal");
    assert_eq!(f.at(mob), Some(f.away), "the mob ran through the exit");
    assert!(f.world.get::<Fighting>(mob).is_none());
    assert!(is_feared(&f.world, mob));
}

#[test]
fn fear_immune_mob_does_not_flee() {
    let mut f = fx();
    f.fear_spell(false);
    let mob = f.mob(10, serde_json::json!({"fear": 0}));
    f.cast_fear("jackal");
    assert_eq!(f.at(mob), Some(f.here), "immune mob stays put");
    assert!(!is_feared(&f.world, mob));
    let status = f
        .world
        .query::<&EffectInstance>()
        .iter(&f.world)
        .filter(|e| e.name == "feared")
        .count();
    assert_eq!(status, 0, "no fear effect lands on an immune mob");
}

#[test]
fn a_fear_resistance_that_is_not_zero_does_not_grant_immunity() {
    let mut f = fx();
    f.fear_spell(false);
    let mob = f.mob(10, serde_json::json!({"fear": 50}));
    f.cast_fear("jackal");
    assert_eq!(f.at(mob), Some(f.away));
}

#[test]
fn cornered_mob_cannot_flee_and_stays() {
    let mut f = fx();
    f.world.entity_mut(f.here).insert(Exits::default());
    let mob = f.mob(10, serde_json::json!({}));
    f.world.entity_mut(mob).insert(Fighting(f.caster));
    assert_eq!(panic_flee(&mut f.world, mob, None), Panic::Cornered);
    assert_eq!(f.at(mob), Some(f.here));
    assert!(
        f.world.get::<Fighting>(mob).is_none(),
        "legacy stop_fighting happens even when the flee fails"
    );
}

#[test]
fn sleeping_victim_only_dreams_of_fleeing() {
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({}));
    f.world
        .entity_mut(mob)
        .insert(Posture(PostureKind::Sleeping));
    assert_eq!(panic_flee(&mut f.world, mob, None), Panic::Unable);
    assert_eq!(f.at(mob), Some(f.here));
}

#[test]
fn sitting_victim_scrambles_to_its_feet_instead_of_fleeing() {
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({}));
    f.world
        .entity_mut(mob)
        .insert(Posture(PostureKind::Sitting));
    assert_eq!(panic_flee(&mut f.world, mob, None), Panic::Unable);
    assert_eq!(
        f.world.get::<Posture>(mob).map(|p| p.0),
        Some(PostureKind::Standing)
    );
    assert_eq!(f.at(mob), Some(f.here));
}

#[test]
fn berserk_fighter_is_too_angry_to_flee() {
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({}));
    f.world.entity_mut(mob).insert(Fighting(f.caster));
    f.world.spawn((
        EffectInstance {
            kind: 1,
            name: "berserk".into(),
            strength: 1,
            remaining_secs: 30,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(mob),
    ));
    assert_eq!(panic_flee(&mut f.world, mob, None), Panic::Unable);
    assert_eq!(f.at(mob), Some(f.here));
    assert!(f.world.get::<Fighting>(mob).is_some());
}

// -- players ----------------------------------------------------------------

#[test]
fn a_feared_player_panics_through_the_flee_command() {
    let mut f = fx();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let victim = f
        .world
        .spawn((
            Player,
            Named {
                name: "Victim".into(),
            },
            Located(f.here),
            Connection(tx),
            Posture(PostureKind::Standing),
        ))
        .id();
    f.world.entity_mut(victim).insert(Fighting(f.caster));
    on_fear_applied(&mut f.world, f.caster, victim, 100, false);
    assert_eq!(f.at(victim), Some(f.away), "players flee too (legacy)");
    assert!(f.world.get::<Fighting>(victim).is_none());
    assert!(is_feared(&f.world, victim));
    let out = drain(&mut rx);
    assert!(out.contains("You flee north!"), "{out}");
}

// -- re-engage --------------------------------------------------------------

#[test]
fn feared_mob_does_not_re_engage_from_its_hate_list() {
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({}));
    f.world
        .entity_mut(mob)
        .insert(crate::combat::HateList(vec![f.caster]));
    f.world.entity_mut(mob).insert(Feared);
    f.world.insert_resource(crate::TickCount(40));
    crate::combat::combat_tick(&mut f.world);
    assert!(
        f.world.get::<Fighting>(mob).is_none(),
        "a feared mob stays out of the fight"
    );
    // Once the fear is gone the grudge resumes.
    try_remove::<Feared>(&mut f.world, mob);
    crate::combat::combat_tick(&mut f.world);
    assert_eq!(f.world.get::<Fighting>(mob).map(|x| x.0), Some(f.caster));
}

#[test]
fn feared_mob_does_not_start_or_resume_fights() {
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({}));
    f.world.entity_mut(mob).insert(Feared);
    assert!(!crate::commands::mob_will_start_fight(
        &f.world, mob, f.caster
    ));
    f.world.entity_mut(mob).remove::<Feared>();
    assert!(crate::commands::mob_will_start_fight(
        &f.world, mob, f.caster
    ));
}

#[test]
fn feared_mob_in_a_fight_runs_instead_of_swinging() {
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({}));
    f.world.entity_mut(mob).insert((Feared, Fighting(f.caster)));
    f.world.entity_mut(f.caster).insert(Fighting(mob));
    f.world.insert_resource(crate::TickCount(40));
    crate::combat::combat_tick(&mut f.world);
    assert_eq!(f.at(mob), Some(f.away));
    assert_eq!(f.world.get::<Health>(f.caster).unwrap().hp, 500, "no swing");
}

#[test]
fn cornered_feared_mob_fights_on() {
    let mut f = fx();
    f.world.entity_mut(f.here).insert(Exits::default());
    let mob = f.mob(10, serde_json::json!({}));
    f.world.entity_mut(mob).insert((Feared, Fighting(f.caster)));
    f.world.entity_mut(f.caster).insert(Fighting(mob));
    f.world.insert_resource(crate::TickCount(40));
    crate::combat::combat_tick(&mut f.world);
    assert_eq!(f.at(mob), Some(f.here));
    assert!(f.world.get::<Fighting>(mob).is_some());
}

// -- marker lifecycle -------------------------------------------------------

#[test]
fn marker_clears_when_the_backing_effect_is_gone_but_not_before() {
    let mut f = fx();
    f.fear_spell(false);
    let mob = f.mob(10, serde_json::json!({}));
    f.cast_fear("jackal");
    assert!(is_feared(&f.world, mob));
    sync_markers(&mut f.world);
    assert!(is_feared(&f.world, mob), "effect still running");
    crate::commands::remove_effect_named(&mut f.world, mob, "feared");
    sync_markers(&mut f.world);
    assert!(!is_feared(&f.world, mob));
}

// -- area chant gates -------------------------------------------------------

fn chant(save: i32, sentinel_save: i32, chance: i32) -> ChantRolls {
    ChantRolls {
        save,
        sentinel_save,
        chance,
    }
}

#[test]
fn chant_flee_needs_a_failed_save_and_the_skill_roll() {
    let mut f = fx();
    f.world
        .entity_mut(f.here)
        .insert(mud_world::BaseLightLevel(1));
    let mob = f.mob(10, serde_json::json!({}));
    // Level 10: save number 100, so no d100 saves; a d100 above the
    // skill stops the chant.
    assert!(chant_gates_pass(&f.world, mob, 80, chant(0, 0, 80)));
    assert!(!chant_gates_pass(&f.world, mob, 80, chant(0, 0, 81)));
    // Level 100: save number 55; a roll of 60 saves in a lit room.
    let tough = f.mob(100, serde_json::json!({}));
    assert!(!chant_gates_pass(&f.world, tough, 100, chant(60, 0, 0)));
}

#[test]
fn dark_rooms_skip_the_chants_saving_throw() {
    let mut f = fx();
    f.world
        .entity_mut(f.here)
        .insert(mud_world::BaseLightLevel(-1));
    let tough = f.mob(100, serde_json::json!({}));
    assert!(chant_gates_pass(&f.world, tough, 100, chant(60, 0, 0)));
}

#[test]
fn sentinel_mobs_get_a_second_save_against_the_chant() {
    let mut f = fx();
    let mob = f.mob(100, serde_json::json!({}));
    f.world
        .entity_mut(mob)
        .insert(mud_world::MobBehaviors(vec![MobBehavior::Sentinel]));
    // Level 100: save number 55, so a d100 of 60 saves.
    assert!(!chant_gates_pass(&f.world, mob, 100, chant(0, 60, 0)));
    assert!(chant_gates_pass(&f.world, mob, 100, chant(0, 0, 0)));
}

// -- roar -------------------------------------------------------------------

#[test]
fn roar_makes_a_mob_that_fails_its_save_flee() {
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({}));
    let out = roar_target(&mut f.world, f.caster, mob, NO_SAVE);
    assert_eq!(out, RoarOutcome::Panicked(Panic::Fled));
    assert_eq!(f.at(mob), Some(f.away));
}

#[test]
fn roar_does_not_apply_a_lasting_fear_effect() {
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({}));
    roar_target(&mut f.world, f.caster, mob, NO_SAVE);
    assert!(
        !is_feared(&f.world, mob),
        "legacy roar leaves nothing behind"
    );
}

#[test]
fn roar_victim_that_saves_stays() {
    let mut f = fx();
    let mob = f.mob(100, serde_json::json!({}));
    let rolls = RoarRolls {
        save: 60,
        ..NO_SAVE
    };
    assert_eq!(
        roar_target(&mut f.world, f.caster, mob, rolls),
        RoarOutcome::Resisted
    );
    assert_eq!(f.at(mob), Some(f.here));
}

#[test]
fn roar_sentinel_must_fail_two_saves() {
    let mut f = fx();
    let mob = f.mob(100, serde_json::json!({}));
    f.world
        .entity_mut(mob)
        .insert(mud_world::MobBehaviors(vec![MobBehavior::Sentinel]));
    let rolls = RoarRolls {
        sentinel_save: 60,
        ..NO_SAVE
    };
    assert_eq!(
        roar_target(&mut f.world, f.caster, mob, rolls),
        RoarOutcome::Resisted
    );
    assert_eq!(f.at(mob), Some(f.here));
}

#[test]
fn roar_ignores_aware_nosummon_and_fear_immune_mobs() {
    for flag in [MobBehavior::Aware, MobBehavior::NoSummon] {
        let mut f = fx();
        let mob = f.mob(10, serde_json::json!({}));
        f.world
            .entity_mut(mob)
            .insert(mud_world::MobBehaviors(vec![flag]));
        assert_eq!(
            roar_target(&mut f.world, f.caster, mob, NO_SAVE),
            RoarOutcome::Resisted
        );
        assert_eq!(f.at(mob), Some(f.here));
    }
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({"fear": 0}));
    assert_eq!(
        roar_target(&mut f.world, f.caster, mob, NO_SAVE),
        RoarOutcome::Resisted
    );
}

#[test]
fn roar_victim_with_a_clumsy_roll_trips_instead_of_fleeing() {
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({}));
    // dex defaults to 50: trips when 35 < roll.
    let rolls = RoarRolls {
        trip: 36,
        ..NO_SAVE
    };
    assert_eq!(
        roar_target(&mut f.world, f.caster, mob, rolls),
        RoarOutcome::Tripped
    );
    assert_eq!(f.at(mob), Some(f.here));
    assert_eq!(
        f.world.get::<Posture>(mob).map(|p| p.0),
        Some(PostureKind::Sitting)
    );
    let flee = RoarRolls {
        trip: 35,
        ..NO_SAVE
    };
    let nimble = f.mob(10, serde_json::json!({}));
    assert_eq!(
        roar_target(&mut f.world, f.caster, nimble, flee),
        RoarOutcome::Panicked(Panic::Fled)
    );
}

#[test]
fn roar_wakes_a_sleeper_half_the_time_and_never_moves_it() {
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({}));
    f.world
        .entity_mut(mob)
        .insert(Posture(PostureKind::Sleeping));
    let rolls = RoarRolls {
        wake: false,
        ..NO_SAVE
    };
    assert_eq!(
        roar_target(&mut f.world, f.caster, mob, rolls),
        RoarOutcome::SleptOn
    );
    assert_eq!(
        f.world.get::<Posture>(mob).map(|p| p.0),
        Some(PostureKind::Sleeping)
    );
    assert_eq!(
        roar_target(&mut f.world, f.caster, mob, NO_SAVE),
        RoarOutcome::Woke
    );
    assert_eq!(
        f.world.get::<Posture>(mob).map(|p| p.0),
        Some(PostureKind::Sitting)
    );
    assert_eq!(f.at(mob), Some(f.here));
}

#[test]
fn roar_frightens_players_too() {
    let mut f = fx();
    let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let victim = f
        .world
        .spawn((
            Player,
            Named {
                name: "Victim".into(),
            },
            Located(f.here),
            Connection(tx),
            Posture(PostureKind::Standing),
        ))
        .id();
    assert_eq!(
        roar_target(&mut f.world, f.caster, victim, NO_SAVE),
        RoarOutcome::Panicked(Panic::Fled)
    );
    assert_eq!(f.at(victim), Some(f.away));
    // Players are never fear-immune by proto.
    try_insert(&mut f.world, victim, Feared);
}

// -- marker survives effects_tick (real spell) ---------------------------------

fn tick_effects(f: &mut Fx, tick: u64) {
    f.world.insert_resource(crate::TickCount(tick));
    crate::effects::effects_tick(&mut f.world);
}

fn expire_fear_effects(f: &mut Fx, victim: Entity) {
    let mut q = f.world.query::<(&mut EffectInstance, &AppliedTo)>();
    for (mut inst, applied) in q.iter_mut(&mut f.world) {
        if applied.0 == victim {
            inst.remaining_secs = 1;
        }
    }
}

fn marker_lasts_until_expiry(override_flag: bool) {
    let mut f = fx();
    f.fear_spell_with(false, override_flag);
    let mob = f.mob(10, serde_json::json!({}));
    f.cast_fear("jackal");
    assert!(is_feared(&f.world, mob));
    tick_effects(&mut f, 10);
    tick_effects(&mut f, 20);
    assert!(
        is_feared(&f.world, mob),
        "effects_tick must not strip Feared while the effect runs"
    );
    expire_fear_effects(&mut f, mob);
    tick_effects(&mut f, 30);
    tick_effects(&mut f, 40);
    assert!(!is_feared(&f.world, mob), "cleared once the effect expires");
}

#[test]
fn feared_marker_persists_through_effects_tick_until_expiry() {
    marker_lasts_until_expiry(true);
}

#[test]
fn feared_marker_persists_when_the_flag_is_only_in_the_effect_defaults() {
    marker_lasts_until_expiry(false);
}

#[test]
fn failed_chant_gate_removes_the_effect_whatever_it_is_named() {
    let mut f = fx();
    f.fear_spell_with(true, false);
    let mob = f.mob(10, serde_json::json!({}));
    f.world.spawn((
        EffectInstance {
            kind: EFFECT,
            name: "status".into(),
            strength: 1,
            remaining_secs: 60,
            source: EffectSource::Spell,
            ability_id: Some(ABILITY),
        },
        AppliedTo(mob),
    ));
    // A skill of -1 can never beat the d100 chance roll.
    on_fear_applied(&mut f.world, f.caster, mob, -1, true);
    assert_eq!(f.world.query::<&EffectInstance>().iter(&f.world).count(), 0);
    assert!(!is_feared(&f.world, mob));
    assert_eq!(f.at(mob), Some(f.here));
}

// -- mounts, berserk ------------------------------------------------------------

#[test]
fn a_fleeing_rider_takes_its_mount_along() {
    let mut f = fx();
    let mount = f.mob(10, serde_json::json!({}));
    let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let rider = f
        .world
        .spawn((
            Player,
            Named {
                name: "Rider".into(),
            },
            Located(f.here),
            Connection(tx),
            Posture(PostureKind::Standing),
            mud_world::Mounted(mount),
        ))
        .id();
    f.world.entity_mut(mount).insert(mud_world::RiddenBy(rider));
    assert_eq!(panic_flee(&mut f.world, rider, None), Panic::Fled);
    assert_eq!(f.at(rider), Some(f.away));
    assert_eq!(f.at(mount), Some(f.away), "mount stays with its rider");
    assert!(f.world.get::<mud_world::Mounted>(rider).is_some());
}

#[test]
fn a_ridden_mount_cannot_flee_on_its_own() {
    let mut f = fx();
    let mount = f.mob(10, serde_json::json!({}));
    f.world
        .entity_mut(mount)
        .insert((mud_world::RiddenBy(f.caster), Feared, Fighting(f.caster)));
    assert_eq!(panic_flee(&mut f.world, mount, None), Panic::Unable);
    f.world.insert_resource(crate::TickCount(40));
    super::feared_mobs_flee(&mut f.world);
    assert_eq!(f.at(mount), Some(f.here));
}

#[test]
fn feared_berserk_mob_stays_in_the_fight() {
    let mut f = fx();
    let mob = f.mob(10, serde_json::json!({}));
    f.world.entity_mut(mob).insert((Feared, Fighting(f.caster)));
    f.world.spawn((
        EffectInstance {
            kind: 1,
            name: "berserk".into(),
            strength: 1,
            remaining_secs: 30,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(mob),
    ));
    super::feared_mobs_flee(&mut f.world);
    assert_eq!(f.at(mob), Some(f.here));
    assert!(f.world.get::<Fighting>(mob).is_some());
}

// -- the inflict_fear cascade -----------------------------------------------

/// All four rolls pinned to `n`.
fn all(n: i32) -> FearRolls {
    FearRolls {
        freeze: n,
        drop: n,
        flee: n,
        falter: n,
    }
}

impl Fx {
    /// A mob that already carries the lasting `feared` status, fighting the
    /// caster, as it is when the cascade starts.
    fn feared_mob(&mut self, level: i32) -> Entity {
        let mob = self.mob(level, serde_json::json!({}));
        self.world
            .entity_mut(mob)
            .insert((Fighting(self.caster), Feared));
        self.world.entity_mut(self.caster).insert(Fighting(mob));
        self.world.spawn((
            EffectInstance {
                kind: EFFECT,
                name: "feared".into(),
                strength: 1,
                remaining_secs: 60,
                source: EffectSource::Spell,
                ability_id: Some(ABILITY),
            },
            AppliedTo(mob),
        ));
        mob
    }

    fn wield(&mut self, holder: Entity, restrictions: bool) -> Entity {
        let item = self
            .world
            .spawn((
                Item,
                Named {
                    name: "a rusty sword".into(),
                },
                Located(holder),
                EquippedSlot(Slot::Wield),
            ))
            .id();
        if restrictions {
            self.world.entity_mut(item).insert(ObjectRestrictions(vec![
                mud_db::enums::ObjectRestriction::NoDrop,
            ]));
        }
        item
    }

    fn effect_secs(&mut self, victim: Entity, name: &str) -> Option<i32> {
        let mut q = self.world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(&self.world)
            .find(|(e, a)| a.0 == victim && e.name == name)
            .map(|(e, _)| e.remaining_secs)
    }

    fn fear(&mut self, victim: Entity, power: i32, rolls: FearRolls) -> FearOutcome {
        inflict_fear(&mut self.world, self.caster, victim, power, rolls)
    }
}

fn caster_rx(f: &mut Fx) -> String {
    drain(&mut f.rx)
}

#[test]
fn frozen_in_terror_paralyses_and_does_not_flee() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    let rolls = FearRolls {
        freeze: 0,
        ..all(0)
    };
    // power 100 vs level 10: freeze threshold min(80, 153) = 80.
    assert_eq!(f.fear(mob, 100, rolls), FearOutcome::Frozen);
    assert_eq!(f.at(mob), Some(f.here), "frozen victims do not run");
    assert!(f.world.get::<Fighting>(mob).is_none(), "its fight stops");
    assert!(
        f.world.get::<Fighting>(f.caster).is_none(),
        "the caster's fight against it stops too"
    );
    assert!(f.world.get::<Stunned>(mob).is_some());
    // 2 + 100/30 = 5 MUD hours of 75 seconds.
    assert_eq!(f.effect_secs(mob, "paralyzed"), Some(5 * 75));
    assert!(!is_feared(&f.world, mob), "no lasting fear on the freeze");
    assert!(
        f.world
            .get::<MobMemory>(mob)
            .is_some_and(|m| m.0.contains(&f.caster)),
        "the mob remembers the caster"
    );
    let out = caster_rx(&mut f);
    assert!(
        out.contains("You frighten A jackal so bad that they are frozen in terror!"),
        "{out}"
    );
}

#[test]
fn frozen_victim_cannot_flee_and_thaws_when_the_paralysis_expires() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    f.fear(mob, 100, all(0));
    assert_eq!(panic_flee(&mut f.world, mob, None), Panic::Unable);
    expire_fear_effects(&mut f, mob);
    tick_effects(&mut f, 10);
    tick_effects(&mut f, 20);
    assert!(
        f.world.get::<Stunned>(mob).is_none(),
        "marker follows effect"
    );
}

#[test]
fn freeze_threshold_is_a_strict_less_than_on_the_level_difference() {
    let mut f = fx();
    // power 20 vs level 10: 17 * 10 / 10 = 17.
    let mob = f.feared_mob(10);
    let at = |freeze| FearRolls { freeze, ..all(100) };
    assert_eq!(f.fear(mob, 20, at(16)), FearOutcome::Frozen);
    let mob = f.feared_mob(10);
    assert_ne!(f.fear(mob, 20, at(17)), FearOutcome::Frozen);
}

#[test]
fn thresholds_are_capped() {
    let mut f = fx();
    // A huge difference caps freeze at 80, drop at 85, flee at 90, falter at 95.
    let mob = f.feared_mob(1);
    assert_eq!(f.fear(mob, 1000, all(79)), FearOutcome::Frozen);
    let mob = f.feared_mob(1);
    f.wield(mob, false);
    assert_eq!(f.fear(mob, 1000, all(80)), FearOutcome::DroppedWeapon);
    let mob = f.feared_mob(1);
    f.wield(mob, false);
    assert_eq!(f.fear(mob, 1000, all(85)), FearOutcome::Fled(Panic::Fled));
    let mob = f.feared_mob(1);
    assert_eq!(f.fear(mob, 1000, all(90)), FearOutcome::Faltered);
    let mob = f.feared_mob(1);
    assert_eq!(f.fear(mob, 1000, all(95)), FearOutcome::Shrugged);
}

#[test]
fn a_victim_above_the_casters_power_shrugs_everything_off() {
    let mut f = fx();
    let mob = f.feared_mob(50);
    // power 10 vs level 50: every threshold is negative.
    assert_eq!(f.fear(mob, 10, all(0)), FearOutcome::Shrugged);
}

#[test]
fn drop_weapon_puts_the_wielded_item_in_the_room_and_lags_the_victim() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    let sword = f.wield(mob, false);
    // freeze fails (100), drop passes: power 20 vs 10 gives 1 + 18 = 19.
    let rolls = FearRolls {
        drop: 18,
        ..all(100)
    };
    assert_eq!(f.fear(mob, 20, rolls), FearOutcome::DroppedWeapon);
    assert_eq!(f.world.get::<Located>(sword).map(|l| l.0), Some(f.here));
    assert!(f.world.get::<EquippedSlot>(sword).is_none());
    assert_eq!(f.at(mob), Some(f.here), "dropping is not fleeing");
    assert!(!is_feared(&f.world, mob));
    assert!(f.world.get::<Stunned>(mob).is_some(), "one round of lag");
    assert_eq!(f.effect_secs(mob, "stun"), Some(4));
    assert_eq!(
        f.world.get::<Fighting>(mob).map(|x| x.0),
        Some(f.caster),
        "the victim fights back"
    );
    let out = caster_rx(&mut f);
    assert!(
        out.contains("You made A jackal drop their a rusty sword!"),
        "{out}"
    );
}

#[test]
fn drop_weapon_mirrors_legacy_and_drops_a_no_drop_weapon() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    let sword = f.wield(mob, true);
    assert_eq!(
        f.fear(
            mob,
            100,
            FearRolls {
                drop: 0,
                ..all(100)
            }
        ),
        FearOutcome::DroppedWeapon
    );
    assert_eq!(f.world.get::<Located>(sword).map(|l| l.0), Some(f.here));
    assert!(f.world.get::<EquippedSlot>(sword).is_none());
}

#[test]
fn the_drop_branch_needs_a_wielded_weapon() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    // Rolls pass drop but there is nothing in hand: fall through to the
    // later branches (here none pass).
    let rolls = FearRolls {
        drop: 0,
        ..all(100)
    };
    assert_eq!(f.fear(mob, 100, rolls), FearOutcome::Shrugged);
}

#[test]
fn flee_branch_stops_the_fight_runs_remembers_and_keeps_the_fear() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    // power 20 vs 10: flee threshold 3 + 19 = 22.
    let rolls = FearRolls {
        flee: 21,
        ..all(100)
    };
    assert_eq!(f.fear(mob, 20, rolls), FearOutcome::Fled(Panic::Fled));
    assert_eq!(f.at(mob), Some(f.away));
    assert!(f.world.get::<Fighting>(mob).is_none());
    assert!(
        is_feared(&f.world, mob),
        "the fear status stays on a runner"
    );
    assert!(
        f.world
            .get::<MobMemory>(mob)
            .is_some_and(|m| m.0.contains(&f.caster))
    );
    assert!(
        f.world.get::<Stunned>(mob).is_some(),
        "lagged after running"
    );
    let out = caster_rx(&mut f);
    assert!(
        out.contains("A jackal shrieks madly at your vision of terror!"),
        "{out}"
    );
}

#[test]
fn flee_roll_just_over_the_threshold_does_not_run() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    let rolls = FearRolls {
        flee: 22,
        ..all(100)
    };
    assert_ne!(f.fear(mob, 20, rolls), FearOutcome::Fled(Panic::Fled));
}

#[test]
fn flee_clears_fears_own_lag_first() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    super::lag_for(&mut f.world, mob, 4);
    assert_eq!(
        f.fear(
            mob,
            100,
            FearRolls {
                flee: 0,
                ..all(100)
            }
        ),
        FearOutcome::Fled(Panic::Fled)
    );
    assert_eq!(f.at(mob), Some(f.away), "a lagged victim can still panic");
}

#[test]
fn cornered_flee_branch_reports_cornered() {
    let mut f = fx();
    f.world.entity_mut(f.here).insert(Exits::default());
    let mob = f.feared_mob(10);
    assert_eq!(
        f.fear(
            mob,
            100,
            FearRolls {
                flee: 0,
                ..all(100)
            }
        ),
        FearOutcome::Fled(Panic::Cornered)
    );
    assert_eq!(f.at(mob), Some(f.here));
}

#[test]
fn falter_prints_its_message_stops_the_fight_and_the_victim_fights_back() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    // power 20 vs 10: falter threshold 5 + 20 = 25.
    let rolls = FearRolls {
        falter: 24,
        ..all(100)
    };
    assert_eq!(f.fear(mob, 20, rolls), FearOutcome::Faltered);
    assert_eq!(f.at(mob), Some(f.here));
    assert!(!is_feared(&f.world, mob));
    assert_eq!(f.effect_secs(mob, "stun"), Some(2), "half a round of lag");
    assert_eq!(f.world.get::<Fighting>(mob).map(|x| x.0), Some(f.caster));
    let out = caster_rx(&mut f);
    assert!(
        out.contains("A jackal gets a scared look, but soldiers on."),
        "{out}"
    );
}

#[test]
fn nothing_happens_when_every_roll_misses() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    assert_eq!(f.fear(mob, 20, all(100)), FearOutcome::Shrugged);
    assert_eq!(f.at(mob), Some(f.here));
    assert!(!is_feared(&f.world, mob));
    assert!(f.world.get::<Stunned>(mob).is_none(), "no lag on a shrug");
    assert_eq!(f.world.get::<Fighting>(mob).map(|x| x.0), Some(f.caster));
    let out = caster_rx(&mut f);
    assert!(
        out.contains("A jackal barely raises an eyebrow at your fearful illusion."),
        "{out}"
    );
}

#[test]
fn a_sleeper_does_not_notice_the_illusion() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    f.world
        .entity_mut(mob)
        .insert(Posture(PostureKind::Sleeping));
    assert_eq!(f.fear(mob, 100, all(0)), FearOutcome::Unnoticed);
    assert!(!is_feared(&f.world, mob));
    assert_eq!(f.at(mob), Some(f.here));
    let out = caster_rx(&mut f);
    assert!(
        out.contains("A jackal is in no condition to notice your illusion."),
        "{out}"
    );
}

#[test]
fn an_already_paralysed_victim_does_not_even_move() {
    let mut f = fx();
    let mob = f.feared_mob(10);
    f.world.spawn((
        EffectInstance {
            kind: 1,
            name: "paralyzed".into(),
            strength: 1,
            remaining_secs: 30,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(mob),
    ));
    assert_eq!(f.fear(mob, 100, all(0)), FearOutcome::AlreadyParalysed);
    assert_eq!(f.at(mob), Some(f.here));
    let out = caster_rx(&mut f);
    assert!(out.contains("A jackal doesn't even move."), "{out}");
}

#[test]
fn the_spell_path_runs_the_cascade_after_the_effect_lands() {
    let mut f = fx();
    f.fear_spell(false);
    let mob = f.mob(10, serde_json::json!({}));
    f.world.insert_resource(PinnedFearRolls(FearRolls {
        freeze: 0,
        ..all(100)
    }));
    f.cast_fear("jackal");
    assert_eq!(f.at(mob), Some(f.here), "frozen, not fled");
    assert!(f.world.get::<Stunned>(mob).is_some());
    assert!(!is_feared(&f.world, mob));
}
