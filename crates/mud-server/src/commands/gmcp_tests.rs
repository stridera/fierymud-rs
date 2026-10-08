//! GMCP payload shape and cadence: `Room.Info` ahead of the room text,
//! valid JSON for hostile strings, change-only prompt frames, and the
//! `Char.Effects` / `Char.Aggro` feeds.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Direction, ExitState, UserRole, effective_rank};
use mud_world::{
    Account, ExitData, Exits, Health, KnownAbilities, Located, Mob, Named, Online, Player, Profile,
    Room, WorldKey, WorldKeyIndex, Zone,
};
use serde_json::Value;

use super::test_support::{Rx, ability_def};
use super::{Connection, dispatch};
use crate::combat::{HateList, MobMemory};

/// Everything queued for the player, raw.
fn drain_bytes(rx: &mut Rx) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(b) = rx.try_recv() {
        out.extend(b);
    }
    out
}

/// GMCP frames (`IAC SB 201 <package> <json> IAC SE`) in wire order.
fn frames(bytes: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if bytes[i..i + 3] == [255, 250, 201] {
            let start = i + 3;
            let end = (start..bytes.len().saturating_sub(1))
                .find(|&j| bytes[j] == 255 && bytes[j + 1] == 240)
                .expect("unterminated GMCP frame");
            let body = String::from_utf8(bytes[start..end].to_vec()).expect("utf8 frame");
            let (pkg, json) = body.split_once(' ').unwrap_or((body.as_str(), ""));
            out.push((pkg.to_string(), json.to_string()));
            i = end + 2;
        } else {
            i += 1;
        }
    }
    out
}

fn of(frames: &[(String, String)], pkg: &str) -> Vec<String> {
    frames
        .iter()
        .filter(|(p, _)| p == pkg)
        .map(|(_, j)| j.clone())
        .collect()
}

struct Fx {
    world: World,
    a: Entity,
    b: Entity,
}

fn fixture() -> Fx {
    let mut world = World::new();
    world.insert_resource(mud_script::LuaHost::default());
    world.insert_resource(WorldKeyIndex::default());
    world.insert_resource(mud_world::WeatherCatalog::default());
    world.insert_resource(mud_world::AbilityCatalog::default());
    world.insert_resource(mud_world::EffectCatalog::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    world.insert_resource(mud_world::RuntimeConfig::default());
    let zone = world
        .spawn((
            Zone,
            WorldKey { zone: 550, id: 0 },
            Named {
                name: "Tech".into(),
            },
        ))
        .id();
    world
        .resource_mut::<WorldKeyIndex>()
        .zones
        .insert(550, zone);
    let room = |world: &mut World, id: i32, desc: &str| {
        let r = world
            .spawn((
                Room,
                WorldKey { zone: 550, id },
                Named {
                    name: format!("Room {id}"),
                },
                mud_world::Description(desc.into()),
                Located(zone),
                Exits::default(),
            ))
            .id();
        world
            .resource_mut::<WorldKeyIndex>()
            .rooms
            .insert((550, id), r);
        r
    };
    let a = room(
        &mut world,
        18,
        "<green>A \"quoted\" hall.</>\r\nSecond\tline \\ done.\r\n",
    );
    let b = room(&mut world, 22, "Another room.");
    Fx { world, a, b }
}

fn exit(to: Entity, state: ExitState, hidden: bool, kw: &[&str]) -> ExitData {
    ExitData {
        to: Some(to),
        state,
        key: None,
        description: None,
        keywords: kw.iter().map(|s| (*s).to_string()).collect(),
        is_hidden: hidden,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    }
}

fn player(world: &mut World, room: Entity, name: &str) -> (Entity, Rx) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);
    let e = world
        .spawn((
            Player,
            Online,
            Named { name: name.into() },
            Located(room),
            Connection(tx),
            Account {
                user_id: String::new(),
                character_id: format!("c-{name}"),
                role: effective_rank(20, UserRole::Player),
                account_role: UserRole::Player,
                perms: vec![],
            },
            Profile {
                level: 20,
                class_id: None,
                race: "Human".into(),
                experience: 0,
                gender: "neutral".into(),
            },
        ))
        .id();
    (e, rx)
}

fn look_frames(fx: &mut Fx, p: Entity, rx: &mut Rx) -> (String, Vec<(String, String)>) {
    drain_bytes(rx);
    dispatch(&mut fx.world, p, "look");
    let bytes = drain_bytes(rx);
    (String::from_utf8_lossy(&bytes).into_owned(), frames(&bytes))
}

