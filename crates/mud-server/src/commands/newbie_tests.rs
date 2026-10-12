//! New-player experience (playtest round 5): class skills known from level 1,
//! `study` / `practice` on skills, the starting kit, shop "(can't use)" marks,
//! quest variables as the trigger corpus reads them, and scripted escorts that
//! look and greet like a typed move. Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Direction, ObjectType};
use mud_world::{
    AbilityCatalog, ClassSkillsData, Exits, KnownAbilities, Located, Named, ObjectPrototypes,
    Profile, Room, SpellSlotData, StartingKit, StartingKitItem, TriggerAttach, TriggerCatalog,
    TriggerDef, TriggerEvent,
};

use super::test_support::{Rx, ability_def, drain, object_proto, player_in};

const WARRIOR: i32 = 4;
const KICK: i32 = 11;
const BASH: i32 = 12;
const DODGE: i32 = 13;
const PARRY: i32 = 14;
const FIREBALL: i32 = 90;
const METEOR: i32 = 91;

fn warrior_world() -> (World, Entity, Rx) {
    let mut world = World::new();
    let mut catalog = AbilityCatalog::default();
    for (id, name, kind) in [
        (KICK, "Kick", AbilityKind::Skill),
        (BASH, "Bash", AbilityKind::Skill),
        (DODGE, "Dodge", AbilityKind::Skill),
        (PARRY, "Parry", AbilityKind::Skill),
        (FIREBALL, "Fireball", AbilityKind::Spell),
        (METEOR, "Meteor", AbilityKind::Spell),
    ] {
        catalog
            .by_name
            .insert(name.to_ascii_lowercase(), ability_def(id, name, kind));
    }
    world.insert_resource(catalog);
    let mut skills = ClassSkillsData::default();
    for (id, min) in [(KICK, 1), (BASH, 1), (DODGE, 1), (PARRY, 10)] {
        skills.min_level.insert((WARRIOR, id), min);
        skills.proficiency_cap.insert((WARRIOR, id), 100);
    }
    // A (bogus) spell row in the skill table must not be granted as a skill.
    skills.min_level.insert((WARRIOR, FIREBALL), 1);
    world.insert_resource(skills);
    world.insert_resource(SpellSlotData::default());
    let room = world
        .spawn((
            Room,
            Named {
                name: "Yard".into(),
            },
            Exits::default(),
        ))
        .id();
    let (player, rx) = player_in(&mut world, room);
    world.entity_mut(player).insert(Profile {
        level: 1,
        class_id: Some(WARRIOR),
        race: "HUMAN".into(),
        experience: 0,
        gender: "male".into(),
    });
    (world, player, rx)
}

fn ids(known: &KnownAbilities) -> Vec<i32> {
    known.entries.iter().map(|(id, _, _)| *id).collect()
}

#[test]
fn a_new_warrior_knows_the_level_one_class_skills_at_legacy_proficiency() {
    let (world, _player, _rx) = warrior_world();
    let mut known = KnownAbilities::default();
    let granted = crate::login::grant_class_skills_for(&world, Some(WARRIOR), 1, &mut known);
    assert_eq!(granted, vec![KICK, BASH, DODGE]);
    // Legacy: skills that improve with use start at 50 of 1000.
    assert_eq!(
        known.entries,
        vec![(KICK, 50, true), (BASH, 50, true), (DODGE, 50, true)]
    );
    // Not the level-10 skill, and not the spell row.
    assert!(!known.has_any(PARRY));
    assert!(!known.has_any(FIREBALL));
}

#[test]
fn quest_only_skills_are_never_granted_by_class_level() {
    let (mut world, _player, _rx) = warrior_world();
    // Make Dodge a quest-only skill; the class table still lists it at level 1.
    world
        .resource_mut::<AbilityCatalog>()
        .by_name
        .get_mut("dodge")
        .unwrap()
        .quest_only = true;
    let mut known = KnownAbilities::default();
    let granted = crate::login::grant_class_skills_for(&world, Some(WARRIOR), 99, &mut known);
    assert_eq!(granted, vec![KICK, BASH, PARRY]);
    assert!(!known.has_any(DODGE));
}

