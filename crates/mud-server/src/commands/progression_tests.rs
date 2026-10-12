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
        alignment_bias: 0,
        campcraft_bonus: false,
        combat_rates: mud_db::characters::ClassCombatRates::default(),
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
        assert!(out.is_ascii(), "'{line}' printed non-ASCII: {out:?}");
        assert!(out.contains("Level 5"), "{out}");
    }
    dispatch(&mut world, player, "level");
    let out = strip_ansi(&drain(&mut rx));
    assert!(out.contains("XP to level 6"), "{out}");
    assert!(!out.contains("(level 5)"), "no repeated level text: {out}");
}

#[test]
fn immortals_get_no_level_progress_rows() {
    let mut world = world();
    world.init_resource::<mud_world::AchievementCatalog>();
    let (mortal, mut mrx) = spawn(&mut world, "Mortal", 20, 5_000, CLERIC);
    dispatch(&mut world, mortal, "level");
    assert!(strip_ansi(&drain(&mut mrx)).contains("XP to level 21"));
    dispatch(&mut world, mortal, "score");
    assert!(strip_ansi(&drain(&mut mrx)).contains("Next level"));
    let progress = |w: &World, e| {
        let p = w.get::<Profile>(e).unwrap();
        crate::commands::level_progress_for(w, e, p)
    };
    assert!(progress(&world, mortal).is_some());

    // A level-20 character holding an immortal rank is a god.
    let (god, mut grx) = spawn(&mut world, "Godly", 20, 5_000, CLERIC);
    world.get_mut::<Account>(god).unwrap().role = UserRole::Immortal;
    assert!(progress(&world, god).is_none());
    for line in ["level", "experience", "score"] {
        dispatch(&mut world, god, line);
        let out = strip_ansi(&drain(&mut grx));
        assert!(!out.contains("Next level"), "{line}: {out}");
        assert!(!out.contains("XP to level"), "{line}: {out}");
        assert!(!out.contains("to go"), "{line}: {out}");
    }
}

#[test]
fn level_and_experience_agree_on_xp_to_go() {
    let mut world = world();
    // Level 5 -> 6 costs 5000 total; 4500 earned leaves 500, however the
    // bracket is measured.
    let (player, mut rx) = spawn(&mut world, "Reader", 5, 4_500, CLERIC);
    dispatch(&mut world, player, "level");
    let level = strip_ansi(&drain(&mut rx));
    dispatch(&mut world, player, "experience");
    let exp = strip_ansi(&drain(&mut rx));
    assert!(level.contains("(500 XP to level 6)"), "{level}");
    assert!(exp.contains("(500 to go)"), "{exp}");
    // A character whose XP has fallen below their level's floor still
    // gets the same number from both.
    world.get_mut::<Profile>(player).unwrap().experience = 0;
    dispatch(&mut world, player, "level");
    let level = strip_ansi(&drain(&mut rx));
    dispatch(&mut world, player, "experience");
    let exp = strip_ansi(&drain(&mut rx));
    assert!(level.contains("(5000 XP to level 6)"), "{level}");
    assert!(exp.contains("(5000 to go)"), "{exp}");
}

// --- Combat growth on level-up -------------------------------------------

fn rates_class(id: i32, acc: f64, eva: f64, ap: f64) -> ClassDef {
    ClassDef {
        combat_rates: mud_db::characters::ClassCombatRates {
            accuracy_per_level: acc,
            evasion_per_level: eva,
            dex_evasion_mult: 1.5,
            attack_power_per_level: ap,
            base_crit_chance: 5,
        },
        ..class(id, "Fighter", 1.0)
    }
}

/// Spawn a level-1 character whose combat columns are the creation baseline
/// for `rates` (dex 80), exactly as `login.rs` seeds them.
fn spawn_with_baseline(
    world: &mut World,
    name: &str,
    xp: i32,
    class_id: i32,
    rates: &mud_db::characters::ClassCombatRates,
) -> Entity {
    let (e, _rx) = spawn(world, name, 1, xp, class_id);
    let b = rates.baseline(1, 80);
    world.entity_mut(e).insert(mud_world::CombatStats {
        accuracy: b.accuracy,
        evasion: b.evasion,
        attack_power: b.attack_power,
        ..Default::default()
    });
    e
}