#[test]
fn room_info_has_zone_area_desc_and_valid_json() {
    let mut fx = fixture();
    let (a, b) = (fx.a, fx.b);
    fx.world
        .get_mut::<Exits>(a)
        .unwrap()
        .0
        .insert(Direction::East, exit(b, ExitState::Open, false, &[]));
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    let (_, fr) = look_frames(&mut fx, p, &mut rx);
    let info = of(&fr, "Room.Info");
    assert_eq!(info.len(), 1, "{fr:?}");
    let v: Value = serde_json::from_str(&info[0]).expect("valid JSON");
    assert_eq!(v["zone"], "Tech");
    assert_eq!(v["area"], "Tech");
    assert_eq!(v["num"], 55_000_018);
    let desc = v["desc"].as_str().unwrap();
    assert!(desc.starts_with("A \"quoted\" hall.\r\nSecond\tline \\ done."));
    assert!(!desc.contains('<'), "colour tags stripped: {desc}");
    assert_eq!(v["exits"]["east"], 55_000_022);
    assert_eq!(v["exit_details"]["east"]["to"], 55_000_022);
    assert!(v["exit_details"]["east"].get("door").is_none());
}

#[test]
fn exit_details_describe_doors() {
    let mut fx = fixture();
    let (a, b) = (fx.a, fx.b);
    fx.world.get_mut::<Exits>(a).unwrap().0.insert(
        Direction::North,
        exit(b, ExitState::Locked, false, &["gate", "iron"]),
    );
    fx.world
        .get_mut::<Exits>(a)
        .unwrap()
        .0
        .insert(Direction::South, exit(b, ExitState::Open, false, &["arch"]));
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    let (_, fr) = look_frames(&mut fx, p, &mut rx);
    let v: Value = serde_json::from_str(&of(&fr, "Room.Info")[0]).unwrap();
    assert_eq!(v["doors"]["north"], "locked");
    assert_eq!(v["exit_details"]["north"]["door"], true);
    assert_eq!(v["exit_details"]["north"]["door_name"], "gate");
    assert_eq!(v["exit_details"]["south"]["door"], true);
    assert!(v["doors"].get("south").is_none(), "open door not in doors");
}

#[test]
fn room_info_precedes_room_text_and_movement_sends_it() {
    let mut fx = fixture();
    let (a, b) = (fx.a, fx.b);
    fx.world
        .get_mut::<Exits>(a)
        .unwrap()
        .0
        .insert(Direction::East, exit(b, ExitState::Open, false, &[]));
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    let (out, _) = look_frames(&mut fx, p, &mut rx);
    let info_at = out.find("Room.Info").expect("Room.Info sent by look");
    assert!(info_at < out.find("Room 18").unwrap_or(usize::MAX) || !out.contains("Room 18"));
    let text_at = out.find("Second").expect("room text printed");
    assert!(info_at < text_at, "Room.Info before room text");

    drain_bytes(&mut rx);
    dispatch(&mut fx.world, p, "east");
    let bytes = drain_bytes(&mut rx);
    let out = String::from_utf8_lossy(&bytes);
    let info_at = out.find("Room.Info").expect("move sends Room.Info");
    let text_at = out.find("Another room.").expect("new room text");
    assert!(info_at < text_at, "Room.Info before room text on move");
    let v: Value = serde_json::from_str(&of(&frames(&bytes), "Room.Info")[0]).unwrap();
    assert_eq!(v["num"], 55_000_022);
}

#[test]
fn dark_room_sends_empty_room_info() {
    let mut fx = fixture();
    let a = fx.a;
    fx.world
        .entity_mut(a)
        .insert(mud_world::RoomMagicalDarkness);
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    let (out, fr) = look_frames(&mut fx, p, &mut rx);
    assert!(out.contains("pitch black"), "{out}");
    assert_eq!(of(&fr, "Room.Info"), vec!["{}".to_string()]);
    // The prompt path must not leak the room either.
    drain_bytes(&mut rx);
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    assert!(of(&fr, "Room.Info").iter().all(|j| j == "{}"), "{fr:?}");
}

