//! The four per-command policy lists (allowed while casting / paralyzed, keeps hidden, held
//! by cast lag) moved from Rust consts to `"Command"` boolean columns. With the seed patch
//! applied, with an empty catalog (table or columns missing), with no catalog at all, and
//! with rows missing for some commands, every registry command must answer exactly as the
//! built-in lists do.

use bevy_ecs::prelude::*;
use mud_world::{CommandPolicy, PolicyFlag, PolicyFlags};

use super::{Command, all_commands, builtin_paralysis_allows, paralysis_allows};
use crate::casting::{allowed_while_casting, builtin_allowed_while_casting};
use crate::commands::input_queue::{builtin_held_by_cast_lag, held_by_cast_lag};
use crate::hiding::{builtin_hide_safe, is_hide_safe};

/// (casting, paralysis, hide-safe, lag-held) as the dispatcher sees them.
type Answers = (bool, bool, bool, bool);

fn answers(world: &World, cmd: &Command) -> Answers {
    let name = cmd.names[0];
    (
        allowed_while_casting(world, name),
        paralysis_allows(world, cmd),
        is_hide_safe(world, name, name, false),
        held_by_cast_lag(world, name),
    )
}

fn builtin(cmd: &Command) -> Answers {
    let name = cmd.names[0];
    (
        builtin_allowed_while_casting(name),
        builtin_paralysis_allows(cmd),
        builtin_hide_safe(name),
        builtin_held_by_cast_lag(name),
    )
}

fn flags_of((c, p, h, l): Answers) -> PolicyFlags {
    PolicyFlags {
        allowed_while_casting: c,
        allowed_while_paralyzed: p,
        keeps_hidden: h,
        held_by_cast_lag: l,
    }
}

fn assert_matches_builtin(world: &World, what: &str) {
    let mut checked = 0;
    for cmd in all_commands() {
        assert_eq!(
            answers(world, cmd),
            builtin(cmd),
            "{what}: {}",
            cmd.names[0]
        );
        checked += 1;
    }
    assert!(checked > 400, "only {checked} commands");
}

#[test]
fn no_catalog_uses_the_builtin_lists() {
    assert_matches_builtin(&World::new(), "no catalog");
}

#[test]
fn empty_catalog_uses_the_builtin_lists() {
    // What the loader yields for a database without the table or the columns.
    let mut world = World::new();
    world.insert_resource(CommandPolicy::default());
    assert_matches_builtin(&world, "empty catalog");
}

#[test]
fn a_catalog_seeded_from_the_lists_changes_nothing() {
    let mut world = World::new();
    world.insert_resource(CommandPolicy::from_rows(
        all_commands().map(|c| (c.names[0].to_string(), flags_of(builtin(c)))),
    ));
    assert_matches_builtin(&world, "seeded catalog");
}

#[test]
fn commands_without_a_row_fall_back_per_command() {
    // Rows for every other command, all flags off: those answer from the (all-false) rows,
    // the rest from the lists.
    let mut world = World::new();
    let with_row = |i: usize| i.is_multiple_of(2);
    world.insert_resource(CommandPolicy::from_rows(
        all_commands()
            .enumerate()
            .filter(|(i, _)| with_row(*i))
            .map(|(_, c)| (c.names[0].to_string(), PolicyFlags::default())),
    ));
    for (i, cmd) in all_commands().enumerate() {
        let expected = if with_row(i) {
            (false, false, false, false)
        } else {
            builtin(cmd)
        };
        assert_eq!(answers(&world, cmd), expected, "{}", cmd.names[0]);
    }
}

#[test]
fn a_row_flag_overrides_the_builtin_list() {
    let mut world = World::new();
    world.insert_resource(CommandPolicy::from_rows([
        // `look` is on every list; a builder switched all four off.
        ("look".to_string(), PolicyFlags::default()),
        // `get` is on none; a builder switched casting and hide on.
        (
            "get".to_string(),
            PolicyFlags {
                allowed_while_casting: true,
                keeps_hidden: true,
                ..PolicyFlags::default()
            },
        ),
    ]));
    assert!(!allowed_while_casting(&world, "look"));
    assert!(!is_hide_safe(&world, "look", "l", false));
    assert!(allowed_while_casting(&world, "get"));
    assert!(is_hide_safe(&world, "get", "get", false));
    assert!(!held_by_cast_lag(&world, "get"));
    // Staff and the `shadow` spelling never consult the flag.
    assert!(is_hide_safe(&world, "look", "l", true));
    assert!(is_hide_safe(&world, "follow", "shadow", false));
}

#[test]
fn policy_lookup_reads_each_column() {
    let policy = CommandPolicy::from_rows([(
        "x".to_string(),
        PolicyFlags {
            keeps_hidden: true,
            ..PolicyFlags::default()
        },
    )]);
    assert_eq!(policy.flag("x", PolicyFlag::KeepsHidden), Some(true));
    assert_eq!(
        policy.flag("x", PolicyFlag::AllowedWhileCasting),
        Some(false)
    );
    assert_eq!(policy.flag("y", PolicyFlag::KeepsHidden), None);
}

#[tokio::test]
#[ignore = "requires live fierydev DB with the command-policy patch; run with: cargo test -p mud-server -- --ignored"]
async fn seeded_command_rows_reproduce_every_builtin_policy_answer() {
    let _ = dotenvy::from_path("../../.env");
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = mud_db::connect(&url).await.expect("connect");
    let rows = mud_db::command_policy::list_all(&pool).await.expect("rows");

    let mut missing = Vec::new();
    for cmd in all_commands() {
        if !rows.iter().any(|r| r.name == cmd.names[0]) {
            missing.push(cmd.names[0]);
        }
    }
    eprintln!("registry commands without a Command row: {missing:?}");

    let mut world = World::new();
    world.insert_resource(CommandPolicy::from_rows(rows.into_iter().map(|r| {
        (
            r.name,
            PolicyFlags {
                allowed_while_casting: r.allowed_while_casting,
                allowed_while_paralyzed: r.allowed_while_paralyzed,
                keeps_hidden: r.keeps_hidden,
                held_by_cast_lag: r.held_by_cast_lag,
            },
        )
    })));
    assert_matches_builtin(&world, "seeded DB");
}
