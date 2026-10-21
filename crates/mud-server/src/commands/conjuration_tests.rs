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

/// Name the dagger the way the shop lists it, so output assertions can
/// look for it.
fn name_object(f: &mut Fx, key: (i32, i32), name: &str, weight: f64) {
    let mut protos = f.world.resource_mut::<ObjectPrototypes>();
    let proto = protos.by_key.get_mut(&key).unwrap();
    proto.name = name.into();
    proto.weight = weight;
}

#[test]
fn minor_creation_tells_the_caster_what_was_made_and_where_it_went() {
    let mut f = minor_creation_fx();
    name_object(&mut f, (10, 12), "an iron dagger", 1.0);
    let out = f.cast("'minor creation' dagger");
    assert!(
        out.contains("You create an iron dagger, which appears in your hands."),
        "{out}"
    );
    assert!(!out.contains("You cast minor creation"), "{out}");
    assert_eq!(f.carried(), vec![(10, 12)]);
}

#[test]
fn minor_creation_tells_the_room_what_was_made() {
    let mut f = minor_creation_fx();
    name_object(&mut f, (10, 12), "an iron dagger", 1.0);
    let room = f.world.get::<Located>(f.caster).unwrap().0;
    let (watcher, mut watcher_rx) = player_in(&mut f.world, room);
    f.world.entity_mut(watcher).insert(mud_world::Named {
        name: "Watcher".into(),
    });
    let caster = f.caster;
    f.world.entity_mut(caster).insert(mud_world::Named {
        name: "Tester".into(),
    });
    f.cast("'minor creation' dagger");
    let seen = drain(&mut watcher_rx);
    assert!(seen.contains("Tester creates an iron dagger."), "{seen}");
}

#[test]
fn a_conjured_item_too_heavy_to_carry_falls_to_the_ground() {
    let mut f = minor_creation_fx();
    name_object(&mut f, (10, 12), "a stone anvil", 5000.0);
    let out = f.cast("'minor creation' dagger");
    assert!(
        out.contains("You create a stone anvil, which falls to the ground."),
        "{out}"
    );
    assert!(f.carried().is_empty(), "{out}");
    let room = f.world.get::<Located>(f.caster).unwrap().0;
    let mut q = f.world.query_filtered::<&Located, With<Item>>();
    assert_eq!(q.iter(&f.world).filter(|l| l.0 == room).count(), 1);
}

#[test]
fn the_conjure_message_comes_from_the_effect_params_when_set() {
    let mut f = minor_creation_fx();
    name_object(&mut f, (10, 12), "an iron dagger", 1.0);
    f.spell(
        "MINOR_CREATION",
        "create",
        serde_json::json!({
            "message": "Light flares and {object} {where}!",
            "roomMessage": "{actor.name} conjures {object}."
        }),
    );
    let out = f.cast("'minor creation' dagger");
    assert!(
        out.contains("Light flares and an iron dagger appears in your hands!"),
        "{out}"
    );
}