#[test]
fn hidden_exits_are_filtered_from_room_info() {
    let mut fx = fixture();
    let (a, b) = (fx.a, fx.b);
    fx.world.get_mut::<Exits>(a).unwrap().0.insert(
        Direction::East,
        exit(b, ExitState::Open, true, &["monolith"]),
    );
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    let (_, fr) = look_frames(&mut fx, p, &mut rx);
    let v: Value = serde_json::from_str(&of(&fr, "Room.Info")[0]).unwrap();
    assert!(v["exits"].get("east").is_none());
    assert!(v["exit_details"].get("east").is_none());

    dispatch(&mut fx.world, p, "search monolith");
    drain_bytes(&mut rx);
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    let info = of(&fr, "Room.Info");
    assert_eq!(info.len(), 1, "reveal triggers a fresh Room.Info");
    let v: Value = serde_json::from_str(&info[0]).unwrap();
    assert_eq!(v["exits"]["east"], 55_000_022);
}

#[test]
fn channel_text_with_control_chars_is_valid_json() {
    let mut fx = fixture();
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    super::send_comm_channel_text(
        &fx.world,
        p,
        "gossip",
        "Bo\u{1}b",
        "hi \u{7} \"q\" \\ tab\there\r\nnext\u{1b}",
    );
    let fr = frames(&drain_bytes(&mut rx));
    let v: Value = serde_json::from_str(&of(&fr, "Comm.Channel.Text")[0]).expect("valid JSON");
    assert_eq!(v["channel"], "gossip");
    assert!(v["text"].as_str().unwrap().contains("tab\there\r\nnext"));
}

#[test]
fn char_name_and_status_survive_hostile_names() {
    let mut fx = fixture();
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Se\"ek\u{1}er");
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    for pkg in [
        "Char.Name",
        "Char.Status",
        "Char.StatusVars",
        "Char.Effects",
    ] {
        let j = of(&fr, pkg);
        assert_eq!(j.len(), 1, "{pkg} sent once: {fr:?}");
        serde_json::from_str::<Value>(&j[0]).unwrap_or_else(|e| panic!("{pkg}: {e}: {}", j[0]));
    }
}

const ARMOR: i32 = 1;
const EFFECT: i32 = 10;

