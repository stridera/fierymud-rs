//! `help <command>` text moved from the Rust registry to the database
//! (`"Command".help_summary` / `help_text`). With the seed patch applied, with
//! an empty catalog (table or columns missing), and with no catalog at all, the
//! page of every registry command must read exactly as before.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, CommandHelp, HelpCatalog, Room, SocialRegistry};

use super::test_support::{Rx, drain, player_in};
use super::{Category, all_commands, info};

/// Implementor, so every registry command is visible in `help` / `wizhelp`.
fn admin_world() -> (World, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(HelpCatalog::default());
    world.insert_resource(SocialRegistry::default());
    let room = world.spawn(Room).id();
    let (p, rx) = player_in(&mut world, room);
    world.entity_mut(p).insert(Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Implementor,
        account_role: UserRole::Implementor,
        perms: Vec::new(),
    });
    (world, p, rx)
}

/// `help <name>` (or `wizhelp <name>` for Admin commands) for every registry
/// command, keyed by canonical name.
fn render_all(world: &mut World, p: Entity, rx: &mut Rx) -> Vec<(&'static str, String)> {
    let mut pages: Vec<(&'static str, String)> = Vec::new();
    for cmd in all_commands() {
        let name = cmd.names[0];
        if cmd.category == Category::Admin {
            info::cmd_wizhelp(world, p, name);
        } else {
            info::cmd_help(world, p, name);
        }
        pages.push((name, drain(rx)));
    }
    pages.sort_by_key(|(n, _)| *n);
    pages
}

/// Rows exactly as the seed patch writes them: every command's registry
/// summary and long text.
fn seeded_rows() -> Vec<(String, Option<String>, Option<String>)> {
    all_commands()
        .map(|c| {
            (
                c.names[0].to_string(),
                Some(c.help.summary.to_string()),
                Some(c.help.long.to_string()),
            )
        })
        .collect()
}

#[test]
fn every_command_page_is_identical_with_patch_empty_table_or_no_catalog() {
    let (mut world, p, mut rx) = admin_world();
    let before = render_all(&mut world, p, &mut rx);
    assert!(before.len() > 400, "{} commands", before.len());
    for (name, page) in &before {
        assert!(page.contains(name), "{name}: {page}");
    }

    // Table or columns missing: the loader inserts an empty catalog.
    world.insert_resource(CommandHelp::default());
    assert_eq!(before, render_all(&mut world, p, &mut rx), "empty catalog");

    // Patch applied: every command has its registry text in the catalog.
    world.insert_resource(CommandHelp::from_rows(seeded_rows()));
    assert_eq!(before, render_all(&mut world, p, &mut rx), "seeded catalog");
}

#[test]
fn builder_text_replaces_the_builtin_and_blank_text_falls_back() {
    let (mut world, p, mut rx) = admin_world();
    info::cmd_help(&mut world, p, "say");
    let say_before = drain(&mut rx);
    world.insert_resource(CommandHelp::from_rows([
        (
            "look".to_string(),
            Some("Edited one-liner.".to_string()),
            Some("Edited long paragraph.".to_string()),
        ),
        // Whitespace-only and NULL values mean "use the built-in text".
        ("say".to_string(), Some("  ".to_string()), None),
    ]));
    info::cmd_help(&mut world, p, "look");
    let look = drain(&mut rx);
    assert!(look.contains("Edited one-liner."), "{look}");
    assert!(look.contains("Edited long paragraph."), "{look}");

    info::cmd_help(&mut world, p, "say");
    assert_eq!(say_before, drain(&mut rx));
}

#[tokio::test]
#[ignore = "requires live fierydev DB with the command-help patch; run with: cargo test -p mud-server -- --ignored"]
async fn seeded_command_rows_reproduce_every_builtin_help_page() {
    let _ = dotenvy::from_path("../../.env");
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = mud_db::connect(&url).await.expect("connect");
    let rows = mud_db::command_help::list_all(&pool).await.expect("rows");

    for cmd in all_commands() {
        let row = rows
            .iter()
            .find(|r| r.name == cmd.names[0])
            .unwrap_or_else(|| panic!("no Command row for {}", cmd.names[0]));
        assert_eq!(
            row.help_summary.as_deref(),
            Some(cmd.help.summary),
            "{} summary",
            cmd.names[0]
        );
        assert_eq!(
            row.help_text.as_deref().unwrap_or(""),
            cmd.help.long,
            "{} long",
            cmd.names[0]
        );
    }

    let (mut world, p, mut rx) = admin_world();
    let before = render_all(&mut world, p, &mut rx);
    world.insert_resource(CommandHelp::from_rows(
        rows.into_iter()
            .map(|r| (r.name, r.help_summary, r.help_text)),
    ));
    assert_eq!(before, render_all(&mut world, p, &mut rx), "DB catalog");
}
