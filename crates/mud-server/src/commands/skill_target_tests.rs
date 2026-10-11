//! Skills and item spells hit the entity the command resolved, not whatever
//! its display name re-resolves to ("a rat" never matches the keyword
//! lookup), and an area staff works for a mortal. Portals carry the mount.
//! Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::ObjectType;
use mud_world::{
    AbilityCatalog, CombatStats, EffectCatalog, EffectDef, Fighting, Health, Item, Keywords,
    KnownAbilities, Located, Mob, Named, Posture, PostureKind, Stamina, WorldKey,
};

use super::dispatch;
use super::gmcp_tests::{Fx, fixture, player};
use super::test_support::{Rx, ability_def, drain, object_proto};

const DAMAGE: i32 = 9;
const BACKSTAB: i32 = 70;
const KICK: i32 = 71;
const TAME: i32 = 72;
const QUAKE: i32 = 73;
const STAFF: i32 = 40;

fn damage_effect() -> EffectDef {
    EffectDef {
        id: DAMAGE,
        name: "damage".into(),
        description: None,
        effect_type: "damage".into(),
        tags: vec![],
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

/// A mortal "Hero" with no spells, and a catalog of damaging backstab / kick /
/// tame skills plus an area `earthquake` spell.
fn arena() -> (Fx, Entity, Rx) {
    let mut fx = fixture();
    let mut catalog = AbilityCatalog::default();
    for (id, name, violent) in [
        (BACKSTAB, "Backstab", true),
        (KICK, "Kick", true),
        (TAME, "Tame", false),
    ] {
        let mut def = ability_def(id, name, AbilityKind::Skill);
        def.violent = violent;
        def.cast_time_rounds = 0;
        catalog.by_name.insert(name.to_ascii_lowercase(), def);
        catalog.effects_for.insert(
            id,
            vec![(DAMAGE, Some(serde_json::json!({ "amount": 10 })))],
        );
    }
    let mut quake = ability_def(QUAKE, "Earthquake", AbilityKind::Spell);
    quake.violent = true;
    quake.is_area = true;
    quake.target_scope = "ROOM_ENEMIES".into();
    quake.cast_time_rounds = 0;
    catalog.by_name.insert("earthquake".into(), quake);
    catalog.effects_for.insert(
        QUAKE,
        vec![(DAMAGE, Some(serde_json::json!({ "amount": 10 })))],
    );
    fx.world.insert_resource(catalog);
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(DAMAGE, damage_effect());
    fx.world.insert_resource(effects);
    fx.world
        .insert_resource(mud_world::SpellSlotData::default());
    fx.world
        .insert_resource(mud_world::ClassSkillsData::default());
    fx.world.insert_resource(crate::TickCount(0));
    let a = fx.a;
    let (p, rx) = player(&mut fx.world, a, "Hero");
    fx.world.entity_mut(p).insert((
        Health { hp: 100, max: 100 },
        Stamina {
            current: 100,
            max: 100,
        },
        CombatStats::default(),
        Posture(PostureKind::Standing),
        KnownAbilities::default(),
    ));
    (fx, p, rx)
}

fn rat(world: &mut World, room: Entity) -> Entity {
    world
        .spawn((
            Mob,
            Named {
                name: "a rat".into(),
            },
            Keywords(vec!["rat".into()]),
            Located(room),
            Health { hp: 100, max: 100 },
            CombatStats::default(),
            Posture(PostureKind::Standing),
        ))
        .id()
}

fn hp(world: &World, e: Entity) -> i32 {
    world.get::<Health>(e).unwrap().hp
}

#[test]
fn backstab_hits_a_mob_whose_name_starts_with_an_article() {
    let (mut fx, p, mut rx) = arena();
    let a = fx.a;
    let r = rat(&mut fx.world, a);
    dispatch(&mut fx.world, p, "backstab rat");
    let out = drain(&mut rx);
    assert!(hp(&fx.world, r) < 100, "the rat took the stab: {out}");
}

#[test]
fn kick_hits_the_fought_mob_not_the_first_of_two_with_the_same_name() {
    let (mut fx, p, mut rx) = arena();
    let a = fx.a;
    let first = rat(&mut fx.world, a);
    let second = rat(&mut fx.world, a);
    fx.world.entity_mut(p).insert(Fighting(second));
    dispatch(&mut fx.world, p, "kick");
    let out = drain(&mut rx);
    assert!(
        hp(&fx.world, second) < 100,
        "the fought rat was kicked: {out}"
    );
    assert_eq!(hp(&fx.world, first), 100, "the other rat was not: {out}");
}

#[test]
fn tame_targets_the_named_mob_instead_of_falling_back_to_self() {
    let (mut fx, p, mut rx) = arena();
    let a = fx.a;
    let r = rat(&mut fx.world, a);
    dispatch(&mut fx.world, p, "tame rat");
    let out = drain(&mut rx);
    assert!(hp(&fx.world, r) < 100, "the rat was the target: {out}");
    assert_eq!(hp(&fx.world, p), 100, "not the caster: {out}");
}

#[test]
fn a_mortal_can_tap_an_area_staff_and_spends_one_charge() {
    let (mut fx, p, mut rx) = arena();
    let a = fx.a;
    let r = rat(&mut fx.world, a);
    let mut proto = object_proto(1, STAFF, ObjectType::Staff);
    proto.name = "a quaking staff".into();
    proto.keywords = vec!["staff".into()];
    let mut protos = mud_world::ObjectPrototypes::default();
    protos.by_key.insert((1, STAFF), proto);
    fx.world.insert_resource(protos);
    let mut bindings = mud_world::ObjectAbilityCatalog::default();
    bindings.by_key.insert(
        (1, STAFF),
        vec![mud_world::resources::ObjectAbilityBinding {
            ability_id: QUAKE,
            level: 10,
            charges: Some(3),
        }],
    );
    fx.world.insert_resource(bindings);
    let staff = fx
        .world
        .spawn((
            Item,
            Named {
                name: "a quaking staff".into(),
            },
            Keywords(vec!["staff".into()]),
            WorldKey { zone: 1, id: STAFF },
            mud_world::Charges(3),
            Located(p),
        ))
        .id();
    dispatch(&mut fx.world, p, "tap staff");
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
    let out = drain(&mut rx);
    assert!(!out.contains("don't know how"), "{out}");
    assert!(hp(&fx.world, r) < 100, "the quake hit the rat: {out}");
    assert_eq!(fx.world.get::<mud_world::Charges>(staff).unwrap().0, 2);
}

#[test]
fn an_area_staff_with_nobody_to_hit_keeps_its_charge() {
    let (mut fx, p, mut rx) = arena();
    let mut proto = object_proto(1, STAFF, ObjectType::Staff);
    proto.keywords = vec!["staff".into()];
    let mut protos = mud_world::ObjectPrototypes::default();
    protos.by_key.insert((1, STAFF), proto);
    fx.world.insert_resource(protos);
    let mut bindings = mud_world::ObjectAbilityCatalog::default();
    bindings.by_key.insert(
        (1, STAFF),
        vec![mud_world::resources::ObjectAbilityBinding {
            ability_id: QUAKE,
            level: 10,
            charges: Some(3),
        }],
    );
    fx.world.insert_resource(bindings);
    let staff = fx
        .world
        .spawn((
            Item,
            Named {
                name: "a quaking staff".into(),
            },
            Keywords(vec!["staff".into()]),
            WorldKey { zone: 1, id: STAFF },
            mud_world::Charges(3),
            Located(p),
        ))
        .id();
    dispatch(&mut fx.world, p, "tap staff");
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
    let out = drain(&mut rx);
    assert_eq!(
        fx.world.get::<mud_world::Charges>(staff).unwrap().0,
        3,
        "nothing was cast: {out}"
    );
}

#[test]
fn entering_a_portal_while_mounted_takes_the_mount_along() {
    let (mut fx, p, _rx) = arena();
    let (a, b) = (fx.a, fx.b);
    let steed = rat(&mut fx.world, a);
    fx.world.entity_mut(steed).insert(mud_world::RiddenBy(p));
    fx.world.entity_mut(p).insert(mud_world::Mounted(steed));
    let mut proto = object_proto(1, 50, ObjectType::Portal);
    proto.keywords = vec!["portal".into()];
    proto.portal_destination_vnum = Some(2222);
    let mut protos = mud_world::ObjectPrototypes::default();
    protos.by_key.insert((1, 50), proto);
    fx.world.insert_resource(protos);
    fx.world
        .resource_mut::<mud_world::WorldKeyIndex>()
        .legacy_vnums
        .insert(2222, (550, 22));
    fx.world.spawn((
        Item,
        Named {
            name: "a portal".into(),
        },
        Keywords(vec!["portal".into()]),
        WorldKey { zone: 1, id: 50 },
        Located(a),
    ));
    dispatch(&mut fx.world, p, "enter portal");
    assert_eq!(fx.world.get::<Located>(p).unwrap().0, b, "the rider moved");
    assert_eq!(
        fx.world.get::<Located>(steed).unwrap().0,
        b,
        "the mount came too"
    );
}

#[test]
fn a_lethal_kick_leaves_no_fight_behind() {
    let (mut fx, p, mut rx) = arena();
    let a = fx.a;
    let r = rat(&mut fx.world, a);
    fx.world.get_mut::<Health>(r).unwrap().hp = 1;
    dispatch(&mut fx.world, p, "kick rat");
    let out = drain(&mut rx);
    assert!(
        fx.world.get_entity(r).is_err() || hp(&fx.world, r) <= 0,
        "the rat died: {out}"
    );
    assert!(fx.world.get::<Fighting>(p).is_none(), "{out}");
}

#[test]
fn kicking_without_knowing_kick_starts_no_fight_and_costs_no_stamina() {
    let (mut fx, p, mut rx) = arena();
    let a = fx.a;
    let r = rat(&mut fx.world, a);
    fx.world.entity_mut(p).insert(KnownAbilities {
        entries: vec![(TAME, 500, true)],
    });
    dispatch(&mut fx.world, p, "kick rat");
    let out = drain(&mut rx);
    assert!(out.contains("don't know how to use Kick"), "{out}");
    assert!(fx.world.get::<Fighting>(p).is_none(), "{out}");
    assert!(fx.world.get::<Fighting>(r).is_none(), "{out}");
    assert_eq!(fx.world.get::<Stamina>(p).unwrap().current, 100, "{out}");
    assert_eq!(hp(&fx.world, r), 100, "{out}");
}

#[test]
fn kicking_a_new_victim_engages_and_breaks_invisibility() {
    let (mut fx, p, mut rx) = arena();
    let a = fx.a;
    let r = rat(&mut fx.world, a);
    fx.world.entity_mut(p).insert(mud_world::Invisible);
    dispatch(&mut fx.world, p, "kick rat");
    let out = drain(&mut rx);
    assert!(fx.world.get::<mud_world::Invisible>(p).is_none(), "{out}");
    assert_eq!(fx.world.get::<Fighting>(p).map(|f| f.0), Some(r), "{out}");
    assert_eq!(fx.world.get::<Fighting>(r).map(|f| f.0), Some(p), "{out}");
    assert!(fx.world.get::<Stamina>(p).unwrap().current < 100, "{out}");
}
