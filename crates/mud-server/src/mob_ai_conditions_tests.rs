//! WP-D additions to the combat-round engine: the extra rule conditions
//! (target HP above, alignment bands, negated life force, level cap, named
//! debuff, shield / weapon / outdoors, level-scaled chance), race breath, and
//! the flag-on hazards found reviewing WP-C (futile retries, telegraph spam,
//! area-attack bystanders, pets healing the wrong side).

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Composition, LifeForce, ObjectType};
use mud_world::{
    AiAlign, AiCondition, AiKind, AiTarget, AlignMask, AppliedTo, CombatStats, CompositionOverride,
    ConfigValue, EffectInstance, EffectSource, EquippedSlot, Fighting, Follower, GroupMember,
    Health, IndoorRoom, Item, LifeForceTag, Located, MobAi, MobPrototypes, Named, ObjectPrototypes,
    Slot, SpellSlots, WorldKey,
};

use crate::commands::test_support::{ability_def, drain, mob_proto, object_proto, player_in};
use crate::mob_combat_ai_tests::{
    BLESS, BREATH, FIREBALL, FX_DAMAGE, HEAL, KICK, PLAYER_HP, fireball, fixture, fixture_with, hp,
    last_used, round, rule, set_flag, spawn_mob,
};

const BREATHE_FIRE: i32 = 21;
const BREATHE_FROST: i32 = 22;
const POISON: i32 = 23;

fn with_conditions(mut r: mud_world::AiRule, when: Vec<AiCondition>) -> mud_world::AiRule {
    r.when = when;
    r
}

/// Add the extra abilities these tests use to the fixture's catalog.
fn add_abilities(world: &mut World) {
    let mut catalog = world.resource_mut::<mud_world::AbilityCatalog>();
    for (id, name, kind) in [
        (BREATHE_FIRE, "Breathe Fire", AbilityKind::Skill),
        (BREATHE_FROST, "Breathe Frost", AbilityKind::Skill),
        (POISON, "Poison", AbilityKind::Spell),
    ] {
        let mut def = ability_def(id, name, kind);
        def.violent = true;
        def.cast_time_rounds = 0;
        catalog.by_name.insert(name.to_ascii_lowercase(), def);
        catalog.effects_for.insert(
            id,
            vec![(
                FX_DAMAGE,
                Some(serde_json::json!({ "amount": 40, "duration": 30 })),
            )],
        );
    }
}

fn fires(f: &mut crate::mob_combat_ai_tests::Fixture, n: u64) -> bool {
    !round(&mut f.world, n).is_empty()
}

// ---- target_hp_above ----

#[test]
fn target_hp_above_gates_on_the_targets_health() {
    let r = with_conditions(
        fireball(AiTarget::Auto),
        vec![AiCondition::TargetHpAbove(50)],
    );
    let mut f = fixture(vec![r]);
    f.world.get_mut::<Health>(f.player).unwrap().hp = PLAYER_HP / 4;
    assert!(!fires(&mut f, 1));
    f.world.get_mut::<Health>(f.player).unwrap().hp = PLAYER_HP * 3 / 4;
    assert!(fires(&mut f, 2));
}

#[test]
fn a_self_buff_reads_the_opponents_health_not_its_own() {
    // Legacy: buff only while the victim is above 15%. Rule aims at the mob
    // itself; the HP test is about the player it fights.
    let r = with_conditions(
        rule(BLESS, AiKind::Spell, AiTarget::Auto),
        vec![AiCondition::TargetHpAbove(15)],
    );
    let mut f = fixture(vec![r]);
    f.world.get_mut::<Health>(f.player).unwrap().hp = PLAYER_HP / 10;
    assert!(!fires(&mut f, 1), "victim nearly dead: no buffing");
    f.world.get_mut::<Health>(f.player).unwrap().hp = PLAYER_HP;
    assert!(fires(&mut f, 2));
}

// ---- alignment ----

