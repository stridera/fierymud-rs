//! Magical invisibility (issue #39): the shared `can_see_player`
//! predicate drives room lists, name-based target resolution, aggro and
//! per-observer "Someone" messages; attacking breaks invisibility.

use bevy_ecs::prelude::*;
use mud_db::enums::{PlayerFlag, UserRole};
use mud_world::{
    Account, CombatStats, DetectInvis, Exits, Fighting, Health, Invisible, Located, Mob, Named,
    PlayerFlags, Room,
};

use super::test_support::{Rx, drain, player_in};
use super::{
    break_invisibility, broadcast_room_visible, can_see_player, dispatch, engage_combat,
    find_actor_in_room, try_engage_aggressive_mob,
};

fn account(role: UserRole) -> Account {
    Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role,
        account_role: role,
        perms: vec![],
    }
}

struct Fx {
    world: World,
    room: Entity,
    /// "Tester": the (non-detecting) observer.
    watcher: Entity,
    wrx: Rx,
    /// "Ghost": the invisible player.
    ghost: Entity,
    grx: Rx,
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
        let (watcher, wrx) = player_in(&mut world, room);
        world.entity_mut(watcher).insert((
            CombatStats::default(),
            Health { hp: 100, max: 100 },
            account(UserRole::Player),
        ));
        let (ghost, grx) = player_in(&mut world, room);
        world.entity_mut(ghost).insert((
            Named {
                name: "Ghost".into(),
            },
            CombatStats::default(),
            Health { hp: 100, max: 100 },
            account(UserRole::Player),
            Invisible,
        ));
        Self {
            world,
            room,
            watcher,
            wrx,
            ghost,
            grx,
        }
    }

    fn mob(&mut self, name: &str, alignment: i32) -> Entity {
        self.world
            .spawn((
                Mob,
                Named { name: name.into() },
                Located(self.room),
                CombatStats {
                    alignment,
                    ..CombatStats::default()
                },
                Health { hp: 100, max: 100 },
            ))
            .id()
    }
}

#[test]
fn look_omits_an_invisible_player_for_a_non_detecting_observer() {
    let mut fx = Fx::new();
    dispatch(&mut fx.world, fx.watcher, "look");
    let out = drain(&mut fx.wrx);
    assert!(out.contains("A quiet hall"), "{out}");
    assert!(!out.contains("Ghost"), "{out}");
}

#[test]
fn look_shows_an_invisible_player_to_detect_invisibility() {
    let mut fx = Fx::new();
    fx.world.entity_mut(fx.watcher).insert(DetectInvis);
    dispatch(&mut fx.world, fx.watcher, "look");
    let out = drain(&mut fx.wrx);
    assert!(out.contains("Ghost"), "{out}");
}

#[test]
fn look_shows_an_invisible_player_to_gods() {
    let mut fx = Fx::new();
    fx.world
        .entity_mut(fx.watcher)
        .insert(account(UserRole::Immortal));
    dispatch(&mut fx.world, fx.watcher, "look");
    let out = drain(&mut fx.wrx);
    assert!(out.contains("Ghost"), "{out}");
}

#[test]
fn holy_light_sees_invisible_and_the_actor_sees_itself() {
    let mut fx = Fx::new();
    assert!(!can_see_player(&fx.world, fx.watcher, fx.ghost));
    assert!(can_see_player(&fx.world, fx.ghost, fx.ghost));
    fx.world
        .entity_mut(fx.watcher)
        .insert(PlayerFlags(vec![PlayerFlag::HolyLight]));
    assert!(can_see_player(&fx.world, fx.watcher, fx.ghost));
}

#[test]
fn look_at_an_invisible_player_fails_but_detectors_can_examine() {
    let mut fx = Fx::new();
    dispatch(&mut fx.world, fx.watcher, "look ghost");
    let out = drain(&mut fx.wrx);
    assert!(!out.contains("Ghost"), "{out}");
    fx.world.entity_mut(fx.watcher).insert(DetectInvis);
    dispatch(&mut fx.world, fx.watcher, "look ghost");
    assert!(drain(&mut fx.wrx).contains("Ghost"));
}

#[test]
fn name_resolver_skips_invisible_actors() {
    let mut fx = Fx::new();
    assert_eq!(
        find_actor_in_room(&mut fx.world, "ghost", fx.room, fx.watcher),
        None
    );
    fx.world.entity_mut(fx.watcher).insert(DetectInvis);
    assert_eq!(
        find_actor_in_room(&mut fx.world, "ghost", fx.room, fx.watcher),
        Some(fx.ghost)
    );
}

#[test]
fn kill_an_invisible_player_fails_and_starts_no_fight() {
    let mut fx = Fx::new();
    dispatch(&mut fx.world, fx.watcher, "kill ghost");
    let out = drain(&mut fx.wrx);
    assert!(out.contains("You don't see 'ghost' here."), "{out}");
    assert!(fx.world.get::<Fighting>(fx.watcher).is_none());
    assert!(fx.world.get::<Fighting>(fx.ghost).is_none());
    assert_eq!(fx.world.get::<Health>(fx.ghost).unwrap().hp, 100);
}

