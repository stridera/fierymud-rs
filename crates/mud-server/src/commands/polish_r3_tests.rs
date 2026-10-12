//! Round-3 playtest polish: one usage line in `help`, the `who` legend,
//! one word for the stamina resource, and the list commands `areas`,
//! `levels`, `wizlist`, `followers` and `pets`.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::{UserRole, effective_rank};
use mud_world::{
    AbilityCatalog, Account, ClassCatalog, ClassDef, Follower, GodZone, GroupMember, Health,
    HelpCatalog, HelpEntry, LevelRow, LevelTable, Located, Mob, Named, Online, Player, Profile,
    Room, SocialRegistry, WorldKeyIndex, Zone,
};

use super::lists::{StaffRow, render_wizlist};
use super::test_support::{Rx, drain};
use super::{ColorMode, Connection, dispatch, render_color_tags};

const WARRIOR: i32 = 1;

fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn class(id: i32, name: &str, factor: f64) -> ClassDef {
    ClassDef {
        id,
        name: name.to_string(),
        plain_name: name.to_string(),
        is_subclass: false,
        parent_class_id: None,
        description: None,
        hit_dice: "1d8".to_string(),
        primary_stat: None,
        hp_per_level: 0,
        exp_gain_factor: factor,
        alignment_bias: 0,
        campcraft_bonus: false,
        resistances: HashMap::new(),
    }
}

fn world() -> World {
    let mut world = World::new();
    world.insert_resource(ClassCatalog::default());
    world.insert_resource(SocialRegistry::default());
    world.insert_resource(AbilityCatalog::default());
    world.insert_resource(HelpCatalog::default());
    world.insert_resource(WorldKeyIndex::default());
    world
}

fn player(world: &mut World, name: &str, level: i32, class_id: Option<i32>) -> (Entity, Rx) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let e = world
        .spawn((
            Player,
            Online,
            Named {
                name: name.to_string(),
            },
            Connection(tx),
            Account {
                user_id: String::new(),
                character_id: format!("c-{name}"),
                role: effective_rank(level, UserRole::Player),
                account_role: UserRole::Player,
                perms: vec![],
            },
            Profile {
                level,
                class_id,
                race: "Human".into(),
                experience: 0,
                gender: "neutral".into(),
            },
        ))
        .id();
    (e, rx)
}

fn help_entry(keyword: &str, content: &str, usage: Option<&str>) -> HelpEntry {
    HelpEntry {
        id: 1,
        title: keyword.to_string(),
        content: content.to_string(),
        min_level: 0,
        category: Some("command".into()),
        usage: usage.map(str::to_string),
        duration: None,
        sphere: None,
        keywords: vec![keyword.to_string()],
    }
}

fn install_help(world: &mut World, e: HelpEntry) {
    let mut catalog = HelpCatalog::default();
    for k in &e.keywords {
        catalog
            .by_keyword
            .entry(k.to_ascii_lowercase())
            .or_default()
            .push(e.id);
    }
    catalog.entries.insert(e.id, e);
    world.insert_resource(catalog);
}

fn usage_lines(out: &str) -> Vec<&str> {
    out.lines()
        .filter(|l| l.trim_start().to_ascii_lowercase().starts_with("usage"))
        .collect()
}

// ---------------------------------------------------------------- help

const KICK_BOX: &str = "================================================\n\
Usage      : kick <victim>\n\
Aggressive : yes\n\
================================================\n\
\n\
KICK is one of the most basic actions.";

#[test]
fn help_topic_with_a_header_box_prints_usage_once() {
    let mut world = world();
    install_help(
        &mut world,
        help_entry("kick", KICK_BOX, Some("kick <victim>")),
    );
    let (p, mut rx) = player(&mut world, "Reader", 10, None);
    dispatch(&mut world, p, "help kick");
    let out = plain(&drain(&mut rx));
    assert_eq!(usage_lines(&out).len(), 1, "{out}");
    assert!(!out.contains("Command usage"), "{out}");
    assert!(out.contains("Usage      : kick <victim>"), "{out}");
}

#[test]
fn help_topic_without_a_box_row_shows_the_usage_column_once() {
    let mut world = world();
    install_help(
        &mut world,
        help_entry("bash", "Knocks a foe down.", Some("bash [<victim>]")),
    );
    let (p, mut rx) = player(&mut world, "Reader", 10, None);
    dispatch(&mut world, p, "help bash");
    let out = plain(&drain(&mut rx));
    assert_eq!(usage_lines(&out).len(), 1, "{out}");
    assert!(out.contains("Usage: bash [<victim>]"), "{out}");
    assert!(!out.contains("Command usage"), "{out}");
}

