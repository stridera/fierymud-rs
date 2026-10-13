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

// --- playtest round 10: put/DROP triggers, script gives, junk -------------

fn object_trigger(zone: i32, id: i32, event: TriggerEvent, body: &str) -> TriggerDef {
    TriggerDef {
        attach_type: TriggerAttach::Object,
        ..lua_trigger(zone, id, event, body)
    }
}

fn plain_item(world: &mut World, name: &str, keyword: &str, key: (i32, i32), at: Entity) -> Entity {
    world
        .spawn((
            mud_world::Item,
            Named { name: name.into() },
            mud_world::Keywords(vec![keyword.into()]),
            mud_world::WorldKey {
                zone: key.0,
                id: key.1,
            },
            Located(at),
        ))
        .id()
}

/// Legacy `perform_put` fires `drop_otrigger(obj, ch, cont)` and
/// `drop_otrigger(cont, ch, obj)`: the Academy's `put stick bag` lesson (and the
/// Heavens Gate pedestal) listen for exactly that, reading `target`.
#[test]
fn put_fires_drop_triggers_on_the_item_and_the_container() {
    let (mut world, player, mut rx) = warrior_world();
    let mut protos = ObjectPrototypes::default();
    protos
        .by_key
        .insert((99, 18), object_proto(99, 18, ObjectType::Container));
    world.insert_resource(protos);
    let mut catalog = TriggerCatalog::default();
    catalog.by_key.insert(
        (99, 1),
        object_trigger(
            99,
            1,
            TriggerEvent::Drop,
            r#"if target and target.id == 18 then actor:send("stick sees bag") end"#,
        ),
    );
    catalog.by_key.insert(
        (99, 2),
        object_trigger(
            99,
            2,
            TriggerEvent::Drop,
            r#"actor:send("bag sees item " .. tostring(target.id))"#,
        ),
    );
    world.insert_resource(catalog);
    world.insert_resource(mud_script::LuaHost::new());
    let bag = plain_item(&mut world, "a leather bag", "bag", (99, 18), player);
    let stick = plain_item(&mut world, "a stick", "stick", (99, 17), player);
    world
        .entity_mut(stick)
        .insert(mud_world::AttachedTriggers(vec![(99, 1)]));
    world
        .entity_mut(bag)
        .insert(mud_world::AttachedTriggers(vec![(99, 2)]));

    super::info::cmd_put(&mut world, player, "stick bag");
    let out = drain(&mut rx);
    assert!(out.contains("You put a stick in a leather bag."), "{out}");
    assert!(out.contains("tick sees bag"), "{out}");
    assert!(out.contains("ag sees item 17"), "{out}");

    // A plain drop carries no `target`; scripts that test it stay quiet.
    world.entity_mut(stick).insert(Located(player));
    super::info::cmd_drop(&mut world, player, "stick");
    let out = drain(&mut rx);
    assert!(!out.contains("tick sees bag"), "{out}");
}

