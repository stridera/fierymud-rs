//! Issues #90 and #91: Identify reads a character, and a scroll or wand
//! with no usable target is not used up. Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Composition, LifeForce, ObjectType, UserRole};
use mud_world::{
    AbilityCatalog, Account, Charges, EffectCatalog, EffectDef, Item, Keywords, Located, Mob,
    Named, ObjectAbilityBinding, ObjectAbilityCatalog, ObjectPrototypes, SpellSlotData, WorldKey,
};

use super::test_support::{Rx, ability_def, drain, object_proto, player_in};
use crate::commands::dispatch;

const IDENTIFY: i32 = 195;
const INSPECT: i32 = 50;

fn world_with_identify() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(SpellSlotData::default());
    world.insert_resource(mud_world::ClassSkillsData::default());
    let mut catalog = AbilityCatalog::default();
    let mut def = ability_def(IDENTIFY, "Identify", AbilityKind::Spell);
    def.cast_time_rounds = 0;
    catalog.by_name.insert("identify".into(), def);
    catalog.effects_for.insert(IDENTIFY, vec![(INSPECT, None)]);
    world.insert_resource(catalog);
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(
        INSPECT,
        EffectDef {
            id: INSPECT,
            name: "inspect".into(),
            description: None,
            effect_type: "inspect".into(),
            tags: Vec::new(),
            presence_override: None,
            default_params: serde_json::json!({}),
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        },
    );
    world.insert_resource(effects);
    let mut protos = ObjectPrototypes::default();
    protos
        .by_key
        .insert((30, 1), object_proto(30, 1, ObjectType::Scroll));
    protos
        .by_key
        .insert((30, 2), object_proto(30, 2, ObjectType::Wand));
    world.insert_resource(protos);
    let mut bindings = ObjectAbilityCatalog::default();
    for key in [(30, 1), (30, 2)] {
        bindings.by_key.insert(
            key,
            vec![ObjectAbilityBinding {
                ability_id: IDENTIFY,
                level: 10,
                charges: Some(3),
            }],
        );
    }
    world.insert_resource(bindings);
    let room = world.spawn_empty().id();
    let (caster, rx) = player_in(&mut world, room);
    world.entity_mut(caster).insert(Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    });
    (world, room, caster, rx)
}

fn goblin(world: &mut World, room: Entity) -> Entity {
    world
        .spawn((
            Mob,
            Named {
                name: "a snarling goblin".into(),
            },
            Keywords(vec!["goblin".into()]),
            Located(room),
            mud_world::LifeForceTag(LifeForce::Undead),
        ))
        .id()
}

fn carry(world: &mut World, holder: Entity, name: &str, kw: &str, key: (i32, i32)) -> Entity {
    let mut e = world.spawn((
        Item,
        Named { name: name.into() },
        Keywords(vec![kw.into()]),
        Located(holder),
        WorldKey {
            zone: key.0,
            id: key.1,
        },
    ));
    if key == (30, 2) {
        e.insert(Charges(3));
    }
    e.id()
}

fn alive(world: &World, e: Entity) -> bool {
    world.get_entity(e).is_ok()
}

#[test]
fn identify_on_a_mob_reads_its_nature() {
    let (mut world, room, caster, mut rx) = world_with_identify();
    goblin(&mut world, room);
    world.entity_mut(caster).insert(mud_world::KnownAbilities {
        entries: vec![(IDENTIFY, 500, true)],
    });
    dispatch(&mut world, caster, "cast identify goblin");
    let out = drain(&mut rx);
    assert!(out.contains("Name: a snarling goblin"), "{out}");
    assert!(out.contains("is composed of"), "{out}");
    assert!(out.contains("nature is"), "{out}");
    assert!(out.contains("undead"), "{out}");
}

#[test]
fn identify_character_branch_lists_composition_and_lifeforce() {
    let (mut world, room, caster, mut rx) = world_with_identify();
    let mob = goblin(&mut world, room);
    super::identify_actor::identify_actor(&mut world, caster, mob);
    let out = drain(&mut rx);
    assert!(out.contains("It is composed of"), "{out}");
    assert!(
        out.contains(&format!("{:?}", Composition::Flesh).to_lowercase()),
        "{out}"
    );
}

#[test]
fn recite_with_no_target_keeps_the_scroll() {
    let (mut world, _room, caster, mut rx) = world_with_identify();
    let scroll = carry(
        &mut world,
        caster,
        "a scroll of identify",
        "scroll",
        (30, 1),
    );
    dispatch(&mut world, caster, "recite scroll");
    let out = drain(&mut rx);
    assert!(out.contains("What do you want to recite"), "{out}");
    assert!(alive(&world, scroll), "scroll must survive");
}

#[test]
fn recite_at_a_missing_target_keeps_the_scroll() {
    let (mut world, _room, caster, mut rx) = world_with_identify();
    let scroll = carry(
        &mut world,
        caster,
        "a scroll of identify",
        "scroll",
        (30, 1),
    );
    dispatch(&mut world, caster, "recite scroll nobody");
    let out = drain(&mut rx);
    assert!(out.contains("You can't see any nobody here"), "{out}");
    assert!(!out.contains("You read aloud"), "{out}");
    assert!(alive(&world, scroll), "scroll must survive");
}

#[test]
fn recite_at_a_mob_identifies_it_and_uses_up_the_scroll() {
    let (mut world, room, caster, mut rx) = world_with_identify();
    goblin(&mut world, room);
    let scroll = carry(
        &mut world,
        caster,
        "a scroll of identify",
        "scroll",
        (30, 1),
    );
    dispatch(&mut world, caster, "recite scroll goblin");
    let out = drain(&mut rx);
    assert!(out.contains("Name: a snarling goblin"), "{out}");
    assert!(!alive(&world, scroll), "scroll is spent by a landed cast");
}

#[test]
fn wave_wand_with_no_target_keeps_its_charge() {
    let (mut world, _room, caster, mut rx) = world_with_identify();
    let wand = carry(&mut world, caster, "a wand of identify", "wand", (30, 2));
    dispatch(&mut world, caster, "wave wand");
    let out = drain(&mut rx);
    assert!(out.contains("At what should"), "{out}");
    assert_eq!(world.get::<Charges>(wand).map(|c| c.0), Some(3));
    dispatch(&mut world, caster, "wave wand nobody");
    assert_eq!(world.get::<Charges>(wand).map(|c| c.0), Some(3));
}

#[test]
fn wave_wand_at_a_mob_spends_a_charge() {
    let (mut world, room, caster, mut rx) = world_with_identify();
    goblin(&mut world, room);
    let wand = carry(&mut world, caster, "a wand of identify", "wand", (30, 2));
    dispatch(&mut world, caster, "wave wand goblin");
    let out = drain(&mut rx);
    assert!(out.contains("Name: a snarling goblin"), "{out}");
    assert_eq!(world.get::<Charges>(wand).map(|c| c.0), Some(2));
}