#[test]
fn help_topic_without_any_usage_falls_back_to_the_command() {
    let mut world = world();
    install_help(&mut world, help_entry("kick", "Kicks things.", None));
    let (p, mut rx) = player(&mut world, "Reader", 10, None);
    dispatch(&mut world, p, "help kick");
    let out = plain(&drain(&mut rx));
    assert_eq!(usage_lines(&out).len(), 1, "{out}");
    assert!(out.contains("Usage: kick [<victim>]"), "{out}");
}

#[test]
fn spell_box_sphere_and_duration_are_not_repeated_in_the_header() {
    let mut world = world();
    let mut e = help_entry(
        "hush",
        "====\nUsage         : cast 'hush'\nSphere        : fire\nDuration      : 2 hours\n====\nBody.",
        Some("cast 'hush'"),
    );
    e.sphere = Some("fire".into());
    e.duration = Some("2 hours".into());
    install_help(&mut world, e);
    let (p, mut rx) = player(&mut world, "Reader", 10, None);
    dispatch(&mut world, p, "help hush");
    let out = plain(&drain(&mut rx));
    assert_eq!(out.matches("Sphere").count(), 1, "{out}");
    assert_eq!(out.matches("Duration").count(), 1, "{out}");
}

// ----------------------------------------------------- stamina wording

#[test]
fn prompt_help_uses_st_not_mv() {
    let mut world = world();
    let (p, mut rx) = player(&mut world, "Reader", 10, None);
    dispatch(&mut world, p, "help prompt");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("%v/%V st"), "{out}");
    assert!(!out.contains(" mv"), "{out}");
}

#[test]
fn report_says_hp_and_stamina() {
    let mut world = world();
    let room = world.spawn_empty().id();
    let (p, mut rx) = player(&mut world, "Reader", 10, None);
    world.entity_mut(p).insert((
        Located(room),
        Health { hp: 80, max: 100 },
        mud_world::Stamina {
            current: 40,
            max: 50,
        },
    ));
    dispatch(&mut world, p, "report");
    let out = plain(&drain(&mut rx));
    assert!(
        out.contains("I have 80 (100) hit and 40 (50) movement points"),
        "{out}"
    );
}

// ----------------------------------------------------------- who legend

#[test]
fn who_legend_explains_only_the_markers_shown() {
    let mut world = world();
    let (viewer, mut rx) = player(&mut world, "Viewer", 10, None);
    let (mate, _m) = player(&mut world, "Mate", 11, None);
    let (_god, _g) = player(&mut world, "Zeus", 105, None);
    // A group of two puts [G] on both rows.
    world.entity_mut(mate).insert(GroupMember(viewer));
    dispatch(&mut world, viewer, "who");
    let out = plain(&drain(&mut rx));
    let legend = out
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .to_string();
    assert!(legend.contains("[G] in a group"), "{out}");
    assert!(legend.contains("staff"), "{out}");
    assert!(!legend.contains("AFK"), "{out}");
    assert!(!legend.contains("idle"), "{out}");
}

#[test]
fn who_has_no_legend_when_no_marker_appears() {
    let mut world = world();
    let (viewer, mut rx) = player(&mut world, "Viewer", 10, None);
    dispatch(&mut world, viewer, "who");
    let out = plain(&drain(&mut rx));
    assert!(!out.contains("in a group"), "{out}");
    assert!(!out.contains("away from keyboard"), "{out}");
}

// ---------------------------------------------------------------- areas

fn add_zone(world: &mut World, id: i32, name: &str, rooms: usize, god: bool) {
    let zone = world
        .spawn((
            Zone,
            Named {
                name: name.to_string(),
            },
        ))
        .id();
    if god {
        world.entity_mut(zone).insert(GodZone);
    }
    for _ in 0..rooms {
        world.spawn((Room, Located(zone)));
    }
    world.resource_mut::<WorldKeyIndex>().zones.insert(id, zone);
}

fn areas_world() -> World {
    let mut world = world();
    add_zone(&mut world, 1, "Reserved - create food items", 1, false);
    add_zone(&mut world, 4, "imm quest items zone", 3, true);
    add_zone(&mut world, 5, "Legacy Zone 5 (Quest Only)", 0, false);
    add_zone(&mut world, 30, "Mielikki", 4, false);
    add_zone(&mut world, 23, "South Caelia Forests", 2, false);
    world
}

