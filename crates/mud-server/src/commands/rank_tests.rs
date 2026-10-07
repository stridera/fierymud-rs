//! Tests for level-derived staff rank (`mud_db::enums::effective_rank`),
//! the gates that consume it, and the guards on paths that set staff
//! levels. See `effective_rank` for the security reasoning.

use bevy_ecs::prelude::*;
use mud_db::enums::{UserRole, effective_rank};
use mud_world::{Account, LevelRow, LevelTable, Named, Online, Player, Profile};

use super::{Command, all_commands, command_permitted, dispatch};
use crate::DevMode;

/// Spawn a player the way `login::spawn_player` does: the cached
/// `Account.role` is the effective rank of (level, website role).
fn spawn(world: &mut World, name: &str, level: i32, account_role: UserRole) -> Entity {
    world
        .spawn((
            Player,
            Online,
            Named {
                name: name.to_string(),
            },
            Account {
                user_id: String::new(),
                character_id: format!("c-{name}"),
                role: effective_rank(level, account_role),
                account_role,
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
        .id()
}

fn cmd(name: &str) -> &'static Command {
    all_commands()
        .find(|c| c.names.contains(&name))
        .unwrap_or_else(|| panic!("no command {name}"))
}

fn level_table(max: i32) -> LevelTable {
    LevelTable {
        rows: (1..=max)
            .map(|level| LevelRow {
                level,
                name: None,
                exp_required: level * 10,
                hp_gain: 1,
                stamina_gain: 1,
                is_immortal: level >= 100,
                permissions: vec![],
            })
            .collect(),
    }
}

fn level_of(world: &World, e: Entity) -> i32 {
    world.get::<Profile>(e).unwrap().level
}

#[test]
fn unlinked_l105_passes_implementor_gated_commands() {
    let mut world = World::new();
    // Unlinked legacy god: website role is Player, level is 105.
    let god = spawn(&mut world, "Chinok", 105, UserRole::Player);
    for name in ["set", "advance", "skillset", "goto", "transfer", "teleport"] {
        assert!(
            command_permitted(&world, god, cmd(name)),
            "L105 unlinked should be permitted `{name}`"
        );
    }
    // And it actually executes: `set` mutates.
    dispatch(&mut world, god, "set me xp 77");
    assert_eq!(world.get::<Profile>(god).unwrap().experience, 77);
}

#[test]
fn unlinked_l104_is_coder_not_implementor() {
    let mut world = World::new();
    let coder = spawn(&mut world, "Daedela", 104, UserRole::Player);
    assert_eq!(world.get::<Account>(coder).unwrap().role, UserRole::Coder);
    assert!(
        command_permitted(&world, coder, cmd("areload")),
        "Coder cmd"
    );
    assert!(command_permitted(&world, coder, cmd("goto")), "Builder cmd");
    // `set` and `advance` are Implementor-only.
    assert!(!command_permitted(&world, coder, cmd("set")));
    assert!(!command_permitted(&world, coder, cmd("advance")));
    dispatch(&mut world, coder, "set me xp 77");
    assert_eq!(world.get::<Profile>(coder).unwrap().experience, 0);
}

#[test]
fn linked_player_account_with_l100_character_is_immortal() {
    let mut world = World::new();
    let imm = spawn(&mut world, "Laoris", 100, UserRole::Player);
    let acct = world.get::<Account>(imm).unwrap();
    assert_eq!(acct.role, UserRole::Immortal);
    assert_eq!(acct.account_role, UserRole::Player);
    assert!(
        command_permitted(&world, imm, cmd("wiznet")),
        "Immortal cmd"
    );
    assert!(!command_permitted(&world, imm, cmd("goto")), "Builder cmd");
    // A mortal with the same account role gets nothing.
    let mortal = spawn(&mut world, "Mortal", 99, UserRole::Player);
    assert!(!command_permitted(&world, mortal, cmd("wiznet")));
}

#[test]
fn account_role_is_never_lowered_by_level() {
    let mut world = World::new();
    let p = spawn(&mut world, "Linked", 5, UserRole::Implementor);
    assert_eq!(world.get::<Account>(p).unwrap().role, UserRole::Implementor);
    world.get_mut::<Account>(p).unwrap().refresh_rank(5);
    assert_eq!(world.get::<Account>(p).unwrap().role, UserRole::Implementor);
}

#[test]
fn non_implementor_cannot_advance_to_staff_level() {
    let mut world = World::new();
    world.insert_resource(level_table(105));
    // DevMode waives min_role for everyone; the level guard must hold anyway.
    world.insert_resource(DevMode(true));
    let coder = spawn(&mut world, "Coder", 104, UserRole::Player);
    let victim = spawn(&mut world, "Victim", 50, UserRole::Player);
    dispatch(&mut world, coder, "advance Victim 100");
    assert_eq!(
        level_of(&world, victim),
        50,
        "advance to 100 must be refused"
    );
    assert_eq!(world.get::<Account>(victim).unwrap().role, UserRole::Player);
    // A Coder may still advance mortals below the staff threshold.
    dispatch(&mut world, coder, "advance Victim 60");
    assert_eq!(level_of(&world, victim), 60);
    // Refusal is audited.
    let log = world.resource::<crate::commands::AdminAuditLog>();
    assert!(
        log.entries
            .iter()
            .any(|e| e.verb == "staff_level_grant_denied" && e.args == "Victim 100"),
        "denied grant should be audit-logged"
    );
}

#[test]
fn non_implementor_cannot_set_staff_level_even_in_dev_mode() {
    let mut world = World::new();
    world.insert_resource(DevMode(true));
    let coder = spawn(&mut world, "Coder", 104, UserRole::Player);
    dispatch(&mut world, coder, "set me level 105");
    assert_eq!(level_of(&world, coder), 104);
    assert_eq!(world.get::<Account>(coder).unwrap().role, UserRole::Coder);
}

#[test]
fn implementor_advance_and_set_grant_staff_rank_and_audit() {
    let mut world = World::new();
    world.insert_resource(level_table(105));
    let imp = spawn(&mut world, "Imp", 105, UserRole::Player);
    let target = spawn(&mut world, "Target", 50, UserRole::Player);

    dispatch(&mut world, imp, "advance Target 100");
    assert_eq!(level_of(&world, target), 100);
    // Cached rank refreshed on level change, no relog needed.
    assert_eq!(
        world.get::<Account>(target).unwrap().role,
        UserRole::Immortal
    );
    assert!(command_permitted(&world, target, cmd("wiznet")));

    // `set level` lowering drops level-derived rank again.
    dispatch(&mut world, imp, "set me level 50");
    // (`set <name>` targets the room; `me` is enough for the refresh path.)
    assert_eq!(level_of(&world, imp), 50);
    assert_eq!(world.get::<Account>(imp).unwrap().role, UserRole::Player);

    let log = world.resource::<crate::commands::AdminAuditLog>();
    assert!(
        log.entries
            .iter()
            .any(|e| e.verb == "staff_level_grant" && e.args == "Target 100"),
        "granted staff level should be audit-logged"
    );
}

#[test]
fn xp_cannot_level_past_99() {
    let mut world = World::new();
    // Level table deliberately defines levels above 99.
    world.insert_resource(level_table(110));
    let p = spawn(&mut world, "Grinder", 98, UserRole::Player);
    world.get_mut::<Profile>(p).unwrap().experience = i32::MAX;
    crate::combat::check_level_up(&mut world, p);
    assert_eq!(level_of(&world, p), 99, "XP level-up stops at 99");
    crate::combat::check_level_up(&mut world, p);
    assert_eq!(level_of(&world, p), 99);
    assert_eq!(world.get::<Account>(p).unwrap().role, UserRole::Player);
}