/// DG `mload obj` in a mob script (converted as `self.room:spawn_object`)
/// loads into the mob's inventory, so the `give` that follows can hand it
/// over; `tostring(actor)` is the bare name DG's `%actor%` expands to.
#[test]
fn mob_script_loads_an_object_and_gives_it_to_the_player() {
    let (mut world, player, mut rx) = warrior_world();
    let mut protos = ObjectPrototypes::default();
    let mut ration = object_proto(99, 5, ObjectType::Food);
    ration.name = "a ration".into();
    ration.keywords = vec!["ration".into()];
    protos.by_key.insert((99, 5), ration);
    let mut anvil = object_proto(99, 6, ObjectType::Other);
    anvil.name = "an anvil".into();
    anvil.keywords = vec!["anvil".into()];
    anvil.restrictions = vec![mud_db::enums::ObjectRestriction::NoTake];
    protos.by_key.insert((99, 6), anvil);
    world.insert_resource(protos);
    let mut catalog = TriggerCatalog::default();
    catalog.by_key.insert(
        (99, 1),
        lua_trigger(
            99,
            1,
            TriggerEvent::Speech,
            r#"
            self.room:spawn_object(99, 5)
            self:command("give ration " .. tostring(actor))
            self.room:spawn_object(99, 6)
            "#,
        ),
    );
    world.insert_resource(catalog);
    world.insert_resource(mud_script::LuaHost::new());
    let room = world.get::<Located>(player).unwrap().0;
    let mob = mob_with_trigger(&mut world, room, "the quartermaster", (99, 1));

    crate::triggers::fire_speech_in_room(&mut world, player, room, "hello");
    let out = drain(&mut rx);
    assert!(out.contains("gives you"), "{out}");

    let mut q = world.query::<(&Named, &Located)>();
    let find = |name: &str, q: &mut QueryState<(&Named, &Located)>, w: &World| {
        q.iter(w).find(|(n, _)| n.name == name).map(|(_, l)| l.0)
    };
    assert_eq!(find("a ration", &mut q, &world), Some(player));
    // A NO_TAKE object stays where `mload` could only put it: the floor.
    assert_eq!(find("an anvil", &mut q, &world), Some(room));
    assert_ne!(mob, player);
}

#[test]
fn junk_takes_all_dot_and_counts_like_drop() {
    let (mut world, player, mut rx) = warrior_world();
    let belt1 = plain_item(&mut world, "a small belt", "belt", (99, 1), player);
    let belt2 = plain_item(&mut world, "a small belt", "belt", (99, 1), player);
    let sword = plain_item(&mut world, "a sword", "sword", (99, 2), player);
    let rag1 = plain_item(&mut world, "a rag", "rag", (99, 3), player);
    let rag2 = plain_item(&mut world, "a rag", "rag", (99, 3), player);
    let rag3 = plain_item(&mut world, "a rag", "rag", (99, 3), player);

    super::info::cmd_junk(&mut world, player, "all.belt");
    assert!(drain(&mut rx).contains("You destroy a small belt."));
    assert!(world.get_entity(belt1).is_err() && world.get_entity(belt2).is_err());
    assert!(world.get_entity(sword).is_ok());

    super::info::cmd_junk(&mut world, player, "2 rag");
    let alive = [rag1, rag2, rag3]
        .iter()
        .filter(|e| world.get_entity(**e).is_ok())
        .count();
    assert_eq!(alive, 1);

    // Bare `junk all` is refused; a keyword nobody carries says so.
    super::info::cmd_junk(&mut world, player, "all");
    assert!(drain(&mut rx).contains("Go to the dump"));
    assert!(world.get_entity(sword).is_ok());
    super::info::cmd_junk(&mut world, player, "all.shield");
    assert!(drain(&mut rx).contains("You don't seem to have any shield."));
}

/// `read map` keeps the object's line breaks and the first line's indentation
/// (r10 #33: the Academy map came out as one 2,000-character line).
#[test]
fn read_keeps_the_layout_of_a_map() {
    let (mut world, player, mut rx) = warrior_world();
    let map = plain_item(&mut world, "a small map", "map", (30, 90), player);
    world.entity_mut(map).insert(mud_world::Description(
        "\n        Mielikki, by Kara\n\n   #   |  1: Temple\n   #   |  2: Inn\n".to_string(),
    ));
    super::info::cmd_read(&mut world, player, "map");
    // The wire layer turns bare newlines into CRLF.
    let out = drain(&mut rx).replace("\r\n", "\n");
    assert!(
        out.contains(":\n        Mielikki, by Kara\n\n   #   |  1: Temple\n   #   |  2: Inn\n"),
        "{out:?}"
    );
}

// --- put/drop: DROP fires before the move and can cancel it ---------------

