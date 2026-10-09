//! `help <topic>` for the player-facing topic articles (combat, death, ...)
//! comes from the `HelpEntry` catalog, not from a table in the binary.

use bevy_ecs::prelude::*;
use mud_db::enums::{UserRole, effective_rank};
use mud_world::{
    AbilityCatalog, Account, ClassCatalog, HelpCatalog, HelpEntry, Named, Online, Player,
    PlayerFlags, Profile, ScriptVars, SocialRegistry,
};

use super::test_support::Rx;
use super::{Connection, dispatch};

fn world_with_help(entries: Vec<HelpEntry>) -> World {
    let mut world = World::new();
    world.insert_resource(ClassCatalog::default());
    world.insert_resource(SocialRegistry::default());
    world.insert_resource(AbilityCatalog::default());
    let mut catalog = HelpCatalog::default();
    for e in entries {
        for k in &e.keywords {
            catalog
                .by_keyword
                .entry(k.to_ascii_lowercase())
                .or_default()
                .push(e.id);
        }
        catalog.entries.insert(e.id, e);
    }
    world.insert_resource(catalog);
    world
}

fn entry(id: i32, keyword: &str, title: &str, content: &str, min_level: i32) -> HelpEntry {
    HelpEntry {
        id,
        title: title.into(),
        content: content.into(),
        min_level,
        category: Some("guide".into()),
        usage: None,
        duration: None,
        sphere: None,
        keywords: vec![keyword.into()],
    }
}

fn player(world: &mut World, level: i32) -> (Entity, Rx) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let id = world
        .spawn((
            Player,
            Online,
            Named {
                name: "Reader".into(),
            },
            Connection(tx),
            PlayerFlags::default(),
            ScriptVars::default(),
            Account {
                user_id: String::new(),
                character_id: "c-reader".into(),
                role: effective_rank(level, UserRole::Player),
                account_role: UserRole::Player,
                perms: vec![],
            },
            Profile {
                level,
                class_id: None,
                race: "HUMAN".into(),
                experience: 0,
                gender: "neutral".into(),
            },
        ))
        .id();
    (id, rx)
}

fn text(rx: &mut Rx) -> String {
    let mut out = Vec::new();
    while let Ok(b) = rx.try_recv() {
        out.extend(b);
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn help_combat_is_the_database_article() {
    let mut world = world_with_help(vec![entry(
        1,
        "combat",
        "Combat",
        "Hit chance comes from a catalog row.",
        0,
    )]);
    let (p, mut rx) = player(&mut world, 10);
    dispatch(&mut world, p, "help combat");
    let out = text(&mut rx);
    assert!(
        out.contains("Hit chance comes from a catalog row."),
        "{out}"
    );
}

#[test]
fn help_topics_without_a_row_no_longer_fall_back_to_the_binary() {
    let mut world = world_with_help(vec![]);
    let (p, mut rx) = player(&mut world, 10);
    for topic in ["combat", "death", "tank", "stealth"] {
        dispatch(&mut world, p, &format!("help {topic}"));
        let out = text(&mut rx);
        assert!(out.contains("No help on"), "help {topic}: {out}");
    }
}

#[test]
fn staff_only_row_hides_from_players_but_a_player_row_serves_them() {
    let mut world = world_with_help(vec![
        entry(1, "death", "Death", "Staff notes.", 100),
        entry(
            2,
            "death",
            "Death",
            "Release to return to your recall point.",
            0,
        ),
    ]);
    let (p, mut rx) = player(&mut world, 10);
    dispatch(&mut world, p, "help death");
    let out = text(&mut rx);
    assert!(out.contains("Release to return"), "{out}");
    assert!(!out.contains("Staff notes."), "{out}");
}
