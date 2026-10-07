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

// -- every damage source breaks invisibility ------------------------------

#[test]
fn any_damage_dealt_breaks_invisibility() {
    // `apply_damage_from` is the shared sink for melee, skills (bash,
    // hitall, taunt, class strikes) and spell damage.
    let mut fx = Fx::new();
    let ogre = fx.mob("an ogre", 0);
    super::apply_damage_from(&mut fx.world, ogre, 5, fx.ghost);
    assert!(fx.world.get::<Invisible>(fx.ghost).is_none());
    assert!(drain(&mut fx.wrx).contains("Ghost snaps into visibility."));
}

#[test]
fn zero_damage_and_self_damage_do_not_break_invisibility() {
    let mut fx = Fx::new();
    let ogre = fx.mob("an ogre", 0);
    super::apply_damage_from(&mut fx.world, ogre, 0, fx.ghost);
    super::apply_damage_from(&mut fx.world, fx.ghost, 5, fx.ghost);
    assert!(fx.world.get::<Invisible>(fx.ghost).is_some());
}

// -- expiry ---------------------------------------------------------------

#[test]
fn expiry_announces_refreshes_the_panel_and_lets_mobs_aggro() {
    use crate::TickCount;
    let mut fx = Fx::new();
    let wolf = fx.mob("a wolf", -1000);
    fx.world.spawn((
        mud_world::EffectInstance {
            kind: 1,
            name: "invisible".into(),
            strength: 1,
            remaining_secs: 1,
            source: mud_world::EffectSource::Spell,
            ability_id: None,
        },
        mud_world::AppliedTo(fx.ghost),
        mud_world::InvisibleSource,
    ));
    let _ = drain(&mut fx.wrx);
    let _ = drain(&mut fx.grx);
    fx.world.insert_resource(TickCount(10));
    crate::effects::effects_tick(&mut fx.world);
    assert!(fx.world.get::<Invisible>(fx.ghost).is_none());
    let seen = drain(&mut fx.wrx);
    assert!(seen.contains("Ghost fades back into view."), "{seen}");
    assert!(seen.contains("Room.Players"), "panel refreshed: {seen}");
    assert!(drain(&mut fx.grx).contains("You fade back into view."));
    assert_eq!(fx.world.get::<Fighting>(wolf).map(|f| f.0), Some(fx.ghost));
}

// -- MobDefaultEffects ----------------------------------------------------

#[test]
fn default_effect_detect_invisible_lets_a_spawned_mob_aggro() {
    use mud_world::{EffectCatalog, EffectDef, MobDefaultEffect, MobDefaultEffectCatalog};
    let mut fx = Fx::new();
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(
        4,
        EffectDef {
            id: 4,
            name: "status".into(),
            description: None,
            effect_type: "status".into(),
            tags: vec![],
            presence_override: None,
            default_params: serde_json::json!({"flag": "detect_invisible"}),
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        },
    );
    fx.world.insert_resource(effects);
    let mut defaults = MobDefaultEffectCatalog::default();
    defaults.by_key.insert(
        (30, 1),
        vec![MobDefaultEffect {
            effect_id: 4,
            strength: 1,
            modifier_data: serde_json::json!({}),
        }],
    );
    fx.world.insert_resource(defaults);
    let blind = fx.mob("a blind wolf", -1000);
    let seeing = fx.mob("a seeing wolf", -1000);
    mud_world::mob_effects::apply_mob_default_effects(&mut fx.world, seeing, (30, 1));
    assert!(fx.world.get::<DetectInvis>(seeing).is_some());
    assert!(fx.world.get::<DetectInvis>(blind).is_none());
    try_engage_aggressive_mob(&mut fx.world, fx.ghost, fx.room);
    assert_eq!(
        fx.world.get::<Fighting>(seeing).map(|f| f.0),
        Some(fx.ghost)
    );
    assert!(fx.world.get::<Fighting>(blind).is_none());
}

// -- anonymise_for --------------------------------------------------------

#[test]
fn anonymise_matches_whole_words_only() {
    let mut fx = Fx::new();
    fx.world
        .entity_mut(fx.ghost)
        .insert(Named { name: "Al".into() });
    let out = super::anonymise_for(
        &fx.world,
        fx.watcher,
        &[(fx.ghost, "Al")],
        "Al hits Alric; Alric hits Al.\r\n",
    );
    assert_eq!(out, "Someone hits Alric; Alric hits someone.\r\n");
}

#[test]
fn anonymise_handles_the_capitalised_mob_form() {
    let mut fx = Fx::new();
    fx.world.entity_mut(fx.ghost).insert(Named {
        name: "a goblin".into(),
    });
    let out = super::anonymise_for(
        &fx.world,
        fx.watcher,
        &[(fx.ghost, "a goblin")],
        "<dim>A goblin swings at Bob; Bob dodges a goblin.</>\r\n",
    );
    assert_eq!(
        out,
        "<dim>Someone swings at Bob; Bob dodges someone.</>\r\n"
    );
}

// -- where ----------------------------------------------------------------

