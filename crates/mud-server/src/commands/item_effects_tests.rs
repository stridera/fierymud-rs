//! Worn-item effects (issue #87): stat applies and granted status flags
//! take hold on wear, come off exactly on removal, are rebuilt from the
//! worn items at login without stacking, survive no save, and show up in
//! `identify`. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole, WearFlag};
use mud_world::{
    Account, AppliedTo, Bless, CoreStats, DetectInvis, EffectCatalog, EffectDef, EffectInstance,
    EffectSource, EquippedSlot, Flying, Health, Item, Keywords, Located, Named,
    ObjectGrantedEffect, ObjectPrototypes, Profile, RaceEffect, RaceEffectCatalog, Room,
    WearableIn, WorldKey, wear_flags_primary_slot,
};

use super::dispatch;
use super::test_support::{Rx, drain, object_proto, player_in};
use crate::equip_apply::{
    GrantedDeltas, base_core_stats, base_current, despawn_item, gear_offsets,
    recompute_equipped_for, recompute_equipped_keeping_vitals, release_gear,
};

const MODIFY: i32 = 3;
const STATUS: i32 = 4;
const GLOBE: i32 = 17;

fn effect_def(id: i32, name: &str, kind: &str, params: serde_json::Value) -> EffectDef {
    EffectDef {
        id,
        name: name.into(),
        description: None,
        effect_type: kind.into(),
        tags: vec![],
        presence_override: None,
        default_params: params,
        prevents_speaking: false,
        prevents_casting: false,
        prevents_movement: false,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    }
}

fn setup() -> (World, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(ObjectPrototypes::default());
    world.init_resource::<mud_world::ObjectAbilityCatalog>();
    world.init_resource::<mud_world::AbilityCatalog>();
    world.init_resource::<mud_world::ClassCatalog>();
    world.init_resource::<mud_world::LiquidCatalog>();
    let mut catalog = EffectCatalog::default();
    catalog.by_id.insert(
        MODIFY,
        effect_def(MODIFY, "modify", "modify", serde_json::json!({})),
    );
    // The real `status` row: its default flag is bless.
    catalog.by_id.insert(
        STATUS,
        effect_def(
            STATUS,
            "status",
            "status",
            serde_json::json!({"flag": "bless"}),
        ),
    );
    // The real `globe` row: a minor globe unless the item says more.
    catalog.by_id.insert(
        GLOBE,
        effect_def(
            GLOBE,
            "globe",
            "globe",
            serde_json::json!({"maxCircle": 3, "duration": "level"}),
        ),
    );
    world.insert_resource(catalog);
    let room = world
        .spawn((
            Room,
            Named {
                name: "A quiet hall".into(),
            },
            mud_world::Exits::default(),
        ))
        .id();
    let (player, rx) = player_in(&mut world, room);
    world.entity_mut(player).insert((
        Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        },
        CoreStats {
            strength: 13,
            dexterity: 13,
            constitution: 13,
            intelligence: 13,
            wisdom: 13,
            charisma: 13,
        },
        Health { hp: 100, max: 100 },
        Profile {
            level: 10,
            class_id: None,
            race: "HUMAN".into(),
            experience: 0,
            gender: "male".into(),
        },
    ));
    (world, player, rx)
}

fn modify(target: &str, amount: i32) -> ObjectGrantedEffect {
    ObjectGrantedEffect {
        effect_id: MODIFY,
        strength: 1,
        modifier_data: serde_json::json!({"target": target, "amount": amount}),
        wear_location: None,
    }
}

fn status(flags: &[&str]) -> ObjectGrantedEffect {
    ObjectGrantedEffect {
        effect_id: STATUS,
        strength: 1,
        modifier_data: serde_json::json!({ "flags": flags }),
        wear_location: None,
    }
}

fn ring(
    world: &mut World,
    holder: Entity,
    id: i32,
    keyword: &str,
    grants: Vec<ObjectGrantedEffect>,
) -> Entity {
    let mut proto = object_proto(1, id, ObjectType::Armor);
    proto.name = format!("a {keyword} ring");
    proto.keywords = vec![keyword.into(), "ring".into()];
    proto.wear_flags = vec![WearFlag::Finger];
    proto.granted_effects = grants;
    world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .insert((1, id), proto);
    world
        .spawn((
            Item,
            Named {
                name: format!("a {keyword} ring"),
            },
            Keywords(vec![keyword.into(), "ring".into()]),
            WorldKey { zone: 1, id },
            Located(holder),
            WearableIn(wear_flags_primary_slot(&[WearFlag::Finger]).unwrap()),
        ))
        .id()
}

