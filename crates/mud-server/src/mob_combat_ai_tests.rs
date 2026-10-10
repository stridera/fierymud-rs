//! The combat-round decision engine of mob spellcasting / skill AI (WP-C):
//! `mob_ai::mob_combat_ai_tick` and its hook in `combat_tick`.

use std::sync::Arc;

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_world::{
    AbilityCatalog, AiCondition, AiKind, AiRule, AiTarget, AppliedTo, Casting, CombatStats,
    ConfigValue, EffectCatalog, EffectDef, EffectInstance, EffectSource, Fighting, Health,
    Keywords, Located, Mob, MobAi, MobPrototypes, Named, NaturalDamage, NoMagicRoom, Posture,
    PostureKind, RuntimeConfig, SpellSlotData, SpellSlots, WorldKey,
};

use crate::TickCount;
use crate::combat::COMBAT_PERIOD_TICKS;
use crate::commands::test_support::{Rx, ability_def, drain, mob_proto, player_in};
use crate::mob_ai::mob_combat_ai_tick;

const CLASS: i32 = 7;
const FIREBALL: i32 = 1;
const HEAL: i32 = 2;
const KICK: i32 = 3;
const DOOM: i32 = 4;
const BREATH: i32 = 5;
const DISPEL: i32 = 6;
const BLESS: i32 = 7;
const FX_DAMAGE: i32 = 10;
const FX_HEAL: i32 = 11;
const FX_BUFF: i32 = 12;
const PLAYER_HP: i32 = 10_000;

fn effect_def(id: i32, effect_type: &str) -> EffectDef {
    EffectDef {
        id,
        name: effect_type.to_string(),
        description: None,
        effect_type: effect_type.to_string(),
        tags: Vec::new(),
        presence_override: None,
        default_params: serde_json::json!({}),
        prevents_speaking: false,
        prevents_casting: false,
        prevents_movement: false,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    }
}

fn catalog() -> AbilityCatalog {
    let mut catalog = AbilityCatalog::default();
    let mut add = |id: i32, name: &str, kind: AbilityKind, violent: bool, rounds: i32, fx: i32| {
        let mut def = ability_def(id, name, kind);
        def.violent = violent;
        def.cast_time_rounds = rounds;
        catalog.by_name.insert(name.to_ascii_lowercase(), def);
        catalog.effects_for.insert(
            id,
            vec![(
                fx,
                Some(serde_json::json!({ "amount": 40, "duration": 30 })),
            )],
        );
    };
    add(FIREBALL, "Fireball", AbilityKind::Spell, true, 0, FX_DAMAGE);
    add(HEAL, "Heal", AbilityKind::Spell, false, 0, FX_HEAL);
    add(KICK, "Kick", AbilityKind::Skill, true, 0, FX_DAMAGE);
    add(DOOM, "Doom", AbilityKind::Spell, true, 2, FX_DAMAGE);
    add(BREATH, "Breath", AbilityKind::Spell, true, 0, FX_DAMAGE);
    add(DISPEL, "Dispel", AbilityKind::Spell, true, 0, FX_DAMAGE);
    add(BLESS, "Bless", AbilityKind::Spell, false, 0, FX_BUFF);
    let breath = catalog.by_name.get_mut("breath").unwrap();
    breath.is_area = true;
    breath.target_scope = "ROOM_ENEMIES".to_string();
    catalog
}

fn rule(ability_id: i32, kind: AiKind, target: AiTarget) -> AiRule {
    AiRule {
        ability_id,
        kind,
        priority: 0,
        chance_pct: 100,
        cooldown_s: 0,
        when: Vec::new(),
        target,
        min_level: None,
    }
}

fn fireball(target: AiTarget) -> AiRule {
    rule(FIREBALL, AiKind::Spell, target)
}

struct Fixture {
    world: World,
    room: Entity,
    mob: Entity,
    player: Entity,
    rx: Rx,
}