#[test]
fn areas_lists_player_zones_and_hides_god_internal_and_empty_ones() {
    let mut world = areas_world();
    let (p, mut rx) = player(&mut world, "Reader", 10, None);
    dispatch(&mut world, p, "areas");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("Areas of the realm (2)"), "{out}");
    assert!(out.contains("[ 30] Mielikki"), "{out}");
    assert!(out.contains("[ 23] South Caelia Forests"), "{out}");
    assert!(
        out.find("[ 23]") < out.find("[ 30]"),
        "sorted by zone id: {out}"
    );
    for hidden in ["Reserved", "imm quest", "Legacy Zone 5"] {
        assert!(!out.contains(hidden), "{hidden} leaked: {out}");
    }
}

#[test]
fn areas_filters_by_name_text() {
    let mut world = areas_world();
    let (p, mut rx) = player(&mut world, "Reader", 10, None);
    dispatch(&mut world, p, "areas forest");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("South Caelia Forests"), "{out}");
    assert!(!out.contains("Mielikki"), "{out}");
    dispatch(&mut world, p, "areas nowhere");
    assert!(plain(&drain(&mut rx)).contains("No area matches 'nowhere'"));
}

// --------------------------------------------------------------- levels

fn levels_world(factor: f64) -> World {
    let mut world = world();
    world.insert_resource(LevelTable {
        rows: (1..=105)
            .map(|level| LevelRow {
                level,
                name: None,
                exp_required: if level <= 100 {
                    (level - 1) * 1000
                } else {
                    300_000_000 + level
                },
                hp_gain: 1,
                stamina_gain: 1,
                is_immortal: level >= 100,
                permissions: vec![],
            })
            .collect(),
    });
    let mut catalog = ClassCatalog::default();
    catalog
        .by_id
        .insert(WARRIOR, class(WARRIOR, "Warrior", factor));
    world.insert_resource(catalog);
    world
}

#[test]
fn levels_prints_the_class_scaled_table() {
    let mut world = levels_world(1.5);
    let (p, mut rx) = player(&mut world, "Reader", 3, Some(WARRIOR));
    world.get_mut::<Profile>(p).unwrap().experience = 2_500;
    dispatch(&mut world, p, "levels");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("(Warrior)"), "{out}");
    // Level 2 costs 1000 * 1.5, level 99 costs 98000 * 1.5.
    assert!(out.contains("[ 2]        1,500"), "{out}");
    assert!(out.contains("[99]      147,000"), "{out}");
    assert!(!out.contains("[ 1]"), "level 1 costs nothing: {out}");
    // Level 4 is 3000 * 1.5 = 4500; 2000 more from 2500.
    assert!(
        out.contains("You are level 3, with 2,500 experience; 2,000 more for level 4."),
        "{out}"
    );
    assert!(out.lines().all(|l| l.len() <= 79), "{out}");
}

#[test]
fn levels_for_staff_says_experience_is_meaningless() {
    let mut world = levels_world(1.0);
    let (p, mut rx) = player(&mut world, "God", 105, Some(WARRIOR));
    dispatch(&mut world, p, "levels");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("Experience has no meaning for you."), "{out}");
}

// ------------------------------------------------------------- wizlist

fn staff(name: &str, level: i32, role: UserRole) -> StaffRow {
    StaffRow {
        name: name.to_string(),
        level,
        role,
    }
}

#[test]
fn wizlist_groups_by_role_highest_first_and_marks_online() {
    let rows = vec![
        staff("Bob", 102, UserRole::Builder),
        staff("Zed", 105, UserRole::Implementor),
        staff("Ann", 105, UserRole::Implementor),
        staff("Cy", 104, UserRole::Coder),
        staff("Imm", 100, UserRole::Immortal),
    ];
    let out = render_color_tags(
        &render_wizlist(&rows, &["ann".to_string()], 80),
        ColorMode::Strip,
    );
    let pos = |needle: &str| {
        out.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {out}"))
    };
    assert!(
        pos("Implementors") < pos("Coders")
            && pos("Coders") < pos("Builders")
            && pos("Builders") < pos("Immortals"),
        "{out}"
    );
    assert!(out.contains("Ann*"), "{out}");
    assert!(!out.contains("Zed*"), "{out}");
    assert!(pos("Ann") < pos("Zed"), "same level sorts by name: {out}");
    assert!(out.contains("* = online now"), "{out}");
}

#[test]
fn wizlist_with_no_staff_says_so() {
    let out = render_color_tags(&render_wizlist(&[], &[], 80), ColorMode::Strip);
    assert!(out.contains("empty"), "{out}");
}

// ----------------------------------------------------- followers, pets

fn follower_mob(world: &mut World, name: &str, leader: Entity, room: Entity) -> Entity {
    world
        .spawn((
            Mob,
            Named {
                name: name.to_string(),
            },
            Follower(leader),
            Located(room),
            Health { hp: 7, max: 10 },
        ))
        .id()
}