#[test]
fn conjure_lines_fill_object_and_where_tokens() {
    let (caster, room) = conjuration::conjure_lines(None, "a loaf", false);
    assert_eq!(caster, "You create a loaf, which appears in your hands.");
    assert_eq!(room, "{actor.name} creates a loaf.");
    let (caster, _) = conjuration::conjure_lines(None, "a loaf", true);
    assert_eq!(caster, "You create a loaf, which falls to the ground.");
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

// -- wielded conjuration (ice dagger) ---------------------------------------

fn ice_dagger_fx() -> Fx {
    let mut f = fx();
    f.world
        .insert_resource(mud_world::ObjectAbilityCatalog::default());
    f.object((10, 47), ObjectType::Weapon);
    {
        let mut protos = f.world.resource_mut::<ObjectPrototypes>();
        let dagger = protos.by_key.get_mut(&(10, 47)).unwrap();
        dagger.name = "a dagger of ice".into();
        dagger.wear_flags = vec![mud_db::enums::WearFlag::Mainhand];
    }
    f.spell(
        "ICE_DAGGER",
        "create",
        serde_json::json!({"objectZoneId": 10, "objectId": 47, "wield": true}),
    );
    f
}

/// What the caster has in the wield slot.
fn wielded(f: &mut Fx) -> Vec<(i32, i32)> {
    let caster = f.caster;
    let mut q = f
        .world
        .query_filtered::<(&Located, &WorldKey, &mud_world::EquippedSlot), With<Item>>();
    q.iter(&f.world)
        .filter(|(l, _, eq)| l.0 == caster && eq.0 == mud_world::Slot::Wield)
        .map(|(_, k, _)| (k.zone, k.id))
        .collect()
}

#[test]
fn a_create_effect_with_wield_puts_the_object_in_the_wield_slot() {
    let mut f = ice_dagger_fx();
    let out = f.cast("'ice dagger'");
    assert_eq!(f.carried(), vec![(10, 47)], "{out}");
    assert_eq!(wielded(&mut f), vec![(10, 47)], "{out}");
}

#[test]
fn a_create_effect_with_wield_makes_nothing_when_the_hand_is_full() {
    let mut f = ice_dagger_fx();
    let room = f.world.get::<Located>(f.caster).unwrap().0;
    let (watcher, mut watcher_rx) = player_in(&mut f.world, room);
    f.world.entity_mut(watcher).insert(mud_world::Named {
        name: "Watcher".into(),
    });
    let first = f.cast("'ice dagger'");
    drain(&mut watcher_rx);
    let out = f.cast("'ice dagger'");
    assert!(
        out.contains("Your hands are not free to wield a dagger of ice"),
        "{out}"
    );
    assert_eq!(f.carried(), vec![(10, 47)], "still just the first one");
    assert_eq!(
        wielded(&mut f),
        vec![(10, 47)],
        "first dagger stays wielded"
    );
    // The failed cast is only the refusal: the success line of the first
    // (working) cast must not appear, for the caster or the room.
    let success_line = first
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.contains("wield"))
        .unwrap_or_else(|| panic!("no success line in first cast: {first}"))
        .to_string();
    assert!(!out.contains(&success_line), "{out}");
    assert_eq!(out.trim().lines().count(), 1, "{out}");
    let seen = drain(&mut watcher_rx);
    assert!(seen.trim().is_empty(), "room told of a failed cast: {seen}");
}

#[test]
fn a_create_effect_without_wield_only_carries() {
    let mut f = ice_dagger_fx();
    f.spell(
        "ICE_DAGGER",
        "create",
        serde_json::json!({"objectZoneId": 10, "objectId": 47}),
    );
    f.cast("'ice dagger'");
    assert_eq!(f.carried(), vec![(10, 47)]);
    assert!(wielded(&mut f).is_empty());
}

#[test]
fn a_created_object_with_a_lifetime_gets_a_decay_timer() {
    let mut f = ice_dagger_fx();
    f.world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .get_mut(&(10, 47))
        .unwrap()
        .timer_hours = 2;
    f.cast("'ice dagger'");
    let mut q = f.world.query::<&mud_world::ItemTimer>();
    let secs: Vec<i32> = q.iter(&f.world).map(|t| t.remaining_secs).collect();
    assert_eq!(secs, vec![2 * crate::item_decay::SECS_PER_MUD_HOUR]);
}

#[test]
fn a_conjured_item_has_the_prototypes_flags_restrictions_triggers_but_no_concealment_in_hand() {
    use mud_db::enums::{ObjectFlag, ObjectRestriction};
    let mut f = ice_dagger_fx();
    {
        let mut protos = f.world.resource_mut::<ObjectPrototypes>();
        let dagger = protos.by_key.get_mut(&(10, 47)).unwrap();
        dagger.flags = vec![ObjectFlag::Magic];
        dagger.restrictions = vec![ObjectRestriction::NoDrop];
        dagger.concealment = 30;
    }
    let mut triggers = mud_world::TriggerCatalog::default();
    triggers.object_attachments.insert((10, 47), vec![(10, 5)]);
    f.world.insert_resource(triggers);
    f.cast("'ice dagger'");
    let caster = f.caster;
    let mut q = f.world.query_filtered::<(
        Entity,
        &Located,
        Option<&mud_world::ObjectFlags>,
        Option<&mud_world::ObjectRestrictions>,
        Option<&mud_world::AttachedTriggers>,
        Option<&mud_world::Hiddenness>,
    ), With<Item>>();
    let rows: Vec<_> = q
        .iter(&f.world)
        .filter(|r| r.1.0 == caster)
        .map(|r| {
            (
                r.2.is_some_and(|x| x.has(ObjectFlag::Magic)),
                r.3.is_some_and(|x| x.has(ObjectRestriction::NoDrop)),
                r.4.map(|t| t.0.clone()),
                r.5.map(|h| h.0),
            )
        })
        .collect();
    assert_eq!(rows, vec![(true, true, Some(vec![(10, 5)]), None)]);
}

