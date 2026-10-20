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
/// `Account.role` is the effective rank of the level.
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
                role: effective_rank(level),
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
            "L105 unlinked should be permitted '{name}'"
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
    // `slay` and `apply` are Implementor-only; `set` (legacy 101) is not.
    assert!(!command_permitted(&world, coder, cmd("slay")));
    assert!(!command_permitted(&world, coder, cmd("apply")));
    assert!(command_permitted(&world, coder, cmd("set")));
    assert!(command_permitted(&world, coder, cmd("advance")));
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
    assert!(!command_permitted(&world, weak, cmd("advance")));
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
    crate::combat::after_level_change(&mut world, target);
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

fn posture_of(world: &World, e: Entity) -> mud_world::PostureKind {
    world.get::<mud_world::Posture>(e).unwrap().0
}

fn force_audited(world: &World, verb: &str) -> bool {
    world
        .resource::<crate::commands::AdminAuditLog>()
        .entries
        .iter()
        .any(|e| e.verb == verb)
}

#[test]
fn force_respects_level() {
    let mut world = World::new();
    world.insert_resource(DevMode(true));
    let l104 = spawn(&mut world, "Coder", 104, UserRole::Player);
    let l105 = spawn(&mut world, "Boss", 105, UserRole::Player);
    let peer = spawn(&mut world, "Peer", 104, UserRole::Player);
    let low = spawn(&mut world, "Low", 5, UserRole::Player);
    for e in [l104, l105, peer, low] {
        world
            .entity_mut(e)
            .insert(mud_world::Posture(mud_world::PostureKind::Standing));
    }
    // Higher, equal rank: refused and audited.
    dispatch(&mut world, l104, "force Boss sit");
    assert_eq!(posture_of(&world, l105), mud_world::PostureKind::Standing);
    assert!(force_audited(&world, "force_denied"));
    dispatch(&mut world, l104, "force Peer sit");
    assert_eq!(posture_of(&world, peer), mud_world::PostureKind::Standing);
    // A level-5 DevMode player can't make a god act.
    dispatch(&mut world, low, "force Coder sit");
    assert_eq!(posture_of(&world, l104), mud_world::PostureKind::Standing);
    // Strictly higher forcing lower works.
    dispatch(&mut world, l105, "force Coder sit");
    assert_eq!(posture_of(&world, l104), mud_world::PostureKind::Sitting);
    // Self-force is allowed.
    dispatch(&mut world, low, "force Low sit");
    assert_eq!(posture_of(&world, low), mud_world::PostureKind::Sitting);
}

fn frozen(world: &World, e: Entity) -> bool {
    world.get::<mud_world::Frozen>(e).is_some()
}

#[test]
fn freeze_respects_level_and_thaw_does_too() {
    let mut world = World::new();
    let builder = spawn(&mut world, "Builder", 102, UserRole::Player);
    let peer = spawn(&mut world, "Peer", 102, UserRole::Player);
    let boss = spawn(&mut world, "Boss", 105, UserRole::Player);
    let mortal = spawn(&mut world, "Mortal", 10, UserRole::Player);
    // A Builder can't freeze an Implementor or a peer.
    dispatch(&mut world, builder, "freeze Boss");
    dispatch(&mut world, builder, "freeze Peer");
    assert!(!frozen(&world, boss));
    assert!(!frozen(&world, peer));
    assert!(force_audited(&world, "freeze_denied"));
    // A mortal below them is fine, and thawing works for the same rank.
    dispatch(&mut world, builder, "freeze Mortal");
    assert!(frozen(&world, mortal));
    dispatch(&mut world, builder, "freeze Mortal");
    assert!(!frozen(&world, mortal));
    // The Implementor can freeze the Builder; the Builder can't undo it
    // by toggling a peer-or-higher.
    dispatch(&mut world, boss, "freeze Builder");
    assert!(frozen(&world, builder));
}

#[test]
fn dc_respects_level() {
    use crate::commands::Connection;
    let mut world = World::new();
    let builder = spawn(&mut world, "Builder", 102, UserRole::Player);
    let boss = spawn(&mut world, "Boss", 105, UserRole::Player);
    let mortal = spawn(&mut world, "Mortal", 10, UserRole::Player);
    let mut rxs = Vec::new();
    for e in [boss, mortal] {
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
        rxs.push(rx);
        world.entity_mut(e).insert(Connection(tx));
    }
    dispatch(&mut world, builder, "dc Boss");
    assert!(world.get::<Connection>(boss).is_some());
    assert!(force_audited(&world, "dc_denied"));
    dispatch(&mut world, builder, "dc Mortal");
    assert!(world.get::<Connection>(mortal).is_none());
}

