//! Tests for level-derived staff rank (`mud_db::enums::effective_rank`),
//! the gates that consume it, and the guards on paths that set staff
//! levels. See `effective_rank` for the security reasoning.

use bevy_ecs::prelude::*;
use mud_db::enums::{UserRole, effective_rank};
use mud_world::{Account, LevelRow, LevelTable, Named, Online, Player, Profile};

use crate::combat::{LevelChangeDenied, authorize_level_change};

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

fn denied_in_audit_log(world: &World, args: &str) -> bool {
    world
        .resource::<crate::commands::AdminAuditLog>()
        .entries
        .iter()
        .any(|e| e.verb == "staff_level_grant_denied" && e.args == args)
}

#[test]
fn advance_target_must_be_strictly_below_actor_level() {
    let mut world = World::new();
    world.insert_resource(level_table(105));
    // DevMode waives min_role for everyone; the level rules must hold anyway.
    world.insert_resource(DevMode(true));
    let coder = spawn(&mut world, "Coder", 104, UserRole::Player);
    let victim = spawn(&mut world, "Victim", 50, UserRole::Player);
    // 104 -> 105 and 104 -> 104 denied; 105 would mint an Implementor.
    dispatch(&mut world, coder, "advance Victim 105");
    assert_eq!(level_of(&world, victim), 50);
    dispatch(&mut world, coder, "advance Victim 104");
    assert_eq!(level_of(&world, victim), 50);
    assert!(denied_in_audit_log(&world, "Victim 105"));
    assert!(denied_in_audit_log(&world, "Victim 104"));
    // 104 -> 103 is fine, and 104 can set up to 103.
    dispatch(&mut world, coder, "advance Victim 103");
    assert_eq!(level_of(&world, victim), 103);
    assert_eq!(
        world.get::<Account>(victim).unwrap().role,
        UserRole::HeadBuilder
    );
}

#[test]
fn implementor_can_promote_to_104_but_not_105() {
    let mut world = World::new();
    world.insert_resource(level_table(105));
    let imp = spawn(&mut world, "Imp", 105, UserRole::Player);
    let target = spawn(&mut world, "Target", 50, UserRole::Player);
    dispatch(&mut world, imp, "advance Target 105");
    assert_eq!(level_of(&world, target), 50, "no Implementors in game");
    dispatch(&mut world, imp, "advance Target 100");
    assert_eq!(level_of(&world, target), 100);
    assert_eq!(
        world.get::<Account>(target).unwrap().role,
        UserRole::Immortal
    );
    dispatch(&mut world, imp, "advance Target 104");
    assert_eq!(level_of(&world, target), 104);
    assert_eq!(world.get::<Account>(target).unwrap().role, UserRole::Coder);
    // `set` follows the same ceiling; the target (104) is below the actor.
    // (`set <name>` resolves targets in the actor's room, which the test
    // world doesn't model, so exercise `set` through `me` below.)
    let log = world.resource::<crate::commands::AdminAuditLog>();
    assert!(
        log.entries
            .iter()
            .any(|e| e.verb == "staff_level_grant" && e.args == "Target 100"),
        "granted staff level should be audit-logged"
    );
}

#[test]
fn set_level_ceiling_and_self_lowering() {
    let mut world = World::new();
    world.insert_resource(DevMode(true));
    let coder = spawn(&mut world, "Coder", 104, UserRole::Player);
    // Raising oneself to or above one's own level is refused.
    dispatch(&mut world, coder, "set me level 105");
    assert_eq!(level_of(&world, coder), 104);
    dispatch(&mut world, coder, "set me level 104");
    assert_eq!(level_of(&world, coder), 104);
    assert_eq!(world.get::<Account>(coder).unwrap().role, UserRole::Coder);
    assert!(denied_in_audit_log(&world, "Coder 105"));

    // An Implementor can lower themselves (rule 3 exempts actor == target).
    let imp = spawn(&mut world, "Imp", 105, UserRole::Player);
    dispatch(&mut world, imp, "set me level 50");
    assert_eq!(level_of(&world, imp), 50);
    assert_eq!(world.get::<Account>(imp).unwrap().role, UserRole::Player);
}

