//! Tests for the player-facing terminal settings (`color`, `charset`)
//! and for `who` under plain-ASCII output (#10, #31).

use bevy_ecs::prelude::*;
use mud_db::enums::{UserRole, effective_rank};
use mud_net::{Charset, ColorDepth, OutputHandle};
use mud_world::{
    Account, ClassCatalog, ClassDef, Named, Online, PREF_CHARSET_KEY, PREF_COLOR_KEY, Player,
    PlayerFlags, Profile, ScriptVars, Title,
};

use super::test_support::Rx;
use super::{Connection, dispatch};
use crate::terminal::{ClientOutput, sync_output};

fn spawn(world: &mut World, name: &str, level: i32, title: Option<&str>) -> (Entity, Rx) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let mut e = world.spawn((
        Player,
        Online,
        Named {
            name: name.to_string(),
        },
        Connection(tx),
        PlayerFlags::default(),
        ScriptVars::default(),
        Account {
            user_id: String::new(),
            character_id: format!("c-{name}"),
            role: effective_rank(level, UserRole::Player),
            account_role: UserRole::Player,
            perms: vec![],
        },
        Profile {
            level,
            class_id: Some(1),
            race: "HALF_ELF".into(),
            experience: 0,
            gender: "neutral".into(),
        },
    ));
    if let Some(t) = title {
        e.insert(Title(t.to_string()));
    }
    (e.id(), rx)
}

fn world() -> World {
    let mut world = World::new();
    let mut catalog = ClassCatalog::default();
    catalog.by_id.insert(
        1,
        ClassDef {
            id: 1,
            name: "Cleric".to_string(),
            plain_name: "Cleric".to_string(),
            is_subclass: false,
            parent_class_id: None,
            description: None,
            hit_dice: "1d8".to_string(),
            primary_stat: None,
            hp_per_level: 0,
            exp_gain_factor: 1.0,
            alignment_bias: 0,
            campcraft_bonus: false,
            resistances: std::collections::HashMap::new(),
        },
    );
    world.insert_resource(catalog);
    world
}

/// Attach a fresh (16-colour ASCII) connection handle to `player`.
fn attach(world: &mut World, player: Entity) -> OutputHandle {
    let handle = OutputHandle::new();
    world
        .entity_mut(player)
        .insert(ClientOutput(handle.clone()));
    handle
}