#[test]
fn level_up_raises_accuracy_evasion_and_power_by_the_class_rates() {
    const WARRIOR: i32 = 4;
    const SORCERER: i32 = 1;
    let mut world = world();
    let warrior = rates_class(WARRIOR, 2.7, 2.0, 5.0);
    let sorcerer = rates_class(SORCERER, 1.5, 0.5, 1.0);
    let (w_rates, s_rates) = (warrior.combat_rates, sorcerer.combat_rates);
    {
        let mut catalog = world.resource_mut::<ClassCatalog>();
        catalog.by_id.insert(WARRIOR, warrior);
        catalog.by_id.insert(SORCERER, sorcerer);
    }
    // 10_000 xp clears level 11 on the (L-1)*1000 table at class factor 1.0.
    let w = spawn_with_baseline(&mut world, "Warrior", 10_000, WARRIOR, &w_rates);
    let s = spawn_with_baseline(&mut world, "Sorcerer", 10_000, SORCERER, &s_rates);
    level_sweep_tick(&mut world);
    assert_eq!(profile(&world, w).level, 11);
    assert_eq!(profile(&world, s).level, 11);

    // Same numbers a character created at level 11 would have.
    for (e, rates) in [(w, &w_rates), (s, &s_rates)] {
        let want = rates.baseline(11, 80);
        let got = world.get::<mud_world::CombatStats>(e).unwrap();
        assert_eq!(
            (got.accuracy, got.evasion, got.attack_power),
            (want.accuracy, want.evasion, want.attack_power)
        );
    }
    // Spot-check the absolute warrior numbers: 50 + 2.7*11, 5*11.
    let cs = world.get::<mud_world::CombatStats>(w).unwrap();
    assert_eq!(cs.accuracy, 80);
    assert_eq!(cs.attack_power, 55);
}

#[test]
fn level_up_growth_leaves_gear_and_buff_bonuses_in_place() {
    const WARRIOR: i32 = 4;
    let mut world = world();
    let warrior = rates_class(WARRIOR, 2.7, 2.0, 5.0);
    let rates = warrior.combat_rates;
    world
        .resource_mut::<ClassCatalog>()
        .by_id
        .insert(WARRIOR, warrior);
    let w = spawn_with_baseline(&mut world, "Geared", 1_000, WARRIOR, &rates);
    // A +10 hitroll item worn before the level-up.
    world.get_mut::<mud_world::CombatStats>(w).unwrap().accuracy += 20;
    let before = world.get::<mud_world::CombatStats>(w).unwrap().accuracy;
    level_sweep_tick(&mut world);
    assert_eq!(profile(&world, w).level, 2);
    let after = world.get::<mud_world::CombatStats>(w).unwrap().accuracy;
    // L1 -> L2 warrior: round(2.7*2) - round(2.7) = 5 - 3 = 2.
    assert_eq!(after - before, 2);
}

#[test]
fn classless_level_up_uses_the_default_rates() {
    let mut world = world();
    let rates = mud_db::characters::ClassCombatRates::default();
    let (e, _rx) = spawn(&mut world, "Classless", 1, 2_000, CLERIC);
    world.get_mut::<Profile>(e).unwrap().class_id = None;
    let b = rates.baseline(1, 80);
    world.entity_mut(e).insert(mud_world::CombatStats {
        accuracy: b.accuracy,
        evasion: b.evasion,
        attack_power: b.attack_power,
        ..Default::default()
    });
    level_sweep_tick(&mut world);
    assert_eq!(profile(&world, e).level, 3);
    let want = rates.baseline(3, 80);
    let got = world.get::<mud_world::CombatStats>(e).unwrap();
    assert_eq!(
        (got.accuracy, got.evasion, got.attack_power),
        (want.accuracy, want.evasion, want.attack_power)
    );
}