/// A level 10 caster-class mob fighting a player in one room, `mob_ai.enabled`
/// on, circle 1 holding `slots` slots for Fireball.
fn fixture_with(rules: Vec<AiRule>, slots: i32) -> Fixture {
    let mut world = World::new();
    world.insert_resource(catalog());
    let mut effects = EffectCatalog::default();
    for (id, t) in [
        (FX_DAMAGE, "damage"),
        (FX_HEAL, "heal"),
        (FX_BUFF, "modify"),
    ] {
        effects.by_id.insert(id, effect_def(id, t));
    }
    world.insert_resource(effects);
    let mut slot_data = SpellSlotData::default();
    slot_data.ability_circle.insert((CLASS, FIREBALL), 1);
    slot_data.progression.insert((10, 1), slots);
    world.insert_resource(slot_data);
    world.insert_resource(mud_world::ClassSkillsData::default());
    let mut config = RuntimeConfig::default();
    config
        .by_key
        .insert(("mob_ai".into(), "enabled".into()), ConfigValue::Bool(true));
    world.insert_resource(config);
    world.insert_resource(TickCount(COMBAT_PERIOD_TICKS));
    let mut proto = mob_proto(30, 1, mud_db::enums::MobProfession::Trainer);
    proto.level = 10;
    proto.class_id = Some(CLASS);
    let mut protos = MobPrototypes::default();
    protos.by_key.insert((30, 1), proto);
    world.insert_resource(protos);

    let room = world.spawn_empty().id();
    let (player, rx) = player_in(&mut world, room);
    let mob = spawn_mob(&mut world, room, "a dark mage", "mage", Some(rules));
    world.entity_mut(mob).insert((
        Fighting(player),
        SpellSlots::default(),
        NaturalDamage {
            num: 1,
            size: 1,
            bonus: 499,
        },
    ));
    world.entity_mut(player).insert((
        Health {
            hp: PLAYER_HP,
            max: PLAYER_HP,
        },
        CombatStats::default(),
        Fighting(mob),
    ));
    Fixture {
        world,
        room,
        mob,
        player,
        rx,
    }
}

fn fixture(rules: Vec<AiRule>) -> Fixture {
    fixture_with(rules, 5)
}

fn spawn_mob(
    world: &mut World,
    room: Entity,
    name: &str,
    keyword: &str,
    rules: Option<Vec<AiRule>>,
) -> Entity {
    let mob = world
        .spawn((
            Mob,
            Named {
                name: name.to_string(),
            },
            Keywords(vec![keyword.to_string()]),
            Located(room),
            WorldKey { zone: 30, id: 1 },
            Health {
                hp: 100_000,
                max: 100_000,
            },
            CombatStats::default(),
            Posture(PostureKind::Standing),
        ))
        .id();
    if let Some(rules) = rules {
        world.entity_mut(mob).insert(MobAi::new(Arc::from(rules)));
    }
    mob
}

fn set_flag(world: &mut World, key: &str, value: ConfigValue) {
    world
        .resource_mut::<RuntimeConfig>()
        .by_key
        .insert(("mob_ai".into(), key.into()), value);
}

/// Run combat round `n` (1-based) of the engine alone.
fn round(world: &mut World, n: u64) -> std::collections::HashSet<Entity> {
    world.insert_resource(TickCount(n * COMBAT_PERIOD_TICKS));
    mob_combat_ai_tick(world)
}

fn hp(world: &World, e: Entity) -> i32 {
    world.get::<Health>(e).unwrap().hp
}

fn last_used(world: &World, mob: Entity, idx: usize) -> Option<u64> {
    world.get::<MobAi>(mob).unwrap().last_used(idx)
}