#[test]
fn attacking_while_invisible_drops_invisibility() {
    let mut fx = Fx::new();
    let _ogre = fx.mob("ogre", 0);
    dispatch(&mut fx.world, fx.ghost, "kill ogre");
    assert!(
        fx.world.get::<Invisible>(fx.ghost).is_none(),
        "attack must break invisibility"
    );
    assert!(can_see_player(&fx.world, fx.watcher, fx.ghost));
    let seen = drain(&mut fx.wrx);
    assert!(seen.contains("Ghost snaps into visibility."), "{seen}");
    assert!(drain(&mut fx.grx).contains("You snap into visibility."));
}

#[test]
fn break_invisibility_is_a_no_op_when_visible() {
    let mut fx = Fx::new();
    fx.world.entity_mut(fx.ghost).remove::<Invisible>();
    break_invisibility(&mut fx.world, fx.ghost);
    assert!(drain(&mut fx.wrx).is_empty());
}

#[test]
fn aggressive_mobs_do_not_aggro_an_invisible_player() {
    let mut fx = Fx::new();
    let wolf = fx.mob("a wolf", -1000);
    try_engage_aggressive_mob(&mut fx.world, fx.ghost, fx.room);
    assert!(fx.world.get::<Fighting>(wolf).is_none());
    assert!(fx.world.get::<Fighting>(fx.ghost).is_none());
    // Control: once visible, the same mob attacks.
    fx.world.entity_mut(fx.ghost).remove::<Invisible>();
    try_engage_aggressive_mob(&mut fx.world, fx.ghost, fx.room);
    assert_eq!(fx.world.get::<Fighting>(wolf).map(|f| f.0), Some(fx.ghost));
}

#[test]
fn aggressive_mob_with_detect_invisible_still_aggros() {
    let mut fx = Fx::new();
    let wolf = fx.mob("a wolf", -1000);
    fx.world.entity_mut(wolf).insert(DetectInvis);
    try_engage_aggressive_mob(&mut fx.world, fx.ghost, fx.room);
    assert_eq!(fx.world.get::<Fighting>(wolf).map(|f| f.0), Some(fx.ghost));
}

#[test]
fn room_messages_say_someone_for_an_invisible_attacker() {
    let mut fx = Fx::new();
    let ogre = fx.mob("an ogre", 0);
    // The ghost (invisible) attacks the ogre; the watcher cannot see it.
    engage_combat(&mut fx.world, fx.ghost, ogre, fx.room);
    let out = drain(&mut fx.wrx);
    assert!(out.contains("Someone sees an ogre and attacks!"), "{out}");
    assert!(!out.contains("Ghost"), "{out}");
}

#[test]
fn the_victim_of_an_invisible_attacker_is_told_someone() {
    let mut fx = Fx::new();
    engage_combat(&mut fx.world, fx.ghost, fx.watcher, fx.room);
    let out = drain(&mut fx.wrx);
    assert!(out.contains("Someone sees you and attacks!"), "{out}");
    assert!(!out.contains("Ghost"), "{out}");
}

#[test]
fn detecting_observers_see_the_real_name_in_room_messages() {
    let mut fx = Fx::new();
    let ogre = fx.mob("an ogre", 0);
    fx.world.entity_mut(fx.watcher).insert(DetectInvis);
    engage_combat(&mut fx.world, fx.ghost, ogre, fx.room);
    let out = drain(&mut fx.wrx);
    assert!(out.contains("Ghost sees an ogre and attacks!"), "{out}");
}

#[test]
fn visible_broadcasts_stay_silent_for_an_invisible_sender() {
    // Legacy `act(..., hide_invisible = true)`: an invisible actor's
    // movement is not announced to observers who cannot see it.
    let mut fx = Fx::new();
    broadcast_room_visible(
        &mut fx.world,
        fx.room,
        fx.ghost,
        &[fx.ghost],
        "Ghost leaves north.\r\n",
    );
    assert!(drain(&mut fx.wrx).is_empty());
    fx.world.entity_mut(fx.watcher).insert(DetectInvis);
    broadcast_room_visible(
        &mut fx.world,
        fx.room,
        fx.ghost,
        &[fx.ghost],
        "Ghost leaves north.\r\n",
    );
    assert!(drain(&mut fx.wrx).contains("Ghost leaves north."));
}

#[test]
fn combat_swing_lines_say_someone_for_an_invisible_attacker() {
    // Whatever the swing rolls (hit / miss / dodge), neither the victim
    // nor a bystander is told the invisible attacker's name.
    let mut fx = Fx::new();
    let ogre = fx.mob("an ogre", 0);
    let (bystander, mut brx) = player_in(&mut fx.world, fx.room);
    fx.world.entity_mut(bystander).insert(Named {
        name: "Bystander".into(),
    });
    engage_combat(&mut fx.world, fx.ghost, ogre, fx.room);
    let _ = drain(&mut brx);
    for _ in 0..8 {
        crate::combat::engage_swing_now(&mut fx.world, fx.ghost, ogre);
    }
    let out = drain(&mut brx);
    assert!(out.contains("Someone"), "{out}");
    assert!(!out.contains("Ghost"), "{out}");
}