#[test]
fn reroll_respects_level_and_uses_the_creation_roller() {
    let mut world = World::new();
    let builder = spawn(&mut world, "Builder", 102, UserRole::Player);
    let boss = spawn(&mut world, "Boss", 105, UserRole::Player);
    let mortal = spawn(&mut world, "Mortal", 10, UserRole::Player);
    let marker = mud_world::CoreStats {
        strength: 99,
        dexterity: 99,
        constitution: 99,
        intelligence: 99,
        wisdom: 99,
        charisma: 99,
    };
    world.entity_mut(boss).insert(marker);
    world.entity_mut(mortal).insert(marker);
    dispatch(&mut world, builder, "reroll Boss");
    assert_eq!(
        world.get::<mud_world::CoreStats>(boss).unwrap().strength,
        99
    );
    assert!(force_audited(&world, "reroll_denied"));
    dispatch(&mut world, builder, "reroll Mortal");
    let cs = *world.get::<mud_world::CoreStats>(mortal).unwrap();
    assert!(
        [
            cs.strength,
            cs.dexterity,
            cs.constitution,
            cs.intelligence,
            cs.wisdom,
            cs.charisma
        ]
        .iter()
        .all(|v| (30..=100).contains(v)),
        "creation roller is the legacy 0..100 natural roll: {cs:?}"
    );
}

#[test]
fn set_level_and_xp_need_coder_and_players_must_be_outranked() {
    let mut world = World::new();
    world.insert_resource(DevMode(true));
    world.insert_resource(level_table(110));
    let room = world.spawn_empty().id();
    let builder = spawn(&mut world, "Builder", 102, UserRole::Player);
    let coder = spawn(&mut world, "Coder", 104, UserRole::Player);
    let mortal = spawn(&mut world, "Mortal", 10, UserRole::Player);
    let boss = spawn(&mut world, "Boss", 105, UserRole::Player);
    for e in [builder, coder, mortal, boss] {
        world.entity_mut(e).insert(mud_world::Located(room));
    }
    // A Builder can't mint a level-101 mortal, or give XP, to anyone.
    dispatch(&mut world, builder, "set Mortal level 101");
    assert_eq!(level_of(&world, mortal), 10);
    dispatch(&mut world, builder, "set Mortal xp 5000");
    assert_eq!(world.get::<Profile>(mortal).unwrap().experience, 0);
    dispatch(&mut world, builder, "set me level 50");
    assert_eq!(level_of(&world, builder), 102);
    assert!(force_audited(&world, "set_denied"));
    // Other fields still work on a lower player, but not on a higher one.
    dispatch(&mut world, builder, "set Mortal align 100");
    assert_eq!(world.get::<Profile>(mortal).unwrap().experience, 0);
    world
        .entity_mut(boss)
        .insert(mud_world::Health { hp: 10, max: 10 });
    dispatch(&mut world, builder, "set Boss hp 1");
    assert_eq!(world.get::<mud_world::Health>(boss).unwrap().hp, 10);
    // A Coder can set level, still strictly below their own.
    dispatch(&mut world, coder, "set Mortal level 50");
    assert_eq!(level_of(&world, mortal), 50);
    dispatch(&mut world, coder, "set Mortal level 104");
    assert_eq!(level_of(&world, mortal), 50);
}

#[test]
fn non_typed_yes_does_not_confirm_rent() {
    use crate::commands::{CommandOrigin, PendingRentConfirm, with_command_origin};
    let mut world = World::new();
    world.insert_resource(DevMode(true));
    world.init_resource::<crate::commands::SocialRegistry>();
    let p = spawn(&mut world, "Renter", 20, UserRole::Player);
    let boss = spawn(&mut world, "Boss", 105, UserRole::Player);
    world.entity_mut(p).insert((
        mud_world::Wealth(1_000_000),
        PendingRentConfirm {
            tier_name: "Suite".into(),
            tier: 2,
            fee_gp: 5,
        },
    ));
    let paid = |w: &World| w.get::<mud_world::Wealth>(p).unwrap().0 < 1_000_000;
    run_lua_command(&mut world, p, "yes");
    assert!(!paid(&world), "script yes must not pay rent");
    assert!(world.get::<PendingRentConfirm>(p).is_some());
    dispatch(&mut world, boss, "force Renter yes");
    assert!(!paid(&world), "forced yes must not pay rent");
    with_command_origin(CommandOrigin::Script, || dispatch(&mut world, p, "no"));
    assert!(
        world.get::<PendingRentConfirm>(p).is_some(),
        "non-typed no must not cancel either"
    );
    dispatch(&mut world, p, "yes");
    assert!(paid(&world), "typed yes confirms");
    assert!(world.get::<PendingRentConfirm>(p).is_none());
}

