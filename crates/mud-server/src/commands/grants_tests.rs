//! `grant` / `revoke` / `ungrant`: a grant opens a command above the
//! holder's rank, a revoke closes one within it, and the guards on who can
//! change what. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{UserRole, effective_rank};
use mud_world::{
    Account, CommandGrants, ConfigValue, GrantEntry, GrantUsability, Located, Named, Online,
    Player, Profile, Room, RuntimeConfig, WorldKeyIndex,
};

use super::dispatch;
use super::grants::grant_honoured;
use super::test_support::{Rx, drain};
use crate::commands::{Abbrev, Command, Connection, all_commands, grant_usability, resolve_abbrev};

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

fn allow(world: &mut World, json: &str) {
    let mut cfg = RuntimeConfig::default();
    cfg.by_key.insert(
        ("grants".into(), "mortal_allowlist".into()),
        ConfigValue::Json(json.into()),
    );
    world.insert_resource(cfg);
}

fn entry(command: &str) -> GrantEntry {
    GrantEntry {
        command: command.into(),
        grantor: "Boss".into(),
        level: 105,
    }
}

fn find(name: &str) -> &'static Command {
    all_commands()
        .find(|c| c.names[0] == name)
        .unwrap_or_else(|| panic!("no command {name}"))
}

#[test]
fn a_mortal_is_refused_a_command_not_on_the_allowlist() {
    let (mut w, room) = world();
    let (boss, mut boss_rx) = person(&mut w, room, "Boss", 105);
    let (mort, _rx) = person(&mut w, room, "Mort", 20);

    // The default allowlist is empty: even an immortal-rank command is refused.
    for cmd in ["ptell", "goto", "send", "areload"] {
        dispatch(&mut w, boss, &format!("grant mort command {cmd}"));
        let out = drain(&mut boss_rx);
        assert!(out.contains("mortals can only be granted"), "{cmd}: {out}");
    }
    // A listed command is still refused when it is over the rank cap or is a
    // delegation command.
    allow(&mut w, r#"["goto", "ptell", "grant"]"#);
    dispatch(&mut w, boss, "grant mort command goto");
    assert!(drain(&mut boss_rx).contains("mortals can only be granted"));
    dispatch(&mut w, boss, "grant mort command grant");
    assert!(drain(&mut boss_rx).contains("cannot itself be granted"));
    assert!(w.get::<CommandGrants>(mort).is_none());
}

#[test]
fn a_mortal_granted_an_allowlisted_command_can_use_it() {
    let (mut w, room) = world();
    allow(&mut w, r#"["PTell"]"#);
    let (boss, mut boss_rx) = person(&mut w, room, "Boss", 105);
    let (mort, mut mort_rx) = person(&mut w, room, "Mort", 20);

    dispatch(&mut w, mort, "ptell boss hi");
    assert!(drain(&mut mort_rx).contains("You can't do that."));

    dispatch(&mut w, boss, "grant mort command ptell");
    assert!(drain(&mut boss_rx).contains("Granted ptell to Mort"));
    assert_eq!(grants_of(&w, mort).grants[0].command, "ptell");
    dispatch(&mut w, mort, "ptell boss hi");
    assert!(!drain(&mut mort_rx).contains("You can't do that."));

    // Dropping it from the allowlist takes effect at the next use.
    allow(&mut w, "[]");
    dispatch(&mut w, mort, "ptell boss hi");
    assert!(drain(&mut mort_rx).contains("You can't do that."));
}

#[test]
fn a_demoted_accounts_staff_grants_are_ignored_at_use() {
    let (mut w, room) = world();
    allow(&mut w, "[]");
    // A mortal-role account carrying hand-edited / left-over grants.
    let (mort, mut rx) = person(&mut w, room, "Mort", 20);
    w.entity_mut(mort).insert(CommandGrants {
        grants: vec![entry("snoop"), entry("set")],
        revokes: vec![],
    });
    for line in ["snoop boss", "set mort level 105"] {
        dispatch(&mut w, mort, line);
        assert!(drain(&mut rx).contains("You can't do that."), "{line}");
    }
    // The same grants work while the holder is still staff.
    let (imm, mut imm_rx) = person(&mut w, room, "Imm", 100);
    w.entity_mut(imm).insert(CommandGrants {
        grants: vec![entry("snoop")],
        revokes: vec![],
    });
    dispatch(&mut w, imm, "snoop nobody");
    assert!(!drain(&mut imm_rx).contains("You can't do that."));
    // Demote: the account role (and so the effective role) drops to Player.
    {
        let mut a = w.get_mut::<Account>(imm).unwrap();
        a.account_role = UserRole::Player;
        a.role = UserRole::Player;
    }
    dispatch(&mut w, imm, "snoop nobody");
    assert!(drain(&mut imm_rx).contains("You can't do that."));
}

#[test]
fn a_raised_min_role_makes_a_mortal_grant_ignored() {
    // `Command` records are static, so build the "raised" copy by hand.
    let ptell = find("ptell");
    let raised = Command {
        min_role: UserRole::Coder,
        ..*ptell
    };
    let g = CommandGrants {
        grants: vec![entry("ptell")],
        revokes: vec![],
    };
    let list = vec!["ptell".to_string()];
    assert_eq!(
        grant_usability(ptell, UserRole::Player, Some(&g), &list),
        GrantUsability::Granted
    );
    assert_eq!(
        grant_usability(&raised, UserRole::Player, Some(&g), &list),
        GrantUsability::NotGranted
    );
    // Staff still hold it (the staff rule is the role, not the command rank).
    assert_eq!(
        grant_usability(&raised, UserRole::Builder, Some(&g), &list),
        GrantUsability::Granted
    );
    // A required permission also voids a mortal grant.
    let perm = Command {
        required_perm: Some(mud_db::enums::Permission::Build),
        ..raised
    };
    let low = Command {
        min_role: UserRole::Player,
        ..perm
    };
    assert_eq!(
        grant_usability(&low, UserRole::Player, Some(&g), &list),
        GrantUsability::NotGranted
    );
    // Delegation commands are never honoured, even for staff.
    let g = CommandGrants {
        grants: vec![entry("grant")],
        revokes: vec![],
    };
    assert_eq!(
        grant_usability(find("grant"), UserRole::Builder, Some(&g), &list),
        GrantUsability::NotGranted
    );
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
            resolve_abbrev("sen", UserRole::Builder, &[], grants, &[], None),
            Some(Abbrev::Command(c)) if c.names[0] == "send"
        )
    };
    assert!(hit(Some(&g)));
    assert!(!hit(None));
}