/// A world where `armor` is a real spell applying a `modify` effect.
fn world_with_armor() -> (Fx, Entity, Rx) {
    let mut fx = fixture();
    let mut catalog = mud_world::AbilityCatalog::default();
    let mut armor = ability_def(ARMOR, "Armor", AbilityKind::Spell);
    armor.cast_time_rounds = 0;
    catalog.by_name.insert("armor".to_string(), armor);
    catalog.effects_for.insert(
        ARMOR,
        vec![(
            EFFECT,
            Some(serde_json::json!({ "target": "ward", "amount": 10, "duration": 60 })),
        )],
    );
    fx.world.insert_resource(catalog);
    let mut effects = mud_world::EffectCatalog::default();
    effects.by_id.insert(
        EFFECT,
        mud_world::EffectDef {
            id: EFFECT,
            name: "ward".to_string(),
            description: None,
            effect_type: "modify".to_string(),
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
    fx.world.insert_resource(effects);
    fx.world
        .insert_resource(mud_world::SpellSlotData::default());
    fx.world
        .insert_resource(mud_world::ClassSkillsData::default());
    let a = fx.a;
    let (p, rx) = player(&mut fx.world, a, "Caster");
    fx.world.entity_mut(p).insert((
        Health { hp: 50, max: 50 },
        KnownAbilities {
            entries: vec![(ARMOR, 500, true)],
        },
    ));
    (fx, p, rx)
}

#[test]
fn char_effects_lists_a_cast_buff() {
    let (mut fx, p, mut rx) = world_with_armor();
    dispatch(&mut fx.world, p, "cast 'armor'");
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
    let said = String::from_utf8_lossy(&drain_bytes(&mut rx)).into_owned();
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    let eff = of(&fr, "Char.Effects");
    assert_eq!(eff.len(), 1, "{fr:?}");
    let v: Value = serde_json::from_str(&eff[0]).unwrap();
    let arr = v.as_array().unwrap();
    assert!(!arr.is_empty(), "cast output: {said}\nframes: {eff:?}");
    assert_eq!(arr[0]["ability"], "ARMOR");
    assert_eq!(arr[0]["source"], "spell");
}

#[test]
fn second_prompt_without_changes_sends_no_duplicate_frames() {
    let (mut fx, p, mut rx) = world_with_armor();
    dispatch(&mut fx.world, p, "cast 'armor'");
    for _ in 0..10 {
        crate::casting::casting_tick(&mut fx.world);
    }
    super::send_prompt(&mut fx.world, p);
    let first = frames(&drain_bytes(&mut rx));
    for pkg in [
        "Char.Name",
        "Char.StatusVars",
        "Char.Status",
        "Char.Effects",
        "Room.Info",
    ] {
        assert_eq!(of(&first, pkg).len(), 1, "first prompt sends {pkg}");
    }
    super::send_prompt(&mut fx.world, p);
    let second = frames(&drain_bytes(&mut rx));
    for pkg in [
        "Char.Name",
        "Char.StatusVars",
        "Char.Status",
        "Char.Effects",
        "Char.Aggro",
        "Room.Info",
    ] {
        assert!(of(&second, pkg).is_empty(), "{pkg} repeated: {second:?}");
    }
}

#[test]
fn renegotiation_resends_everything() {
    let mut fx = fixture();
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    super::send_prompt(&mut fx.world, p);
    let first = frames(&drain_bytes(&mut rx));
    super::send_prompt(&mut fx.world, p);
    assert!(of(&frames(&drain_bytes(&mut rx)), "Char.Status").is_empty());

    super::clear_gmcp_sent(&mut fx.world, p);
    super::send_prompt(&mut fx.world, p);
    let again = frames(&drain_bytes(&mut rx));
    for pkg in ["Char.Name", "Char.StatusVars", "Char.Status", "Room.Info"] {
        assert_eq!(of(&again, pkg), of(&first, pkg), "{pkg} resent");
    }
}

#[test]
fn look_always_sends_room_info_even_when_unchanged() {
    let mut fx = fixture();
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    super::send_prompt(&mut fx.world, p);
    drain_bytes(&mut rx);
    let (_, fr) = look_frames(&mut fx, p, &mut rx);
    assert_eq!(of(&fr, "Room.Info").len(), 1);
}

fn mob(world: &mut World, room: Entity, name: &str) -> Entity {
    world
        .spawn((Mob, Named { name: name.into() }, Located(room)))
        .id()
}

#[test]
fn aggro_lists_hating_mobs_and_ignores_unrelated_ones() {
    let mut fx = fixture();
    let (a, b) = (fx.a, fx.b);
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    let (other, _orx) = player(&mut fx.world, b, "Other");

    // Mobs in other rooms that do not hate the player change nothing.
    mob(&mut fx.world, b, "a bystander");
    let stranger = mob(&mut fx.world, b, "a stranger");
    fx.world.entity_mut(stranger).insert(HateList(vec![other]));
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    let aggro = of(&fr, "Char.Aggro");
    let v: Value = serde_json::from_str(&aggro[0]).unwrap();
    assert_eq!(v["hating"], serde_json::json!([]));
    assert_eq!(v["remembering"], serde_json::json!([]));

    // A mob that hates the player (here) and one that remembers (elsewhere).
    let wolf = mob(&mut fx.world, a, "a wolf");
    fx.world.entity_mut(wolf).insert(HateList(vec![p]));
    let bear = mob(&mut fx.world, b, "a bear");
    fx.world
        .entity_mut(bear)
        .insert(MobMemory([p].into_iter().collect()));
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    let v: Value = serde_json::from_str(&of(&fr, "Char.Aggro")[0]).unwrap();
    assert_eq!(v["hating"], serde_json::json!(["a wolf"]));
    assert_eq!(v["remembering"], serde_json::json!(["a bear"]));

    // Unchanged: no repeat.
    super::send_prompt(&mut fx.world, p);
    assert!(of(&frames(&drain_bytes(&mut rx)), "Char.Aggro").is_empty());
}

const DUAL_WIELD: i32 = 20;
const KICK: i32 = 21;
const PER_PROMPT: [&str; 6] = [
    "Char.Vitals",
    "Group",
    "Char.Combat",
    "Room.Mobs",
    "Room.Services",
    "Char.Skills",
];

/// A player with vitals and two known skills: a passive one (tagged in
/// the ability data) and an active one.
fn world_with_skills() -> (Fx, Entity, Rx) {
    let mut fx = fixture();
    let mut catalog = mud_world::AbilityCatalog::default();
    let mut dual = ability_def(DUAL_WIELD, "Dual Wield", AbilityKind::Skill);
    dual.passive = true;
    catalog.by_name.insert("dual wield".to_string(), dual);
    catalog.by_name.insert(
        "kick".to_string(),
        ability_def(KICK, "Kick", AbilityKind::Skill),
    );
    fx.world.insert_resource(catalog);
    let a = fx.a;
    let (p, rx) = player(&mut fx.world, a, "Kicker");
    fx.world.entity_mut(p).insert((
        Health { hp: 50, max: 50 },
        mud_world::Stamina {
            current: 30,
            max: 30,
        },
        KnownAbilities {
            entries: vec![(DUAL_WIELD, 500, true), (KICK, 500, true)],
        },
    ));
    (fx, p, rx)
}

#[test]
fn char_skills_marks_dual_wield_passive_and_kick_active() {
    let (mut fx, p, mut rx) = world_with_skills();
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    let skills = of(&fr, "Char.Skills");
    assert_eq!(skills.len(), 1, "{fr:?}");
    let v: Value = serde_json::from_str(&skills[0]).unwrap();
    let by_name = |n: &str| {
        v["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == n)
            .unwrap_or_else(|| panic!("{n} missing: {v}"))
            .clone()
    };
    assert_eq!(by_name("DUAL WIELD")["passive"], true);
    assert_eq!(by_name("KICK")["passive"], false);
}

#[test]
fn unchanged_second_prompt_sends_no_per_prompt_panels() {
    let (mut fx, p, mut rx) = world_with_skills();
    super::send_prompt(&mut fx.world, p);
    let first = frames(&drain_bytes(&mut rx));
    for pkg in PER_PROMPT {
        assert_eq!(of(&first, pkg).len(), 1, "first prompt sends {pkg}");
    }
    super::send_prompt(&mut fx.world, p);
    let second = frames(&drain_bytes(&mut rx));
    for pkg in PER_PROMPT {
        assert!(of(&second, pkg).is_empty(), "{pkg} repeated: {second:?}");
    }
}

#[test]
fn hp_change_sends_only_vitals() {
    let (mut fx, p, mut rx) = world_with_skills();
    super::send_prompt(&mut fx.world, p);
    drain_bytes(&mut rx);
    fx.world.get_mut::<Health>(p).unwrap().hp = 41;
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    let vitals = of(&fr, "Char.Vitals");
    assert_eq!(vitals.len(), 1, "{fr:?}");
    assert!(vitals[0].contains("\"hp\":41"), "{}", vitals[0]);
    for pkg in PER_PROMPT.iter().filter(|p| **p != "Char.Vitals") {
        assert!(of(&fr, pkg).is_empty(), "{pkg} sent on HP change: {fr:?}");
    }
}

#[test]
fn renegotiation_resends_per_prompt_panels() {
    let (mut fx, p, mut rx) = world_with_skills();
    super::send_prompt(&mut fx.world, p);
    drain_bytes(&mut rx);
    super::clear_gmcp_sent(&mut fx.world, p);
    super::send_prompt(&mut fx.world, p);
    let again = frames(&drain_bytes(&mut rx));
    for pkg in PER_PROMPT {
        assert_eq!(of(&again, pkg).len(), 1, "{pkg} resent after renegotiation");
    }
}

#[test]
fn look_forces_room_mobs_even_when_unchanged() {
    let (mut fx, p, mut rx) = world_with_skills();
    super::send_prompt(&mut fx.world, p);
    drain_bytes(&mut rx);
    let (_, fr) = look_frames(&mut fx, p, &mut rx);
    assert_eq!(of(&fr, "Room.Info").len(), 1, "{fr:?}");
    assert_eq!(of(&fr, "Room.Mobs").len(), 1, "{fr:?}");
}

#[test]
fn combat_and_room_mobs_resend_when_they_change() {
    let (mut fx, p, mut rx) = world_with_skills();
    let a = fx.a;
    super::send_prompt(&mut fx.world, p);
    drain_bytes(&mut rx);
    let wolf = mob(&mut fx.world, a, "a wolf");
    fx.world
        .entity_mut(wolf)
        .insert(mud_world::Health { hp: 10, max: 10 });
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    assert_eq!(of(&fr, "Room.Mobs").len(), 1, "{fr:?}");
    assert!(of(&fr, "Char.Combat").is_empty(), "{fr:?}");
    fx.world.entity_mut(p).insert(mud_world::Fighting(wolf));
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    assert_eq!(of(&fr, "Char.Combat").len(), 1, "{fr:?}");
}