#[test]
fn granting_is_idempotent_and_never_resets_trained_skills() {
    let (world, _player, _rx) = warrior_world();
    let mut known = KnownAbilities {
        entries: vec![(KICK, 700, true), (PARRY, 30, false)],
    };
    let granted = crate::login::grant_class_skills_for(&world, Some(WARRIOR), 1, &mut known);
    assert_eq!(granted, vec![BASH, DODGE]);
    assert_eq!(known.entries[0], (KICK, 700, true));
    assert_eq!(ids(&known), vec![KICK, BASH, DODGE, PARRY]);
    assert!(crate::login::grant_class_skills_for(&world, Some(WARRIOR), 1, &mut known).is_empty());
}

#[test]
fn reaching_the_level_grants_the_next_skills() {
    let (world, _player, _rx) = warrior_world();
    let mut known = KnownAbilities::default();
    crate::login::grant_class_skills_for(&world, Some(WARRIOR), 1, &mut known);
    let granted = crate::login::grant_class_skills_for(&world, Some(WARRIOR), 10, &mut known);
    assert_eq!(granted, vec![PARRY]);
    assert!(crate::login::grant_class_skills_for(&world, None, 10, &mut known).is_empty());
}

#[test]
fn level_up_teaches_and_announces_the_new_skills() {
    let (mut world, player, mut rx) = warrior_world();
    world.entity_mut(player).insert(KnownAbilities::default());
    crate::combat::grant_level_skills(&mut world, player, 10);
    let out = drain(&mut rx);
    assert!(
        out.contains("You can now use: Bash, Dodge, Kick, Parry."),
        "{out}"
    );
    let known = world.get::<KnownAbilities>(player).unwrap();
    assert!(known.has_any(PARRY));
}

#[test]
fn study_learns_a_class_skill_and_gates_it_by_level() {
    let (mut world, player, mut rx) = warrior_world();
    super::spells::cmd_study(&mut world, player, "kick");
    let out = drain(&mut rx);
    assert!(
        out.contains("You commit Kick to memory. (proficiency 50)"),
        "{out}"
    );
    assert!(world.get::<KnownAbilities>(player).unwrap().has_any(KICK));

    super::spells::cmd_study(&mut world, player, "kick");
    assert!(drain(&mut rx).contains("You already know Kick."));

    super::spells::cmd_study(&mut world, player, "parry");
    let out = drain(&mut rx);
    assert!(
        out.contains("You must reach level 10 before you can learn Parry."),
        "{out}"
    );
    assert!(!world.get::<KnownAbilities>(player).unwrap().has_any(PARRY));

    super::spells::cmd_study(&mut world, player, "meteor");
    assert!(drain(&mut rx).contains("Meteor isn't on your class's list."));
}

#[test]
fn practice_raises_a_skill_up_to_the_class_cap() {
    let (mut world, player, mut rx) = warrior_world();
    world.entity_mut(player).insert(KnownAbilities {
        entries: vec![(KICK, 50, true)],
    });
    world.entity_mut(player).insert(mud_world::SkillPoints(3));
    super::practice_one(&mut world, player, "kick");
    let out = drain(&mut rx);
    assert!(!out.contains("isn't on your class's list"), "{out}");
    let prof = world.get::<KnownAbilities>(player).unwrap().entries[0].1;
    assert!(prof > 50, "practice raised the skill: {out}");
}

fn kit_world() -> (World, Entity, Rx) {
    let (mut world, player, rx) = warrior_world();
    let mut protos = ObjectPrototypes::default();
    for (zone, id, name) in [(0, 23, "a wooden torch"), (10, 14, "a crude longsword")] {
        let mut p = object_proto(zone, id, ObjectType::Other);
        p.name = name.into();
        protos.by_key.insert((zone, id), p);
    }
    world.insert_resource(protos);
    world.insert_resource(StartingKit {
        items: vec![
            StartingKitItem {
                class_id: None,
                object_zone_id: 0,
                object_id: 23,
                quantity: 1,
            },
            StartingKitItem {
                class_id: Some(WARRIOR),
                object_zone_id: 10,
                object_id: 14,
                quantity: 1,
            },
            StartingKitItem {
                class_id: Some(1),
                object_zone_id: 10,
                object_id: 29,
                quantity: 1,
            },
            StartingKitItem {
                class_id: None,
                object_zone_id: 77,
                object_id: 77,
                quantity: 1,
            },
        ],
    });
    (world, player, rx)
}

