//! `grant` / `revoke` / `ungrant`: a grant opens a command above the
//! holder's rank, a revoke closes one within it, and the guards on who can
//! change what. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{UserRole, effective_rank};
use mud_world::{
    Account, CommandGrants, Located, Named, Online, Player, Profile, Room, WorldKeyIndex,
};

use super::dispatch;
use super::test_support::{Rx, drain};
use crate::commands::{Abbrev, Connection, resolve_abbrev};

fn world() -> (World, Entity) {
    let mut world = World::new();
    world.insert_resource(WorldKeyIndex::default());
    let room = world
        .spawn((
            Room,
            Named {
                name: "A room".into(),
            },
        ))
        .id();
    (world, room)
}

fn person(world: &mut World, room: Entity, name: &str, level: i32) -> (Entity, Rx) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
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
                role: effective_rank(level, UserRole::Player),
                account_role: UserRole::Player,
                perms: vec![],
            },
            Profile {
                level,
                class_id: None,
                race: "Human".into(),
                experience: 0,
                gender: "neutral".into(),
            },
        ))
        .id();
    (e, rx)
}

fn grants_of(world: &World, e: Entity) -> CommandGrants {
    world.get::<CommandGrants>(e).cloned().unwrap_or_default()
}

#[test]
fn a_grant_lets_a_builder_use_a_command_and_a_revoke_removes_it() {
    let (mut w, room) = world();
    let (boss, mut boss_rx) = person(&mut w, room, "Boss", 105);
    let (bob, mut bob_rx) = person(&mut w, room, "Bob", 101);

    // `send` is Implementor-only: a builder is refused.
    dispatch(&mut w, bob, "send boss hello");
    assert!(drain(&mut bob_rx).contains("You can't do that."));
    assert!(!drain(&mut boss_rx).contains("hello"));

    dispatch(&mut w, boss, "grant bob command send");
    let out = drain(&mut boss_rx);
    assert!(out.contains("Granted send to Bob at level 105."), "{out}");
    assert_eq!(grants_of(&w, bob).grants.len(), 1);

    dispatch(&mut w, bob, "send boss hello");
    assert!(!drain(&mut bob_rx).contains("You can't do that."));
    assert!(drain(&mut boss_rx).contains("hello"));

    // Revoke takes it away again, and records the revoke instead.
    dispatch(&mut w, boss, "revoke bob command send");
    let out = drain(&mut boss_rx);
    assert!(out.contains("Revoked send from Bob at level 105."), "{out}");
    let g = grants_of(&w, bob);
    assert!(g.grants.is_empty());
    assert_eq!(g.revokes.len(), 1);
    dispatch(&mut w, bob, "send boss again");
    assert!(drain(&mut bob_rx).contains("You can't do that."));
    assert!(!drain(&mut boss_rx).contains("again"));

    // Ungrant clears the entry; the component goes with it.
    dispatch(&mut w, boss, "ungrant bob command send");
    assert!(w.get::<CommandGrants>(bob).is_none());
}

#[test]
fn a_revoke_closes_a_command_the_rank_allows() {
    let (mut w, room) = world();
    let (boss, mut boss_rx) = person(&mut w, room, "Boss", 105);
    let (bob, mut bob_rx) = person(&mut w, room, "Bob", 101);
    dispatch(&mut w, bob, "stat me");
    assert!(!drain(&mut bob_rx).contains("You can't do that."));

    dispatch(&mut w, boss, "revoke bob command stat");
    assert!(drain(&mut boss_rx).contains("Revoked stat from Bob"));
    dispatch(&mut w, bob, "stat me");
    assert!(drain(&mut bob_rx).contains("You can't do that."));
}

#[test]
fn a_mortal_cannot_be_granted_above_the_cap() {
    let (mut w, room) = world();
    let (boss, mut boss_rx) = person(&mut w, room, "Boss", 105);
    let (mort, _rx) = person(&mut w, room, "Mort", 20);

    // Builder and Implementor commands are over the immortal-rank cap.
    for cmd in ["goto", "send", "areload"] {
        dispatch(&mut w, boss, &format!("grant mort command {cmd}"));
        let out = drain(&mut boss_rx);
        assert!(out.contains("mortals can only be granted"), "{cmd}: {out}");
    }
    assert!(w.get::<CommandGrants>(mort).is_none());

    // An immortal-rank command is within the cap.
    dispatch(&mut w, boss, "grant mort command ptell");
    assert!(drain(&mut boss_rx).contains("Granted ptell to Mort"));
    assert_eq!(grants_of(&w, mort).grants[0].command, "ptell");
}

#[test]
fn the_delegation_commands_cannot_be_granted() {
    let (mut w, room) = world();
    let (boss, mut boss_rx) = person(&mut w, room, "Boss", 105);
    let (bob, _rx) = person(&mut w, room, "Bob", 101);
    for cmd in ["grant", "revoke", "ungrant"] {
        dispatch(&mut w, boss, &format!("grant bob command {cmd}"));
        assert!(drain(&mut boss_rx).contains("cannot itself be granted"));
    }
    assert!(w.get::<CommandGrants>(bob).is_none());
}