// -- up-front validation (#14) and message order (#35) ------------------------

impl Fx {
    /// A cast typed by the player: every gate runs, nothing is skipped.
    fn cast_typed(&mut self, args: &str) -> String {
        crate::commands::invoke_ability_with(
            &mut self.world,
            self.caster,
            args,
            AbilityKind::Spell,
            "cast",
            false,
            false,
            false,
            None,
        );
        drain(&mut self.rx)
    }
}

/// Class 7 owns the spell at circle 1 with a single slot at the caster's level.
fn with_one_slot(f: &mut Fx) {
    let mut slots = mud_world::SpellSlotData::default();
    slots.ability_circle.insert((7, ABILITY), 1);
    slots.progression.insert((50, 1), 1);
    f.world.insert_resource(slots);
    f.set_class(Some(7));
}

fn slots_in_use(f: &Fx) -> i32 {
    f.world
        .get::<mud_world::SpellSlots>(f.caster)
        .map_or(0, |s| s.used_in_circle(1))
}

#[test]
fn minor_creation_without_a_word_asks_before_spending_the_slot() {
    let mut f = minor_creation_fx();
    with_one_slot(&mut f);
    let out = f.cast_typed("'minor creation'");
    assert!(out.contains("What are you trying to create?"), "{out}");
    assert_eq!(slots_in_use(&f), 0);
    assert!(f.carried().is_empty());
}

#[test]
fn minor_creation_of_an_unknown_word_is_refused_before_the_slot_is_spent() {
    let mut f = minor_creation_fx();
    with_one_slot(&mut f);
    let out = f.cast_typed("'minor creation' xyzzy");
    assert!(
        out.contains("You have no idea how to create such an item."),
        "{out}"
    );
    assert_eq!(slots_in_use(&f), 0, "slot untouched");
    assert!(f.carried().is_empty());
}

#[test]
fn minor_creation_of_a_known_word_still_casts_and_charges() {
    let mut f = minor_creation_fx();
    with_one_slot(&mut f);
    let out = f.cast_typed("'minor creation' dagger");
    assert_eq!(f.carried(), vec![(10, 12)], "{out}");
    assert!(
        f.world
            .get::<mud_world::SpellSlots>(f.caster)
            .is_some_and(|s| !s.in_flight.is_empty()),
        "slot committed to recovery"
    );
}

#[test]
fn ice_dagger_announces_the_conjuring_before_the_wielding() {
    let mut f = ice_dagger_fx();
    f.world
        .resource_mut::<mud_world::AbilityCatalog>()
        .messages
        .insert(
            ABILITY,
            mud_world::AbilityMessageSet {
                success_to_caster: Some("You conjure a dagger of ice.".into()),
                ..Default::default()
            },
        );
    let out = f.cast("'ice dagger'");
    let conjured = out.find("You conjure a dagger of ice.").expect(&out);
    let wielded = out.find("wield").expect(&out);
    assert!(conjured < wielded, "{out}");
}