#[test]
fn staff_xp_does_not_burn_repose_or_rest_source() {
    use mud_world::RestState;
    let mut world = World::new();
    world.insert_resource(level_table(110));
    let god = spawn(&mut world, "God", 104, UserRole::Player);
    let rest = RestState {
        repose: 5000,
        source: mud_db::enums::RestSource::Inn,
        tier: 2,
    };
    world.entity_mut(god).insert(rest);
    assert_eq!(crate::rest::award_experience(&mut world, god, 1000), 0);
    let after = world.get::<RestState>(god).unwrap();
    assert_eq!(after.repose, 5000);
    assert_eq!(after.source, mud_db::enums::RestSource::Inn);
    assert_eq!(after.tier, 2);
}

#[test]
fn forced_and_scripted_quit_are_refused() {
    let mut world = World::new();
    world.insert_resource(DevMode(true));
    let boss = spawn(&mut world, "Boss", 105, UserRole::Player);
    let bob = spawn(&mut world, "Bob", 5, UserRole::Player);
    dispatch(&mut world, boss, "force Bob quit");
    assert!(world.get::<crate::commands::Quitting>(bob).is_none());
    assert!(world.get_entity(bob).is_ok());
    run_lua_command(&mut world, bob, "quit");
    assert!(world.get::<crate::commands::Quitting>(bob).is_none());
    // Typed by the player themself still works.
    dispatch(&mut world, bob, "quit");
    assert!(world.get::<crate::commands::Quitting>(bob).is_some());
}

/// `set <player> gold N` sets N gold coins (not N copper), as `help set`
/// says; the other denominations keep their count, `wealth` is the raw
/// copper total. Each coin field is absolute: repeating it changes nothing
/// and setting 0 undoes it. Gold, silver and copper hold 0-9 in the
/// canonical split, so a larger count clamps instead of carrying (carrying
/// made the second `set gold 20` add 20 again).
#[test]
fn set_coin_fields_use_coin_units() {
    let mut world = World::new();
    let god = spawn(&mut world, "Chinok", 105, UserRole::Player);
    let coins = |world: &World| world.get::<mud_world::Wealth>(god).map_or(0, |w| w.0);

    dispatch(&mut world, god, "set me gold 4");
    assert_eq!(coins(&world), 400, "4 gold, not 4 copper");
    dispatch(&mut world, god, "set me silver 5");
    assert_eq!(coins(&world), 450, "gold untouched");
    dispatch(&mut world, god, "set me platinum 3");
    assert_eq!(coins(&world), 3_450, "gold and silver untouched");
    dispatch(&mut world, god, "set me copper 7");
    assert_eq!(coins(&world), 3_457);
    dispatch(&mut world, god, "set me platinum 1000");
    assert_eq!(coins(&world), 1_000_457, "platinum is unbounded");
    dispatch(&mut world, god, "set me wealth 1234");
    assert_eq!(coins(&world), 1_234, "wealth is raw copper");
    dispatch(&mut world, god, "set me gold -5");
    assert_eq!(
        coins(&world),
        1_034,
        "a negative count clamps at zero coins of that kind"
    );
}

#[test]
fn set_coin_fields_are_absolute_and_repeatable() {
    let mut world = World::new();
    let god = spawn(&mut world, "Chinok", 105, UserRole::Player);
    let coins = |world: &World| world.get::<mud_world::Wealth>(god).map_or(0, |w| w.0);
    dispatch(&mut world, god, "set me wealth 1234");

    for field in ["platinum 7", "gold 20", "silver 20", "copper 20", "gold 3"] {
        dispatch(&mut world, god, &format!("set me {field}"));
        let once = coins(&world);
        dispatch(&mut world, god, &format!("set me {field}"));
        dispatch(&mut world, god, &format!("set me {field}"));
        assert_eq!(coins(&world), once, "set {field} repeated changes nothing");
    }
    // 20 gold, silver, copper clamped to 9 each; platinum 7, then gold 3.
    assert_eq!(coins(&world), 7_000 + 3 * 100 + 90 + 9);

    // Setting 0 undoes it: only that denomination goes.
    dispatch(&mut world, god, "set me gold 0");
    assert_eq!(coins(&world), 7_099);
    dispatch(&mut world, god, "set me platinum 0");
    assert_eq!(coins(&world), 99);
    dispatch(&mut world, god, "set me silver 0");
    dispatch(&mut world, god, "set me copper 0");
    assert_eq!(coins(&world), 0);

    dispatch(&mut world, god, "set me gold 20");
    assert_eq!(coins(&world), 900, "clamped to the 9 gold a purse holds");
    dispatch(&mut world, god, "set me gold 0");
    assert_eq!(coins(&world), 0);
}