#[test]
fn you_cannot_grant_a_command_you_cannot_use() {
    let (mut w, room) = world();
    // A coder cannot use the implementor-only `send`.
    let (coder, mut rx) = person(&mut w, room, "Coder", 104);
    let (bob, _bob_rx) = person(&mut w, room, "Bob", 101);
    dispatch(&mut w, coder, "grant bob command send");
    assert!(drain(&mut rx).contains("cannot grant or revoke a command you yourself cannot use"));
    assert!(w.get::<CommandGrants>(bob).is_none());
}

#[test]
fn only_lower_level_characters_can_be_changed_and_not_yourself() {
    let (mut w, room) = world();
    let (a, mut a_rx) = person(&mut w, room, "Alpha", 104);
    let (b, _b_rx) = person(&mut w, room, "Beta", 104);
    dispatch(&mut w, a, "grant beta command goto");
    assert!(drain(&mut a_rx).contains("You cannot grant or revoke Beta's commands."));
    dispatch(&mut w, a, "grant alpha command goto");
    assert!(drain(&mut a_rx).contains("You cannot grant or revoke your own commands."));
    assert!(w.get::<CommandGrants>(a).is_none());
    assert!(w.get::<CommandGrants>(b).is_none());
}

#[test]
fn a_lower_level_staffer_cannot_undo_a_higher_entry() {
    let (mut w, room) = world();
    let (boss, mut boss_rx) = person(&mut w, room, "Boss", 105);
    let (coder, mut coder_rx) = person(&mut w, room, "Coder", 104);
    let (bob, _rx) = person(&mut w, room, "Bob", 101);

    dispatch(&mut w, boss, "revoke bob command goto");
    assert!(drain(&mut boss_rx).contains("Revoked goto from Bob at level 105."));

    for line in ["grant bob command goto", "ungrant bob command goto"] {
        dispatch(&mut w, coder, line);
        let out = drain(&mut coder_rx);
        assert!(
            out.contains("because Boss (level 105) granted or revoked it"),
            "{line}: {out}"
        );
    }
    assert_eq!(grants_of(&w, bob).revokes.len(), 1);

    // The same level can.
    dispatch(&mut w, boss, "ungrant bob command goto");
    assert!(w.get::<CommandGrants>(bob).is_none());
}

#[test]
fn the_entry_level_never_exceeds_the_grantors() {
    let (mut w, room) = world();
    let (coder, mut rx) = person(&mut w, room, "Coder", 104);
    let (bob, _bob_rx) = person(&mut w, room, "Bob", 101);
    // A coder asks for level 200: capped to their own 104.
    dispatch(&mut w, coder, "revoke bob command goto 200");
    assert!(drain(&mut rx).contains("at level 104."));
    assert_eq!(grants_of(&w, bob).revokes[0].level, 104);
}

#[test]
fn list_and_clear() {
    let (mut w, room) = world();
    let (boss, mut rx) = person(&mut w, room, "Boss", 105);
    let (bob, _bob_rx) = person(&mut w, room, "Bob", 101);
    dispatch(&mut w, boss, "grant bob command send");
    dispatch(&mut w, boss, "revoke bob command goto");
    drain(&mut rx);
    dispatch(&mut w, boss, "grant bob list");
    let out = drain(&mut rx);
    assert!(
        out.contains("Bob's grants:") && out.contains("send Boss (105)"),
        "{out}"
    );
    assert!(
        out.contains("Bob's revocations:") && out.contains("goto Boss (105)"),
        "{out}"
    );
    dispatch(&mut w, boss, "grant bob clear");
    assert!(drain(&mut rx).contains("2 grants cleared."));
    assert!(w.get::<CommandGrants>(bob).is_none());
}

#[test]
fn grants_round_trip_through_json() {
    let (mut w, room) = world();
    let (boss, _rx) = person(&mut w, room, "Boss", 105);
    let (bob, _bob_rx) = person(&mut w, room, "Bob", 101);
    dispatch(&mut w, boss, "grant bob command send");
    dispatch(&mut w, boss, "revoke bob command goto");
    let g = grants_of(&w, bob);
    let json = serde_json::to_value(&g).unwrap();
    assert_eq!(serde_json::from_value::<CommandGrants>(json).unwrap(), g);
}

#[test]
fn abbreviations_honour_grants() {
    let g = CommandGrants {
        grants: vec![mud_world::GrantEntry {
            command: "send".into(),
            grantor: "Boss".into(),
            level: 105,
        }],
        revokes: vec![],
    };
    let hit = |grants: Option<&CommandGrants>| {
        matches!(
            resolve_abbrev("sen", UserRole::Builder, &[], grants, None),
            Some(Abbrev::Command(c)) if c.names[0] == "send"
        )
    };
    assert!(hit(Some(&g)));
    assert!(!hit(None));
}