#[test]
fn a_conjured_decomposing_dagger_does_not_rot_while_held_but_does_once_dropped() {
    use mud_db::enums::ObjectFlag;
    let mut f = ice_dagger_fx();
    f.world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .get_mut(&(10, 47))
        .unwrap()
        .flags = vec![ObjectFlag::Magic, ObjectFlag::Decomposing];
    f.cast("'ice dagger'");
    let dagger = {
        let mut q = f
            .world
            .query_filtered::<Entity, With<mud_world::DecaysUnheld>>();
        q.single(&f.world).expect("one conjured dagger")
    };
    let rotting = |w: &World| {
        w.get::<mud_world::ObjectFlags>(dagger)
            .is_some_and(|fl| fl.has(ObjectFlag::Decomposing))
    };
    assert!(!rotting(&f.world), "wielded: not decomposing");
    assert!(f.world.get::<mud_world::ItemTimer>(dagger).is_none());
    for _ in 0..3 {
        crate::item_decay::item_decay_tick(&mut f.world);
    }
    assert!(f.world.get::<mud_world::ItemTimer>(dagger).is_none());
    assert!(!rotting(&f.world));

    // Drop it on the floor.
    let room = f.world.get::<Located>(f.caster).unwrap().0;
    f.world
        .entity_mut(dagger)
        .remove::<mud_world::EquippedSlot>()
        .insert(Located(room));
    crate::item_decay::item_decay_tick(&mut f.world);
    assert!(f.world.get::<mud_world::ItemTimer>(dagger).is_some());
    assert!(rotting(&f.world), "dropped: decomposing");
}

/// Flame Blade (10:43) carries `timer` hours in the database: the conjured
/// blade burns out after a period of use, even while wielded.
#[test]
fn a_conjured_blade_with_a_timer_burns_out_while_wielded() {
    use mud_db::enums::ObjectFlag;
    let mut f = ice_dagger_fx();
    {
        let mut protos = f.world.resource_mut::<ObjectPrototypes>();
        let blade = protos.by_key.get_mut(&(10, 47)).unwrap();
        blade.flags = vec![ObjectFlag::Magic, ObjectFlag::Decomposing];
        blade.timer_hours = 1;
    }
    f.cast("'ice dagger'");
    assert_eq!(wielded(&mut f), vec![(10, 47)]);
    let blade = {
        let mut q = f
            .world
            .query_filtered::<Entity, With<mud_world::ItemTimer>>();
        q.single(&f.world).expect("the conjured blade has a timer")
    };
    assert_eq!(
        f.world
            .get::<mud_world::ItemTimer>(blade)
            .unwrap()
            .remaining_secs,
        mud_world::SECS_PER_MUD_HOUR,
    );
    // Holding it does not stop its own timer.
    for _ in 0..(mud_world::SECS_PER_MUD_HOUR - 1) {
        crate::item_decay::item_decay_tick(&mut f.world);
    }
    assert_eq!(wielded(&mut f), vec![(10, 47)], "still burning");
    crate::item_decay::item_decay_tick(&mut f.world);
    assert!(wielded(&mut f).is_empty(), "burned out");
    assert!(f.carried().is_empty());
}

fn ice_dagger_messages() -> mud_world::AbilityMessageSet {
    mud_world::AbilityMessageSet {
        success_to_caster: Some("<b:white>You conjure a dagger of ice.</>".into()),
        success_to_room: Some("{actor.name} conjures a dagger of ice.".into()),
        ..Default::default()
    }
}

#[test]
fn the_room_hears_the_conjuring_before_the_wielding() {
    let mut f = ice_dagger_fx();
    f.world
        .resource_mut::<mud_world::AbilityCatalog>()
        .messages
        .insert(ABILITY, ice_dagger_messages());
    let room = f.world.get::<Located>(f.caster).unwrap().0;
    let (_watcher, mut watcher_rx) = player_in(&mut f.world, room);
    f.cast("'ice dagger'");
    let seen = drain(&mut watcher_rx);
    let conjured = seen.find("conjures a dagger of ice").expect(&seen);
    let wielded = seen.find("wields").expect(&seen);
    assert!(conjured < wielded, "{seen}");
    assert_eq!(
        seen.matches("conjures a dagger of ice").count(),
        1,
        "told once: {seen}"
    );
}

#[test]
fn the_caster_cast_line_keeps_its_colour() {
    let mut f = ice_dagger_fx();
    f.world
        .resource_mut::<mud_world::AbilityCatalog>()
        .messages
        .insert(ABILITY, ice_dagger_messages());
    let out = f.cast("'ice dagger'");
    let at = out.find("You conjure a dagger of ice.").expect(&out);
    assert!(out[..at].contains('\x1b'), "coloured: {out:?}");
}

// -- fixed conjurations: Flame Blade, Armor of Gaia, Create Spring -----------