fn drop_trigger_world(body: &str) -> (World, Entity, Rx, Entity, Entity) {
    let (mut world, player, rx) = warrior_world();
    let mut protos = ObjectPrototypes::default();
    protos
        .by_key
        .insert((99, 18), object_proto(99, 18, ObjectType::Container));
    world.insert_resource(protos);
    let mut catalog = TriggerCatalog::default();
    catalog
        .by_key
        .insert((99, 1), object_trigger(99, 1, TriggerEvent::Drop, body));
    world.insert_resource(catalog);
    world.insert_resource(mud_script::LuaHost::new());
    let bag = plain_item(&mut world, "a leather bag", "bag", (99, 18), player);
    let stick = plain_item(&mut world, "a stick", "stick", (99, 17), player);
    world
        .entity_mut(stick)
        .insert(mud_world::AttachedTriggers(vec![(99, 1)]));
    (world, player, rx, bag, stick)
}

/// Legacy `drop_otrigger` returns `script_driver(..) && obj`: a DROP trigger
/// that purges its item cancels the put, so nothing is put and nothing is
/// announced (the burning branch relied on this ordering, r10 #11).
#[test]
fn put_is_cancelled_when_the_drop_trigger_purges_the_item() {
    let (mut world, player, mut rx, bag, stick) = drop_trigger_world("world.destroy(self)");
    super::info::cmd_put(&mut world, player, "stick bag");
    let out = drain(&mut rx);
    assert!(!out.contains("You put"), "{out}");
    assert!(world.get_entity(stick).is_err(), "the trigger purged it");
    assert!(world.get_entity(bag).is_ok());
}

#[test]
fn drop_is_cancelled_when_the_drop_trigger_purges_the_item() {
    let (mut world, player, mut rx, _bag, stick) = drop_trigger_world("world.destroy(self)");
    super::info::cmd_drop(&mut world, player, "stick");
    let out = drain(&mut rx);
    assert!(!out.contains("You drop"), "{out}");
    assert!(world.get_entity(stick).is_err());
    // Same through `drop all`.
    let (mut world, player, mut rx, _bag, stick) = drop_trigger_world("world.destroy(self)");
    super::info::cmd_drop(&mut world, player, "all.stick");
    assert!(!drain(&mut rx).contains("You drop"));
    assert!(world.get_entity(stick).is_err());
}