#[test]
fn self_align_is_the_casters_own_band() {
    let mask = AlignMask::of(&[AiAlign::Good]);
    let r = with_conditions(fireball(AiTarget::Auto), vec![AiCondition::SelfAlign(mask)]);
    let mut f = fixture(vec![r]);
    assert!(!fires(&mut f, 1), "a neutral mob is not good");
    f.world.get_mut::<CombatStats>(f.mob).unwrap().alignment = 500;
    assert!(fires(&mut f, 2));
    // The target's alignment is irrelevant to self_align.
    f.world.get_mut::<CombatStats>(f.player).unwrap().alignment = -900;
    assert!(fires(&mut f, 3));
}

#[test]
fn self_align_accepts_several_bands() {
    // "Not evil": good or neutral.
    let mask = AlignMask::of(&[AiAlign::Good, AiAlign::Neutral]);
    let r = with_conditions(fireball(AiTarget::Auto), vec![AiCondition::SelfAlign(mask)]);
    let mut f = fixture(vec![r]);
    assert!(fires(&mut f, 1), "neutral passes");
    f.world.get_mut::<CombatStats>(f.mob).unwrap().alignment = -600;
    assert!(!fires(&mut f, 2), "evil does not");
}

#[test]
fn align_and_not_align_are_about_the_target() {
    let evil_target = with_conditions(
        fireball(AiTarget::Auto),
        vec![AiCondition::Align(AiAlign::Evil)],
    );
    let mut f = fixture(vec![evil_target]);
    f.world.get_mut::<CombatStats>(f.mob).unwrap().alignment = -900;
    assert!(!fires(&mut f, 1), "the caster being evil does not count");
    f.world.get_mut::<CombatStats>(f.player).unwrap().alignment = -500;
    assert!(fires(&mut f, 2));

    let not_evil = with_conditions(
        fireball(AiTarget::Auto),
        vec![AiCondition::NotAlign(AiAlign::Evil)],
    );
    let mut g = fixture(vec![not_evil]);
    g.world.get_mut::<CombatStats>(g.player).unwrap().alignment = -500;
    assert!(
        !fires(&mut g, 1),
        "victim is evil: skipped (legacy hell bolt)"
    );
    g.world.get_mut::<CombatStats>(g.player).unwrap().alignment = 0;
    assert!(fires(&mut g, 2));
}

// ---- not_lifeforce, target_level_max ----

#[test]
fn not_lifeforce_skips_the_named_life_force() {
    let r = with_conditions(
        fireball(AiTarget::Auto),
        vec![
            AiCondition::NotLifeforce(LifeForce::Undead),
            AiCondition::NotLifeforce(LifeForce::Magic),
        ],
    );
    let mut f = fixture(vec![r]);
    f.world
        .entity_mut(f.player)
        .insert(LifeForceTag(LifeForce::Undead));
    assert!(!fires(&mut f, 1));
    f.world
        .entity_mut(f.player)
        .insert(LifeForceTag(LifeForce::Magic));
    assert!(!fires(&mut f, 2));
    f.world
        .entity_mut(f.player)
        .insert(LifeForceTag(LifeForce::Life));
    assert!(fires(&mut f, 3));
}

#[test]
fn target_level_max_is_relative_to_the_casters_level() {
    // The fixture mob is level 10; the player stands in for a level 40 mob.
    let mut f = fixture(vec![with_conditions(
        fireball(AiTarget::Auto),
        vec![AiCondition::TargetLevelMax(19)],
    )]);
    let mut big = mob_proto(30, 2, mud_db::enums::MobProfession::Trainer);
    big.level = 40;
    f.world
        .resource_mut::<MobPrototypes>()
        .by_key
        .insert((30, 2), big);
    f.world
        .entity_mut(f.player)
        .insert(WorldKey { zone: 30, id: 2 });
    assert!(!fires(&mut f, 1), "40 > 10 + 19");
    f.world.get_mut::<MobAi>(f.mob).unwrap().rules = std::sync::Arc::from(vec![with_conditions(
        fireball(AiTarget::Auto),
        vec![AiCondition::TargetLevelMax(30)],
    )]);
    assert!(fires(&mut f, 2), "40 <= 10 + 30");
}

