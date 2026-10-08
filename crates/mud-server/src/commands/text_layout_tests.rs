//! End-to-end tests for server-side text layout: room description
//! indent/reflow through `look`, the `columns` setting, NAWS-driven
//! width, and the `spells` grid.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_world::{
    AbilityCatalog, ClientWidth, Description, KnownAbilities, Named, PREF_COLUMNS_KEY, Room,
    ScriptVars, SpellSlotData,
};

use super::test_support::{ability_def, drain, player_in};
use super::{ColorMode, cmd_look, render_color_tags};
use crate::layout::{text_width, wrap_width};

fn room_with(world: &mut World, desc: &str) -> Entity {
    world
        .spawn((
            Room,
            Named {
                name: "A secret hollow".to_string(),
            },
            Description(desc.to_string()),
        ))
        .id()
}

/// Plain text of delivered output: drops ANSI sequences and any
/// leftover markup tags.
fn strip(s: &str) -> String {
    let mut plain = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    render_color_tags(&plain, ColorMode::Strip)
}

fn look_output(world: &mut World, player: Entity, rx: &mut super::test_support::Rx) -> String {
    cmd_look(world, player, "");
    // Drop GMCP subnegotiation frames (`IAC SB 201 ... IAC SE`): they
    // ride the same stream but are not text the player reads.
    let mut bytes = Vec::new();
    while let Ok(b) = rx.try_recv() {
        bytes.extend(b);
    }
    let mut text = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(&[255, 250, 201]) {
            while i + 1 < bytes.len() && bytes[i..i + 2] != [255, 240] {
                i += 1;
            }
            i += 2;
        } else {
            text.push(bytes[i]);
            i += 1;
        }
    }
    strip(&String::from_utf8_lossy(&text))
}

#[test]
fn look_indents_and_separates_imported_paragraphs() {
    let mut world = World::new();
    let imported = "Part of the bark of this giant oak has been fashioned into a door.\n In the little kitchen area is a large stone fireplace.";
    let room = room_with(&mut world, imported);
    let (player, mut rx) = player_in(&mut world, room);
    let out = look_output(&mut world, player, &mut rx);
    assert!(
        out.contains(
            "\r\n   Part of the bark of this giant oak has been fashioned into a door.\r\n\r\n   In the little kitchen area is a large stone fireplace.\r\n"
        ),
        "{out:?}"
    );
}

#[test]
fn look_wraps_to_client_width_and_updates_on_resize() {
    let mut world = World::new();
    let long = "The trail runs north and west from here through dense pines, \
                climbing steadily toward a ridge that is lost in low cloud.";
    let room = room_with(&mut world, long);
    let (player, mut rx) = player_in(&mut world, room);

    // No NAWS yet: default 80.
    assert_eq!(wrap_width(&world, player), 80);
    let wide = look_output(&mut world, player, &mut rx);
    for l in wide.split("\r\n") {
        assert!(text_width(l) <= 80, "{l:?}");
    }

    // Client reports 50 columns.
    world.entity_mut(player).insert(ClientWidth(50));
    assert_eq!(wrap_width(&world, player), 50);
    let narrow = look_output(&mut world, player, &mut rx);
    let desc_lines: Vec<&str> = narrow
        .split("\r\n")
        .filter(|l| l.contains("trail") || l.contains("pines") || l.contains("cloud"))
        .collect();
    assert!(desc_lines.len() >= 2, "{narrow:?}");
    for l in narrow.split("\r\n") {
        assert!(text_width(l) <= 50, "{l:?}");
    }
}

#[test]
fn columns_command_pins_width_and_auto_restores_client_width() {
    let mut world = World::new();
    let room = room_with(&mut world, "x");
    let (player, mut rx) = player_in(&mut world, room);
    world.entity_mut(player).insert(ClientWidth(120));
    assert_eq!(wrap_width(&world, player), 120);

    super::info::cmd_columns(&mut world, player, "100");
    assert_eq!(wrap_width(&world, player), 100);
    assert_eq!(
        world
            .get::<ScriptVars>(player)
            .and_then(|v| v.0.get(PREF_COLUMNS_KEY).cloned())
            .as_deref(),
        Some("100"),
        "persisted through ScriptVars (Characters.script_vars)"
    );
    assert!(drain(&mut rx).contains("wrap at 100 columns"));

    // Out of range is rejected and leaves the setting alone.
    super::info::cmd_columns(&mut world, player, "10");
    assert_eq!(wrap_width(&world, player), 100);
    assert!(drain(&mut rx).contains("Usage: columns"));
    super::info::cmd_columns(&mut world, player, "junk");
    assert_eq!(wrap_width(&world, player), 100);

    // `toggle columns <n>` and `toggle wrap` route to the same command.
    super::info::cmd_toggle(&mut world, player, "columns 90");
    assert_eq!(wrap_width(&world, player), 90);
    super::info::cmd_toggle(&mut world, player, "wrap auto");
    assert_eq!(wrap_width(&world, player), 120);
    assert!(drain(&mut rx).contains("follows your client"));
}