#[test]
fn cannot_change_level_of_peer_or_superior() {
    let mut world = World::new();
    world.insert_resource(level_table(105));
    world.insert_resource(DevMode(true));
    let coder = spawn(&mut world, "Coder", 104, UserRole::Player);
    let peer = spawn(&mut world, "Peer", 104, UserRole::Player);
    let boss = spawn(&mut world, "Boss", 105, UserRole::Player);
    for target in ["Peer", "Boss"] {
        dispatch(&mut world, coder, &format!("advance {target} 105"));
        dispatch(&mut world, coder, &format!("advance {target} 103"));
    }
    assert_eq!(level_of(&world, peer), 104);
    assert_eq!(level_of(&world, boss), 105);
    // Peer/boss at or above actor: advance only raises anyway, so 104 -> 103
    // is refused as "not a raise"; the explicit target-outranks case is
    // covered by `authorize_level_change` below.
    assert_eq!(
        authorize_level_change(&mut world, coder, peer, "Peer", 50),
        Err(LevelChangeDenied::TargetOutranks)
    );
    assert_eq!(
        authorize_level_change(&mut world, coder, boss, "Boss", 50),
        Err(LevelChangeDenied::TargetOutranks)
    );
    let low = spawn(&mut world, "Low", 50, UserRole::Player);
    assert_eq!(
        authorize_level_change(&mut world, coder, low, "Low", 103),
        Ok(())
    );
}

#[test]
fn ceiling_uses_character_level_not_account_role() {
    let mut world = World::new();
    world.insert_resource(level_table(105));
    // Website Implementor playing a level-1 character.
    let weak = spawn(&mut world, "Weak", 1, UserRole::Implementor);
    let victim = spawn(&mut world, "Victim", 1, UserRole::Player);
    assert!(command_permitted(&world, weak, cmd("advance")));
    dispatch(&mut world, weak, "advance Victim 105");
    dispatch(&mut world, weak, "advance Victim 50");
    assert_eq!(level_of(&world, victim), 1);
}

#[test]
fn level_changes_refuse_mobs() {
    let mut world = World::new();
    world.insert_resource(DevMode(true));
    let imp = spawn(&mut world, "Imp", 105, UserRole::Player);
    let mob = world
        .spawn((
            mud_world::Mob,
            Named { name: "rat".into() },
            Profile {
                level: 3,
                class_id: None,
                race: "Human".into(),
                experience: 0,
                gender: "neutral".into(),
            },
        ))
        .id();
    assert_eq!(
        authorize_level_change(&mut world, imp, mob, "rat", 4),
        Err(LevelChangeDenied::NotAPlayer)
    );
}

#[test]
fn level_table_perms_follow_level_both_ways() {
    use mud_db::enums::Permission;
    let mut world = World::new();
    let mut table = level_table(105);
    table.rows[99].permissions = vec![Permission::Summon]; // level 100
    world.insert_resource(table);
    let imp = spawn(&mut world, "Imp", 105, UserRole::Player);
    let target = spawn(&mut world, "Target", 50, UserRole::Player);
    dispatch(&mut world, imp, "advance Target 100");
    assert!(
        world
            .get::<Account>(target)
            .unwrap()
            .perms
            .contains(&Permission::Summon)
    );
    // Lowering the level removes the level-conferred permission.
    world.get_mut::<Profile>(target).unwrap().level = 50;
    crate::combat::after_level_change(&mut world, target, 100);
    assert!(
        !world
            .get::<Account>(target)
            .unwrap()
            .perms
            .contains(&Permission::Summon)
    );
    assert_eq!(world.get::<Account>(target).unwrap().role, UserRole::Player);
}

/// Spawn a builder-owned trigger scenario: a Lua body running on `actor`
/// queues `line` through `actor:command`, then the outbox drains.
fn run_lua_command(world: &mut World, actor: Entity, line: &str) {
    let mut host = mud_script::LuaHost::default();
    host.exec_for_actor(world, actor, &format!("actor:command({line:?})"))
        .expect("lua ok");
    crate::commands::drain_lua_outbox(world);
}