/// What the client would receive: each frame run through the writer's
/// encoder for `handle`'s current capabilities.
fn wire(rx: &mut Rx, handle: &OutputHandle) -> String {
    let mut out = Vec::new();
    while let Ok(bytes) = rx.try_recv() {
        out.extend(handle.encode(bytes));
    }
    String::from_utf8(out).unwrap()
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
fn who_puts_the_title_right_after_the_name_like_a_last_name() {
    let mut world = world();
    let (viewer, mut rx) = spawn(&mut world, "Viewer", 10, None);
    let handle = attach(&mut world, viewer);
    let _ = spawn(&mut world, "Strider", 50, Some("the Wanderer"));
    dispatch(&mut world, viewer, "who");
    let out = strip_ansi(&wire(&mut rx, &handle));
    let row = out.lines().find(|l| l.contains("Strider")).expect("row");
    assert!(row.contains("Strider the Wanderer"), "{row}");
    assert!(row.contains("(Half-Elf)"), "race missing: {row}");
    // The class tag follows; the title is not separated from the name.
    assert!(row.find("(Half-Elf)") < row.find("[Cleric]"), "{row}");
}

#[test]
fn who_in_ascii_mode_is_pure_ascii_with_a_plain_star() {
    let mut world = world();
    let (viewer, mut rx) = spawn(&mut world, "Viewer", 10, None);
    let handle = attach(&mut world, viewer);
    let _ = spawn(&mut world, "Zeus", 105, Some("the \u{2014} Thunderer"));
    let _ = spawn(&mut world, "Mortal", 12, None);
    dispatch(&mut world, viewer, "who");
    let out = wire(&mut rx, &handle);
    assert!(out.is_ascii(), "non-ASCII reached an ASCII client: {out:?}");
    let plain = strip_ansi(&out);
    let zeus = plain.lines().find(|l| l.contains("Zeus")).expect("row");
    assert!(zeus.contains("[*]"), "{zeus}");
    assert!(zeus.contains("the -- Thunderer"), "{zeus}");
    // Staff first, then a blank line, then mortals.
    let zi = plain.find("Zeus").unwrap();
    let mi = plain.find("Mortal").unwrap();
    assert!(zi < mi);
    assert!(plain[zi..mi].contains("\r\n\r\n"), "{plain:?}");

    // The same output reaches a UTF-8 client untouched.
    handle.set_charset_override(Some(Charset::Utf8));
    dispatch(&mut world, viewer, "who");
    let out = wire(&mut rx, &handle);
    assert!(out.contains('\u{2605}'), "{out:?}");
}

#[test]
fn color_command_sets_overrides_and_persists_in_script_vars() {
    let mut world = world();
    let (p, mut rx) = spawn(&mut world, "Pat", 10, None);
    let handle = attach(&mut world, p);
    handle.apply_mtts(1 | 4 | 8);
    assert_eq!(handle.color(), ColorDepth::Ansi256);

    dispatch(&mut world, p, "color 16");
    assert_eq!(handle.color(), ColorDepth::Ansi16);
    assert_eq!(
        world.get::<ScriptVars>(p).unwrap().0.get(PREF_COLOR_KEY),
        Some(&"16".to_string())
    );

    dispatch(&mut world, p, "color off");
    assert_eq!(handle.color(), ColorDepth::None);
    let _ = wire(&mut rx, &handle);
    // With colour off the encoder strips even stray escapes.
    assert_eq!(
        String::from_utf8(handle.encode(b"\x1b[31mx\x1b[0m".to_vec())).unwrap(),
        "x"
    );

    dispatch(&mut world, p, "color 256");
    assert_eq!(handle.color(), ColorDepth::Ansi256);
    assert!(
        !world
            .get::<PlayerFlags>(p)
            .unwrap()
            .has(mud_db::enums::PlayerFlag::ColorBlind)
    );

    dispatch(&mut world, p, "color auto");
    assert!(
        !world
            .get::<ScriptVars>(p)
            .unwrap()
            .0
            .contains_key(PREF_COLOR_KEY)
    );
    assert_eq!(handle.color(), ColorDepth::Ansi256);

    dispatch(&mut world, p, "color bogus");
    assert!(wire(&mut rx, &handle).contains("Usage: color"));
}

#[test]
fn toggle_color_flips_the_same_switch_as_color_off() {
    let mut world = world();
    let (p, _rx) = spawn(&mut world, "Pat", 10, None);
    let handle = attach(&mut world, p);
    dispatch(&mut world, p, "toggle colorblind");
    assert_eq!(handle.color(), ColorDepth::None);
    dispatch(&mut world, p, "toggle colorblind");
    assert_eq!(handle.color(), ColorDepth::Ansi16);
}

#[test]
fn charset_command_overrides_the_negotiated_charset() {
    let mut world = world();
    let (p, mut rx) = spawn(&mut world, "Pat", 10, None);
    let handle = attach(&mut world, p);
    assert_eq!(handle.charset(), Charset::Ascii);

    dispatch(&mut world, p, "charset utf8");
    assert_eq!(handle.charset(), Charset::Utf8);
    assert_eq!(
        world.get::<ScriptVars>(p).unwrap().0.get(PREF_CHARSET_KEY),
        Some(&"utf8".to_string())
    );
    dispatch(&mut world, p, "charset ascii");
    assert_eq!(handle.charset(), Charset::Ascii);
    dispatch(&mut world, p, "charset auto");
    assert!(
        !world
            .get::<ScriptVars>(p)
            .unwrap()
            .0
            .contains_key(PREF_CHARSET_KEY)
    );
    // Auto follows the client again.
    handle.mark_utf8();
    assert_eq!(handle.charset(), Charset::Utf8);
    let _ = wire(&mut rx, &handle);
    dispatch(&mut world, p, "charset");
    assert!(wire(&mut rx, &handle).contains("Charset: UTF-8 (from your client)"));
}

#[test]
fn saved_preferences_apply_when_a_connection_attaches() {
    let mut world = world();
    let (p, _rx) = spawn(&mut world, "Pat", 10, None);
    {
        let mut vars = world.get_mut::<ScriptVars>(p).unwrap();
        vars.0.insert(PREF_CHARSET_KEY.into(), "utf8".into());
        vars.0.insert(PREF_COLOR_KEY.into(), "256".into());
    }
    let handle = attach(&mut world, p);
    assert_eq!(handle.charset(), Charset::Ascii);
    sync_output(&world, p);
    assert_eq!(handle.charset(), Charset::Utf8);
    assert_eq!(handle.color(), ColorDepth::Ansi256);
}