#[test]
fn explicit_columns_overrides_client_for_prose() {
    let mut world = World::new();
    let long = "word ".repeat(60);
    let room = room_with(&mut world, long.trim());
    let (player, mut rx) = player_in(&mut world, room);
    world.entity_mut(player).insert(ClientWidth(200));
    super::info::cmd_columns(&mut world, player, "60");
    let _ = drain(&mut rx);
    let out = look_output(&mut world, player, &mut rx);
    for l in out.split("\r\n") {
        assert!(text_width(l) <= 60, "{l:?}");
    }
}

fn spells_world(names: &[(&str, &str)]) -> (World, Entity, super::test_support::Rx) {
    let mut world = World::new();
    let mut catalog = AbilityCatalog::default();
    let mut known = Vec::new();
    for (i, (name, sphere)) in names.iter().enumerate() {
        let id = i32::try_from(i).unwrap() + 1;
        let mut def = ability_def(id, name, AbilityKind::Spell);
        def.sphere = Some((*sphere).to_string());
        catalog.by_name.insert(def.plain_name.clone(), def);
        known.push((id, 100, true));
    }
    world.insert_resource(catalog);
    world.insert_resource(SpellSlotData::default());
    let room = room_with(&mut world, "x");
    let (player, rx) = player_in(&mut world, room);
    world
        .entity_mut(player)
        .insert(KnownAbilities { entries: known });
    (world, player, rx)
}

/// Column (visible-char offset) at which each cell starts in a row.
fn cell_starts(row: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let chars: Vec<char> = row.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != ' ' && (i == 0 || (chars[i - 1] == ' ' && i >= 2 && chars[i - 2] == ' ')) {
            starts.push(i);
        }
        i += 1;
    }
    starts
}

#[test]
fn spells_grid_uses_spaces_and_aligns_columns_across_blocks() {
    // Coloured sphere tags in every entry; one long name per list.
    let (mut world, player, mut rx) = spells_world(&[
        ("Detect Magic", "divination"),
        ("Ice Darts", "water"),
        ("Minor Creation", "summoning"),
        ("Waterwalk", "enchantment"),
        ("Chill Touch", "water"),
        ("Concealment", "enchantment"),
        ("Detect Invisibility", "divination"),
        ("Enhance Ability", "enchantment"),
    ]);
    // A 120-column client fits three 34-wide columns.
    world.entity_mut(player).insert(ClientWidth(120));
    super::info::cmd_spells(&mut world, player, "");
    let raw = drain(&mut rx);
    assert!(raw.contains("\x1b["), "colour tags rendered as ANSI");
    assert!(!raw.contains('\t'), "no hard tabs");
    let out = strip(&raw);
    // Every list is one circle-0 block here: rows of 3 cells, aligned.
    let rows: Vec<&str> = out
        .split("\r\n")
        .filter(|l| l.starts_with("  ") && !l.trim().is_empty())
        .collect();
    assert_eq!(rows.len(), 3, "8 entries / 3 per row: {out:?}");
    let first = cell_starts(rows[0]);
    let second = cell_starts(rows[1]);
    assert_eq!(first.len(), 3, "{rows:?}");
    assert_eq!(first, second, "columns line up row to row: {rows:?}");
    for r in &rows {
        assert!(!r.ends_with(' '), "no trailing padding: {r:?}");
        assert!(text_width(r) <= 120, "{r:?}");
    }
}

#[test]
fn spells_grid_reflows_to_narrow_clients() {
    let (mut world, player, mut rx) = spells_world(&[
        ("Detect Magic", "divination"),
        ("Ice Darts", "water"),
        ("Minor Creation", "summoning"),
        ("Waterwalk", "enchantment"),
    ]);
    world.entity_mut(player).insert(ClientWidth(45));
    super::info::cmd_spells(&mut world, player, "");
    let out = strip(&drain(&mut rx));
    for l in out.split("\r\n") {
        assert!(text_width(l) <= 45, "{l:?}");
    }
    // 45 cols with a 28-wide column leaves room for only one per row.
    let rows = out
        .split("\r\n")
        .filter(|l| l.starts_with("  ") && !l.trim().is_empty())
        .count();
    assert_eq!(rows, 4, "{out:?}");
}
