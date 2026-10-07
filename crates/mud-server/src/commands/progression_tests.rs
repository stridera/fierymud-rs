//! Tests for XP -> level progression: the class-scaled XP table, the
//! per-tick level sweep that every XP source relies on, the mortal cap,
//! the `**` display, and the `level` readout.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::{MAX_MORTAL_LEVEL, UserRole, effective_rank};
use mud_world::{
    Account, ClassCatalog, ClassDef, LevelRow, LevelTable, Named, Online, Player, Profile,
};

use super::test_support::{Rx, drain};
use super::{Connection, PendingPlayerUpdate, PlayerUpdateInbox, dispatch, drain_player_updates};
use crate::combat::level_sweep_tick;
use crate::commands::info::{LevelReport, render_level_report};

const NECRO: i32 = 7;
const CLERIC: i32 = 2;

/// Class-neutral table: reaching level L costs `(L - 1) * 1000`; level 100
/// (the `**` threshold) costs `99_000`; staff rows follow. Class factors
/// scale it (necromancer 1.3, cleric 1.0).
fn table() -> LevelTable {
    LevelTable {
        rows: (1..=105)
            .map(|level| LevelRow {
                level,
                name: (level >= 100).then(|| format!("Staff {level}")),
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
    }
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
        resistances: HashMap::new(),
    }
}

fn world() -> World {
    let mut world = World::new();
    world.insert_resource(table());
    let mut catalog = ClassCatalog::default();
    catalog
        .by_id
        .insert(NECRO, class(NECRO, "<magenta>Necromancer</>", 1.3));
    catalog
        .by_id
        .insert(CLERIC, class(CLERIC, "<cyan>Cleric</>", 1.0));
    world.insert_resource(catalog);
    world
}

fn spawn(world: &mut World, name: &str, level: i32, xp: i32, class_id: i32) -> (Entity, Rx) {
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
                class_id: Some(class_id),
                race: "Human".into(),
                experience: xp,
                gender: "neutral".into(),
            },
        ))
        .id();
    (e, rx)
}

fn profile(world: &World, e: Entity) -> &Profile {
    world.get::<Profile>(e).unwrap()
}