#[test]
fn where_name_hides_invisible_players_from_mortals_but_not_gods() {
    let mut fx = Fx::new();
    fx.world.entity_mut(fx.ghost).insert(mud_world::Online);
    dispatch(&mut fx.world, fx.watcher, "where ghost");
    let out = drain(&mut fx.wrx);
    assert!(out.contains("isn't online"), "{out}");
    fx.world
        .entity_mut(fx.watcher)
        .insert(account(UserRole::Immortal));
    dispatch(&mut fx.world, fx.watcher, "where ghost");
    let out = drain(&mut fx.wrx);
    assert!(out.contains("Ghost is in"), "{out}");
}

// -- MobDefaultEffects: importer `flags` array ----------------------------

fn status_effect_catalog() -> mud_world::EffectCatalog {
    let mut effects = mud_world::EffectCatalog::default();
    effects.by_id.insert(
        4,
        mud_world::EffectDef {
            id: 4,
            name: "status".into(),
            description: None,
            effect_type: "status".into(),
            tags: vec![],
            presence_override: None,
            default_params: serde_json::json!({"duration": "level * 2"}),
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        },
    );
    effects
}

fn defaults_with(flags: &serde_json::Value) -> mud_world::MobDefaultEffectCatalog {
    let mut defaults = mud_world::MobDefaultEffectCatalog::default();
    defaults.by_key.insert(
        (30, 1),
        vec![mud_world::MobDefaultEffect {
            effect_id: 4,
            strength: 1,
            modifier_data: serde_json::json!({ "flags": flags }),
        }],
    );
    defaults
}

#[test]
fn flags_array_installs_every_mapped_marker_and_ignores_unknown_flags() {
    let mut fx = Fx::new();
    fx.world.insert_resource(status_effect_catalog());
    fx.world.insert_resource(defaults_with(&serde_json::json!([
        "detect_invisible",
        "sanctuary",
        "infravision",
        "poisoned",
        "sleeping",
        "not_a_real_flag"
    ])));
    let wolf = fx.mob("a wolf", 0);
    mud_world::mob_effects::apply_mob_default_effects(&mut fx.world, wolf, (30, 1));
    assert!(fx.world.get::<DetectInvis>(wolf).is_some());
    assert!(fx.world.get::<mud_world::Sanctuary>(wolf).is_some());
    // Flags without a marker spawn nothing: no source-less poison tick,
    // no permanent sleep fighting the mob's posture.
    let names: Vec<String> = {
        let mut q = fx
            .world
            .query::<(&mud_world::EffectInstance, &mud_world::AppliedTo)>();
        q.iter(&fx.world)
            .filter(|(_, a)| a.0 == wolf)
            .map(|(e, _)| e.name.clone())
            .collect()
    };
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(
        names
            .iter()
            .all(|n| n == "detect_invisible" || n == "sanctuary")
    );
}

#[test]
fn default_invisible_is_stripped_when_the_mob_attacks_like_legacy() {
    let mut fx = Fx::new();
    fx.world.insert_resource(status_effect_catalog());
    fx.world
        .insert_resource(defaults_with(&serde_json::json!(["invisible"])));
    let imp = fx.mob("an imp", 0);
    mud_world::mob_effects::apply_mob_default_effects(&mut fx.world, imp, (30, 1));
    assert!(fx.world.get::<Invisible>(imp).is_some());
    assert!(!can_see_player(&fx.world, fx.watcher, imp));
    super::apply_damage_from(&mut fx.world, fx.watcher, 3, imp);
    assert!(fx.world.get::<Invisible>(imp).is_none());
    // The permanent backing effect is gone too, so it can't come back.
    let left = {
        let mut q = fx.world.query::<&mud_world::AppliedTo>();
        q.iter(&fx.world).filter(|a| a.0 == imp).count()
    };
    assert_eq!(left, 0);
}

/// Against the live dev DB: every mob proto whose rows list
/// `detect_invisible` ends up with `DetectInvis`. Skips when fierydev is
/// unreachable.
#[tokio::test]
async fn live_db_default_effects_install_detect_invis() {
    let _db_lock = super::test_support::db_test_lock().await;
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
    let Ok(Ok(pool)) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        mud_db::connect_with(&url, super::test_support::db_test_pool_settings()),
    )
    .await
    else {
        return;
    };
    let Ok(effects) = mud_world::loader::load_effect_catalog(&pool).await else {
        return;
    };
    let Ok(defaults) = mud_world::loader::load_mob_default_effect_catalog(&pool).await else {
        return;
    };
    let keys: Vec<(i32, i32)> = defaults.by_key.keys().copied().collect();
    let mut world = World::new();
    world.insert_resource(effects);
    world.insert_resource(defaults);
    let mut with_detect = 0;
    for key in &keys {
        let mob = world.spawn(Mob).id();
        mud_world::mob_effects::apply_mob_default_effects(&mut world, mob, *key);
        if world.get::<DetectInvis>(mob).is_some() {
            with_detect += 1;
        }
    }
    eprintln!(
        "live DB: {} mobs with default effects, {with_detect} got DetectInvis",
        keys.len()
    );
    if !keys.is_empty() {
        assert!(
            with_detect > 0,
            "no mob got DetectInvis from {} rows",
            keys.len()
        );
    }
}
