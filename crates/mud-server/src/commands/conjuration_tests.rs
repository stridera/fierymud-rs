//! Conjuration is data-driven: summon spells spawn the mob named in their
//! params, and creation spells conjure what the `CreationRecipe` rows say.
//! End to end through `invoke_ability_with`. Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{MobProfession, ObjectType};
use mud_world::{
    CreationRecipe, CreationRecipes, Follower, Item, KnownAbilities, Located, Mob, MobPrototypes,
    ObjectPrototypes, Profile, Room, WorldKey, WorldKeyIndex,
};

use super::conjuration::{self, Conjured};
use super::test_support::{Rx, ability_def, drain, mob_proto, object_proto, player_in};

const ABILITY: i32 = 500;
const EFFECT: i32 = 501;

/// (ability, keyword, class id, object zone, object id)
type Row<'a> = (&'a str, Option<&'a str>, Option<i32>, i32, Option<i32>);

struct Fx {
    world: World,
    caster: Entity,
    rx: Rx,
}

fn fx() -> Fx {
    let mut world = World::new();
    world.insert_resource(WorldKeyIndex::default());
    world.insert_resource(MobPrototypes::default());
    world.insert_resource(ObjectPrototypes::default());
    world.insert_resource(mud_script::LuaHost::default());
    world.insert_resource(mud_world::WeatherCatalog::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    world.insert_resource(mud_world::SpellSlotData::default());
    let room = world
        .spawn((
            Room,
            WorldKey { zone: 30, id: 1 },
            mud_world::Named {
                name: "Here".into(),
            },
        ))
        .id();
    world
        .resource_mut::<WorldKeyIndex>()
        .rooms
        .insert((30, 1), room);
    let (caster, rx) = player_in(&mut world, room);
    world.entity_mut(caster).insert((
        Profile {
            level: 50,
            class_id: None,
            race: "Human".into(),
            experience: 0,
            gender: "neutral".into(),
        },
        KnownAbilities {
            entries: vec![(ABILITY, 1000, true)],
        },
    ));
    Fx { world, caster, rx }
}

impl Fx {
    /// Install a one-effect spell `plain_name` whose effect has `effect_type`
    /// and `params`.
    fn spell(&mut self, plain_name: &str, effect_type: &str, params: serde_json::Value) {
        let mut abilities = mud_world::AbilityCatalog::default();
        let mut def = ability_def(ABILITY, plain_name, AbilityKind::Spell);
        def.cast_time_rounds = 0;
        abilities
            .by_name
            .insert(plain_name.to_ascii_lowercase(), def);
        abilities
            .effects_for
            .insert(ABILITY, vec![(EFFECT, Some(params))]);
        self.world.insert_resource(abilities);
        let mut effects = mud_world::EffectCatalog::default();
        effects.by_id.insert(
            EFFECT,
            mud_world::EffectDef {
                id: EFFECT,
                name: effect_type.into(),
                description: None,
                effect_type: effect_type.into(),
                tags: vec![],
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
        self.world.insert_resource(effects);
    }

    fn cast(&mut self, args: &str) -> String {
        crate::commands::invoke_ability_with(
            &mut self.world,
            self.caster,
            args,
            AbilityKind::Spell,
            "cast",
            false,
            true,
            true,
            None,
        );
        drain(&mut self.rx)
    }

    fn mob_proto(&mut self, key: (i32, i32)) {
        self.world
            .resource_mut::<MobPrototypes>()
            .by_key
            .insert(key, mob_proto(key.0, key.1, MobProfession::Banker));
    }

    fn object(&mut self, key: (i32, i32), kind: ObjectType) {
        self.world
            .resource_mut::<ObjectPrototypes>()
            .by_key
            .insert(key, object_proto(key.0, key.1, kind));
    }

    fn recipes(&mut self, rows: &[Row]) {
        let mut recipes = CreationRecipes::default();
        for &(ability, keyword, class_id, zone, id) in rows {
            recipes.insert(
                ability,
                CreationRecipe {
                    keyword: keyword.map(str::to_string),
                    class_id,
                    object_zone_id: zone,
                    object_id: id,
                },
            );
        }
        self.world.insert_resource(recipes);
    }

    fn set_class(&mut self, class_id: Option<i32>) {
        self.world.get_mut::<Profile>(self.caster).unwrap().class_id = class_id;
    }

    /// Keys of every item the caster carries.
    fn carried(&mut self) -> Vec<(i32, i32)> {
        let caster = self.caster;
        let mut q = self
            .world
            .query_filtered::<(&Located, &WorldKey), With<Item>>();
        let mut keys: Vec<_> = q
            .iter(&self.world)
            .filter(|(l, _)| l.0 == caster)
            .map(|(_, k)| (k.zone, k.id))
            .collect();
        keys.sort_unstable();
        keys
    }

    /// Keys of every mob following the caster.
    fn followers(&mut self) -> Vec<(i32, i32)> {
        let caster = self.caster;
        let mut q = self
            .world
            .query_filtered::<(&Follower, &WorldKey), With<Mob>>();
        q.iter(&self.world)
            .filter(|(f, _)| f.0 == caster)
            .map(|(_, k)| (k.zone, k.id))
            .collect()
    }
}

// -- summon ---------------------------------------------------------------

/// Every summon spell with the mob it is configured with in
/// `fierylib/data/abilities.json`.
const SUMMON_SPELLS: [(&str, &str, i32, i32); 10] = [
    ("ANIMATE_DEAD", "", 54, 20),
    ("CLONE", "", 163, 8),
    ("MOUNT", "mount", 324, 21),
    ("SIMULACRUM", "simulacrum", 163, 8),
    ("SPHERE_SUMMON", "elemental", 52, 12),
    ("SUMMON_DEMON", "demon", 510, 24),
    ("SUMMON_DRACOLICH", "dracolich", 533, 11),
    ("SUMMON_ELEMENTAL", "elemental", 52, 12),
    ("SUMMON_GREATER_DEMON", "greater_demon", 160, 11),
    ("SUMMON_MOUNT", "mount", 324, 21),
];

#[test]
fn each_summon_spell_spawns_the_mob_its_params_name() {
    for (spell, mob_type, zone, id) in SUMMON_SPELLS {
        let mut f = fx();
        f.mob_proto((zone, id));
        let mut params = serde_json::json!({"mobZone": zone, "mobId": id});
        if !mob_type.is_empty() {
            params["mobType"] = mob_type.into();
        }
        f.spell(spell, "summon", params);
        let out = f.cast(&spell.to_ascii_lowercase());
        assert_eq!(f.followers(), vec![(zone, id)], "{spell}: {out}");
        assert!(out.contains("You summon"), "{spell}: {out}");
    }
}

#[test]
fn the_mob_comes_from_the_params_not_the_spell_name_or_mob_type() {
    // A builder repoints a classic spell at another mob; nothing in the
    // runtime knows the old pairing.
    let mut f = fx();
    f.mob_proto((77, 3));
    f.mob_proto((324, 21));
    f.spell(
        "SUMMON_MOUNT",
        "summon",
        serde_json::json!({"mobType": "mount", "mobZone": 77, "mobId": 3}),
    );
    f.cast("summon_mount");
    assert_eq!(f.followers(), vec![(77, 3)]);
}

#[test]
fn a_summon_without_mob_ids_fails_cleanly_and_spawns_nothing() {
    let mut f = fx();
    f.mob_proto((324, 21));
    // The retired mobType-only shape: no mobZone/mobId.
    f.spell(
        "SUMMON_MOUNT",
        "summon",
        serde_json::json!({"mobType": "mount"}),
    );
    let out = f.cast("summon_mount");
    assert!(out.contains("nothing answers the call"), "{out}");
    assert!(f.followers().is_empty());
}

#[test]
fn the_builder_message_names_the_ability_and_the_missing_keys() {
    let msg = conjuration::summon_mob_key(
        "SUMMON_GARGOYLE",
        Some(&serde_json::json!({"mobType": "gargoyle"})),
    )
    .unwrap_err();
    assert!(msg.contains("SUMMON_GARGOYLE"), "{msg}");
    assert!(msg.contains("mobType 'gargoyle'"), "{msg}");
    assert!(msg.contains("mobZone/mobId"), "{msg}");
    // Missing params entirely, or non-integer ids, are the same error.
    assert!(conjuration::summon_mob_key("X", None).is_err());
    assert!(
        conjuration::summon_mob_key("X", Some(&serde_json::json!({"mobZone": "a", "mobId": 1})))
            .is_err()
    );
    assert_eq!(
        conjuration::summon_mob_key("X", Some(&serde_json::json!({"mobZone": 5, "mobId": 6}))),
        Ok((5, 6))
    );
}

#[test]
fn a_summon_naming_an_unloaded_mob_spawns_nothing() {
    let mut f = fx();
    f.spell(
        "SUMMON_DEMON",
        "summon",
        serde_json::json!({"mobType": "demon", "mobZone": 510, "mobId": 24}),
    );
    f.cast("summon_demon");
    assert!(f.followers().is_empty());
}

// -- minor creation -------------------------------------------------------

fn minor_creation_fx() -> Fx {
    let mut f = fx();
    for id in 10..=15 {
        f.object((10, id), ObjectType::Weapon);
    }
    f.spell(
        "MINOR_CREATION",
        "create",
        serde_json::json!({"objectType": "magical_item"}),
    );
    f.recipes(&[
        ("MINOR_CREATION", Some("club"), None, 10, Some(10)),
        ("MINOR_CREATION", Some("mace"), None, 10, Some(11)),
        ("MINOR_CREATION", Some("dagger"), None, 10, Some(12)),
        ("MINOR_CREATION", Some("greatsword"), None, 10, Some(13)),
        ("MINOR_CREATION", Some("longsword"), None, 10, Some(14)),
    ]);
    f
}

#[test]
fn minor_creation_of_a_keyword_makes_the_item_the_recipe_names() {
    let mut f = minor_creation_fx();
    let out = f.cast("'minor creation' dagger");
    assert_eq!(f.carried(), vec![(10, 12)], "{out}");
}

#[test]
fn minor_creation_matches_an_abbreviation_in_table_order() {
    let mut f = minor_creation_fx();
    f.cast("'minor creation' gre");
    assert_eq!(f.carried(), vec![(10, 13)]);
    let mut f = minor_creation_fx();
    f.cast("'minor creation' lo");
    assert_eq!(f.carried(), vec![(10, 14)]);
}

#[test]
fn minor_creation_object_ids_are_data_not_list_positions() {
    // The same word can point anywhere; the position in the table means nothing.
    let mut f = minor_creation_fx();
    f.object((200, 7), ObjectType::Weapon);
    f.recipes(&[
        ("MINOR_CREATION", Some("mace"), None, 10, Some(11)),
        ("MINOR_CREATION", Some("dagger"), None, 200, Some(7)),
    ]);
    f.cast("'minor creation' dagger");
    assert_eq!(f.carried(), vec![(200, 7)]);
}

#[test]
fn minor_creation_of_an_unknown_word_makes_nothing() {
    let mut f = minor_creation_fx();
    f.cast("'minor creation' xyzzy");
    assert!(f.carried().is_empty());
    f.cast("'minor creation'");
    assert!(f.carried().is_empty());
}

#[test]
fn recipe_lookup_is_per_ability() {
    let mut f = minor_creation_fx();
    f.spell(
        "OTHER_CREATION",
        "create",
        serde_json::json!({"objectType": "magical_item"}),
    );
    f.cast("'other creation' dagger");
    assert!(f.carried().is_empty());
}

// -- create food ----------------------------------------------------------

const PRIEST: i32 = 16;
const PALADIN: i32 = 5;
const CLERIC: i32 = 2;

fn create_food_fx() -> Fx {
    let mut f = fx();
    // One food per class zone, plus a non-food decoy at the lowest id of each.
    for (zone, food_id) in [(100, 4), (110, 5), (120, 6)] {
        f.object((zone, 0), ObjectType::Armor);
        f.object((zone, food_id), ObjectType::Food);
    }
    // Waybread, the ability's own fallback.
    f.object((185, 8), ObjectType::Food);
    f.spell(
        "CREATE_FOOD",
        "create",
        serde_json::json!({"objectType": "Magical food item", "objectZoneId": 185, "objectId": 8}),
    );
    f.recipes(&[
        ("CREATE_FOOD", None, Some(PRIEST), 100, None),
        ("CREATE_FOOD", None, Some(PALADIN), 110, None),
        ("CREATE_FOOD", None, None, 120, None),
    ]);
    f
}

#[test]
fn create_food_picks_the_zone_by_caster_class() {
    for (class_id, expected) in [
        (Some(PRIEST), (100, 4)),
        (Some(PALADIN), (110, 5)),
        // Cleric has no row of its own: the all-classes default.
        (Some(CLERIC), (120, 6)),
        (None, (120, 6)),
    ] {
        let mut f = create_food_fx();
        f.set_class(class_id);
        let out = f.cast("'create food'");
        assert_eq!(f.carried(), vec![expected], "class {class_id:?}: {out}");
    }
}

#[test]
fn create_food_only_conjures_food_typed_protos() {
    // The decoy armor at (100, 0) is never picked, whatever the skill.
    for _ in 0..20 {
        let mut f = create_food_fx();
        f.set_class(Some(PRIEST));
        f.cast("'create food'");
        assert_eq!(f.carried(), vec![(100, 4)]);
    }
}

#[test]
fn create_food_falls_back_to_the_ability_params_when_the_zone_has_no_food() {
    let mut f = create_food_fx();
    f.recipes(&[("CREATE_FOOD", None, None, 999, None)]);
    f.set_class(None);
    f.cast("'create food'");
    assert_eq!(f.carried(), vec![(185, 8)]);
}

#[test]
fn a_recipe_with_an_object_id_conjures_exactly_that_object() {
    let mut f = create_food_fx();
    f.recipes(&[("CREATE_FOOD", None, Some(PRIEST), 110, Some(5))]);
    f.set_class(Some(PRIEST));
    f.cast("'create food'");
    assert_eq!(f.carried(), vec![(110, 5)]);
}

// -- lookup and pick helpers ---------------------------------------------

#[test]
fn creation_choice_prefers_the_keyword_row_then_class_then_default() {
    let mut recipes = CreationRecipes::default();
    let row = |keyword: Option<&str>, class_id, zone, id| CreationRecipe {
        keyword: keyword.map(str::to_string),
        class_id,
        object_zone_id: zone,
        object_id: id,
    };
    recipes.insert("FOO", row(Some("axe"), None, 1, Some(1)));
    recipes.insert("FOO", row(None, Some(7), 2, None));
    recipes.insert("FOO", row(None, None, 3, None));
    let pick = |word, class| conjuration::creation_choice(&recipes, "foo", word, class);
    assert_eq!(pick(Some("ax"), Some(7)), Some(Conjured::Fixed(1, 1)));
    assert_eq!(pick(None, Some(7)), Some(Conjured::FoodIn(2)));
    assert_eq!(pick(Some("nope"), Some(7)), Some(Conjured::FoodIn(2)));
    assert_eq!(pick(None, Some(9)), Some(Conjured::FoodIn(3)));
    assert_eq!(pick(None, None), Some(Conjured::FoodIn(3)));
    assert_eq!(
        conjuration::creation_choice(&recipes, "BAR", None, None),
        None
    );
}

#[test]
fn pick_food_scales_with_skill_and_never_overruns() {
    let foods = [(1, 0), (1, 1), (1, 2), (1, 3)];
    assert_eq!(conjuration::pick_food(&foods, 0, 0), Some((1, 0)));
    assert_eq!(conjuration::pick_food(&foods, 100, 0), Some((1, 3)));
    assert_eq!(conjuration::pick_food(&foods, 100, 2), Some((1, 3)));
    assert_eq!(conjuration::pick_food(&foods, 50, 0), Some((1, 1)));
    assert_eq!(conjuration::pick_food(&foods, -5, 0), Some((1, 0)));
    assert_eq!(conjuration::pick_food(&[], 50, 1), None);
}
