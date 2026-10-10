//! The world-changing bindings without a server attached: argument forms
//! and the safety rules that live in this crate (players are never
//! destroyed, an actor is never its own target). The server-backed
//! behavior (peaceful rooms, death handling, extraction) is tested in
//! mud-server.

use super::*;

fn world() -> (World, Entity, Entity, Entity) {
    let mut world = World::new();
    let room = world.spawn_empty().id();
    let spawn = |world: &mut World, name: &str, player: bool| {
        let mut e = world.spawn((
            Named { name: name.into() },
            Keywords(vec![name.to_ascii_lowercase()]),
            Located(room),
            Health { hp: 20, max: 20 },
        ));
        if player {
            e.insert(Player);
        } else {
            e.insert(Mob);
        }
        e.id()
    };
    let me = spawn(&mut world, "Warden", false);
    let bob = spawn(&mut world, "Bob", true);
    (world, room, me, bob)
}

fn run(world: &mut World, me: Entity, actor: Entity, body: &str) {
    LuaHost::new()
        .exec_for_listener_with_extras(world, me, actor, body, &[])
        .unwrap();
}

#[test]
fn engage_takes_attacker_then_target_by_actor_or_name() {
    for body in [
        "combat.engage(self, actor)",
        "combat.engage(self, actor.name)",
        "combat.engage(actor)",
    ] {
        let (mut world, _room, me, bob) = world();
        run(&mut world, me, bob, body);
        assert_eq!(world.get::<Fighting>(me).map(|f| f.0), Some(bob), "{body}");
        assert_eq!(world.get::<Fighting>(bob).map(|f| f.0), Some(me), "{body}");
    }
}

#[test]
fn engage_ignores_self_unknown_and_nil_targets() {
    let (mut world, _room, me, bob) = world();
    run(
        &mut world,
        me,
        bob,
        "combat.engage(self, self)\ncombat.engage(self, self.name)\n\
         combat.engage(self, 'nobody')\ncombat.engage(self, nil)\ncombat.engage()",
    );
    assert!(world.get::<Fighting>(me).is_none());
    assert!(world.get::<Fighting>(bob).is_none());
}

#[test]
fn destroy_refuses_players_and_takes_mobs() {
    let (mut world, _room, me, bob) = world();
    run(&mut world, me, bob, "world.destroy(actor)");
    assert!(world.get_entity(bob).is_ok());
    run(&mut world, bob, me, "world.destroy(actor)");
    assert!(world.get_entity(me).is_err());
}

#[test]
fn damage_without_a_server_clamps_and_reports() {
    let (mut world, _room, me, bob) = world();
    world.get_mut::<Health>(bob).unwrap().hp = 10;
    run(
        &mut world,
        me,
        bob,
        "actor:setvar('a', actor:damage(4))\nactor:setvar('b', actor:damage(-100))",
    );
    assert_eq!(
        world.get::<Health>(bob).unwrap().hp,
        20,
        "heal stops at max"
    );
}
