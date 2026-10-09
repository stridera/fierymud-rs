//! Every route that puts a mob into the world builds it through
//! `mud_world::spawn_mob_from_proto`, so the proto's `MobDefaultEffects`
//! apply and a hostile arrival picks its fight on the next aggro pulse. Boot, respawn and shop-pet coverage live beside
//! those paths (`respawn.rs`, `shop_tests.rs`).

use bevy_ecs::prelude::*;
use mud_db::enums::MobProfession;
use mud_world::{
    AppliedTo, DetectInvis, EffectInstance, Exits, Fighting, Located, Mob, MobPrototypes, Named,
    Room, WorldKey, WorldKeyIndex,
};

use super::test_support::{Rx, grant_default_flags, make_aggro_target, mob_proto, player_in};

const KEY: (i32, i32) = (30, 1);
const ROOM_KEY: (i32, i32) = (30, 100);

struct Fx {
    world: World,
    room: Entity,
    player: Entity,
    _rx: Rx,
}

/// A room with a player, the proto catalog holding `KEY` (alignment
/// `alignment`) and a `detect_invisible` default effect on it.
fn fixture(alignment: i32) -> Fx {
    let mut world = World::new();
    let mut protos = MobPrototypes::default();
    let mut proto = mob_proto(KEY.0, KEY.1, MobProfession::Trainer);
    proto.alignment = alignment;
    protos.by_key.insert(KEY, proto);
    world.insert_resource(protos);
    grant_default_flags(&mut world, KEY, &["detect_invisible"]);
    let room = world
        .spawn((
            Room,
            Named {
                name: "A hall".into(),
            },
            Exits::default(),
            WorldKey {
                zone: ROOM_KEY.0,
                id: ROOM_KEY.1,
            },
        ))
        .id();
    let mut index = WorldKeyIndex::default();
    index.rooms.insert(ROOM_KEY, room);
    world.insert_resource(index);
    let (player, rx) = player_in(&mut world, room);
    make_aggro_target(&mut world, player);
    Fx {
        world,
        room,
        player,
        _rx: rx,
    }
}

fn spawned_mob(world: &mut World) -> Entity {
    let mut q = world.query_filtered::<(Entity, &WorldKey), With<Mob>>();
    q.iter(world)
        .find(|(_, k)| (k.zone, k.id) == KEY)
        .map(|(e, _)| e)
        .expect("mob spawned from the proto")
}

fn has_default_effect_instance(world: &mut World, mob: Entity) -> bool {
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    q.iter(world)
        .any(|(e, a)| a.0 == mob && e.name == "detect_invisible")
}

#[test]
fn lua_spawn_mobile_applies_default_effects() {
    let mut fx = fixture(0);
    let mut host = mud_script::LuaHost::new();
    host.exec_for_actor(
        &mut fx.world,
        fx.player,
        &format!("self.room:spawn_mobile({}, {})", KEY.0, KEY.1),
    )
    .expect("script runs");
    let mob = spawned_mob(&mut fx.world);
    assert!(fx.world.get::<DetectInvis>(mob).is_some());
    assert!(has_default_effect_instance(&mut fx.world, mob));
    assert_eq!(fx.world.get::<Located>(mob).map(|l| l.0), Some(fx.room));
}

#[test]
fn lua_spawned_aggressive_mob_attacks_the_player_on_the_aggro_pulse() {
    let mut fx = fixture(-1000);
    let mut host = mud_script::LuaHost::new();
    host.exec_for_actor(
        &mut fx.world,
        fx.player,
        &format!("self.room:spawn_mobile({}, {})", KEY.0, KEY.1),
    )
    .expect("script runs");
    let mob = spawned_mob(&mut fx.world);
    // Not inline inside the Lua frame, and not at spawn time either ...
    assert!(fx.world.get::<Fighting>(mob).is_none());
    // ... but on the mob AI pulse, like legacy `mobile_activity`.
    super::aggro_pulse(&mut fx.world);
    assert_eq!(fx.world.get::<Fighting>(mob).map(|f| f.0), Some(fx.player));
}

#[test]
fn admin_spawn_applies_default_effects() {
    let mut fx = fixture(0);
    crate::admin::spawn_into(&mut fx.world, "mob", KEY.0, KEY.1, ROOM_KEY.0, ROOM_KEY.1)
        .expect("spawn ok");
    let mob = spawned_mob(&mut fx.world);
    assert!(fx.world.get::<DetectInvis>(mob).is_some());
    assert!(has_default_effect_instance(&mut fx.world, mob));
    assert!(fx.world.get::<Fighting>(mob).is_none(), "neutral mob");
}

#[test]
fn admin_spawned_aggressive_mob_attacks_a_player_in_the_room_on_the_pulse() {
    let mut fx = fixture(-1000);
    crate::admin::spawn_into(&mut fx.world, "mob", KEY.0, KEY.1, ROOM_KEY.0, ROOM_KEY.1)
        .expect("spawn ok");
    let mob = spawned_mob(&mut fx.world);
    assert!(fx.world.get::<Fighting>(mob).is_none(), "not at spawn time");
    super::aggro_pulse(&mut fx.world);
    assert_eq!(fx.world.get::<Fighting>(mob).map(|f| f.0), Some(fx.player));
}

// -- pets and charmed mobs never start a fight -----------------------------

fn evil_mob(world: &mut World, room: Entity) -> Entity {
    world
        .spawn((
            Mob,
            Named {
                name: "a hellhound".into(),
            },
            Located(room),
            mud_world::CombatStats {
                alignment: -1000,
                ..mud_world::CombatStats::default()
            },
            mud_world::Health { hp: 50, max: 50 },
            mud_world::Posture(mud_world::PostureKind::Standing),
        ))
        .id()
}

#[test]
fn evil_pet_does_not_attack_its_owner_or_a_stranger_on_room_entry() {
    let mut fx = fixture(0);
    let (stranger, _srx) = player_in(&mut fx.world, fx.room);
    make_aggro_target(&mut fx.world, stranger);
    let pet = evil_mob(&mut fx.world, fx.room);
    fx.world
        .entity_mut(pet)
        .insert(mud_world::Follower(fx.player));
    super::recheck_aggro_in_room(&mut fx.world, fx.player);
    super::recheck_aggro_in_room(&mut fx.world, stranger);
    assert!(fx.world.get::<Fighting>(pet).is_none());
}

#[test]
fn evil_charmed_follower_of_a_mob_never_starts_a_fight() {
    let mut fx = fixture(0);
    let master = evil_mob(&mut fx.world, fx.room);
    // Neutral master, so the thrall is the only aggro candidate.
    fx.world
        .entity_mut(master)
        .insert(mud_world::CombatStats::default());
    let thrall = evil_mob(&mut fx.world, fx.room);
    fx.world
        .entity_mut(thrall)
        .insert(mud_world::Follower(master));
    fx.world.spawn((
        EffectInstance {
            kind: 4,
            name: "charmed".into(),
            strength: 1,
            remaining_secs: 60,
            source: mud_world::EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(thrall),
    ));
    super::recheck_aggro_in_room(&mut fx.world, fx.player);
    assert!(fx.world.get::<Fighting>(thrall).is_none());
}

#[test]
fn ordinary_evil_mob_still_attacks_on_room_entry() {
    let mut fx = fixture(0);
    let mob = evil_mob(&mut fx.world, fx.room);
    super::recheck_aggro_in_room(&mut fx.world, fx.player);
    assert_eq!(fx.world.get::<Fighting>(mob).map(|f| f.0), Some(fx.player));
}