// ---- named debuff ----

#[test]
fn a_named_debuff_rule_fires_only_for_that_debuff() {
    let cure = with_conditions(
        rule(HEAL, AiKind::Spell, AiTarget::SelfTarget),
        vec![AiCondition::SelfHasDebuffNamed(vec!["poison".into()])],
    );
    let mut f = fixture(vec![cure]);
    f.world.get_mut::<Health>(f.mob).unwrap().hp = 50_000;
    let chill = f
        .world
        .spawn((
            EffectInstance {
                kind: FX_DAMAGE,
                name: "curse".into(),
                strength: 1,
                remaining_secs: 60,
                source: EffectSource::Spell,
                ability_id: None,
            },
            AppliedTo(f.mob),
        ))
        .id();
    assert!(!fires(&mut f, 1), "a different debuff does not count");
    f.world.despawn(chill);
    f.world.spawn((
        EffectInstance {
            kind: FX_DAMAGE,
            name: "poison".into(),
            strength: 1,
            remaining_secs: 60,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(f.mob),
    ));
    assert!(fires(&mut f, 2), "matched by effect name");
}

#[test]
fn a_named_debuff_also_matches_the_applying_ability() {
    let cure = with_conditions(
        rule(HEAL, AiKind::Spell, AiTarget::SelfTarget),
        vec![AiCondition::SelfHasDebuffNamed(vec!["poison".into()])],
    );
    let mut f = fixture(vec![cure]);
    add_abilities(&mut f.world);
    f.world.get_mut::<Health>(f.mob).unwrap().hp = 50_000;
    f.world.spawn((
        EffectInstance {
            kind: FX_DAMAGE,
            name: "modify".into(),
            strength: 1,
            remaining_secs: 60,
            source: EffectSource::Spell,
            ability_id: Some(POISON),
        },
        AppliedTo(f.mob),
    ));
    assert!(fires(&mut f, 1));
}

// ---- equipment and room ----

fn equip(
    world: &mut World,
    mob: Entity,
    id: i32,
    slot: Slot,
    kind: ObjectType,
    damage: Option<&str>,
) {
    let mut proto = object_proto(1, id, kind);
    proto.weapon_damage_type = damage.map(str::to_string);
    if !world.contains_resource::<ObjectPrototypes>() {
        world.insert_resource(ObjectPrototypes::default());
    }
    world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .insert((1, id), proto);
    world.spawn((
        Item,
        Named {
            name: format!("item {id}"),
        },
        WorldKey { zone: 1, id },
        Located(mob),
        EquippedSlot(slot),
    ));
}

#[test]
fn requires_shield_needs_an_armor_piece_in_the_off_hand() {
    let r = with_conditions(
        rule(KICK, AiKind::Skill, AiTarget::Auto),
        vec![AiCondition::RequiresShield],
    );
    let mut f = fixture(vec![r]);
    assert!(!fires(&mut f, 1));
    // A torch held in the off hand is no shield.
    equip(&mut f.world, f.mob, 1, Slot::Hold, ObjectType::Light, None);
    assert!(!fires(&mut f, 2));
    equip(&mut f.world, f.mob, 2, Slot::Hold, ObjectType::Armor, None);
    // Two items in one slot cannot happen in play; this checks the armor one is found.
    let mut g = fixture(vec![with_conditions(
        rule(KICK, AiKind::Skill, AiTarget::Auto),
        vec![AiCondition::RequiresShield],
    )]);
    equip(&mut g.world, g.mob, 2, Slot::Hold, ObjectType::Armor, None);
    assert!(fires(&mut g, 1));
}

#[test]
fn requires_weapon_type_matches_the_wielded_damage_type() {
    let piercing = with_conditions(
        rule(KICK, AiKind::Skill, AiTarget::Auto),
        vec![AiCondition::RequiresWeaponType("piercing".into())],
    );
    let mut f = fixture(vec![piercing.clone()]);
    assert!(!fires(&mut f, 1), "no weapon");
    equip(
        &mut f.world,
        f.mob,
        3,
        Slot::Wield,
        ObjectType::Weapon,
        Some("Slash"),
    );
    assert!(!fires(&mut f, 2), "a slashing weapon is not piercing");

    for (damage, wanted, ok) in [
        ("Pierce", "piercing", true),
        ("Stab", "piercing", true),
        ("Slash", "slashing", true),
        ("Crush", "bludgeoning", true),
        ("Pierce", "slashing", false),
        ("Fire", "fire", true),
        ("Whip", "slashing", true),
        ("Claw", "slashing", true),
        ("Sting", "piercing", true),
        ("Maul", "bludgeoning", true),
        ("Whip", "piercing", false),
    ] {
        let r = with_conditions(
            rule(KICK, AiKind::Skill, AiTarget::Auto),
            vec![AiCondition::RequiresWeaponType(wanted.into())],
        );
        let mut g = fixture(vec![r]);
        equip(
            &mut g.world,
            g.mob,
            3,
            Slot::Wield,
            ObjectType::Weapon,
            Some(damage),
        );
        assert_eq!(fires(&mut g, 1), ok, "{damage} vs {wanted}");
    }
}

#[test]
fn outdoors_fails_in_an_indoor_room() {
    let r = with_conditions(fireball(AiTarget::Auto), vec![AiCondition::Outdoors]);
    let mut f = fixture(vec![r]);
    f.world.entity_mut(f.room).insert(IndoorRoom);
    assert!(!fires(&mut f, 1));
    f.world.entity_mut(f.room).remove::<IndoorRoom>();
    assert!(fires(&mut f, 2));
}

#[test]
fn composition_conditions_read_the_mobs_body() {
    let in_water = with_conditions(
        rule(KICK, AiKind::Skill, AiTarget::Auto),
        vec![AiCondition::Composition(vec![
            Composition::Water,
            Composition::Ice,
        ])],
    );
    let not_flesh = with_conditions(
        rule(KICK, AiKind::Skill, AiTarget::Auto),
        vec![AiCondition::NotComposition(vec![Composition::Flesh])],
    );
    let mut f = fixture(vec![in_water]);
    assert!(!fires(&mut f, 1), "flesh");
    f.world
        .entity_mut(f.mob)
        .insert(CompositionOverride(Composition::Water));
    assert!(fires(&mut f, 2));
    let mut g = fixture(vec![not_flesh]);
    assert!(!fires(&mut g, 1));
    g.world
        .entity_mut(g.mob)
        .insert(CompositionOverride(Composition::Stone));
    assert!(fires(&mut g, 2));
}

// ---- level-scaled chance, level roll ----

/// How many of `rounds` combat rounds the mob acted in.
fn acted_rounds(fx: &mut crate::mob_combat_ai_tests::Fixture, rounds: u64) -> u64 {
    let mut n = 0;
    for r in 1..=rounds {
        // Keep the player alive across thousands of kicks.
        fx.world.get_mut::<Health>(fx.player).unwrap().hp = PLAYER_HP;
        if !round(&mut fx.world, r).is_empty() {
            n += 1;
        }
    }
    n
}

fn kick_when(cond: AiCondition) -> mud_world::AiRule {
    with_conditions(rule(KICK, AiKind::Skill, AiTarget::Auto), vec![cond])
}

#[test]
fn level_scaled_chance_is_the_mobs_level_percent_times_the_scale() {
    let kick = kick_when(AiCondition::LevelScaledChance(1000));
    // Level 10: random(0, 101) < 10, about 10 in 102 of 1500 rounds.
    let mut base = fixture(vec![kick.clone()]);
    let n = acted_rounds(&mut base, 1500);
    assert!(
        (75..240).contains(&n),
        "level 10 acts about 10% of rounds, got {n}"
    );
    // The scale multiplies it: x5 is about half.
    let mut fives = fixture(vec![kick.clone()]);
    set_flag(&mut fives.world, "scale", ConfigValue::Float(5.0));
    let n = acted_rounds(&mut fives, 1500);
    assert!((570..930).contains(&n), "x5 scale, got {n}");
    // Scale 0 silences it; a huge scale always passes.
    let mut none = fixture(vec![kick.clone()]);
    set_flag(&mut none.world, "scale", ConfigValue::Float(0.0));
    assert_eq!(acted_rounds(&mut none, 50), 0);
    let mut always = fixture(vec![kick]);
    set_flag(&mut always.world, "scale", ConfigValue::Float(100.0));
    assert_eq!(acted_rounds(&mut always, 50), 50);
}

#[test]
fn a_level_scaled_factor_scales_the_level_percent() {
    let mut half = fixture(vec![kick_when(AiCondition::LevelScaledChance(5000))]);
    let n = acted_rounds(&mut half, 1500);
    assert!(
        (570..930).contains(&n),
        "factor 5 on level 10 is about 50%, got {n}"
    );
}

#[test]
fn level_roll_is_the_legacy_dragon_roll() {
    // random(0, 150 - 10) < 5 : 5 in 141 at level 10.
    let breath = with_conditions(
        rule(BREATHE_FIRE, AiKind::Skill, AiTarget::Auto),
        vec![AiCondition::LevelRoll { min: 0, max: 5 }],
    );
    let mut f = fixture(vec![breath]);
    add_abilities(&mut f.world);
    let n = acted_rounds(&mut f, 4000);
    assert!(
        (60..240).contains(&n),
        "about 5/141 of 4000 rounds, got {n}"
    );
}

// ---- race breath ----

#[test]
fn breath_element_follows_the_composition() {
    let fire_comp = vec![Composition::Fire, Composition::Lava];
    let frost_comp = vec![Composition::Water, Composition::Ice, Composition::Mist];
    let always = AiCondition::LevelRoll { min: 0, max: 1000 };
    let rules = vec![
        with_conditions(
            rule(BREATHE_FIRE, AiKind::Skill, AiTarget::Auto),
            vec![always.clone(), AiCondition::Composition(fire_comp)],
        ),
        with_conditions(
            rule(BREATHE_FROST, AiKind::Skill, AiTarget::Auto),
            vec![always, AiCondition::Composition(frost_comp)],
        ),
    ];
    for (body, expect_fire, expect_frost) in [
        (Composition::Lava, true, false),
        (Composition::Ice, false, true),
        (Composition::Stone, false, false),
    ] {
        let mut f = fixture(rules.clone());
        add_abilities(&mut f.world);
        f.world.entity_mut(f.mob).insert(CompositionOverride(body));
        round(&mut f.world, 1);
        assert_eq!(
            last_used(&f.world, f.mob, 0).is_some(),
            expect_fire,
            "{body:?} fire"
        );
        assert_eq!(
            last_used(&f.world, f.mob, 1).is_some(),
            expect_frost,
            "{body:?} frost"
        );
    }
}

#[test]
fn an_ordinary_body_rolls_its_element() {
    // The legacy "other" composition: the roll itself picks the element, so
    // both bands get used and never together.
    let flesh_only = |lo, hi| {
        vec![
            AiCondition::NotComposition(vec![Composition::Fire, Composition::Water]),
            AiCondition::LevelRoll { min: lo, max: hi },
        ]
    };
    let rules = vec![
        with_conditions(
            rule(BREATHE_FIRE, AiKind::Skill, AiTarget::Auto),
            flesh_only(0, 70),
        ),
        with_conditions(
            rule(BREATHE_FROST, AiKind::Skill, AiTarget::Auto),
            flesh_only(70, 141),
        ),
    ];
    let mut f = fixture(rules);
    add_abilities(&mut f.world);
    let (mut fire, mut frost) = (0, 0);
    for n in 1..=200 {
        f.world.get_mut::<Health>(f.player).unwrap().hp = PLAYER_HP;
        f.world.get_mut::<MobAi>(f.mob).unwrap().last_use.clear();
        round(&mut f.world, n);
        let (a, b) = (
            last_used(&f.world, f.mob, 0).is_some(),
            last_used(&f.world, f.mob, 1).is_some(),
        );
        assert!(!(a && b), "one roll, one breath");
        fire += i32::from(a);
        frost += i32::from(b);
    }
    assert!(fire > 40 && frost > 40, "fire {fire}, frost {frost}");
}

// ---- failed attempts, telegraph spam ----

fn refuse_without_weapon(world: &mut World, ability: i32) {
    world
        .resource_mut::<mud_world::AbilityCatalog>()
        .restriction_rules
        .insert(
            ability,
            vec![serde_json::json!({"type": "has_weapon", "message": "No weapon."})],
        );
}

#[test]
fn a_failed_attempt_rests_the_rule_for_a_few_rounds() {
    let mut f = fixture(vec![fireball(AiTarget::Auto)]);
    refuse_without_weapon(&mut f.world, FIREBALL);
    assert!(!fires(&mut f, 1));
    let failed =
        |f: &crate::mob_combat_ai_tests::Fixture| f.world.get::<MobAi>(f.mob).unwrap().failed_at(0);
    assert_eq!(failed(&f), Some(40), "tried and refused in round 1");
    assert!(!fires(&mut f, 2));
    assert!(!fires(&mut f, 3));
    assert_eq!(failed(&f), Some(40), "rounds 2 and 3 did not retry");
    assert!(!fires(&mut f, 4));
    assert_eq!(failed(&f), Some(160), "round 4 tries again");
}

#[test]
fn a_mob_out_of_slots_does_not_telegraph() {
    let mut breath = rule(BREATH, AiKind::Spell, AiTarget::Room);
    breath.when = vec![AiCondition::Telegraph];
    // The breath is a spell outside the slot model unless the class owns it.
    let mut f = fixture_with(vec![breath], 0);
    f.world
        .resource_mut::<mud_world::SpellSlotData>()
        .ability_circle
        .insert((crate::mob_combat_ai_tests::CLASS, BREATH), 1);
    let _ = drain(&mut f.rx);
    for n in 1..=8 {
        round(&mut f.world, n);
    }
    let seen = drain(&mut f.rx);
    assert!(
        !seen.contains("gathers its power"),
        "no slot, no warning: {seen:?}"
    );
    assert_eq!(hp(&f.world, f.player), PLAYER_HP);
}

#[test]
fn a_telegraph_does_not_repeat_once_the_slots_are_gone() {
    let mut breath = rule(BREATH, AiKind::Spell, AiTarget::Room);
    breath.when = vec![AiCondition::Telegraph];
    let mut f = fixture_with(vec![breath], 1);
    f.world
        .resource_mut::<mud_world::SpellSlotData>()
        .ability_circle
        .insert((crate::mob_combat_ai_tests::CLASS, BREATH), 1);
    let _ = drain(&mut f.rx);
    for n in 1..=10 {
        round(&mut f.world, n);
    }
    let warnings = drain(&mut f.rx).matches("gathers its power").count();
    assert_eq!(
        warnings, 1,
        "one warning, one breath, then nothing while the slot recovers"
    );
}

#[test]
fn a_telegraph_that_cannot_follow_through_is_forgotten() {
    let mut breath = rule(BREATH, AiKind::Spell, AiTarget::Room);
    breath.when = vec![AiCondition::Telegraph];
    let mut f = fixture_with(vec![breath], 1);
    f.world
        .resource_mut::<mud_world::SpellSlotData>()
        .ability_circle
        .insert((crate::mob_combat_ai_tests::CLASS, BREATH), 1);
    round(&mut f.world, 1);
    assert!(f.world.get::<MobAi>(f.mob).unwrap().warned_at(0).is_some());
    // The slot is gone by the time the breath is due.
    f.world
        .get_mut::<SpellSlots>(f.mob)
        .unwrap()
        .in_flight
        .push(mud_world::SpellCooldown {
            circle: 1,
            secs_remaining: 600,
            total_secs: 600,
        });
    let _ = drain(&mut f.rx);
    assert!(round(&mut f.world, 2).is_empty());
    assert!(
        f.world.get::<MobAi>(f.mob).unwrap().warned_at(0).is_none(),
        "the stale warning is gone"
    );
    assert_eq!(hp(&f.world, f.player), PLAYER_HP);
}

#[test]
fn a_spell_without_a_slot_costs_no_evaluation() {
    // Three slotless spells ahead of a skill must not use up the round.
    let mut f = fixture_with(
        vec![
            fireball(AiTarget::Auto),
            fireball(AiTarget::Auto),
            fireball(AiTarget::Auto),
            rule(KICK, AiKind::Skill, AiTarget::Auto),
        ],
        0,
    );
    assert!(fires(&mut f, 1), "the kick behind them still gets its turn");
    assert_eq!(last_used(&f.world, f.mob, 3), Some(40));
}

// ---- area attacks and allies ----

fn bystander_fixture() -> (crate::mob_combat_ai_tests::Fixture, Entity, Entity) {
    let mut f = fixture(vec![rule(BREATH, AiKind::Spell, AiTarget::Room)]);
    let alive = |w: &mut World, name: &str| {
        let (p, rx) = player_in(w, f.room);
        std::mem::forget(rx);
        w.entity_mut(p).insert((
            Health {
                hp: PLAYER_HP,
                max: PLAYER_HP,
            },
            CombatStats::default(),
            Named {
                name: name.to_string(),
            },
        ));
        p
    };
    let groupmate = alive(&mut f.world, "Buddy");
    let bystander = alive(&mut f.world, "Stranger");
    f.world.entity_mut(groupmate).insert(GroupMember(f.player));
    (f, groupmate, bystander)
}

#[test]
fn an_area_breath_hits_the_foes_group_but_not_bystanders() {
    let (mut f, groupmate, bystander) = bystander_fixture();
    assert_eq!(round(&mut f.world, 1).len(), 1);
    assert!(hp(&f.world, f.player) < PLAYER_HP, "the foe");
    assert!(hp(&f.world, groupmate) < PLAYER_HP, "the foe's group");
    assert_eq!(hp(&f.world, bystander), PLAYER_HP, "a bystander is spared");
}

#[test]
fn an_area_breath_hits_the_foes_pets() {
    let (mut f, _groupmate, _bystander) = bystander_fixture();
    let pet = spawn_mob(&mut f.world, f.room, "a wolf", "wolf", None);
    f.world.entity_mut(pet).insert(Follower(f.player));
    round(&mut f.world, 1);
    assert!(hp(&f.world, pet) < 100_000, "a pet fights for its master");
}

#[test]
fn a_charmed_casters_breath_never_hits_its_master() {
    let mut f = fixture(vec![rule(BREATH, AiKind::Spell, AiTarget::Room)]);
    // The master is in the group of the player the charmed mob fights.
    let (master, rx) = player_in(&mut f.world, f.room);
    std::mem::forget(rx);
    f.world.entity_mut(master).insert((
        Health {
            hp: PLAYER_HP,
            max: PLAYER_HP,
        },
        CombatStats::default(),
        Named {
            name: "Master".into(),
        },
        GroupMember(f.player),
    ));
    f.world.entity_mut(f.mob).insert(Follower(master));
    f.world.spawn((
        EffectInstance {
            kind: FX_DAMAGE,
            name: "charmed".into(),
            strength: 1,
            remaining_secs: 60,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(f.mob),
    ));
    assert!(crate::commands::is_servant(&f.world, f.mob));
    let ogre = spawn_mob(&mut f.world, f.room, "an ogre", "ogre", None);
    f.world.entity_mut(ogre).insert(Fighting(f.mob));
    f.world.entity_mut(f.mob).insert(Fighting(ogre));
    let hits = crate::mob_ai::mob_aoe_enemies(&mut f.world, f.mob, f.room);
    let hit: Vec<Entity> = hits.iter().map(|(e, _)| *e).collect();
    assert!(hit.contains(&ogre), "its foe is a target");
    assert!(!hit.contains(&master), "its master is not");
    assert!(!hit.contains(&f.player), "nor is its master's group");
}

#[test]
fn a_pet_heals_its_masters_side_not_the_mobs_hostile_to_it() {
    let mut f = fixture(vec![]);
    let pet = spawn_mob(&mut f.world, f.room, "a wolf", "wolf", None);
    f.world.entity_mut(pet).insert((
        MobAi::new(std::sync::Arc::from(vec![rule(
            HEAL,
            AiKind::Spell,
            AiTarget::LowestAlly,
        )])),
        Follower(f.player),
        Fighting(f.mob),
        SpellSlots::default(),
    ));
    // The hostile mob is more hurt than the master.
    f.world.get_mut::<Health>(f.mob).unwrap().hp = 10_000;
    f.world.get_mut::<Health>(f.player).unwrap().hp = PLAYER_HP / 2;
    f.world.entity_mut(f.mob).insert(Fighting(f.player));
    f.world.entity_mut(f.player).insert(Fighting(f.mob));
    let orc = spawn_mob(&mut f.world, f.room, "an orc", "orc", None);
    f.world.get_mut::<Health>(orc).unwrap().hp = 5_000;
    assert_eq!(round(&mut f.world, 1).len(), 1);
    assert!(
        hp(&f.world, f.player) > PLAYER_HP / 2,
        "the master was healed"
    );
    assert_eq!(hp(&f.world, orc), 5_000, "the orc is on the other side");
    assert_eq!(hp(&f.world, f.mob), 10_000, "so is its master's foe");
}

#[test]
fn a_free_mob_does_not_heal_a_mob_that_is_fighting_another_free_mob() {
    let mut f = fixture(vec![rule(HEAL, AiKind::Spell, AiTarget::LowestAlly)]);
    let brawler = spawn_mob(&mut f.world, f.room, "a brawler", "brawler", None);
    let victim = spawn_mob(&mut f.world, f.room, "a victim", "victim", None);
    f.world.entity_mut(brawler).insert(Fighting(victim));
    f.world.get_mut::<Health>(brawler).unwrap().hp = 1_000;
    let friend = spawn_mob(&mut f.world, f.room, "a friend", "friend", None);
    f.world.get_mut::<Health>(friend).unwrap().hp = 60_000;
    assert_eq!(round(&mut f.world, 1).len(), 1);
    assert_eq!(
        hp(&f.world, brawler),
        1_000,
        "mid-brawl with another free mob: not an ally"
    );
    assert!(hp(&f.world, friend) > 60_000, "the calm friend is");
}

// ---- already-applied effects ----

#[test]
fn a_buff_the_mob_already_wears_is_not_cast_again() {
    let r = rule(BLESS, AiKind::Spell, AiTarget::Auto);
    let mut f = fixture(vec![r]);
    f.world.spawn((
        EffectInstance {
            kind: crate::mob_combat_ai_tests::FX_BUFF,
            name: "bless".into(),
            strength: 1,
            remaining_secs: 60,
            source: EffectSource::Spell,
            ability_id: Some(BLESS),
        },
        AppliedTo(f.mob),
    ));
    assert!(!fires(&mut f, 1));
}
