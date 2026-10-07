//! Wimpy mobs (issue #54): a `Wimpy` mob only attacks sleeping targets
//! (legacy `is_aggr_to`), never starts a fight below a quarter HP
//! (legacy `mobile_spec_activity`), and so does not re-aggro an awake
//! player after fleeing.

use bevy_ecs::prelude::*;
use mud_db::enums::{MobBehavior, UserRole};
use mud_world::{
    Account, CombatStats, Exits, Fighting, Health, Located, Mob, MobBehaviors, Named, Posture,
    PostureKind, Room,
};

use super::test_support::{Rx, player_in};
use super::{recheck_aggro_in_room, try_engage_aggressive_mob};
use crate::combat::{MobMemory, remember_attacker};

struct Fx {
    world: World,
    room: Entity,
    player: Entity,
    _rx: Rx,
}

impl Fx {
    fn new() -> Self {
        let mut world = World::new();
        world.insert_resource(mud_world::ObjectPrototypes::default());
        let room = world
            .spawn((
                Room,
                Named {
                    name: "A quiet hall".into(),
                },
                Exits::default(),
            ))
            .id();
        let (player, rx) = player_in(&mut world, room);
        world.entity_mut(player).insert((
            CombatStats::default(),
            Health { hp: 100, max: 100 },
            Posture(PostureKind::Standing),
            Account {
                user_id: "u".into(),
                character_id: "c".into(),
                role: UserRole::Player,
                account_role: UserRole::Player,
                perms: vec![],
            },
        ));
        Self {
            world,
            room,
            player,
            _rx: rx,
        }
    }

    /// An evil (aggressive) mob with the given behaviors and HP.
    fn mob(&mut self, behaviors: Vec<MobBehavior>, hp: i32) -> Entity {
        self.world
            .spawn((
                Mob,
                Named {
                    name: "a jackal".into(),
                },
                Located(self.room),
                CombatStats {
                    alignment: -1000,
                    ..CombatStats::default()
                },
                Health { hp, max: 100 },
                Posture(PostureKind::Standing),
                MobBehaviors(behaviors),
            ))
            .id()
    }

    fn set_player_posture(&mut self, p: PostureKind) {
        self.world.entity_mut(self.player).insert(Posture(p));
    }

    fn fighting(&self, mob: Entity) -> bool {
        self.world.get::<Fighting>(mob).is_some()
    }
}

#[test]
fn wimpy_aggressive_mob_ignores_an_awake_player() {
    let mut fx = Fx::new();
    let mob = fx.mob(vec![MobBehavior::Wimpy], 100);
    try_engage_aggressive_mob(&mut fx.world, fx.player, fx.room);
    assert!(!fx.fighting(mob));
}

#[test]
fn wimpy_aggressive_mob_attacks_a_sleeping_player() {
    let mut fx = Fx::new();
    let mob = fx.mob(vec![MobBehavior::Wimpy], 100);
    fx.set_player_posture(PostureKind::Sleeping);
    try_engage_aggressive_mob(&mut fx.world, fx.player, fx.room);
    assert_eq!(fx.world.get::<Fighting>(mob).map(|f| f.0), Some(fx.player));
}

#[test]
fn non_wimpy_aggressive_mob_still_attacks_an_awake_player() {
    let mut fx = Fx::new();
    let mob = fx.mob(vec![], 100);
    try_engage_aggressive_mob(&mut fx.world, fx.player, fx.room);
    assert_eq!(fx.world.get::<Fighting>(mob).map(|f| f.0), Some(fx.player));
}

#[test]
fn wimpy_protector_still_attacks_an_awake_player() {
    let mut fx = Fx::new();
    let mob = fx.mob(vec![MobBehavior::Wimpy, MobBehavior::Protector], 100);
    try_engage_aggressive_mob(&mut fx.world, fx.player, fx.room);
    assert!(fx.fighting(mob));
}

#[test]
fn hurt_wimpy_mob_does_not_start_a_fight_even_with_a_sleeper() {
    let mut fx = Fx::new();
    let mob = fx.mob(vec![MobBehavior::Wimpy], 20);
    fx.set_player_posture(PostureKind::Sleeping);
    try_engage_aggressive_mob(&mut fx.world, fx.player, fx.room);
    assert!(!fx.fighting(mob));
}

#[test]
fn fled_wimpy_mob_does_not_re_aggro_the_awake_player_it_remembers() {
    let mut fx = Fx::new();
    // Hurt, grudge-holding wimpy mob (as left behind by a flee).
    let mob = fx.mob(vec![MobBehavior::Wimpy], 20);
    remember_attacker(&mut fx.world, mob, fx.player);
    assert!(fx.world.get::<MobMemory>(mob).is_some());
    recheck_aggro_in_room(&mut fx.world, fx.player);
    assert!(!fx.fighting(mob));
    // Healed past the panic line: still wimpy toward an awake player.
    fx.world.get_mut::<Health>(mob).unwrap().hp = 100;
    recheck_aggro_in_room(&mut fx.world, fx.player);
    assert!(!fx.fighting(mob));
}
