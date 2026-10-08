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