#[test]
fn nobody_can_edit_their_own_entries() {
    let (mut w, room) = world();
    let (coder, mut rx) = person(&mut w, room, "Coder", 104);
    // A revoke placed above the coder's own level by a higher staffer.
    let placed = CommandGrants {
        grants: vec![],
        revokes: vec![GrantEntry {
            command: "goto".into(),
            grantor: "Boss".into(),
            level: 105,
        }],
    };
    w.entity_mut(coder).insert(placed.clone());
    for line in [
        "grant coder clear",
        "grant coder command stat",
        "revoke coder command stat",
        "ungrant coder command goto",
        "ungrant coder command goto 105",
    ] {
        dispatch(&mut w, coder, line);
        let out = drain(&mut rx);
        assert!(
            out.contains("You cannot grant or revoke your own commands."),
            "{line}: {out}"
        );
        assert_eq!(grants_of(&w, coder), placed, "{line}");
    }
    // Listing your own is fine.
    dispatch(&mut w, coder, "grant coder list");
    assert!(drain(&mut rx).contains("Coder's revocations:"));
}

#[test]
fn authority_ignores_a_character_level_above_the_implementor_cap() {
    use super::grants::authority_level;
    let (mut w, room) = world();
    let (boss, mut boss_rx) = person(&mut w, room, "Boss", 105);
    let (giant, mut giant_rx) = person(&mut w, room, "Giant", 130);
    assert_eq!(authority_level(&w, giant), 105);
    assert_eq!(authority_level(&w, boss), 105);

    // Equal authority: neither can touch the other.
    dispatch(&mut w, giant, "revoke boss command stat");
    assert!(drain(&mut giant_rx).contains("You cannot grant or revoke Boss's commands."));
    dispatch(&mut w, boss, "revoke giant command stat");
    assert!(drain(&mut boss_rx).contains("You cannot grant or revoke Giant's commands."));
    assert!(w.get::<CommandGrants>(boss).is_none());
    assert!(w.get::<CommandGrants>(giant).is_none());

    // The role sets the band: a mortal-role account with a runaway level
    // stays below staff, and a coder's level cannot lift them past 104.
    let (mort, _rx) = person(&mut w, room, "Mort", 130);
    {
        let mut a = w.get_mut::<Account>(mort).unwrap();
        a.role = UserRole::Player;
        a.account_role = UserRole::Player;
    }
    assert_eq!(authority_level(&w, mort), 99);
    let (coder, _rx) = person(&mut w, room, "Coder", 104);
    w.get_mut::<Profile>(coder).unwrap().level = 130;
    w.get_mut::<Account>(coder).unwrap().role = UserRole::Coder;
    assert_eq!(authority_level(&w, coder), 104);
}

