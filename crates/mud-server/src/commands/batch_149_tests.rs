//! Item spell slots fire every entry (duplicates included), a god ignores
//! `NO_DROP` when letting go of an item, `modify max_move` raises the stamina
//! maximum, and `mstat` field order. Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{ObjectRestriction, ObjectType, UserRole, effective_rank};
use mud_world::{
    Account, EffectDef, Item, Keywords, Located, Named, ObjectRestrictions, Stamina, WorldKey,
};

use super::dispatch;
use super::skill_target_tests::arena;
use super::test_support::{ability_def, drain, mob_proto, object_proto};

const STING: i32 = 80;
const STAMINA_UP: i32 = 81;
const MODIFY: i32 = 10;
const POTION: i32 = 41;

fn fx_def(id: i32, ty: &str) -> EffectDef {
    EffectDef {
        id,
        name: ty.into(),
        description: None,
        effect_type: ty.into(),
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

/// Install a potion proto carrying `abilities` (in order) and put one in
/// `p`'s inventory.
fn give_potion(world: &mut World, p: Entity, abilities: &[i32]) -> Entity {
    let mut proto = object_proto(1, POTION, ObjectType::Potion);
    proto.name = "a test potion".into();
    proto.keywords = vec!["potion".into()];
    let mut protos = mud_world::ObjectPrototypes::default();
    protos.by_key.insert((1, POTION), proto);
    world.insert_resource(protos);
    let mut bindings = mud_world::ObjectAbilityCatalog::default();
    bindings.by_key.insert(
        (1, POTION),
        abilities
            .iter()
            .map(|a| mud_world::resources::ObjectAbilityBinding {
                ability_id: *a,
                level: 10,
                charges: None,
            })
            .collect(),
    );
    world.insert_resource(bindings);
    world
        .spawn((
            Item,
            Named {
                name: "a test potion".into(),
            },
            Keywords(vec!["potion".into()]),
            WorldKey {
                zone: 1,
                id: POTION,
            },
            Located(p),
        ))
        .id()
}

fn add_spell(world: &mut World, id: i32, name: &str, effect: i32, params: serde_json::Value) {
    let mut def = ability_def(id, name, AbilityKind::Spell);
    def.cast_time_rounds = 0;
    let mut catalog = world.resource_mut::<mud_world::AbilityCatalog>();
    catalog.by_name.insert(name.to_ascii_lowercase(), def);
    catalog.effects_for.insert(id, vec![(effect, Some(params))]);
}

fn hp(world: &World, e: Entity) -> i32 {
    world.get::<mud_world::Health>(e).unwrap().hp
}

#[test]
fn a_potion_fires_every_slot_even_when_the_same_spell_repeats() {
    let (mut fx, p, mut rx) = arena();
    // `arena` installed the damage effect (id 9); a non-violent spell that
    // damages the quaffer stands in for a healing spell.
    add_spell(
        &mut fx.world,
        STING,
        "Sting",
        9,
        serde_json::json!({ "amount": 10 }),
    );
    give_potion(&mut fx.world, p, &[STING, STING, STING]);
    dispatch(&mut fx.world, p, "quaff potion");
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
    let out = drain(&mut rx);
    assert_eq!(hp(&fx.world, p), 70, "three slots, three hits: {out}");
}

#[test]
fn a_potion_with_two_different_spells_fires_both_in_order() {
    let (mut fx, p, mut rx) = arena();
    add_spell(
        &mut fx.world,
        STING,
        "Sting",
        9,
        serde_json::json!({ "amount": 10 }),
    );
    add_spell(
        &mut fx.world,
        82,
        "Jab",
        9,
        serde_json::json!({ "amount": 5 }),
    );
    give_potion(&mut fx.world, p, &[STING, 82, STING]);
    dispatch(&mut fx.world, p, "quaff potion");
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
    let out = drain(&mut rx);
    assert_eq!(hp(&fx.world, p), 75, "10 + 5 + 10: {out}");
}

#[test]
fn quaffing_a_max_move_modify_raises_stamina_max_and_expiry_restores_it() {
    let (mut fx, p, mut rx) = arena();
    fx.world
        .resource_mut::<mud_world::EffectCatalog>()
        .by_id
        .insert(MODIFY, fx_def(MODIFY, "modify"));
    add_spell(
        &mut fx.world,
        STAMINA_UP,
        "Stamina Up",
        MODIFY,
        serde_json::json!({ "amount": 25, "target": "max_move", "duration": 5, "durationUnit": "hours" }),
    );
    give_potion(&mut fx.world, p, &[STAMINA_UP]);
    dispatch(&mut fx.world, p, "quaff potion");
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
    let out = drain(&mut rx);
    assert!(!out.contains("unsupported target"), "{out}");
    assert_eq!(fx.world.get::<Stamina>(p).unwrap().max, 125, "{out}");
    crate::commands::reverse_modify_delta(&mut fx.world, p, "max_move", 25);
    assert_eq!(fx.world.get::<Stamina>(p).unwrap().max, 100);
}

fn nodrop_item(world: &mut World, holder: Entity) -> Entity {
    world
        .spawn((
            Item,
            Named {
                name: "a cursed ring".into(),
            },
            Keywords(vec!["ring".into()]),
            ObjectRestrictions(vec![ObjectRestriction::NoDrop]),
            Located(holder),
        ))
        .id()
}

fn make_god(world: &mut World, p: Entity) {
    world.entity_mut(p).insert(Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: effective_rank(100),
        account_role: UserRole::Immortal,
        perms: vec![],
    });
}

#[test]
fn a_mortal_cannot_drop_a_nodrop_item() {
    let (mut fx, p, mut rx) = arena();
    let ring = nodrop_item(&mut fx.world, p);
    dispatch(&mut fx.world, p, "drop ring");
    let out = drain(&mut rx);
    assert!(out.contains("let go of"), "{out}");
    assert_eq!(fx.world.get::<Located>(ring).unwrap().0, p);
}

#[test]
fn a_god_drops_junks_and_donates_nodrop_items() {
    for verb in ["drop ring", "drop all", "junk ring", "donate ring"] {
        let (mut fx, p, mut rx) = arena();
        make_god(&mut fx.world, p);
        let ring = nodrop_item(&mut fx.world, p);
        dispatch(&mut fx.world, p, verb);
        let out = drain(&mut rx);
        assert!(!out.contains("let go of"), "{verb}: {out}");
        assert!(
            fx.world.get_entity(ring).is_err() || fx.world.get::<Located>(ring).unwrap().0 != p,
            "{verb}: the ring left the god's hands: {out}"
        );
    }
}

#[test]
fn a_god_gives_a_nodrop_item_away_but_a_mortal_cannot() {
    for god in [false, true] {
        let (mut fx, p, mut rx) = arena();
        let a = fx.a;
        let (friend, _frx) = super::gmcp_tests::player(&mut fx.world, a, "Pal");
        if god {
            make_god(&mut fx.world, p);
        }
        let ring = nodrop_item(&mut fx.world, p);
        dispatch(&mut fx.world, p, "give ring Pal");
        let out = drain(&mut rx);
        let moved = fx.world.get::<Located>(ring).unwrap().0 == friend;
        assert_eq!(moved, god, "god={god}: {out}");
    }
}

#[test]
fn mstat_lists_class_after_role_and_composition_after_life_force() {
    let (mut fx, p, mut rx) = arena();
    fx.world.entity_mut(p).insert(Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Builder,
        account_role: UserRole::Builder,
        perms: vec![],
    });
    let mut proto = mob_proto(1, 5, mud_db::enums::MobProfession::Banker);
    proto.composition = mud_db::enums::Composition::Ice;
    fx.world.init_resource::<mud_world::MobPrototypes>();
    fx.world.init_resource::<mud_world::TriggerCatalog>();
    fx.world
        .resource_mut::<mud_world::MobPrototypes>()
        .by_key
        .insert((1, 5), proto);
    dispatch(&mut fx.world, p, "mstat 1 5");
    let out = drain(&mut rx);
    let at = |needle: &str| {
        out.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {out}"))
    };
    assert!(at("role:") < at("class:"), "{out}");
    assert!(at("class:") < at("race:"), "{out}");
    assert!(at("life force:") < at("composition:"), "{out}");
    assert!(out.contains("composition:   Ice"), "{out}");
    assert!(at("composition:") < at("damage type:"), "{out}");
}