#[test]
fn pets_lists_creatures_following_you_and_where_they_are() {
    let mut world = world();
    let here = world
        .spawn(Named {
            name: "The Plaza".into(),
        })
        .id();
    let elsewhere = world
        .spawn(Named {
            name: "A Dark Alley".into(),
        })
        .id();
    let (p, mut rx) = player(&mut world, "Reader", 10, None);
    world.entity_mut(p).insert(Located(here));
    let (mate, _m) = player(&mut world, "Mate", 10, None);
    world.entity_mut(mate).insert((Follower(p), Located(here)));
    follower_mob(&mut world, "a kitten", p, here);
    follower_mob(&mut world, "a donkey", p, elsewhere);
    let stranger = world.spawn_empty().id();
    follower_mob(&mut world, "someone else's dog", stranger, here);
    dispatch(&mut world, p, "pets");
    let out = plain(&drain(&mut rx));
    let kitten = out.lines().find(|l| l.contains("kitten")).expect("kitten");
    assert!(
        kitten.contains("A kitten") && kitten.contains("here") && kitten.contains("7/10 hp"),
        "{out}"
    );
    let donkey = out.lines().find(|l| l.contains("donkey")).expect("donkey");
    assert!(donkey.contains("A Dark Alley"), "{out}");
    assert!(!out.contains("Mate"), "players are not pets: {out}");
    assert!(!out.contains("dog"), "{out}");
}

#[test]
fn followers_includes_players_and_pets() {
    let mut world = world();
    let here = world
        .spawn(Named {
            name: "The Plaza".into(),
        })
        .id();
    let (p, mut rx) = player(&mut world, "Reader", 10, None);
    world.entity_mut(p).insert(Located(here));
    let (mate, _m) = player(&mut world, "Mate", 10, None);
    world.entity_mut(mate).insert((Follower(p), Located(here)));
    follower_mob(&mut world, "a kitten", p, here);
    dispatch(&mut world, p, "followers");
    let out = plain(&drain(&mut rx));
    assert!(out.contains("Following you"), "{out}");
    assert!(out.contains("Mate") && out.contains("A kitten"), "{out}");
}

#[test]
fn followers_and_pets_say_so_when_empty() {
    let mut world = world();
    let (p, mut rx) = player(&mut world, "Reader", 10, None);
    dispatch(&mut world, p, "followers");
    assert!(plain(&drain(&mut rx)).contains("Nobody is following you."));
    dispatch(&mut world, p, "pets");
    assert!(plain(&drain(&mut rx)).contains("no pets or hirelings"));
}

#[test]
fn the_new_commands_are_registered() {
    for name in ["areas", "levels", "wizlist", "followers", "pets"] {
        assert!(
            super::REGISTRY.contains_key(name),
            "{name} is not a registered command"
        );
    }
}

/// The wizlist query runs against the real schema and only returns staff.
/// Skipped when the dev database is unreachable.
#[tokio::test]
async fn wizlist_query_runs_and_returns_only_staff() {
    use super::test_support::{db_test_lock, db_test_pool_settings};
    let _lock = db_test_lock().await;
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
    let Ok(Ok(pool)) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        mud_db::connect_with(&url, db_test_pool_settings()),
    )
    .await
    else {
        eprintln!("skipped: dev database unreachable");
        return;
    };
    let rows = super::lists::staff_rows(&pool).await.expect("staff query");
    assert!(rows.iter().all(|r| r.role != UserRole::Player), "{rows:?}");
    assert!(
        rows.iter()
            .all(|r| r.role.rank() >= UserRole::from_level(r.level).rank()),
        "{rows:?}"
    );
}

#[test]
fn a_pet_ordered_to_report_includes_its_movement_pool() {
    let mut world = world();
    let room = world.spawn_empty().id();
    let (owner, mut rx) = player(&mut world, "Owner", 10, None);
    world.entity_mut(owner).insert(Located(room));
    let mare = world
        .spawn((
            mud_world::Mob,
            mud_world::Named {
                name: "a stout mare".into(),
            },
            mud_world::Keywords(vec!["mare".into()]),
            Located(room),
            mud_world::Follower(owner),
            Health { hp: 30, max: 40 },
            mud_world::MovementPoints {
                current: 90,
                max: 120,
            },
        ))
        .id();
    let _ = mare;
    dispatch(&mut world, owner, "order mare report");
    let out = plain(&drain(&mut rx));
    assert!(
        out.contains("I have 30 (40) hit and 90 (120) movement points"),
        "{out}"
    );
}