fn strip_ansi(s: &str) -> String {
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

#[test]
fn scale_exp_matches_legacy_exp_next_level() {
    // Values from the seeded legacy table (`exp_table[level - 1]`) times
    // the class factor, truncated like `(long)(exp * factor)`.
    assert_eq!(mud_world::scale_exp(5_500, 2, 1.2), 6_600);
    assert_eq!(mud_world::scale_exp(254_500, 10, 1.3), 330_850);
    assert_eq!(mud_world::scale_exp(67_772_000, 85, 1.3), 88_103_600);
    assert_eq!(mud_world::scale_exp(105_806_000, 100, 1.3), 137_547_800);
    assert_eq!(mud_world::scale_exp(105_806_000, 100, 1.0), 105_806_000);
    // Staff levels (>= 101) ignore the class factor.
    assert_eq!(mud_world::scale_exp(299_999_999, 101, 1.3), 299_999_999);
}

#[test]
fn class_factor_scales_thresholds_for_level_up() {
    let mut world = world();
    let (cleric, _c) = spawn(&mut world, "Cleric", 1, 2_500, CLERIC);
    let (necro, _n) = spawn(&mut world, "Necro", 1, 2_500, NECRO);
    level_sweep_tick(&mut world);
    // Cleric: level 3 costs 2000. Necromancer: 2600 -> stays at 2.
    assert_eq!(profile(&world, cleric).level, 3);
    assert_eq!(profile(&world, necro).level, 2);
    world.get_mut::<Profile>(necro).unwrap().experience = 2_600;
    level_sweep_tick(&mut world);
    assert_eq!(profile(&world, necro).level, 3);
}

#[test]
fn quest_xp_levels_the_character_up() {
    let mut world = world();
    let (player, mut rx) = spawn(&mut world, "Quester", 1, 0, CLERIC);
    let (tx, inbox_rx) = tokio::sync::mpsc::channel(8);
    world.insert_resource(PlayerUpdateInbox(std::sync::Mutex::new(inbox_rx)));
    tx.try_send(PendingPlayerUpdate::ExperienceDelta {
        character_id: "c-Quester".into(),
        amount: 1_500,
    })
    .unwrap();
    drain_player_updates(&mut world);
    level_sweep_tick(&mut world);
    assert_eq!(profile(&world, player).level, 2);
    assert!(drain(&mut rx).contains("advanced to level 2"));
}

#[test]
fn lua_award_exp_levels_the_character_up() {
    let mut world = world();
    let (player, _rx) = spawn(&mut world, "Scripted", 1, 0, CLERIC);
    let mut host = mud_script::LuaHost::default();
    host.exec_for_actor(&mut world, player, "actor:award_exp(3500)")
        .expect("lua");
    assert_eq!(profile(&world, player).level, 1, "not before the sweep");
    level_sweep_tick(&mut world);
    assert_eq!(profile(&world, player).level, 4);
}

#[test]
fn xp_levels_cap_at_99_and_xp_caps_at_starstar() {
    let mut world = world();
    let (player, _rx) = spawn(&mut world, "Greedy", 98, 0, CLERIC);
    world
        .get_mut::<Profile>(player)
        .unwrap()
        .grant_experience(i32::MAX / 2);
    level_sweep_tick(&mut world);
    let p = profile(&world, player);
    assert_eq!(p.level, MAX_MORTAL_LEVEL);
    // Legacy: mortal XP caps at (XP to reach 100) - 1.
    assert_eq!(p.experience, 99_000 - 1);
    assert!(mud_world::is_starstar(&world, p));
    // Idempotent.
    level_sweep_tick(&mut world);
    assert_eq!(profile(&world, player).level, MAX_MORTAL_LEVEL);
    assert_eq!(profile(&world, player).experience, 98_999);
}

#[test]
fn staff_are_untouched_by_the_sweep() {
    let mut world = world();
    let (god, _rx) = spawn(&mut world, "God", 105, 5, CLERIC);
    level_sweep_tick(&mut world);
    assert_eq!(profile(&world, god).level, 105);
    assert_eq!(profile(&world, god).experience, 5);
}

#[test]
fn starstar_requires_level_99_and_the_cap_xp() {
    let mut world = world();
    // Necromancer: ** threshold is 99_000 * 1.3 - 1.
    let cap = 128_700 - 1;
    let (at_cap, _a) = spawn(&mut world, "AtCap", 99, cap, NECRO);
    let (below, _b) = spawn(&mut world, "Below", 99, cap - 1, NECRO);
    let (lower, _l) = spawn(&mut world, "Lower", 98, cap, NECRO);
    let (god, _g) = spawn(&mut world, "God", 100, cap, NECRO);
    assert!(mud_world::is_starstar(&world, profile(&world, at_cap)));
    assert!(!mud_world::is_starstar(&world, profile(&world, below)));
    assert!(!mud_world::is_starstar(&world, profile(&world, lower)));
    assert!(!mud_world::is_starstar(&world, profile(&world, god)));
}

#[test]
fn stars_use_the_class_lead_color_like_legacy() {
    assert_eq!(
        mud_world::stars_for_name("<b:magenta>Sorcerer</>"),
        "<b:magenta>**</>"
    );
    assert_eq!(
        mud_world::stars_for_name("<b:red>Anti-</><b:black>Paladin</>"),
        "<b:red>**</>"
    );
    assert_eq!(mud_world::stars_for_name("Paladin"), "**");
}

#[test]
fn who_shows_stars_in_place_of_the_level() {
    let mut world = world();
    let (viewer, mut rx) = spawn(&mut world, "Viewer", 10, 0, CLERIC);
    let (_star, _s) = spawn(&mut world, "Cerworn", 99, 128_699, NECRO);
    let (_plain, _p) = spawn(&mut world, "Nearly", 99, 1_000, NECRO);
    dispatch(&mut world, viewer, "who");
    let out = strip_ansi(&drain(&mut rx));
    let cerworn = out.lines().find(|l| l.contains("Cerworn")).expect("row");
    assert!(cerworn.contains("[L **]"), "{cerworn}");
    assert!(!cerworn.contains("99"), "{cerworn}");
    let nearly = out.lines().find(|l| l.contains("Nearly")).expect("row");
    assert!(nearly.contains("[L 99]"), "{nearly}");
}

#[test]
fn level_report_is_ascii_and_compact() {
    use crate::commands::LevelProgress;
    let progress = LevelProgress {
        current_xp: 79_680_324,
        next_level_xp: 88_103_600,
        level_floor_xp: 67_772_000,
        percent: 58,
    };
    let out = render_level_report(&LevelReport {
        level: 84,
        title: None,
        stars: None,
        progress: Some(progress),
    });
    assert_eq!(
        out,
        "\r\nLevel 84\r\nProgress: [===========---------] 58%  (8423276 XP to level 85)\r\n"
    );
    let star = render_level_report(&LevelReport {
        level: 99,
        title: None,
        stars: Some("<b:magenta>**</>"),
        progress: None,
    });
    assert_eq!(
        star,
        "\r\nLevel 99 <b:magenta>**</>\r\nYou are as powerful as a mortal can be!\r\n"
    );
    let staff = render_level_report(&LevelReport {
        level: 100,
        title: Some("Avatar"),
        stars: None,
        progress: None,
    });
    assert_eq!(
        staff,
        "\r\nLevel 100 (Avatar)\r\nExperience has no meaning for you.\r\n"
    );
    for text in [&out, &star, &staff] {
        assert!(text.is_ascii(), "non-ASCII in {text:?}");
    }
}

#[test]
fn level_and_experience_commands_print_ascii_only() {
    let mut world = world();
    let (player, mut rx) = spawn(&mut world, "Reader", 5, 4_500, CLERIC);
    for line in ["level", "experience"] {
        dispatch(&mut world, player, line);
        let out = strip_ansi(&drain(&mut rx));
        assert!(out.is_ascii(), "`{line}` printed non-ASCII: {out:?}");
        assert!(out.contains("Level 5"), "{out}");
    }
    dispatch(&mut world, player, "level");
    let out = strip_ansi(&drain(&mut rx));
    assert!(out.contains("XP to level 6"), "{out}");
    assert!(!out.contains("(level 5)"), "no repeated level text: {out}");
}