/// A proto of `kind` at `key` that wears on `wear`.
fn wearable(f: &mut Fx, key: (i32, i32), kind: ObjectType, wear: mud_db::enums::WearFlag) {
    f.world.init_resource::<mud_world::ObjectAbilityCatalog>();
    f.object(key, kind);
    let mut protos = f.world.resource_mut::<ObjectPrototypes>();
    let p = protos.by_key.get_mut(&key).unwrap();
    p.name = format!("object {}.{}", key.0, key.1);
    p.wear_flags = vec![wear];
}

/// `(zone, id, slot)` of everything the caster has on, sorted.
fn worn(f: &mut Fx) -> Vec<(i32, i32, mud_world::Slot)> {
    let caster = f.caster;
    let mut q = f
        .world
        .query_filtered::<(&Located, &WorldKey, &mud_world::EquippedSlot), With<Item>>();
    let mut out: Vec<_> = q
        .iter(&f.world)
        .filter(|(l, _, _)| l.0 == caster)
        .map(|(_, k, eq)| (k.zone, k.id, eq.0))
        .collect();
    out.sort_by_key(|&(z, i, _)| (z, i));
    out
}

/// Keys of every item lying in the caster's room.
fn on_floor(f: &mut Fx) -> Vec<(i32, i32)> {
    let room = f.world.get::<Located>(f.caster).unwrap().0;
    let mut q = f
        .world
        .query_filtered::<(&Located, &WorldKey), With<Item>>();
    q.iter(&f.world)
        .filter(|(l, _)| l.0 == room)
        .map(|(_, k)| (k.zone, k.id))
        .collect()
}

#[test]
fn flame_blade_conjures_and_wields_the_blade() {
    use mud_db::enums::WearFlag;
    let mut f = fx();
    wearable(&mut f, (10, 43), ObjectType::Weapon, WearFlag::Mainhand);
    f.spell(
        "FLAME_BLADE",
        "create",
        serde_json::json!({"objectType": "weapon", "objectZoneId": 10, "objectId": 43, "wield": true}),
    );
    let out = f.cast("'flame blade'");
    assert_eq!(f.carried(), vec![(10, 43)], "{out}");
    assert_eq!(wielded(&mut f), vec![(10, 43)], "{out}");
    // Hands full: nothing more is made.
    let again = f.cast("'flame blade'");
    assert!(again.contains("Your hands are not free"), "{again}");
    assert_eq!(f.carried(), vec![(10, 43)]);
}

fn armor_of_gaia_fx() -> Fx {
    use mud_db::enums::WearFlag;
    let mut f = fx();
    wearable(&mut f, (10, 40), ObjectType::Armor, WearFlag::Body);
    wearable(&mut f, (10, 41), ObjectType::Armor, WearFlag::Arms);
    wearable(&mut f, (10, 42), ObjectType::Armor, WearFlag::Legs);
    wearable(&mut f, (10, 44), ObjectType::Armor, WearFlag::Body);
    f.spell(
        "ARMOR_OF_GAIA",
        "create",
        serde_json::json!({
            "equip": [
                {"slot": "Body", "objectId": 40, "objectZoneId": 10},
                {"slot": "Arms", "objectId": 41, "objectZoneId": 10},
                {"slot": "Legs", "objectId": 42, "objectZoneId": 10}
            ],
            "fullMessage": "Your body is too encumbered to don the armor."
        }),
    );
    f
}

#[test]
fn armor_of_gaia_puts_on_the_body_arms_and_legs_pieces() {
    use mud_world::Slot;
    let mut f = armor_of_gaia_fx();
    let out = f.cast("'armor of gaia'");
    assert_eq!(
        worn(&mut f),
        vec![
            (10, 40, Slot::Body),
            (10, 41, Slot::Arms),
            (10, 42, Slot::Legs)
        ],
        "{out}"
    );
}