fn carried_names(world: &mut World, holder: Entity) -> Vec<String> {
    let mut q = world.query::<(&Located, &Named)>();
    let mut names: Vec<String> = q
        .iter(world)
        .filter(|(l, _)| l.0 == holder)
        .map(|(_, n)| n.name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn a_new_character_is_handed_the_common_pack_and_their_class_gear() {
    let (mut world, player, mut rx) = kit_world();
    let given = crate::login::give_starting_kit(&mut world, player);
    // The torch and the warrior's longsword; the sorcerer's book is not for
    // them and the unloaded object is skipped.
    assert_eq!(given, 2);
    assert_eq!(
        carried_names(&mut world, player),
        vec![
            "a crude longsword".to_string(),
            "a wooden torch".to_string()
        ]
    );
    assert!(drain(&mut rx).contains("starting gear"));
}

#[test]
fn an_empty_kit_gives_nothing_and_says_nothing() {
    let (mut world, player, mut rx) = kit_world();
    world.insert_resource(StartingKit::default());
    assert_eq!(crate::login::give_starting_kit(&mut world, player), 0);
    assert!(drain(&mut rx).is_empty());
}

#[test]
fn shop_wares_the_class_cannot_use_are_flagged() {
    let (world, player, _rx) = warrior_world();
    let mut boots = object_proto(1, 1, ObjectType::Armor);
    boots.restricted_class_ids = vec![WARRIOR];
    assert!(crate::commands::proto_unusable_by(&world, player, &boots));
    boots.restricted_class_ids = vec![1];
    assert!(!crate::commands::proto_unusable_by(&world, player, &boots));
    let mut elven = object_proto(1, 2, ObjectType::Armor);
    elven.allowed_races = vec!["ELF".into()];
    assert!(crate::commands::proto_unusable_by(&world, player, &elven));
    elven.allowed_races = vec!["HUMAN".into()];
    assert!(!crate::commands::proto_unusable_by(&world, player, &elven));
}

// --- quest variables and scripted escorts ---------------------------------

fn lua_trigger(zone: i32, id: i32, event: TriggerEvent, body: &str) -> TriggerDef {
    TriggerDef {
        zone_id: zone,
        id,
        name: format!("t{id}"),
        attach_type: TriggerAttach::Mob,
        commands: body.to_string(),
        flags: vec![event],
        arg_list: vec![],
        num_args: 0,
    }
}

fn mob_with_trigger(world: &mut World, room: Entity, name: &str, key: (i32, i32)) -> Entity {
    world
        .spawn((
            mud_world::Mob,
            Named { name: name.into() },
            mud_world::Health { hp: 10, max: 10 },
            mud_world::WorldKey {
                zone: key.0,
                id: key.1,
            },
            Located(room),
            mud_world::Posture(mud_world::PostureKind::Standing),
            mud_world::AttachedTriggers(vec![key]),
        ))
        .id()
}

#[test]
fn quest_vars_read_the_way_the_corpus_writes_them() {
    let (mut world, player, mut rx) = warrior_world();
    let mut catalog = TriggerCatalog::default();
    // Unset is nil (falsy), a stored number is a number, a word is a string.
    let body = r#"
        local out = {}
        out[#out+1] = tostring(actor:get_quest_var("school:gear"))
        if actor:get_quest_var("school:gear") then out[#out+1] = "truthy" end
        actor:set_quest_var("school", "speech", 2)
        out[#out+1] = tostring(actor:get_quest_var("school:speech") == 2)
        out[#out+1] = tostring((actor:get_quest_var("school:speech") or 0) + 1)
        actor:set_quest_var("school", "gear", "complete")
        out[#out+1] = tostring(actor:get_quest_var("school:gear") ~= "complete")
        actor:send(table.concat(out, ","))
    "#;
    catalog
        .by_key
        .insert((99, 1), lua_trigger(99, 1, TriggerEvent::Speech, body));
    world.insert_resource(catalog);
    world.insert_resource(mud_script::LuaHost::new());
    let room = world.get::<Located>(player).unwrap().0;
    mob_with_trigger(&mut world, room, "teacher", (99, 1));
    crate::triggers::fire_speech_in_room(&mut world, player, room, "go");
    let out = drain(&mut rx);
    assert!(out.contains("Nil,true,3,false"), "{out}");
}

/// Two rooms whose GREET triggers walk the player into each other used to
/// recurse arrivals -> GREET -> `actor:move` -> arrivals until the stack
/// overflowed. The depth cap ends the loop after a bounded number of runs.
#[test]
fn greet_triggers_that_walk_the_player_back_and_forth_terminate() {
    let (mut world, player, mut rx) = warrior_world();
    let gate = world.get::<Located>(player).unwrap().0;
    let hall = world
        .spawn((
            Room,
            Named {
                name: "Welcome Hall".into(),
            },
            Exits::default(),
        ))
        .id();
    let open_exit = |to: Entity| mud_world::ExitData {
        to: Some(to),
        state: mud_db::enums::ExitState::Open,
        key: None,
        description: None,
        keywords: Vec::new(),
        is_hidden: false,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    };
    world
        .get_mut::<Exits>(gate)
        .unwrap()
        .0
        .insert(Direction::East, open_exit(hall));
    world
        .get_mut::<Exits>(hall)
        .unwrap()
        .0
        .insert(Direction::West, open_exit(gate));
    let mut catalog = TriggerCatalog::default();
    catalog.by_key.insert(
        (99, 1),
        lua_trigger(
            99,
            1,
            TriggerEvent::GreetAll,
            r#"actor:send("pong"); actor:move("east")"#,
        ),
    );
    catalog.by_key.insert(
        (99, 2),
        lua_trigger(
            99,
            2,
            TriggerEvent::GreetAll,
            r#"actor:send("pong"); actor:move("west")"#,
        ),
    );
    world.insert_resource(catalog);
    world.insert_resource(mud_script::LuaHost::new());
    mob_with_trigger(&mut world, gate, "east walker", (99, 1));
    mob_with_trigger(&mut world, hall, "west walker", (99, 2));

    crate::triggers::fire_greet_in_room(&mut world, player, gate);

    let out = drain(&mut rx);
    let pings = out.to_lowercase().matches("pong").count();
    assert!(
        (2..=12).contains(&pings),
        "the move loop must stop at the depth cap, ran {pings} times"
    );
    // The depth unwinds, so the next greet starts fresh and also terminates.
    crate::triggers::fire_greet_in_room(&mut world, player, gate);
    assert!(drain(&mut rx).to_lowercase().contains("pong"));
}

#[test]
fn a_scripted_escort_shows_the_room_and_wakes_the_greeter() {
    let (mut world, player, mut rx) = warrior_world();
    let gate = world.get::<Located>(player).unwrap().0;
    let hall = world
        .spawn((
            Room,
            Named {
                name: "Welcome Hall".into(),
            },
            Exits::default(),
        ))
        .id();
    world.get_mut::<Exits>(gate).unwrap().0.insert(
        Direction::East,
        mud_world::ExitData {
            to: Some(hall),
            state: mud_db::enums::ExitState::Open,
            key: None,
            description: None,
            keywords: Vec::new(),
            is_hidden: false,
            is_pickproof: false,
            is_bashable: false,
            hit_points: None,
        },
    );
    let mut catalog = TriggerCatalog::default();
    catalog.by_key.insert(
        (99, 1),
        lua_trigger(
            99,
            1,
            TriggerEvent::Speech,
            r#"actor:send("The recruiter escorts you in."); actor:move("east")"#,
        ),
    );
    catalog.by_key.insert(
        (99, 2),
        lua_trigger(
            99,
            2,
            TriggerEvent::GreetAll,
            r#"actor:send("Greetings adventurer!")"#,
        ),
    );
    world.insert_resource(catalog);
    world.insert_resource(mud_script::LuaHost::new());
    mob_with_trigger(&mut world, gate, "recruiter", (99, 1));
    mob_with_trigger(&mut world, hall, "instructor", (99, 2));

    crate::triggers::fire_speech_in_room(&mut world, player, gate, "ready");

    assert_eq!(world.get::<Located>(player).unwrap().0, hall);
    let out = drain(&mut rx);
    let escort = out.find("escorts you in").expect(&out);
    let look = out.find("Welcome Hall").expect(&out);
    let greet = out.find("Greetings adventurer!").expect(&out);
    assert!(
        escort < look && look < greet,
        "escort, then the room, then the greeting: {out}"
    );
}

#[test]
fn a_tell_to_a_mob_in_the_room_reaches_its_speech_script() {
    let (mut world, player, mut rx) = warrior_world();
    let room = world.get::<Located>(player).unwrap().0;
    let mut catalog = TriggerCatalog::default();
    catalog.by_key.insert(
        (99, 1),
        lua_trigger(
            99,
            1,
            TriggerEvent::SpeechTo,
            r#"if string.find(speech, "hello teacher", 1, true) then actor:send("Well hello yourself!") end"#,
        ),
    );
    world.insert_resource(catalog);
    world.insert_resource(mud_script::LuaHost::new());
    let mob = mob_with_trigger(&mut world, room, "the Instructor", (99, 1));
    world
        .entity_mut(mob)
        .insert(mud_world::Keywords(vec!["instructor-adventure".into()]));
    world.entity_mut(player).insert(mud_world::Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: mud_db::enums::UserRole::Player,
        account_role: mud_db::enums::UserRole::Player,
        perms: vec![],
    });
    super::dispatch(&mut world, player, "tell instructor hello teacher");
    let out = drain(&mut rx);
    assert!(
        out.contains("You tell") && out.contains("the Instructor"),
        "{out}"
    );
    assert!(out.contains("Well hello yourself!"), "{out}");
    super::dispatch(&mut world, player, "tell nobody hi");
    assert!(drain(&mut rx).contains("'nobody' isn't online."));
}

// ---- N7: spells are learned with `study`, and the game says so -------------

const SORCERER: i32 = 1;
const MAGIC_MISSILE: i32 = 100;
const BURNING_HANDS: i32 = 101;
const FIREBALL_S: i32 = 102;
const EMPOWER: i32 = 103;
const QUEST_SPELL: i32 = 104;

/// A level-1 Elf Sorcerer: circle 1 from level 1, circle 2 from level 9.
/// Magic Missile is also an elven innate, Empower (circle 2 here) another.
fn sorcerer_world() -> (World, Entity, Rx) {
    let (mut world, player, rx) = warrior_world();
    let mut catalog = AbilityCatalog::default();
    for (id, name) in [
        (MAGIC_MISSILE, "Magic Missile"),
        (BURNING_HANDS, "Burning Hands"),
        (FIREBALL_S, "Fireball"),
        (EMPOWER, "Empower"),
        (QUEST_SPELL, "Quest Bolt"),
    ] {
        let mut def = ability_def(id, name, AbilityKind::Spell);
        def.quest_only = id == QUEST_SPELL;
        catalog.by_name.insert(name.to_ascii_lowercase(), def);
    }
    world.insert_resource(catalog);
    let mut slots = SpellSlotData::default();
    slots.class_circles.insert(SORCERER, vec![(1, 1), (2, 9)]);
    for (id, circle) in [
        (MAGIC_MISSILE, 1),
        (BURNING_HANDS, 1),
        (FIREBALL_S, 2),
        (EMPOWER, 2),
        (QUEST_SPELL, 1),
    ] {
        slots.ability_circle.insert((SORCERER, id), circle);
    }
    world.insert_resource(slots);
    let mut races = mud_world::RaceAbilitiesData::default();
    races.insert("ELF", MAGIC_MISSILE, 100);
    races.insert("ELF", EMPOWER, 100);
    world.insert_resource(races);
    world.entity_mut(player).insert((
        Profile {
            level: 1,
            class_id: Some(SORCERER),
            race: "ELF".into(),
            experience: 0,
            gender: "female".into(),
        },
        KnownAbilities {
            entries: vec![(MAGIC_MISSILE, 1000, true), (EMPOWER, 1000, true)],
        },
    ));
    (world, player, rx)
}

#[test]
fn bare_study_lists_what_the_class_can_learn_at_this_level() {
    let (mut world, player, mut rx) = sorcerer_world();
    super::spells::cmd_study(&mut world, player, "");
    let out = drain(&mut rx);
    assert!(out.contains("Circle 1: Burning Hands"), "{out}");
    // Known (Magic Missile), above the level (Fireball), quest-only: absent.
    assert!(!out.contains("Magic Missile"), "{out}");
    assert!(!out.contains("Fireball"), "{out}");
    assert!(!out.contains("Quest Bolt"), "{out}");
    assert!(out.contains("Type 'study <name>' to learn one."), "{out}");

    super::spells::cmd_study(&mut world, player, "burning hands");
    let learned = drain(&mut rx);
    assert!(
        learned.contains("You commit Burning Hands to memory.\r\n"),
        "no proficiency number on a spell: {learned}"
    );
    super::spells::cmd_study(&mut world, player, "");
    assert!(
        drain(&mut rx).contains("nothing new for you to study"),
        "list is empty once everything available is learned"
    );
}

#[test]
fn study_refuses_a_spell_whose_circle_is_not_unlocked_yet() {
    let (mut world, player, mut rx) = sorcerer_world();
    super::spells::cmd_study(&mut world, player, "fireball");
    let out = drain(&mut rx);
    assert!(
        out.contains("You must reach level 9 before you can learn Fireball."),
        "{out}"
    );
    assert!(
        !world
            .get::<KnownAbilities>(player)
            .unwrap()
            .has_any(FIREBALL_S)
    );
    world.get_mut::<Profile>(player).unwrap().level = 9;
    super::spells::cmd_study(&mut world, player, "fireball");
    assert!(drain(&mut rx).contains("You commit Fireball to memory."));
}

#[test]
fn level_up_and_first_login_say_how_many_spells_can_be_learned() {
    let (mut world, player, mut rx) = sorcerer_world();
    crate::combat::announce_learnable(&mut world, player);
    let out = drain(&mut rx);
    assert!(
        out.contains("You can learn 1 spell at your level. Type 'study' to see them."),
        "{out}"
    );
    // Nothing to say for a warrior (no spell lists).
    let (mut world, player, mut rx) = warrior_world();
    crate::combat::announce_learnable(&mut world, player);
    assert_eq!(drain(&mut rx), "");
}

#[test]
fn spells_hides_a_racial_innate_the_class_has_not_reached_yet() {
    let (mut world, player, mut rx) = sorcerer_world();
    super::info::cmd_spells(&mut world, player, "");
    let out = drain(&mut rx);
    // Magic Missile is circle 1, available; Empower is circle 2, level 9.
    assert!(out.contains("Magic Missile"), "{out}");
    assert!(!out.contains("Empower"), "{out}");
    world.get_mut::<Profile>(player).unwrap().level = 9;
    super::info::cmd_spells(&mut world, player, "");
    assert!(drain(&mut rx).contains("Empower"));
}

// ---- N14: `where corpse` ----------------------------------------------------

fn link(world: &mut World, from: Entity, dir: Direction, to: Entity) {
    world.get_mut::<Exits>(from).unwrap().0.insert(
        dir,
        mud_world::ExitData {
            to: Some(to),
            state: mud_db::enums::ExitState::Open,
            key: None,
            description: None,
            keywords: Vec::new(),
            is_hidden: false,
            is_pickproof: false,
            is_bashable: false,
            hit_points: None,
        },
    );
}

fn named_room(world: &mut World, name: &str, zone: i32, id: i32) -> Entity {
    world
        .spawn((
            Room,
            Named { name: name.into() },
            Exits::default(),
            mud_world::WorldKey { zone, id },
        ))
        .id()
}

#[test]
fn where_corpse_names_the_room_and_the_way_back() {
    let mut world = World::new();
    // Temple -north-> Square (Town Center) -east-> Square (Town Center).
    let temple = named_room(&mut world, "Temple", 30, 1);
    let square_a = named_room(&mut world, "Town Center", 30, 5);
    let square_b = named_room(&mut world, "Town Center", 30, 12);
    link(&mut world, temple, Direction::North, square_a);
    link(&mut world, square_a, Direction::East, square_b);
    let (player, mut rx) = player_in(&mut world, temple);
    world.entity_mut(player).insert(mud_world::Account {
        user_id: "u".into(),
        character_id: "char-a".into(),
        role: mud_db::enums::UserRole::Player,
        account_role: mud_db::enums::UserRole::Player,
        perms: vec![],
    });
    let _ = drain(&mut rx);

    super::admin_world::cmd_where(&mut world, player, "corpse");
    assert!(drain(&mut rx).contains("no corpse"), "no corpse yet");

    world.spawn((
        mud_world::Item,
        Named {
            name: "the corpse of Tester".into(),
        },
        Located(square_b),
        mud_world::Corpse,
        mud_world::PlayerCorpse,
        crate::corpses::PlayerCorpseOwner("char-a".into()),
    ));
    super::admin_world::cmd_where(&mut world, player, "corpse");
    let out = drain(&mut rx);
    assert!(
        out.contains("Your corpse lies in Town Center  [30:12]."),
        "{out}"
    );
    assert!(
        out.contains("2 rooms away; head north."),
        "the id and the first step tell the three Town Centers apart: {out}"
    );

    // Standing on it, and one step away.
    world.entity_mut(player).insert(Located(square_b));
    super::admin_world::cmd_where(&mut world, player, "corpse");
    assert!(drain(&mut rx).contains("right here"));
    world.entity_mut(player).insert(Located(square_a));
    super::admin_world::cmd_where(&mut world, player, "corpse");
    assert!(drain(&mut rx).contains("1 room away; head east."));
}