#[test]
fn script_queued_staff_commands_are_refused_even_for_implementors() {
    let mut world = World::new();
    world.insert_resource(level_table(105));
    world.insert_resource(DevMode(true));
    // The Implementor walks into a trigger room; a builder's script makes
    // *them* run `advance`.
    let imp = spawn(&mut world, "Imp", 105, UserRole::Player);
    let builder = spawn(&mut world, "Builder", 40, UserRole::Builder);
    run_lua_command(&mut world, imp, "advance Builder 104");
    assert_eq!(level_of(&world, builder), 40);
    // Any staff command, not just the level-setting ones.
    run_lua_command(&mut world, imp, "set me xp 77");
    assert_eq!(world.get::<Profile>(imp).unwrap().experience, 0);
    // The same line typed directly still works.
    dispatch(&mut world, imp, "set me xp 77");
    assert_eq!(world.get::<Profile>(imp).unwrap().experience, 77);
    // Mortal-level commands from scripts keep working.
    run_lua_command(&mut world, imp, "stand");
}

#[test]
fn command_permitted_script_origin_gate() {
    use crate::commands::{CommandOrigin, with_command_origin};
    let mut world = World::new();
    let imp = spawn(&mut world, "Imp", 105, UserRole::Player);
    assert!(command_permitted(&world, imp, cmd("set")));
    with_command_origin(CommandOrigin::Script, || {
        assert!(!command_permitted(&world, imp, cmd("set")));
        assert!(!command_permitted(&world, imp, cmd("goto")));
        assert!(command_permitted(&world, imp, cmd("stand")));
    });
    assert!(
        command_permitted(&world, imp, cmd("set")),
        "origin restored"
    );
}

#[test]
fn forced_staff_commands_are_refused() {
    let mut world = World::new();
    world.insert_resource(level_table(105));
    world.insert_resource(DevMode(true));
    let coder = spawn(&mut world, "Coder", 104, UserRole::Player);
    let boss = spawn(&mut world, "Boss", 105, UserRole::Player);
    let low = spawn(&mut world, "Low", 10, UserRole::Player);
    // The forcer's lower rank would be bypassed by running as the victim.
    dispatch(&mut world, coder, "force Boss advance Low 104");
    assert_eq!(level_of(&world, low), 10);
    dispatch(&mut world, coder, "force Boss set me xp 5");
    assert_eq!(world.get::<Profile>(boss).unwrap().experience, 0);
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

#[test]
fn xp_never_levels_or_moves_staff() {
    let mut world = World::new();
    world.insert_resource(level_table(110));
    for level in [100, 104, 105] {
        let god = spawn(&mut world, &format!("God{level}"), level, UserRole::Player);
        // Kill / group / quest / rest XP all go through award_experience.
        assert_eq!(crate::rest::award_experience(&mut world, god, 1_000_000), 0);
        world.get_mut::<Profile>(god).unwrap().experience = i32::MAX;
        crate::combat::check_level_up(&mut world, god);
        assert_eq!(level_of(&world, god), level, "L{level} must not level");
    }
    let god = spawn(&mut world, "Fresh", 104, UserRole::Player);
    crate::rest::award_experience(&mut world, god, 500);
    assert_eq!(world.get::<Profile>(god).unwrap().experience, 0);
}

#[test]
fn lua_award_exp_does_not_move_a_god() {
    let mut world = World::new();
    world.insert_resource(level_table(110));
    let god = spawn(&mut world, "God", 104, UserRole::Player);
    let mortal = spawn(&mut world, "Mortal", 10, UserRole::Player);
    let mut host = mud_script::LuaHost::default();
    host.exec_for_actor(&mut world, god, "actor:award_exp(100000)")
        .expect("lua");
    host.exec_for_actor(&mut world, mortal, "actor:award_exp(25)")
        .expect("lua");
    assert_eq!(world.get::<Profile>(god).unwrap().experience, 0);
    assert_eq!(level_of(&world, god), 104);
    assert_eq!(world.get::<Profile>(mortal).unwrap().experience, 25);
}