// --- Demotion (#174): privileges follow the current level, never a stale grant.

const SNEAK: i32 = 7; // a skill outside the character's class kit

fn world_with_catalog() -> World {
    use mud_db::abilities::AbilityKind;
    let mut world = World::new();
    let mut catalog = mud_world::AbilityCatalog::default();
    catalog.by_name.insert(
        "sneak".to_string(),
        super::test_support::ability_def(SNEAK, "Sneak", AbilityKind::Skill),
    );
    world.insert_resource(catalog);
    world.insert_resource(level_table(105));
    world
}

#[test]
fn demoted_immortal_loses_wiz_commands_and_god_skills() {
    let mut world = world_with_catalog();
    let imp = spawn(&mut world, "Imp", 105, UserRole::Player);
    // Website account role is Immortal too (the Muditor promotion path): it
    // must not keep the rank once the character is set back to 84.
    let target = spawn(&mut world, "Mukashi", 50, UserRole::Immortal);
    assert_eq!(
        world.get::<Account>(target).unwrap().account_role,
        UserRole::Immortal
    );

    dispatch(&mut world, imp, "advance Mukashi 100");
    assert!(command_permitted(&world, target, cmd("wiznet")));
    assert_eq!(mud_world::proficiency_of(&world, target, SNEAK), Some(1000));

    // Staff demote to 84 through the same level-change path `set level` uses.
    world.get_mut::<Profile>(target).unwrap().level = 84;
    crate::combat::after_level_change(&mut world, target);
    assert_eq!(world.get::<Account>(target).unwrap().role, UserRole::Player);
    assert!(!command_permitted(&world, target, cmd("wiznet")));
    assert!(!command_permitted(&world, target, cmd("goto")));
    assert!(!mud_world::has_immortal_role(&world, target));
    // The non-class skill is gone and nothing is left at god proficiency.
    assert_eq!(mud_world::proficiency_of(&world, target, SNEAK), None);
    assert!(mud_world::known_abilities_of(&world, target).is_none_or(|k| !k.has_any(SNEAK)));
}

#[test]
fn demotion_between_staff_tiers_drops_higher_tier_commands() {
    let mut world = world_with_catalog();
    let coder = spawn(&mut world, "Coder", 104, UserRole::Player);
    assert!(
        command_permitted(&world, coder, cmd("areload")),
        "Coder cmd"
    );
    world.get_mut::<Profile>(coder).unwrap().level = 101;
    crate::combat::after_level_change(&mut world, coder);
    assert_eq!(world.get::<Account>(coder).unwrap().role, UserRole::Builder);
    assert!(
        !command_permitted(&world, coder, cmd("areload")),
        "Coder cmd"
    );
    assert!(!command_permitted(&world, coder, cmd("advance")));
    assert!(command_permitted(&world, coder, cmd("goto")), "Builder cmd");
    assert!(
        command_permitted(&world, coder, cmd("wiznet")),
        "Immortal cmd"
    );
}

#[test]
fn stale_stored_permission_is_dropped_on_demotion_and_kept_when_not_level_granted() {
    use mud_db::enums::Permission;
    let mut world = world_with_catalog();
    let mut table = level_table(105);
    table.rows[99].permissions = vec![Permission::Summon]; // level 100
    world.insert_resource(table);
    let p = spawn(&mut world, "Stale", 84, UserRole::Player);
    // Summon restored from the database after a demotion, plus an explicit
    // grant that no level row confers.
    world.get_mut::<Account>(p).unwrap().perms = vec![Permission::Summon, Permission::Build];
    crate::combat::after_level_change(&mut world, p);
    assert_eq!(
        world.get::<Account>(p).unwrap().perms,
        vec![Permission::Build]
    );
}

#[test]
fn account_role_does_not_confer_in_game_rank() {
    let mut world = world_with_catalog();
    // A mortal alt on a website Implementor account is a plain mortal.
    let alt = spawn(&mut world, "Alt", 20, UserRole::Implementor);
    assert_eq!(world.get::<Account>(alt).unwrap().role, UserRole::Player);
    assert!(!command_permitted(&world, alt, cmd("advance")));
    assert!(!mud_world::bypasses_restrictions(&world, alt));
}