/// Every command a mortal could possibly be granted today: rank at or below
/// immortal, no permission requirement, and passing the hard checks, with the
/// allowlist set to "everything".
fn mortal_grantable() -> Vec<&'static str> {
    let all: Vec<String> = all_commands()
        .flat_map(|c| c.names.iter().map(|n| (*n).to_string()))
        .collect();
    let mut out: Vec<&'static str> = all_commands()
        .filter(|c| c.min_role != UserRole::Player)
        .filter(|c| grant_honoured(c, UserRole::Player, &all))
        .map(|c| c.names[0])
        .collect();
    out.sort_unstable();
    out
}

#[test]
fn a_mortal_can_only_ever_be_granted_harmless_communication() {
    // Even with every command name on the allowlist, only these two qualify.
    assert_eq!(mortal_grantable(), vec!["ptell", "wiznet"]);
}

#[test]
fn admin_commands_are_never_granted_to_a_mortal() {
    let (mut w, room) = world();
    let names = [
        "reject_name",
        "approve_name",
        "users",
        "aggrodebug",
        "goto",
        "snoop",
    ];
    let json = format!(
        "[{}]",
        names
            .iter()
            .map(|n| format!("\"{n}\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    allow(&mut w, &json);
    let (boss, mut boss_rx) = person(&mut w, room, "Boss", 105);
    let (mort, mut mort_rx) = person(&mut w, room, "Mort", 20);
    for cmd in &names[..4] {
        dispatch(&mut w, boss, &format!("grant mort command {cmd}"));
        let out = drain(&mut boss_rx);
        assert!(out.contains("mortals can only be granted"), "{cmd}: {out}");
    }
    assert!(w.get::<CommandGrants>(mort).is_none());

    // Hand-edited rows are ignored at use too, allowlist notwithstanding.
    w.entity_mut(mort).insert(CommandGrants {
        grants: names.iter().map(|n| entry(n)).collect(),
        revokes: vec![],
    });
    for line in [
        "reject_name boss Foo",
        "approve_name boss",
        "users",
        "aggrodebug",
    ] {
        dispatch(&mut w, mort, line);
        assert!(drain(&mut mort_rx).contains("You can't do that."), "{line}");
    }
    // The same commands stay with staff.
    let all: Vec<String> = names.iter().map(|n| (*n).to_string()).collect();
    for n in &names[..4] {
        assert!(grant_honoured(find(n), UserRole::Immortal, &all), "{n}");
        assert!(!grant_honoured(find(n), UserRole::Player, &all), "{n}");
    }
}

#[test]
fn the_deny_list_holds_even_for_a_recategorised_command() {
    let all = vec!["reject_name".to_string(), "ptell".to_string()];
    let reject = Command {
        category: crate::commands::Category::Communication,
        ..*find("reject_name")
    };
    assert!(!grant_honoured(&reject, UserRole::Player, &all));
    // A non-denied command moved out of Admin is grantable (the category rule
    // is the only thing that kept it out).
    let ptell = Command {
        category: crate::commands::Category::Communication,
        ..*find("ptell")
    };
    assert!(grant_honoured(&ptell, UserRole::Player, &all));
}