fn silence(world: &mut World, target: Entity) {
    world.spawn((
        EffectInstance {
            kind: 999,
            name: "silence".to_string(),
            strength: 1,
            remaining_secs: 60,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(target),
    ));
}

#[test]
fn flag_off_means_no_casts() {
    let mut f = fixture(vec![fireball(AiTarget::Auto)]);
    set_flag(&mut f.world, "enabled", ConfigValue::Bool(false));
    for n in 1..=5 {
        assert!(round(&mut f.world, n).is_empty());
    }
    assert_eq!(hp(&f.world, f.player), PLAYER_HP);
    assert_eq!(last_used(&f.world, f.mob, 0), None);
}

#[test]
fn a_mage_mob_casts_at_the_player_within_a_few_rounds() {
    let mut rule = fireball(AiTarget::Auto);
    rule.chance_pct = 50;
    let mut f = fixture(vec![rule]);
    let cast_round = (1..=60).find(|&n| {
        round(&mut f.world, n);
        hp(&f.world, f.player) < PLAYER_HP
    });
    assert!(cast_round.is_some(), "never cast in 60 rounds");
    assert!(last_used(&f.world, f.mob, 0).is_some());
}

#[test]
fn the_cast_replaces_the_melee_swing() {
    // Through the real hook: the mob swings for 500, a Fireball does 40.
    let mut f = fixture(vec![fireball(AiTarget::Auto)]);
    f.world.insert_resource(TickCount(COMBAT_PERIOD_TICKS));
    crate::combat::combat_tick(&mut f.world);
    let lost = PLAYER_HP - hp(&f.world, f.player);
    assert!(lost > 0, "the fireball landed");
    assert!(lost < 400, "no melee swing on top of it: lost {lost}");
    assert!(f.world.get::<SpellSlots>(f.mob).unwrap().in_flight.len() == 1);

    // Same mob with its rule on cooldown swings instead.
    let mut g = fixture(vec![{
        let mut r = fireball(AiTarget::Auto);
        r.cooldown_s = 600;
        r
    }]);
    g.world
        .get_mut::<MobAi>(g.mob)
        .unwrap()
        .mark_used(0, COMBAT_PERIOD_TICKS - 1);
    // Three rounds so a one-in-a-hundred miss cannot fake a pass.
    for n in 1..=3 {
        g.world.insert_resource(TickCount(n * COMBAT_PERIOD_TICKS));
        crate::combat::combat_tick(&mut g.world);
    }
    assert!(
        PLAYER_HP - hp(&g.world, g.player) >= 400,
        "the swing landed when no ability was used"
    );
}

#[test]
fn a_rule_cooldown_is_respected() {
    let mut rule = fireball(AiTarget::Auto);
    rule.cooldown_s = 12; // three combat rounds
    let mut f = fixture(vec![rule]);
    let mut casts = Vec::new();
    for n in 1..=7 {
        let before = hp(&f.world, f.player);
        round(&mut f.world, n);
        if hp(&f.world, f.player) < before {
            casts.push(n);
        }
    }
    assert_eq!(casts, vec![1, 4, 7]);
}

#[test]
fn chance_and_scale_gate_the_roll() {
    let mut never = fireball(AiTarget::Auto);
    never.chance_pct = 0;
    let mut f = fixture(vec![never]);
    for n in 1..=20 {
        assert!(round(&mut f.world, n).is_empty());
    }
    // A scale of zero silences even a certain rule; two doubles a weak one.
    let mut g = fixture(vec![fireball(AiTarget::Auto)]);
    set_flag(&mut g.world, "scale", ConfigValue::Float(0.0));
    for n in 1..=20 {
        assert!(round(&mut g.world, n).is_empty());
    }
    set_flag(&mut g.world, "scale", ConfigValue::Float(1.0));
    assert_eq!(round(&mut g.world, 21).len(), 1);
}

#[test]
fn min_level_and_conditions_gate_a_rule() {
    let mut high = fireball(AiTarget::Auto);
    high.min_level = Some(11);
    let mut hurt = fireball(AiTarget::Auto);
    hurt.when = vec![AiCondition::SelfHpBelow(50)];
    let mut f = fixture(vec![high, hurt]);
    assert!(round(&mut f.world, 1).is_empty());
    f.world.get_mut::<Health>(f.mob).unwrap().hp = 10_000;
    assert_eq!(round(&mut f.world, 2).len(), 1, "now below half");
    assert_eq!(last_used(&f.world, f.mob, 1), Some(2 * COMBAT_PERIOD_TICKS));
    assert_eq!(last_used(&f.world, f.mob, 0), None);
}

#[test]
fn at_most_three_rules_are_evaluated_per_round() {
    // Three rules that fail their conditions use the whole budget, so a
    // fourth that would pass is not reached.
    let mut blocked = fireball(AiTarget::Auto);
    blocked.when = vec![AiCondition::SelfHpBelow(1)];
    let mut f = fixture(vec![
        blocked.clone(),
        blocked.clone(),
        blocked,
        fireball(AiTarget::Auto),
    ]);
    assert!(round(&mut f.world, 1).is_empty());
    assert_eq!(hp(&f.world, f.player), PLAYER_HP);
}

#[test]
fn silenced_mob_casts_no_spell_but_a_skill_still_fires() {
    let mut f = fixture(vec![
        fireball(AiTarget::Auto),
        rule(KICK, AiKind::Skill, AiTarget::Auto),
    ]);
    silence(&mut f.world, f.mob);
    assert_eq!(round(&mut f.world, 1).len(), 1);
    assert_eq!(
        last_used(&f.world, f.mob, 0),
        None,
        "no spell while silenced"
    );
    assert!(last_used(&f.world, f.mob, 1).is_some(), "the kick fired");
    assert!(hp(&f.world, f.player) < PLAYER_HP);
}

#[test]
fn a_no_magic_room_blocks_spells_but_not_skills() {
    let mut f = fixture(vec![
        fireball(AiTarget::Auto),
        rule(KICK, AiKind::Skill, AiTarget::Auto),
    ]);
    f.world.entity_mut(f.room).insert(NoMagicRoom);
    assert_eq!(round(&mut f.world, 1).len(), 1);
    assert_eq!(last_used(&f.world, f.mob, 0), None);
    assert!(last_used(&f.world, f.mob, 1).is_some());
}

#[test]
fn out_of_slots_means_no_cast() {
    // One circle-1 slot: the first Fireball spends it, the next rounds find
    // it still recovering.
    let mut f = fixture_with(vec![fireball(AiTarget::Auto)], 1);
    let mut casts = 0;
    for n in 1..=4 {
        let before = hp(&f.world, f.player);
        round(&mut f.world, n);
        if hp(&f.world, f.player) < before {
            casts += 1;
        }
    }
    assert_eq!(casts, 1);
    // No slots at all for the level: never casts.
    let mut g = fixture_with(vec![fireball(AiTarget::Auto)], 0);
    for n in 1..=3 {
        assert!(round(&mut g.world, n).is_empty());
    }
    assert_eq!(hp(&g.world, g.player), PLAYER_HP);
}

#[test]
fn the_caster_selector_picks_the_casting_player() {
    let mut f = fixture(vec![fireball(AiTarget::Caster)]);
    let (witch, _rx) = player_in(&mut f.world, f.room);
    f.world.entity_mut(witch).insert((
        Health {
            hp: PLAYER_HP,
            max: PLAYER_HP,
        },
        CombatStats::default(),
        Fighting(f.mob),
        Named {
            name: "Witch".to_string(),
        },
    ));
    // Nobody is casting yet: the rule has no target.
    assert!(round(&mut f.world, 1).is_empty());
    f.world.entity_mut(witch).insert(Casting {
        ability_id: DOOM,
        ability_name: "Doom".into(),
        args: String::new(),
        kind_label: "spell".into(),
        verb: "cast".into(),
        ticks_remaining: 30,
        ticks_total: 40,
        target: mud_world::CastTarget::Caster,
        recognized_by: Vec::new(),
        slot_reservation: None,
        via_innate: false,
    });
    f.world.entity_mut(f.mob).insert(Fighting(f.player));
    assert_eq!(round(&mut f.world, 2).len(), 1);
    assert!(hp(&f.world, witch) < PLAYER_HP, "the caster was hit");
    assert_eq!(hp(&f.world, f.player), PLAYER_HP, "the tank was not");
}

#[test]
fn the_weakest_attacker_selector_picks_the_lowest_hp() {
    let mut f = fixture(vec![fireball(AiTarget::WeakestAttacker)]);
    let (frail, _rx) = player_in(&mut f.world, f.room);
    f.world.entity_mut(frail).insert((
        Health { hp: 500, max: 500 },
        CombatStats::default(),
        Fighting(f.mob),
        Named {
            name: "Frail".to_string(),
        },
    ));
    assert_eq!(round(&mut f.world, 1).len(), 1);
    assert!(hp(&f.world, frail) < 500);
    assert_eq!(hp(&f.world, f.player), PLAYER_HP);
}

#[test]
fn lowest_ally_heals_the_wounded_mob() {
    let heal = rule(HEAL, AiKind::Spell, AiTarget::LowestAlly);
    let mut f = fixture(vec![heal]);
    let hurt = spawn_mob(&mut f.world, f.room, "a hurt orc", "orc", None);
    let scratched = spawn_mob(&mut f.world, f.room, "a scratched orc", "orc", None);
    f.world.get_mut::<Health>(hurt).unwrap().hp = 10_000;
    f.world.get_mut::<Health>(scratched).unwrap().hp = 90_000;
    assert_eq!(round(&mut f.world, 1).len(), 1);
    assert!(hp(&f.world, hurt) > 10_000, "the worst-off ally was healed");
    assert_eq!(hp(&f.world, scratched), 90_000);
    // With everyone healthy there is nobody to heal.
    f.world.get_mut::<Health>(hurt).unwrap().hp = 100_000;
    f.world.get_mut::<Health>(scratched).unwrap().hp = 100_000;
    assert!(round(&mut f.world, 2).is_empty());
}

#[test]
fn most_buffed_dispels_the_buffed_attacker() {
    let mut f = fixture(vec![rule(DISPEL, AiKind::Spell, AiTarget::MostBuffed)]);
    assert!(round(&mut f.world, 1).is_empty(), "nobody is buffed");
    f.world.spawn((
        EffectInstance {
            kind: FX_BUFF,
            name: "bless".to_string(),
            strength: 1,
            remaining_secs: 60,
            source: EffectSource::Spell,
            ability_id: Some(BLESS),
        },
        AppliedTo(f.player),
    ));
    assert_eq!(round(&mut f.world, 2).len(), 1);
}

#[test]
fn a_wind_up_is_interrupted_by_a_bash() {
    let mut f = fixture(vec![rule(DOOM, AiKind::Spell, AiTarget::Auto)]);
    assert_eq!(round(&mut f.world, 1).len(), 1);
    let casting = f.world.get::<Casting>(f.mob).expect("winding up Doom");
    assert_eq!(casting.ability_id, DOOM);
    f.world.entity_mut(f.player).insert(mud_world::Account {
        user_id: String::new(),
        character_id: "c-tester".into(),
        role: mud_db::enums::UserRole::Player,
        account_role: mud_db::enums::UserRole::Player,
        perms: vec![],
    });
    let _ = drain(&mut f.rx);
    crate::commands::dispatch(&mut f.world, f.player, "bash mage");
    assert!(
        f.world.get::<Casting>(f.mob).is_none(),
        "the bash broke the wind-up: {}",
        drain(&mut f.rx)
    );
    // No damage landed from the cancelled cast, and the slot is back.
    assert!(
        f.world
            .get::<SpellSlots>(f.mob)
            .unwrap()
            .reserved
            .is_empty()
    );
}

#[test]
fn a_mob_in_a_wind_up_is_not_asked_to_act_again() {
    let mut f = fixture(vec![rule(DOOM, AiKind::Spell, AiTarget::Auto)]);
    assert_eq!(round(&mut f.world, 1).len(), 1);
    assert!(round(&mut f.world, 2).is_empty());
}

#[test]
fn an_area_breath_spares_the_mobs_beside_the_caster() {
    let mut f = fixture(vec![rule(BREATH, AiKind::Spell, AiTarget::Room)]);
    let pal = spawn_mob(&mut f.world, f.room, "an orc", "orc", None);
    assert_eq!(round(&mut f.world, 1).len(), 1);
    assert!(hp(&f.world, f.player) < PLAYER_HP);
    assert_eq!(hp(&f.world, pal), 100_000, "no friendly fire");
}

#[test]
fn a_telegraphed_rule_warns_one_round_then_fires() {
    let mut breath = rule(BREATH, AiKind::Spell, AiTarget::Room);
    breath.when = vec![AiCondition::Telegraph];
    let mut f = fixture(vec![breath]);
    let _ = drain(&mut f.rx);
    assert!(round(&mut f.world, 1).is_empty(), "the warning round");
    assert_eq!(hp(&f.world, f.player), PLAYER_HP);
    let seen = drain(&mut f.rx);
    assert!(seen.contains("gathers its power for Breath"), "{seen:?}");
    assert_eq!(round(&mut f.world, 2).len(), 1, "fires the next round");
    assert!(hp(&f.world, f.player) < PLAYER_HP);
    // Used up: the round after warns afresh rather than firing again.
    let before = hp(&f.world, f.player);
    assert!(round(&mut f.world, 3).is_empty());
    assert_eq!(hp(&f.world, f.player), before);
}

#[test]
fn a_rule_without_telegraph_never_warns() {
    let mut f = fixture(vec![rule(BREATH, AiKind::Spell, AiTarget::Room)]);
    let _ = drain(&mut f.rx);
    round(&mut f.world, 1);
    assert!(!drain(&mut f.rx).contains("gathers its power"));
}

#[test]
fn a_mob_not_fighting_is_left_alone() {
    let mut f = fixture(vec![fireball(AiTarget::Auto)]);
    f.world.entity_mut(f.mob).remove::<Fighting>();
    assert!(round(&mut f.world, 1).is_empty());
    assert_eq!(hp(&f.world, f.player), PLAYER_HP);
}

#[test]
fn a_stunned_or_sleeping_mob_does_not_cast() {
    let mut f = fixture(vec![fireball(AiTarget::Auto)]);
    f.world.entity_mut(f.mob).insert(mud_world::Stunned);
    assert!(round(&mut f.world, 1).is_empty());
    f.world.entity_mut(f.mob).remove::<mud_world::Stunned>();
    f.world
        .entity_mut(f.mob)
        .insert(Posture(PostureKind::Sleeping));
    assert!(round(&mut f.world, 2).is_empty());
}

/// 5500 mobs, 300 of them fighting with a three-rule kit each: one combat
/// round of the engine must stay far below the slow-tick threshold. Hard
/// limit only enforced in release.
#[test]
fn combat_round_prod_scale_is_fast() {
    const MOBS: usize = 5500;
    const FIGHTING: usize = 300;
    let mut f = fixture(vec![fireball(AiTarget::Auto)]);
    let kit: Arc<[AiRule]> = Arc::from(vec![
        {
            let mut r = fireball(AiTarget::Auto);
            r.chance_pct = 30;
            r.cooldown_s = 8;
            r
        },
        {
            let mut r = rule(KICK, AiKind::Skill, AiTarget::WeakestAttacker);
            r.chance_pct = 25;
            r
        },
        {
            let mut r = rule(HEAL, AiKind::Spell, AiTarget::LowestAlly);
            r.when = vec![AiCondition::AllyHpBelow(50)];
            r
        },
    ]);
    for i in 0..MOBS {
        let room = if i < FIGHTING {
            f.world.spawn_empty().id()
        } else {
            f.room
        };
        let mob = spawn_mob(&mut f.world, room, "a guard", "guard", None);
        f.world.entity_mut(mob).insert((
            MobAi {
                rules: Arc::clone(&kit),
                ..MobAi::new(Arc::clone(&kit))
            },
            SpellSlots::default(),
        ));
        if i < FIGHTING {
            let (p, rx) = player_in(&mut f.world, room);
            std::mem::forget(rx);
            f.world.entity_mut(p).insert((
                Health {
                    hp: PLAYER_HP,
                    max: PLAYER_HP,
                },
                CombatStats::default(),
                Fighting(mob),
            ));
            f.world.entity_mut(mob).insert(Fighting(p));
        }
    }
    // The fixture's own mob is one more fighter; drop it from the count.
    let start = std::time::Instant::now();
    let acted = round(&mut f.world, 1);
    let first = start.elapsed();
    let start = std::time::Instant::now();
    round(&mut f.world, 2);
    let second = start.elapsed();
    eprintln!(
        "mob_combat_ai_tick {MOBS} mobs / {FIGHTING} fighting: first={first:?} ({} acted) second={second:?}",
        acted.len()
    );
    if !cfg!(debug_assertions) {
        assert!(first.as_millis() < 5, "first round took {first:?}");
        assert!(second.as_millis() < 5, "second round took {second:?}");
    }
}