/// A DROP trigger that does not purge anything lets the put and the drop
/// go through, and it has fired by the time they are announced.
#[test]
fn drop_trigger_that_leaves_the_item_alone_lets_put_and_drop_through() {
    let (mut world, player, mut rx, bag, stick) =
        drop_trigger_world(r#"actor:send("the stick twitches")"#);
    let room = world.get::<Located>(player).unwrap().0;
    super::info::cmd_put(&mut world, player, "stick bag");
    let out = drain(&mut rx);
    let twitch = out.find("twitches").expect(&out);
    assert!(twitch < out.find("You put").expect(&out), "{out}");
    assert_eq!(world.get::<Located>(stick).map(|l| l.0), Some(bag));
    world.entity_mut(stick).insert(Located(player));
    super::info::cmd_drop(&mut world, player, "stick");
    let out = drain(&mut rx);
    assert!(out.find("twitches").expect(&out) < out.find("You drop").expect(&out));
    assert_eq!(world.get::<Located>(stick).map(|l| l.0), Some(room));
}

/// `put all`: a trigger that destroys the container ends the loop; no item
/// is moved into a dead container.
#[test]
fn put_all_stops_when_a_trigger_destroys_the_container() {
    let (mut world, player, mut rx, bag, stick) = drop_trigger_world("world.destroy(target)");
    let rock = plain_item(&mut world, "a rock", "rock", (99, 19), player);
    super::info::cmd_put(&mut world, player, "all.stick bag");
    let out = drain(&mut rx);
    assert!(!out.contains("You put"), "{out}");
    assert!(world.get_entity(bag).is_err(), "the trigger destroyed it");
    assert_eq!(world.get::<Located>(stick).map(|l| l.0), Some(player));
    assert_eq!(world.get::<Located>(rock).map(|l| l.0), Some(player));
    let _ = rock;
}

/// A cancelled item in `put all` / `drop all` stays in hand while the rest go.
#[test]
fn put_all_skips_only_the_cancelled_item() {
    let (mut world, player, mut rx, bag, stick) = drop_trigger_world("world.destroy(self)");
    let rock = plain_item(&mut world, "a rock", "rock", (99, 19), player);
    super::info::cmd_put(&mut world, player, "all bag");
    let out = drain(&mut rx);
    assert!(out.contains("You put a rock in a leather bag."), "{out}");
    assert!(!out.contains("You put a stick"), "{out}");
    assert!(world.get_entity(stick).is_err());
    assert_eq!(world.get::<Located>(rock).map(|l| l.0), Some(bag));
}

/// Break burning branch (520:2): DROP, `wait(1)`, three embers, then the
/// branch purges itself (legacy `opurge burning-branch` matches the script's
/// own object by keyword, wherever it is). Put into a bag the branch is gone
/// after the wait and the embers do not pile up on repeated put / get.
#[test]
fn burning_branch_consumes_itself_whether_dropped_or_put_in_a_bag() {
    let (mut world, player, mut rx) = warrior_world();
    let mut protos = ObjectPrototypes::default();
    protos
        .by_key
        .insert((99, 18), object_proto(99, 18, ObjectType::Container));
    let mut ember = object_proto(520, 35, ObjectType::Other);
    ember.name = "an ember".into();
    ember.keywords = vec!["ember".into()];
    protos.by_key.insert((520, 35), ember);
    world.insert_resource(protos);
    let mut catalog = TriggerCatalog::default();
    catalog.by_key.insert(
        (520, 2),
        object_trigger(
            520,
            2,
            TriggerEvent::Drop,
            "wait(1)\n\
             self.room:send(\"The branch cracks and breaks into 3 pieces!\")\n\
             self.room:spawn_object(520, 35)\n\
             self.room:spawn_object(520, 35)\n\
             self.room:spawn_object(520, 35)\n\
             world.destroy(self)",
        ),
    );
    world.insert_resource(catalog);
    world.insert_resource(mud_script::LuaHost::new());
    let bag = plain_item(&mut world, "a leather bag", "bag", (99, 18), player);
    let branch_in = |world: &mut World, name: &str| {
        let branch = plain_item(world, name, "burning-branch", (520, 34), player);
        world
            .entity_mut(branch)
            .insert(mud_world::AttachedTriggers(vec![(520, 2)]));
        branch
    };
    let embers = |world: &mut World| {
        let mut q = world.query::<&Named>();
        q.iter(world).filter(|n| n.name == "an ember").count()
    };
    let mut tick = 0u64;
    let mut pass_time = |world: &mut World| {
        tick += 100_000;
        world.insert_resource(crate::TickCount(tick));
        crate::triggers::lua_coroutine_tick(world);
    };

    // Into the bag: the trigger ran first, the put went through, and a
    // second later the branch is consumed.
    let branch = branch_in(&mut world, "a burning branch");
    super::info::cmd_put(&mut world, player, "branch bag");
    assert_eq!(world.get::<Located>(branch).map(|l| l.0), Some(bag));
    pass_time(&mut world);
    assert!(drain(&mut rx).contains("cracks and breaks"));
    assert!(world.get_entity(branch).is_err(), "branch consumed");
    assert_eq!(embers(&mut world), 3);

    // A fresh branch dropped on the floor: three more, no leftovers.
    let branch = branch_in(&mut world, "a burning branch");
    super::info::cmd_drop(&mut world, player, "branch");
    pass_time(&mut world);
    assert!(world.get_entity(branch).is_err());
    assert_eq!(embers(&mut world), 6);
}