#[test]
fn armor_of_gaia_only_fills_the_empty_slots_and_refuses_when_all_are_full() {
    use mud_world::Slot;
    let mut f = armor_of_gaia_fx();
    // A helm-less caster already in a body piece keeps it.
    let proto = f
        .world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(10, 44))
        .cloned()
        .unwrap();
    let own = crate::item_spawn::spawn_item_from_proto(&mut f.world, &proto, f.caster);
    assert!(crate::commands::wear_item(
        &mut f.world,
        f.caster,
        own,
        crate::commands::WearWhere::Position(Slot::Body),
        true
    ));
    f.cast("'armor of gaia'");
    assert_eq!(
        worn(&mut f),
        vec![
            (10, 41, Slot::Arms),
            (10, 42, Slot::Legs),
            (10, 44, Slot::Body)
        ]
    );
    // Now everything is worn: refused, and nothing new is made.
    let out = f.cast("'armor of gaia'");
    assert!(
        out.contains("Your body is too encumbered to don the armor."),
        "{out}"
    );
    assert_eq!(worn(&mut f).len(), 3);
    assert_eq!(f.carried().len(), 3);
}

fn create_spring_fx() -> Fx {
    let mut f = fx();
    f.object((0, 75), ObjectType::Fountain);
    f.world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .get_mut(&(0, 75))
        .unwrap()
        .name = "a clear pool of water".into();
    f.spell(
        "CREATE_SPRING",
        "create",
        serde_json::json!({
            "room": true, "objectZoneId": 0, "objectId": 75,
            "message": "A fresh clear spring of water bursts through the ground here.",
            "roomMessage": "A fresh clear spring of water bursts through the ground here.",
            "blockedSectors": ["SHALLOWS", "WATER", "UNDERWATER", "AIR"],
            "blockedMessage": "Nothing happens.",
            "blockedRoomMessage": "{actor.name} completes {actor.pos} spell, but nothing happens."
        }),
    );
    f
}

#[test]
fn create_spring_makes_the_spring_in_the_room_not_the_pack() {
    let mut f = create_spring_fx();
    let out = f.cast("'create spring'");
    assert_eq!(on_floor(&mut f), vec![(0, 75)], "{out}");
    assert!(f.carried().is_empty(), "{out}");
    assert!(
        out.contains("A fresh clear spring of water bursts through the ground here."),
        "{out}"
    );
}

#[test]
fn create_spring_does_nothing_over_water_or_air() {
    use mud_db::enums::Sector;
    let mut f = create_spring_fx();
    let room = f.world.get::<Located>(f.caster).unwrap().0;
    for sector in [
        Sector::Shallows,
        Sector::Water,
        Sector::Underwater,
        Sector::Air,
    ] {
        f.world
            .entity_mut(room)
            .insert(mud_world::RoomSector(sector));
        let out = f.cast("'create spring'");
        assert!(out.contains("Nothing happens."), "{sector:?}: {out}");
        assert!(on_floor(&mut f).is_empty(), "{sector:?}: {out}");
        assert!(!out.contains("bursts through"), "{sector:?}: {out}");
    }
    f.world
        .entity_mut(room)
        .insert(mud_world::RoomSector(Sector::Field));
    f.cast("'create spring'");
    assert_eq!(on_floor(&mut f), vec![(0, 75)]);
}

#[test]
fn armor_of_gaia_is_self_only_and_refuses_another_player() {
    use mud_world::Slot;
    let mut f = armor_of_gaia_fx();
    f.world
        .resource_mut::<mud_world::AbilityCatalog>()
        .targeting
        .insert(
            ABILITY,
            mud_world::TargetingRule {
                valid_targets: vec!["SELF".into()],
                scope: "SINGLE".into(),
                max_targets: 1,
                require_los: false,
            },
        );
    let room = f.world.get::<Located>(f.caster).unwrap().0;
    let (friend, mut friend_rx) = player_in(&mut f.world, room);
    f.world.entity_mut(friend).insert(mud_world::Named {
        name: "Friend".into(),
    });
    f.world
        .entity_mut(friend)
        .insert(mud_world::Keywords(vec!["friend".into()]));
    let out = f.cast("'armor of gaia' friend");
    assert!(out.contains("not a valid target"), "{out}");
    assert!(drain(&mut friend_rx).trim().is_empty());
    assert!(worn(&mut f).is_empty() && f.carried().is_empty(), "{out}");
    // On themselves it still works.
    f.cast("'armor of gaia'");
    assert_eq!(worn(&mut f).len(), 3);
    assert_eq!(worn(&mut f)[0].2, Slot::Body);
}