fn effects_on(world: &mut World, target: Entity) -> Vec<(String, EffectSource)> {
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    let mut v: Vec<_> = q
        .iter(world)
        .filter(|(_, a)| a.0 == target)
        .map(|(i, _)| (i.name.clone(), i.source.clone()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn strength(world: &World, e: Entity) -> i32 {
    world.get::<CoreStats>(e).unwrap().strength
}

#[test]
fn ring_of_strength_applies_on_wear_and_reverses_on_remove() {
    let (mut world, p, mut rx) = setup();
    let r = ring(&mut world, p, 1, "iron", vec![modify("str_bonus", 2)]);
    dispatch(&mut world, p, "wear iron");
    let out = drain(&mut rx);
    assert!(world.get::<EquippedSlot>(r).is_some(), "{out}");
    assert_eq!(strength(&world, p), 15, "{out}");
    dispatch(&mut world, p, "remove iron");
    let out = drain(&mut rx);
    assert!(world.get::<EquippedSlot>(r).is_none(), "{out}");
    assert_eq!(strength(&world, p), 13, "{out}");
}

#[test]
fn ring_granting_detect_invisible_installs_marker_and_remove_clears_it() {
    let (mut world, p, mut rx) = setup();
    ring(&mut world, p, 2, "eye", vec![status(&["detect_invisible"])]);
    dispatch(&mut world, p, "wear eye");
    let _ = drain(&mut rx);
    assert!(world.get::<DetectInvis>(p).is_some(), "marker installed");
    let fx = effects_on(&mut world, p);
    assert_eq!(fx.len(), 1, "{fx:?}");
    assert_eq!(fx[0].0, "detect_invisible");
    assert!(mud_world::mob_effects::is_worn_item_effect(&fx[0].1));
    assert!(
        world.get::<Bless>(p).is_none(),
        "the status default flag (bless) must not leak in"
    );
    dispatch(&mut world, p, "remove eye");
    let _ = drain(&mut rx);
    assert!(world.get::<DetectInvis>(p).is_none(), "marker torn down");
    assert!(effects_on(&mut world, p).is_empty());
}

#[test]
fn removing_the_ring_keeps_a_flag_a_race_innate_still_backs() {
    let (mut world, p, mut rx) = setup();
    let mut races = RaceEffectCatalog::default();
    races.insert(
        "HUMAN",
        RaceEffect {
            effect_id: STATUS,
            strength: 1,
            modifier_data: serde_json::json!({"flags": ["fly"]}),
        },
    );
    world.insert_resource(races);
    // Race innates are applied after the worn items at login, so wear
    // first, then run the race pass: the worn instance must not make the
    // race skip its own.
    ring(&mut world, p, 3, "wing", vec![status(&["fly"])]);
    dispatch(&mut world, p, "wear wing");
    let _ = drain(&mut rx);
    mud_world::mob_effects::apply_race_effects(&mut world, p, "HUMAN");
    assert_eq!(effects_on(&mut world, p).len(), 2, "item + race instance");
    dispatch(&mut world, p, "remove wing");
    let _ = drain(&mut rx);
    assert!(world.get::<Flying>(p).is_some(), "race fly survives");
    let fx = effects_on(&mut world, p);
    assert_eq!(fx.len(), 1, "{fx:?}");
    assert!(mud_world::mob_effects::is_race_effect(&fx[0].1));
}

#[test]
fn removing_the_ring_keeps_a_flag_a_spell_still_backs() {
    let (mut world, p, mut rx) = setup();
    world.spawn((
        EffectInstance {
            kind: STATUS,
            name: "bless".into(),
            strength: 1,
            remaining_secs: 60,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(p),
    ));
    ring(&mut world, p, 4, "holy", vec![status(&["bless"])]);
    dispatch(&mut world, p, "wear holy");
    let _ = drain(&mut rx);
    assert!(world.get::<Bless>(p).is_some());
    dispatch(&mut world, p, "remove holy");
    let _ = drain(&mut rx);
    assert!(world.get::<Bless>(p).is_some(), "spell still backs bless");
    assert_eq!(effects_on(&mut world, p).len(), 1);
}

#[test]
fn dispel_and_cleanse_all_leave_worn_item_effects_alone() {
    let (mut world, p, mut rx) = setup();
    ring(&mut world, p, 5, "eye", vec![status(&["detect_invisible"])]);
    dispatch(&mut world, p, "wear eye");
    let _ = drain(&mut rx);
    super::remove_all_effects_on(&mut world, p);
    super::remove_effect_named(&mut world, p, "detect_invisible");
    super::remove_effects_for_condition(&mut world, p, "detect_invisible");
    assert!(world.get::<DetectInvis>(p).is_some());
    assert_eq!(effects_on(&mut world, p).len(), 1);
}

#[test]
fn worn_item_effects_are_not_restored_from_a_save() {
    let (mut world, p, _rx) = setup();
    let persisted: crate::login::PersistedEffects = serde_json::from_value(serde_json::json!({
        "saved_at_unix": i64::MAX / 2,
        "effects": [{
            "kind": STATUS,
            "name": "detect_invisible",
            "strength": 1,
            "remaining_secs": -1,
            "source": { "Other": mud_world::mob_effects::WORN_ITEM_EFFECT_SOURCE },
            "ability_id": null,
            "modify_delta": null,
        }],
    }))
    .expect("persisted effects shape");
    crate::login::restore_persisted_effects(&mut world, p, persisted);
    assert!(effects_on(&mut world, p).is_empty());
}

#[test]
fn relog_with_a_worn_ring_applies_it_once() {
    let (mut world, p, mut rx) = setup();
    let r = ring(
        &mut world,
        p,
        6,
        "mighty",
        vec![
            modify("str_bonus", 2),
            modify("max_hp", 25),
            status(&["sanctuary"]),
        ],
    );
    dispatch(&mut world, p, "wear mighty");
    let _ = drain(&mut rx);
    assert_eq!(strength(&world, p), 15);
    assert_eq!(world.get::<Health>(p).unwrap().max, 125);
    world.get_mut::<Health>(p).unwrap().hp = 125;
    // What the save writes: values without gear.
    let saved = base_core_stats(&world, p).unwrap();
    assert_eq!(saved.strength, 13, "the row keeps the base strength");
    let off = gear_offsets(&world, p);
    assert_eq!(off.max_hp, 25);
    let saved_hp = base_current(125, 125, off.max_hp);
    assert_eq!(saved_hp, 100);

    // Fresh login: the character row loads, the worn item respawns, the
    // equipped pass re-applies it (twice is still once).
    let (mut world2, p2, _rx2) = setup();
    *world2.get_mut::<CoreStats>(p2).unwrap() = saved;
    *world2.get_mut::<Health>(p2).unwrap() = Health {
        hp: saved_hp,
        max: 100,
    };
    let r2 = ring(
        &mut world2,
        p2,
        6,
        "mighty",
        vec![
            modify("str_bonus", 2),
            modify("max_hp", 25),
            status(&["sanctuary"]),
        ],
    );
    world2
        .entity_mut(r2)
        .insert(EquippedSlot(mud_world::Slot::LeftFinger));
    recompute_equipped_keeping_vitals(&mut world2, p2);
    recompute_equipped_for(&mut world2, p2);
    assert_eq!(strength(&world2, p2), 15, "applied once");
    let hp = world2.get::<Health>(p2).unwrap();
    assert_eq!((hp.hp, hp.max), (100, 125), "gear raises max, not current");
    assert!(world2.get::<mud_world::Sanctuary>(p2).is_some());
    assert_eq!(effects_on(&mut world2, p2).len(), 1, "one worn instance");
    assert!(world.get::<GrantedDeltas>(r).is_some());
}

#[test]
fn hurt_wearer_returns_with_exactly_the_hp_they_had() {
    // 30/150 with a +50 max_hp ring: the old floor saved 1 and returned
    // at 51.
    let (mut world, p, mut rx) = setup();
    ring(&mut world, p, 10, "vital", vec![modify("max_hp", 50)]);
    dispatch(&mut world, p, "wear vital");
    let _ = drain(&mut rx);
    world.get_mut::<Health>(p).unwrap().hp = 30;
    let off = gear_offsets(&world, p);
    let max = world.get::<Health>(p).unwrap().max;
    assert_eq!(max, 150);
    let saved = base_current(30, max, off.max_hp);
    assert_eq!(saved, 30);

    let (mut world2, p2, _rx2) = setup();
    *world2.get_mut::<Health>(p2).unwrap() = Health {
        hp: saved,
        max: 100,
    };
    let r = ring(&mut world2, p2, 10, "vital", vec![modify("max_hp", 50)]);
    world2
        .entity_mut(r)
        .insert(EquippedSlot(mud_world::Slot::LeftFinger));
    recompute_equipped_keeping_vitals(&mut world2, p2);
    let hp = world2.get::<Health>(p2).unwrap();
    assert_eq!((hp.hp, hp.max), (30, 150));
}

#[test]
fn saved_current_points_are_capped_at_the_gearless_max() {
    assert_eq!(base_current(150, 150, 50), 100);
    assert_eq!(base_current(60, 125, 25), 60);
    // A cursed -10 ring needs no correction.
    assert_eq!(base_current(60, 90, -10), 60);
    // Never saves a living wearer as dead.
    assert_eq!(base_current(10, 5, 25), 1);
    assert_eq!(base_current(0, 125, 25), 0);
}

#[test]
fn a_timed_worn_item_that_decays_takes_its_bonus_with_it() {
    let (mut world, p, mut rx) = setup();
    let r = ring(
        &mut world,
        p,
        11,
        "fading",
        vec![modify("str_bonus", 2), status(&["detect_invisible"])],
    );
    world.entity_mut(r).insert(mud_world::ItemTimer {
        remaining_secs: 1,
        decompose_window_secs: 0,
    });
    dispatch(&mut world, p, "wear fading");
    let _ = drain(&mut rx);
    assert_eq!(strength(&world, p), 15);
    assert!(world.get::<DetectInvis>(p).is_some());
    crate::item_decay::item_decay_tick(&mut world);
    assert!(world.get_entity(r).is_err(), "the ring decayed");
    assert_eq!(strength(&world, p), 13);
    assert!(world.get::<DetectInvis>(p).is_none());
    assert!(effects_on(&mut world, p).is_empty());
    // What the next save writes is the base value, not a baked bonus.
    assert_eq!(base_core_stats(&world, p).unwrap().strength, 13);
    assert_eq!(gear_offsets(&world, p).strength, 0);
}

#[test]
fn despawn_item_reverses_a_worn_item_for_sale_purge_and_scripts() {
    let (mut world, p, mut rx) = setup();
    let r = ring(&mut world, p, 12, "gone", vec![modify("str_bonus", 4)]);
    dispatch(&mut world, p, "wear gone");
    let _ = drain(&mut rx);
    assert_eq!(strength(&world, p), 17);
    despawn_item(&mut world, r);
    assert!(world.get_entity(r).is_err());
    assert_eq!(strength(&world, p), 13);
}

fn persisted(effects: &serde_json::Value) -> crate::login::PersistedEffects {
    serde_json::from_value(serde_json::json!({
        "saved_at_unix": i64::MAX / 2,
        "effects": effects,
    }))
    .expect("persisted effects shape")
}

#[test]
fn a_stat_buff_that_expired_offline_gives_back_the_delta_saved_in_the_row() {
    let (mut world, p, _rx) = setup();
    // The row was saved while a +4 str buff was on: strength 17 includes it.
    world.get_mut::<CoreStats>(p).unwrap().strength = 17;
    let fx = persisted(&serde_json::json!([{
        "kind": MODIFY,
        "name": "str",
        "strength": 1,
        "remaining_secs": 0,
        "source": "Spell",
        "ability_id": null,
        "modify_delta": ["str_bonus", 4],
    }]));
    crate::login::restore_persisted_effects(&mut world, p, fx);
    assert_eq!(strength(&world, p), 13, "no permanent inflation");
    assert!(effects_on(&mut world, p).is_empty());
}

#[test]
fn a_live_stat_buff_restores_without_double_counting() {
    let (mut world, p, _rx) = setup();
    world.get_mut::<CoreStats>(p).unwrap().strength = 17;
    let fx = persisted(&serde_json::json!([{
        "kind": MODIFY,
        "name": "str",
        "strength": 1,
        "remaining_secs": 600,
        "source": "Spell",
        "ability_id": null,
        "modify_delta": ["str_bonus", 4],
    }]));
    crate::login::restore_persisted_effects(&mut world, p, fx);
    assert_eq!(strength(&world, p), 17, "already in the saved row");
    assert_eq!(effects_on(&mut world, p).len(), 1);
}

#[test]
fn a_live_max_hp_buff_is_reapplied_because_the_row_never_saved_it() {
    let (mut world, p, _rx) = setup();
    let fx = persisted(&serde_json::json!([{
        "kind": MODIFY,
        "name": "max_hp",
        "strength": 1,
        "remaining_secs": 600,
        "source": "Spell",
        "ability_id": null,
        "modify_delta": ["max_hp", 20],
    }]));
    crate::login::restore_persisted_effects(&mut world, p, fx);
    let hp = world.get::<Health>(p).unwrap();
    assert_eq!((hp.hp, hp.max), (100, 120), "max back, current untouched");
}

#[test]
fn release_gear_reverses_a_worn_item_taken_without_remove() {
    let (mut world, p, mut rx) = setup();
    let r = ring(
        &mut world,
        p,
        7,
        "doom",
        vec![modify("str_bonus", 3), status(&["haste"])],
    );
    dispatch(&mut world, p, "wear doom");
    let _ = drain(&mut rx);
    assert_eq!(strength(&world, p), 16);
    assert!(world.get::<mud_world::Haste>(p).is_some());
    // The death / disarm paths: reverse while the item still points at
    // the wearer, then move it.
    release_gear(&mut world, r);
    world.entity_mut(r).remove::<EquippedSlot>();
    assert_eq!(strength(&world, p), 13);
    assert!(world.get::<mud_world::Haste>(p).is_none());
    assert!(world.get::<GrantedDeltas>(r).is_none());
    // Picking it up and wearing it again applies it fresh.
    dispatch(&mut world, p, "wear doom");
    let _ = drain(&mut rx);
    assert_eq!(strength(&world, p), 16);
}

#[test]
fn identify_lists_applies_and_granted_effects() {
    let (mut world, p, mut rx) = setup();
    ring(
        &mut world,
        p,
        8,
        "seer",
        vec![
            modify("str_bonus", 2),
            modify("max_hp", -5),
            status(&["detect_invisible", "fly"]),
        ],
    );
    dispatch(&mut world, p, "identify seer");
    let out = drain(&mut rx);
    assert!(out.contains("Apply:"), "{out}");
    assert!(out.contains("+2 to strength"), "{out}");
    assert!(out.contains("-5 to max hit points"), "{out}");
    assert!(
        out.contains("Item provides:") && out.contains("detect invisible") && out.contains("fly"),
        "{out}"
    );
}

#[test]
fn identify_of_a_plain_item_has_no_worn_effects_section() {
    let (mut world, p, mut rx) = setup();
    ring(&mut world, p, 9, "plain", vec![]);
    dispatch(&mut world, p, "identify plain");
    let out = drain(&mut rx);
    assert!(!out.contains("Worn Effects"), "{out}");
    assert!(!out.contains("Apply:"), "{out}");
}

fn reset_ring(world: &mut World, holder: Entity, id: i32, keyword: &str) -> Entity {
    let r = ring(world, holder, id, keyword, vec![modify("str_bonus", 2)]);
    world.get_mut::<WorldKey>(r).unwrap().zone = 30;
    world.entity_mut(r).insert(mud_world::FromObjectReset(0));
    r
}

#[test]
fn zreset_keeps_a_reset_ring_a_player_wears_or_carries() {
    let (mut world, p, mut rx) = setup();
    let worn = reset_ring(&mut world, p, 20, "worn");
    let bag = world.spawn((Item, Located(p))).id();
    let bagged = reset_ring(&mut world, bag, 21, "bagged");
    // Proto zone must match the key the item carries.
    let proto = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(1, 20))
        .cloned()
        .unwrap();
    world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .insert((30, 20), proto);
    world.get_mut::<WorldKey>(worn).unwrap().id = 20;
    dispatch(&mut world, p, "wear worn");
    let _ = drain(&mut rx);
    assert_eq!(strength(&world, p), 15);
    super::admin_world::cmd_zreset(&mut world, p, "30");
    let out = drain(&mut rx);
    assert!(world.get_entity(worn).is_ok(), "{out}");
    assert!(world.get_entity(bagged).is_ok(), "{out}");
    assert_eq!(strength(&world, p), 15, "still worn, still applied");
    assert!(out.contains("0 item(s)"), "{out}");
}

#[test]
fn zreset_still_deletes_a_floor_reset_item() {
    let (mut world, p, mut rx) = setup();
    let room = world.get::<Located>(p).unwrap().0;
    let floor = reset_ring(&mut world, room, 22, "floor");
    super::admin_world::cmd_zreset(&mut world, p, "30");
    let out = drain(&mut rx);
    assert!(world.get_entity(floor).is_err(), "{out}");
    assert!(out.contains("1 item(s)"), "{out}");
}

// ---- Worn flags: overlap, save and globe ----

fn globe(strength: i32, modifier_data: serde_json::Value) -> ObjectGrantedEffect {
    ObjectGrantedEffect {
        effect_id: GLOBE,
        strength,
        modifier_data,
        wear_location: None,
    }
}

fn spell_instance(world: &mut World, p: Entity, name: &str, strength: i32) -> Entity {
    world
        .spawn((
            EffectInstance {
                kind: STATUS,
                name: name.into(),
                strength,
                remaining_secs: 60,
                source: EffectSource::Spell,
                ability_id: None,
            },
            AppliedTo(p),
        ))
        .id()
}

fn wear(world: &mut World, p: Entity, rx: &mut Rx, kw: &str) {
    dispatch(world, p, &format!("wear {kw}"));
    let _ = drain(rx);
}

fn remove(world: &mut World, p: Entity, rx: &mut Rx, kw: &str) {
    dispatch(world, p, &format!("remove {kw}"));
    let _ = drain(rx);
}

fn circle(world: &World, p: Entity) -> Option<i32> {
    world.get::<mud_world::MaxAbsorbCircle>(p).map(|m| m.0)
}

#[test]
fn flying_ring_installs_the_marker_on_wear_and_clears_it_on_remove() {
    let (mut world, p, mut rx) = setup();
    ring(
        &mut world,
        p,
        20,
        "wing",
        vec![status(&["fly", "sanctuary"])],
    );
    wear(&mut world, p, &mut rx, "wing");
    assert!(world.get::<Flying>(p).is_some());
    assert!(world.get::<mud_world::Sanctuary>(p).is_some());
    assert_eq!(effects_on(&mut world, p).len(), 2);
    remove(&mut world, p, &mut rx, "wing");
    assert!(world.get::<Flying>(p).is_none());
    assert!(world.get::<mud_world::Sanctuary>(p).is_none());
    assert!(effects_on(&mut world, p).is_empty());
}

#[test]
fn two_worn_items_granting_one_flag_keep_it_until_both_are_off() {
    let (mut world, p, mut rx) = setup();
    ring(&mut world, p, 21, "feather", vec![status(&["fly"])]);
    let second = ring(&mut world, p, 22, "pinion", vec![status(&["fly"])]);
    world
        .entity_mut(second)
        .insert(WearableIn(mud_world::Slot::RightFinger));
    wear(&mut world, p, &mut rx, "feather");
    wear(&mut world, p, &mut rx, "pinion");
    assert!(world.get::<Flying>(p).is_some());
    remove(&mut world, p, &mut rx, "feather");
    assert!(world.get::<Flying>(p).is_some(), "the other ring backs fly");
    remove(&mut world, p, &mut rx, "pinion");
    assert!(world.get::<Flying>(p).is_none());
}

#[test]
fn a_spell_expiring_under_a_worn_flag_leaves_the_marker_and_the_reverse_holds() {
    let (mut world, p, mut rx) = setup();
    let spell = spell_instance(&mut world, p, "fly", 1);
    ring(&mut world, p, 23, "lift", vec![status(&["fly"])]);
    wear(&mut world, p, &mut rx, "lift");
    // The spell fades first: the ring still holds the flag.
    world.despawn(spell);
    crate::effects::teardown_markers_after_removal(&mut world, p, "fly");
    assert!(world.get::<Flying>(p).is_some(), "worn item backs fly");
    // A spell cast over the ring, then the ring comes off.
    spell_instance(&mut world, p, "fly", 1);
    remove(&mut world, p, &mut rx, "lift");
    assert!(world.get::<Flying>(p).is_some(), "spell backs fly");
}

#[test]
fn despawning_a_worn_flag_item_clears_the_flag_unless_a_spell_backs_it() {
    let (mut world, p, mut rx) = setup();
    let r = ring(&mut world, p, 24, "ghost", vec![status(&["sanctuary"])]);
    wear(&mut world, p, &mut rx, "ghost");
    assert!(world.get::<mud_world::Sanctuary>(p).is_some());
    despawn_item(&mut world, r);
    assert!(world.get::<mud_world::Sanctuary>(p).is_none());
    assert!(effects_on(&mut world, p).is_empty());

    spell_instance(&mut world, p, "sanctuary", 1);
    world.entity_mut(p).insert(mud_world::Sanctuary);
    let r2 = ring(&mut world, p, 25, "halo", vec![status(&["sanctuary"])]);
    wear(&mut world, p, &mut rx, "halo");
    despawn_item(&mut world, r2);
    assert!(world.get::<mud_world::Sanctuary>(p).is_some());
}

#[test]
fn a_save_keeps_the_spell_flag_and_drops_the_gear_flag() {
    let (mut world, p, mut rx) = setup();
    spell_instance(&mut world, p, "bless", 1);
    ring(
        &mut world,
        p,
        26,
        "pure",
        vec![status(&["fly", "sanctuary"])],
    );
    wear(&mut world, p, &mut rx, "pure");
    ring(
        &mut world,
        p,
        27,
        "warding",
        vec![globe(6, serde_json::json!({}))],
    );
    wear(&mut world, p, &mut rx, "warding");
    let snap = crate::login::snapshot_player(&mut world, p, 1).expect("snapshot");
    let json = snap.effect_instances_json.expect("the spell is saved");
    let names: Vec<&str> = json["effects"]
        .as_array()
        .expect("effects array")
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(names, vec!["bless"], "no worn-item flag is persisted");

    // Fresh login: the save restores, the worn items re-apply: once each.
    let (mut world2, p2, _rx2) = setup();
    crate::login::restore_persisted_effects(&mut world2, p2, serde_json::from_value(json).unwrap());
    for (id, kw, grant) in [
        (26, "pure", status(&["fly", "sanctuary"])),
        (27, "warding", globe(6, serde_json::json!({}))),
    ] {
        let item = ring(&mut world2, p2, id, kw, vec![grant]);
        world2.entity_mut(item).insert(EquippedSlot(if id == 26 {
            mud_world::Slot::LeftFinger
        } else {
            mud_world::Slot::RightFinger
        }));
    }
    recompute_equipped_for(&mut world2, p2);
    recompute_equipped_for(&mut world2, p2);
    assert!(world2.get::<Flying>(p2).is_some());
    assert_eq!(circle(&world2, p2), Some(6));
    assert_eq!(
        effects_on(&mut world2, p2).len(),
        4,
        "bless + fly + sanct + globe"
    );
}

#[test]
fn a_worn_globe_sets_the_circle_and_removal_recomputes_the_max() {
    let (mut world, p, mut rx) = setup();
    let minor = ring(
        &mut world,
        p,
        30,
        "dim",
        vec![globe(3, serde_json::json!({}))],
    );
    let major = ring(
        &mut world,
        p,
        31,
        "bright",
        vec![globe(6, serde_json::json!({}))],
    );
    world
        .entity_mut(major)
        .insert(WearableIn(mud_world::Slot::RightFinger));
    wear(&mut world, p, &mut rx, "dim");
    assert_eq!(circle(&world, p), Some(3));
    wear(&mut world, p, &mut rx, "bright");
    assert_eq!(circle(&world, p), Some(6));
    remove(&mut world, p, &mut rx, "bright");
    assert_eq!(circle(&world, p), Some(3), "falls back to the minor globe");
    despawn_item(&mut world, minor);
    assert_eq!(circle(&world, p), None);
    assert!(effects_on(&mut world, p).is_empty());
}

#[test]
fn a_worn_globe_and_a_cast_globe_take_the_higher_circle_and_each_outlives_the_other() {
    let (mut world, p, mut rx) = setup();
    // A cast MINOR_GLOBE: instance named "globe", circle in strength.
    let spell = spell_instance(&mut world, p, "globe", 3);
    world.entity_mut(p).insert(mud_world::MaxAbsorbCircle(3));
    let r = ring(
        &mut world,
        p,
        32,
        "aegis",
        vec![globe(6, serde_json::json!({}))],
    );
    wear(&mut world, p, &mut rx, "aegis");
    assert_eq!(circle(&world, p), Some(6));
    // The spell fades: the worn major globe keeps 6.
    world.despawn(spell);
    crate::effects::teardown_markers_after_removal(&mut world, p, "globe");
    assert_eq!(circle(&world, p), Some(6));
    // A spell again, then the item is taken without `remove` (death).
    spell_instance(&mut world, p, "globe", 3);
    release_gear(&mut world, r);
    assert_eq!(
        circle(&world, p),
        Some(3),
        "the spell still absorbs circle 3"
    );
}

#[test]
fn a_globe_row_reads_its_circle_from_modifier_data_then_strength_then_the_effect_default() {
    let (mut world, p, mut rx) = setup();
    ring(
        &mut world,
        p,
        33,
        "etched",
        vec![globe(1, serde_json::json!({"maxCircle": 5}))],
    );
    wear(&mut world, p, &mut rx, "etched");
    assert_eq!(circle(&world, p), Some(5));
    remove(&mut world, p, &mut rx, "etched");
    ring(
        &mut world,
        p,
        34,
        "plain",
        vec![globe(1, serde_json::json!({}))],
    );
    wear(&mut world, p, &mut rx, "plain");
    assert_eq!(
        circle(&world, p),
        Some(3),
        "effect default_params.maxCircle"
    );
}

// ---- Marker table: every path that grants a flag installs the same
// component, and a save round trip keeps what a spell had installed. ----

fn effect_names(world: &mut World, p: Entity) -> Vec<String> {
    effects_on(world, p).into_iter().map(|(n, _)| n).collect()
}

#[test]
fn newly_mapped_marker_flags_install_on_wear_and_clear_on_remove() {
    type Has = fn(&World, Entity) -> bool;
    let cases: [(&str, Has); 3] = [
        ("protect_evil", |w, e| {
            w.get::<mud_world::ProtectFromEvil>(e).is_some()
        }),
        ("protect_good", |w, e| {
            w.get::<mud_world::ProtectFromGood>(e).is_some()
        }),
        ("invisible", |w, e| {
            w.get::<mud_world::Invisible>(e).is_some()
        }),
    ];
    for (i, (flag, has)) in cases.into_iter().enumerate() {
        let (mut world, p, mut rx) = setup();
        let id = 300 + i32::try_from(i).unwrap();
        ring(&mut world, p, id, "mark", vec![status(&[flag])]);
        wear(&mut world, p, &mut rx, "mark");
        assert!(has(&world, p), "{flag}: marker installed on wear");
        assert_eq!(effect_names(&mut world, p), vec![flag.to_string()]);
        remove(&mut world, p, &mut rx, "mark");
        assert!(!has(&world, p), "{flag}: marker cleared on remove");
        assert!(effects_on(&mut world, p).is_empty());
    }
}

#[test]
fn a_worn_invisible_item_is_stripped_by_attacking_like_the_mob_default() {
    let (mut world, p, mut rx) = setup();
    ring(&mut world, p, 310, "veil", vec![status(&["invisible"])]);
    wear(&mut world, p, &mut rx, "veil");
    assert!(world.get::<mud_world::Invisible>(p).is_some());
    // Tagged so the next effects tick does not read it as a faded spell.
    let tagged = {
        let mut q = world.query_filtered::<&AppliedTo, With<mud_world::InvisibleSource>>();
        q.iter(&world).filter(|a| a.0 == p).count()
    };
    assert_eq!(tagged, 1);
    super::break_invisibility(&mut world, p);
    assert!(world.get::<mud_world::Invisible>(p).is_none());
    assert!(effects_on(&mut world, p).is_empty());
    // Taking the already-stripped ring off is harmless.
    remove(&mut world, p, &mut rx, "veil");
    assert!(world.get::<mud_world::Invisible>(p).is_none());
}

#[test]
fn a_worn_protect_evil_survives_the_spell_expiring_and_vice_versa() {
    let (mut world, p, mut rx) = setup();
    // The spell shape: name "resistance", tagged with the alignment.
    let spell = world
        .spawn((
            EffectInstance {
                kind: STATUS,
                name: "resistance".into(),
                strength: 1,
                remaining_secs: 1,
                source: EffectSource::Spell,
                ability_id: None,
            },
            AppliedTo(p),
            mud_world::AlignmentProtectionTag::Evil,
        ))
        .id();
    world.entity_mut(p).insert(mud_world::ProtectFromEvil);
    ring(&mut world, p, 311, "ward", vec![status(&["protect_evil"])]);
    wear(&mut world, p, &mut rx, "ward");
    world.insert_resource(crate::TickCount(10));
    crate::effects::effects_tick(&mut world);
    assert!(world.get_entity(spell).is_err(), "the spell expired");
    assert!(
        world.get::<mud_world::ProtectFromEvil>(p).is_some(),
        "the worn ring still backs it"
    );
    remove(&mut world, p, &mut rx, "ward");
    assert!(world.get::<mud_world::ProtectFromEvil>(p).is_none());
}

#[test]
fn instance_only_flags_become_one_worn_instance_and_no_marker() {
    for flag in [
        "fireshield",
        "coldshield",
        "detect_magic",
        "infravision",
        "detect_life",
        "detect_hidden",
        "detect_align",
    ] {
        let (mut world, p, mut rx) = setup();
        ring(&mut world, p, 320, "seer", vec![status(&[flag])]);
        wear(&mut world, p, &mut rx, "seer");
        assert_eq!(
            effect_names(&mut world, p),
            vec![flag.to_string()],
            "{flag}"
        );
        remove(&mut world, p, &mut rx, "seer");
        assert!(effects_on(&mut world, p).is_empty(), "{flag}");
    }
}

#[test]
fn flags_with_no_runtime_behaviour_stay_unmapped() {
    for flag in ["waterwalk", "blur", "language_fluency", "familiarity"] {
        let (mut world, p, mut rx) = setup();
        ring(&mut world, p, 330, "idle", vec![status(&[flag])]);
        wear(&mut world, p, &mut rx, "idle");
        assert!(effects_on(&mut world, p).is_empty(), "{flag}");
        assert!(world.get::<Bless>(p).is_none(), "{flag}");
    }
}

fn aura_catalog() -> mud_world::EffectAuraCatalog {
    let aura = |key: &str, text: &str, magic: bool| mud_world::EffectAura {
        keys: vec![key.to_string()],
        text: text.to_string(),
        needs_detect_magic: magic,
        exclusive_group: None,
        min_alignment: None,
        max_alignment: None,
    };
    mud_world::EffectAuraCatalog {
        auras: vec![
            aura("fireshield", "FIRE-AURA", false),
            aura("coldshield", "COLD-AURA", false),
            aura("bless", "BLESS-AURA", true),
        ],
    }
}

#[test]
fn worn_fireshield_and_detect_magic_work_through_the_instance_name() {
    let (mut world, p, mut rx) = setup();
    world.insert_resource(aura_catalog());
    let room = world.get::<Located>(p).unwrap().0;
    let (other, _orx) = player_in(&mut world, room);
    world.entity_mut(other).insert(Profile {
        level: 10,
        class_id: None,
        race: "HUMAN".into(),
        experience: 0,
        gender: "male".into(),
    });
    spell_instance(&mut world, other, "bless", 1);
    // The viewer sees the other's bless only with detect magic.
    let lines = super::look_auras::aura_lines(&mut world, p, other);
    assert!(!lines.contains("BLESS-AURA"), "{lines}");
    ring(&mut world, p, 340, "sage", vec![status(&["detect_magic"])]);
    wear(&mut world, p, &mut rx, "sage");
    let lines = super::look_auras::aura_lines(&mut world, p, other);
    assert!(lines.contains("BLESS-AURA"), "worn detect_magic: {lines}");
    // A worn fireshield shows on the wearer to everyone.
    ring(&mut world, p, 341, "ember", vec![status(&["fireshield"])]);
    wear(&mut world, p, &mut rx, "ember");
    let lines = super::look_auras::aura_lines(&mut world, other, p);
    assert!(lines.contains("FIRE-AURA"), "worn fireshield: {lines}");
}

fn saved_effects(world: &mut World, p: Entity) -> crate::login::PersistedEffects {
    let snap = crate::login::snapshot_player(world, p, 1).expect("snapshot");
    serde_json::from_value(snap.effect_instances_json.expect("effects saved")).unwrap()
}

#[test]
fn a_relogged_fly_spell_is_flying_again_and_expires_clean() {
    let (mut world, p, _rx) = setup();
    spell_instance(&mut world, p, "fly", 1);
    world.entity_mut(p).insert(Flying);
    let saved = saved_effects(&mut world, p);
    let (mut world2, p2, _rx2) = setup();
    assert!(world2.get::<Flying>(p2).is_none());
    crate::login::restore_persisted_effects(&mut world2, p2, saved);
    assert!(world2.get::<Flying>(p2).is_some(), "marker re-installed");
    assert_eq!(effect_names(&mut world2, p2), vec!["fly".to_string()]);
    // When the spell runs out the marker goes with it.
    {
        let mut q = world2.query::<&mut EffectInstance>();
        for mut i in q.iter_mut(&mut world2) {
            i.remaining_secs = 1;
        }
    }
    world2.insert_resource(crate::TickCount(10));
    crate::effects::effects_tick(&mut world2);
    assert!(world2.get::<Flying>(p2).is_none());
}

#[test]
fn a_relogged_status_row_without_a_flag_override_uses_the_effect_default_flag() {
    let (mut world, p, _rx) = setup();
    // Name = the effect's own ("status"); its default flag is bless.
    spell_instance(&mut world, p, "status", 1);
    let saved = saved_effects(&mut world, p);
    let (mut world2, p2, _rx2) = setup();
    crate::login::restore_persisted_effects(&mut world2, p2, saved);
    assert!(world2.get::<Bless>(p2).is_some());
}

#[test]
fn a_relogged_stat_spell_is_not_doubled_and_still_unwinds_on_expiry() {
    let (mut world, p, _rx) = setup();
    // Cast: +4 strength, delta recorded on the instance.
    assert!(crate::commands::apply_modify_delta(
        &mut world,
        p,
        "str_bonus",
        4
    ));
    world.spawn((
        EffectInstance {
            kind: MODIFY,
            name: "str".into(),
            strength: 4,
            remaining_secs: 600,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(p),
        mud_world::ModifyDelta {
            target: "str_bonus".into(),
            amount: 4,
        },
    ));
    assert_eq!(strength(&world, p), 17);
    // What the save writes: the core stats (spell included) and the row.
    let saved_stats = base_core_stats(&world, p).unwrap();
    assert_eq!(saved_stats.strength, 17);
    let saved = saved_effects(&mut world, p);

    let (mut world2, p2, _rx2) = setup();
    *world2.get_mut::<CoreStats>(p2).unwrap() = saved_stats;
    crate::login::restore_persisted_effects(&mut world2, p2, saved);
    assert_eq!(strength(&world2, p2), 17, "not doubled to 21");
    assert_eq!(effect_names(&mut world2, p2), vec!["str".to_string()]);
    {
        let mut q = world2.query::<&mut EffectInstance>();
        for mut i in q.iter_mut(&mut world2) {
            i.remaining_secs = 1;
        }
    }
    world2.insert_resource(crate::TickCount(10));
    crate::effects::effects_tick(&mut world2);
    assert_eq!(
        strength(&world2, p2),
        13,
        "expiry gives back exactly the +4"
    );
}

#[test]
fn a_relogged_globe_empowered_and_resistance_spells_come_back_whole() {
    use mud_db::enums::ElementType;
    let (mut world, p, _rx) = setup();
    let spawn = |world: &mut World, name: &str, strength: i32| {
        world
            .spawn((
                EffectInstance {
                    kind: STATUS,
                    name: name.into(),
                    strength,
                    remaining_secs: 600,
                    source: EffectSource::Spell,
                    ability_id: None,
                },
                AppliedTo(p),
            ))
            .id()
    };
    spawn(&mut world, "globe", 6);
    spawn(&mut world, "empowered", 1);
    let fire = spawn(&mut world, "resistance", 1);
    world
        .entity_mut(fire)
        .insert(mud_world::SpellResistanceDelta {
            element: ElementType::Fire,
            percent: 25,
        });
    let evil = spawn(&mut world, "resistance", 1);
    world
        .entity_mut(evil)
        .insert(mud_world::AlignmentProtectionTag::Evil);
    let veil = spawn(&mut world, "evasion", 1);
    world.entity_mut(veil).insert(mud_world::InvisibleSource);
    let saved = saved_effects(&mut world, p);

    let (mut world2, p2, _rx2) = setup();
    crate::login::restore_persisted_effects(&mut world2, p2, saved);
    assert_eq!(circle(&world2, p2), Some(6), "globe");
    assert!(world2.get::<mud_world::Empowered>(p2).is_some());
    assert_eq!(
        world2
            .get::<mud_world::Resistances>(p2)
            .and_then(|r| r.0.get(&ElementType::Fire).copied()),
        Some(25)
    );
    assert!(world2.get::<mud_world::ProtectFromEvil>(p2).is_some());
    assert!(world2.get::<mud_world::ProtectFromGood>(p2).is_none());
    assert!(world2.get::<mud_world::Invisible>(p2).is_some());
    // The resistance unwinds with its instance, exactly once.
    {
        let mut q = world2.query::<(&mut EffectInstance, Has<mud_world::SpellResistanceDelta>)>();
        for (mut i, tagged) in q.iter_mut(&mut world2) {
            if tagged {
                i.remaining_secs = 1;
            }
        }
    }
    world2.insert_resource(crate::TickCount(10));
    crate::effects::effects_tick(&mut world2);
    assert!(
        world2
            .get::<mud_world::Resistances>(p2)
            .is_none_or(|r| !r.0.contains_key(&ElementType::Fire))
    );
}

// --- early removal reverses what expiry reverses -------------------------

const RESIST: i32 = 50;

/// A cast elemental ward: the instance carries the `SpellResistanceDelta`
/// that records its bump, and the bump is already in `Resistances` on top
/// of `baseline` (an item's own resistance).
fn cast_fire_ward(world: &mut World, p: Entity, baseline: i32, bump: i32, secs: i32) -> Entity {
    use mud_db::enums::ElementType;
    world.entity_mut(p).insert(mud_world::Resistances(
        [(ElementType::Fire, baseline + bump)].into_iter().collect(),
    ));
    world
        .spawn((
            EffectInstance {
                kind: RESIST,
                name: "resistance".into(),
                strength: 1,
                remaining_secs: secs,
                source: EffectSource::Spell,
                ability_id: None,
            },
            AppliedTo(p),
            mud_world::SpellResistanceDelta {
                element: ElementType::Fire,
                percent: bump,
            },
        ))
        .id()
}

fn fire_resistance(world: &World, p: Entity) -> Option<i32> {
    world
        .get::<mud_world::Resistances>(p)
        .and_then(|r| r.0.get(&mud_db::enums::ElementType::Fire).copied())
}

fn magic_tagged_resistance_effect(world: &mut World) {
    let mut def = effect_def(RESIST, "status", "status", serde_json::json!({}));
    def.tags = vec!["magic".into()];
    world
        .resource_mut::<EffectCatalog>()
        .by_id
        .insert(RESIST, def);
}

#[test]
fn dispel_returns_a_cast_resistance_to_its_baseline() {
    let (mut world, p, _rx) = setup();
    magic_tagged_resistance_effect(&mut world);
    let ward = cast_fire_ward(&mut world, p, 10, 30, 600);
    assert_eq!(fire_resistance(&world, p), Some(40));
    let removed = super::remove_effects_by_tag(&mut world, p, "magic", super::DispelScope::All);
    assert_eq!(removed, 1);
    assert!(world.get_entity(ward).is_err());
    assert_eq!(fire_resistance(&world, p), Some(10), "item baseline kept");
}

#[test]
fn dispel_drops_the_resistance_entry_when_nothing_else_backs_it() {
    let (mut world, p, _rx) = setup();
    magic_tagged_resistance_effect(&mut world);
    cast_fire_ward(&mut world, p, 0, 30, 600);
    super::remove_effects_by_tag(&mut world, p, "magic", super::DispelScope::All);
    assert_eq!(fire_resistance(&world, p), None);
}

#[test]
fn cleanse_paths_return_a_cast_resistance_to_its_baseline() {
    let (mut world, p, _rx) = setup();
    cast_fire_ward(&mut world, p, 10, 30, 600);
    assert_eq!(
        super::remove_effects_for_condition(&mut world, p, "resistance"),
        1
    );
    assert_eq!(fire_resistance(&world, p), Some(10));
    cast_fire_ward(&mut world, p, 10, 25, 600);
    assert_eq!(super::remove_all_effects_on(&mut world, p), 1);
    assert_eq!(fire_resistance(&world, p), Some(10));
    cast_fire_ward(&mut world, p, 10, 20, 600);
    assert_eq!(super::remove_effect_named(&mut world, p, "resistance"), 1);
    assert_eq!(fire_resistance(&world, p), Some(10));
}

#[test]
fn cancel_returns_a_cast_resistance_to_its_baseline() {
    let (mut world, p, mut rx) = setup();
    cast_fire_ward(&mut world, p, 10, 30, 600);
    dispatch(&mut world, p, "cancel resistance");
    let _ = drain(&mut rx);
    assert_eq!(fire_resistance(&world, p), Some(10));
    assert!(effects_on(&mut world, p).is_empty());
}

#[test]
fn expiry_still_returns_a_cast_resistance_to_its_baseline() {
    let (mut world, p, _rx) = setup();
    cast_fire_ward(&mut world, p, 10, 30, 1);
    world.insert_resource(crate::TickCount(10));
    crate::effects::effects_tick(&mut world);
    assert!(effects_on(&mut world, p).is_empty());
    assert_eq!(fire_resistance(&world, p), Some(10));
}

fn cast_protect(world: &mut World, p: Entity, tag: mud_world::AlignmentProtectionTag) -> Entity {
    match tag {
        mud_world::AlignmentProtectionTag::Evil => {
            world.entity_mut(p).insert(mud_world::ProtectFromEvil);
        }
        mud_world::AlignmentProtectionTag::Good => {
            world.entity_mut(p).insert(mud_world::ProtectFromGood);
        }
    }
    world
        .spawn((
            EffectInstance {
                kind: RESIST,
                name: "resistance".into(),
                strength: 1,
                remaining_secs: 600,
                source: EffectSource::Spell,
                ability_id: None,
            },
            AppliedTo(p),
            tag,
        ))
        .id()
}

#[test]
fn dispel_of_protect_from_evil_and_good_drops_the_marker() {
    let (mut world, p, _rx) = setup();
    magic_tagged_resistance_effect(&mut world);
    cast_protect(&mut world, p, mud_world::AlignmentProtectionTag::Evil);
    cast_protect(&mut world, p, mud_world::AlignmentProtectionTag::Good);
    super::remove_effects_by_tag(&mut world, p, "magic", super::DispelScope::All);
    assert!(world.get::<mud_world::ProtectFromEvil>(p).is_none());
    assert!(world.get::<mud_world::ProtectFromGood>(p).is_none());
}

#[test]
fn dispel_of_protect_from_evil_keeps_the_marker_a_worn_item_backs() {
    let (mut world, p, mut rx) = setup();
    magic_tagged_resistance_effect(&mut world);
    cast_protect(&mut world, p, mud_world::AlignmentProtectionTag::Evil);
    ring(&mut world, p, 312, "ward", vec![status(&["protect_evil"])]);
    wear(&mut world, p, &mut rx, "ward");
    super::remove_effects_by_tag(&mut world, p, "magic", super::DispelScope::All);
    assert!(
        world.get::<mud_world::ProtectFromEvil>(p).is_some(),
        "the worn ring still backs it"
    );
    remove(&mut world, p, &mut rx, "ward");
    assert!(world.get::<mud_world::ProtectFromEvil>(p).is_none());
}

#[test]
fn cleanse_of_one_protect_keeps_the_marker_a_second_cast_backs() {
    let (mut world, p, _rx) = setup();
    let first = cast_protect(&mut world, p, mud_world::AlignmentProtectionTag::Evil);
    cast_protect(&mut world, p, mud_world::AlignmentProtectionTag::Evil);
    let _ = super::despawn_effects_on(&mut world, p, vec![first]);
    assert!(world.get::<mud_world::ProtectFromEvil>(p).is_some());
    assert_eq!(super::remove_all_effects_on(&mut world, p), 1);
    assert!(world.get::<mud_world::ProtectFromEvil>(p).is_none());
}
