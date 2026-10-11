//! Closed / locked containers: `get`, `put`, `look in` and `search` honour
//! the state; `open`, `close`, `lock`, `unlock` and `pick` change it; and a
//! player corpse is looted by its owner's character id, not a name.
//! Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole};
use mud_world::{
    Account, ContainerState, Item, KnownAbilities, Located, Named, ObjectPrototypes, Room, Stamina,
};

use super::containers::container_state;
use super::info::{cmd_close, cmd_get, cmd_lock, cmd_open, cmd_put, cmd_unlock};
use super::item_guard_tests::item_at;
use super::look_in_container;
use super::spells::cmd_pick;
use super::test_support::{Rx, drain, install_core_abilities, object_proto, player_in};

/// (zone, id) of the chest protos below and of the key that fits the
/// locked ones (legacy key vnum 3051).
const KEY: (i32, i32) = (30, 51);

fn container_proto(
    objects: &mut ObjectPrototypes,
    id: i32,
    name: &str,
    kw: &str,
    values: serde_json::Value,
) {
    let mut p = object_proto(30, id, ObjectType::Container);
    p.name = name.to_string();
    p.keywords = vec![kw.to_string()];
    p.values = values;
    objects.by_key.insert((30, id), p);
}

/// Room with a player, a locked chest (id 60, key 3051), a pickproof
/// locked chest (61), an open sack (62) and a sealed non-closeable crate
/// (63). Every container holds a "gem" (id 70).
struct Fx {
    world: World,
    room: Entity,
    player: Entity,
    rx: Rx,
}

impl Fx {
    fn new() -> Self {
        let mut world = World::new();
        let mut objects = ObjectPrototypes::default();
        container_proto(
            &mut objects,
            60,
            "an iron chest",
            "chest",
            serde_json::json!({"Key": 3051, "Flags": ["Closeable", "Closed", "Locked"], "Capacity": 50}),
        );
        container_proto(
            &mut objects,
            61,
            "a vault",
            "vault",
            serde_json::json!({"Key": 3051, "Flags": ["Closeable", "PickProof", "Closed", "Locked"], "Capacity": 50}),
        );
        container_proto(
            &mut objects,
            62,
            "a canvas sack",
            "sack",
            serde_json::json!({"Key": -1, "Flags": ["Closeable"], "Capacity": 50}),
        );
        container_proto(
            &mut objects,
            63,
            "a sealed crate",
            "crate",
            serde_json::json!({"Key": 0, "Flags": ["Closed", "Locked"], "Capacity": 50}),
        );
        let mut key = object_proto(KEY.0, KEY.1, ObjectType::Key);
        key.name = "an iron key".to_string();
        key.keywords = vec!["key".to_string()];
        objects.by_key.insert(KEY, key);
        world.insert_resource(objects);
        let room = world.spawn(Room).id();
        let (player, rx) = player_in(&mut world, room);
        Self {
            world,
            room,
            player,
            rx,
        }
    }

    /// A container instance in the room holding a gem.
    fn chest(&mut self, id: i32, name: &str, kw: &str) -> (Entity, Entity) {
        let chest = item_at(&mut self.world, self.room, id, name, kw);
        let gem = item_at(&mut self.world, chest, 70, "a ruby", "ruby");
        (chest, gem)
    }

    fn give_key(&mut self) -> Entity {
        item_at(&mut self.world, self.player, KEY.1, "an iron key", "key")
    }

    fn out(&mut self) -> String {
        drain(&mut self.rx)
    }

    fn holder(&self, item: Entity) -> Entity {
        self.world.get::<Located>(item).unwrap().0
    }
}

#[test]
fn state_is_parsed_from_the_proto_values() {
    let mut fx = Fx::new();
    let (chest, _) = fx.chest(60, "an iron chest", "chest");
    let s = container_state(&fx.world, chest).unwrap();
    assert!(s.closeable && s.closed && s.locked && !s.pickproof);
    assert_eq!(s.key, Some(KEY));
    let (vault, _) = fx.chest(61, "a vault", "vault");
    assert!(container_state(&fx.world, vault).unwrap().pickproof);
    let (sack, _) = fx.chest(62, "a canvas sack", "sack");
    let s = container_state(&fx.world, sack).unwrap();
    assert!(!s.closed && !s.locked && s.key.is_none());
    // Key 0 is "no keyhole", and zone 0 maps to zone 1000.
    assert_eq!(
        ContainerState::from_values(&serde_json::json!({"Key": 0})).key,
        None
    );
    assert_eq!(
        ContainerState::from_values(&serde_json::json!({"Key": 51})).key,
        Some((1000, 51))
    );
    // A key is not a container.
    let key = item_at(&mut fx.world, fx.room, KEY.1, "an iron key", "key");
    assert!(container_state(&fx.world, key).is_none());
}

#[test]
fn nothing_can_be_taken_from_or_put_into_a_closed_chest() {
    let mut fx = Fx::new();
    let (_chest, gem) = fx.chest(60, "an iron chest", "chest");
    let trinket = item_at(&mut fx.world, fx.player, 71, "a trinket", "trinket");

    cmd_get(&mut fx.world, fx.player, "ruby from chest");
    assert!(fx.out().contains("An iron chest is closed."));
    cmd_get(&mut fx.world, fx.player, "all chest");
    assert!(fx.out().contains("is closed."));
    cmd_get(&mut fx.world, fx.player, "ruby chest");
    assert!(fx.out().contains("is closed."));
    assert_ne!(fx.holder(gem), fx.player);

    cmd_put(&mut fx.world, fx.player, "trinket chest");
    assert!(fx.out().contains("is closed."));
    assert_eq!(fx.holder(trinket), fx.player);

    look_in_container(&mut fx.world, fx.player, "chest");
    let out = fx.out();
    assert!(out.contains("is closed."), "{out}");
    assert!(!out.contains("ruby"), "{out}");
}

#[test]
fn a_chest_without_the_closed_flag_is_open_to_everyone() {
    let mut fx = Fx::new();
    let (_sack, gem) = fx.chest(62, "a canvas sack", "sack");
    cmd_get(&mut fx.world, fx.player, "ruby from sack");
    assert_eq!(fx.holder(gem), fx.player);
    let trinket = item_at(&mut fx.world, fx.player, 71, "a trinket", "trinket");
    cmd_put(&mut fx.world, fx.player, "trinket sack");
    assert_ne!(fx.holder(trinket), fx.player);
}

#[test]
fn a_locked_chest_needs_its_key_then_opens_and_closes() {
    let mut fx = Fx::new();
    let (chest, gem) = fx.chest(60, "an iron chest", "chest");

    cmd_open(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("locked"));
    cmd_unlock(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("proper key"));
    assert!(container_state(&fx.world, chest).unwrap().locked);

    // A wrong key does not fit either.
    item_at(&mut fx.world, fx.player, 99, "a bent key", "key");
    cmd_unlock(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("proper key"));
    fx.give_key();

    cmd_unlock(&mut fx.world, fx.player, "chest");
    let out = fx.out();
    assert!(
        out.contains("You unlock an iron chest with an iron key"),
        "{out}"
    );
    let s = container_state(&fx.world, chest).unwrap();
    assert!(s.closed && !s.locked);
    // Unlocked is not open: still refused.
    cmd_get(&mut fx.world, fx.player, "ruby from chest");
    assert!(fx.out().contains("is closed."));

    cmd_open(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("You open an iron chest."));
    cmd_get(&mut fx.world, fx.player, "ruby from chest");
    assert_eq!(fx.holder(gem), fx.player);

    cmd_open(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("already open"));
    cmd_lock(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("close it first"));
    cmd_close(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("You close an iron chest."));
    cmd_close(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("already closed"));
    cmd_put(&mut fx.world, fx.player, "ruby chest");
    assert!(fx.out().contains("is closed."));

    cmd_lock(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("lock it"));
    assert!(container_state(&fx.world, chest).unwrap().locked);
    cmd_lock(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("already locked"));
}

#[test]
fn a_carried_container_can_be_opened_and_a_sealed_one_cannot() {
    let mut fx = Fx::new();
    let sack = item_at(&mut fx.world, fx.player, 62, "a canvas sack", "sack");
    cmd_close(&mut fx.world, fx.player, "sack");
    assert!(fx.out().contains("You close a canvas sack."));
    assert!(container_state(&fx.world, sack).unwrap().closed);
    cmd_open(&mut fx.world, fx.player, "sack");
    assert!(fx.out().contains("You open a canvas sack."));

    // Closed + Locked without Closeable and without a keyhole: no verb works.
    fx.chest(63, "a sealed crate", "crate");
    fx.give_key();
    for verb in [cmd_open, cmd_unlock] {
        verb(&mut fx.world, fx.player, "crate");
        assert!(fx.out().contains("can't"), "sealed crate must refuse");
    }
}

#[test]
fn a_lock_with_no_keyhole_cannot_be_keyed() {
    let mut fx = Fx::new();
    let (chest, _) = fx.chest(62, "a canvas sack", "sack");
    fx.world.entity_mut(chest).insert(ContainerState {
        closeable: true,
        closed: true,
        locked: true,
        pickproof: false,
        key: None,
    });
    fx.give_key();
    cmd_unlock(&mut fx.world, fx.player, "sack");
    assert!(fx.out().contains("keyhole"));
}

#[test]
fn pick_opens_a_pickable_lock_but_not_a_pickproof_one() {
    let mut fx = Fx::new();
    let (chest, _) = fx.chest(60, "an iron chest", "chest");
    let (vault, _) = fx.chest(61, "a vault", "vault");
    let core = install_core_abilities(&mut fx.world);

    cmd_pick(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("don't know how to pick locks"));

    fx.world.entity_mut(fx.player).insert((
        KnownAbilities {
            entries: vec![(core.pick_lock.unwrap(), 1000, true)],
        },
        Stamina {
            current: 50,
            max: 50,
        },
    ));
    cmd_pick(&mut fx.world, fx.player, "vault");
    assert!(fx.out().contains("resists your attempts"));
    assert!(container_state(&fx.world, vault).unwrap().locked);

    cmd_pick(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("skillfully pick the lock"));
    let s = container_state(&fx.world, chest).unwrap();
    assert!(s.closed && !s.locked, "picked: unlocked but still closed");
    cmd_pick(&mut fx.world, fx.player, "chest");
    assert!(fx.out().contains("wasn't locked"));
}

#[test]
fn a_respawned_container_is_back_in_its_authored_state() {
    let mut fx = Fx::new();
    let key = fx.give_key();
    let (chest, _) = fx.chest(60, "an iron chest", "chest");
    cmd_unlock(&mut fx.world, fx.player, "chest");
    cmd_open(&mut fx.world, fx.player, "chest");
    fx.out();
    assert!(!container_state(&fx.world, chest).unwrap().closed);
    // The reset pass despawns the old chest and spawns a fresh instance.
    fx.world.despawn(chest);
    let fresh = item_at(&mut fx.world, fx.room, 60, "an iron chest", "chest");
    let s = container_state(&fx.world, fresh).unwrap();
    assert!(s.closed && s.locked);
    assert!(fx.world.get_entity(key).is_ok());
}

// -- player corpses are looted by owner id ----------------------------------

fn account(character_id: &str) -> Account {
    Account {
        user_id: "u".into(),
        character_id: character_id.into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    }
}

fn player_corpse(fx: &mut Fx, owner_id: &str, title: &str) -> (Entity, Entity) {
    let corpse = fx
        .world
        .spawn((
            Item,
            Named { name: title.into() },
            mud_world::Keywords(vec!["corpse".into()]),
            Located(fx.room),
            mud_world::Corpse,
            mud_world::PlayerCorpse,
            mud_world::PlayerCorpseId(1),
            crate::corpses::PlayerCorpseOwner(owner_id.into()),
        ))
        .id();
    let gem = item_at(&mut fx.world, corpse, 70, "a ruby", "ruby");
    (corpse, gem)
}

#[test]
fn a_player_corpse_is_looted_by_character_id_not_by_name() {
    let mut fx = Fx::new();
    // The looter is called Tester, like the corpse's title, but is a
    // different character from the one who died.
    fx.world.entity_mut(fx.player).insert(account("char-b"));
    let (_corpse, gem) = player_corpse(&mut fx, "char-a", "the corpse of Tester");
    cmd_get(&mut fx.world, fx.player, "ruby from corpse");
    assert!(fx.out().contains("isn't yours to loot"));
    assert_ne!(fx.holder(gem), fx.player);
}

#[test]
fn the_owner_loots_their_corpse_whatever_the_title_says() {
    let mut fx = Fx::new();
    fx.world.entity_mut(fx.player).insert(account("char-b"));
    let (_corpse, gem) = player_corpse(&mut fx, "char-b", "the corpse of Somebody");
    cmd_get(&mut fx.world, fx.player, "ruby from corpse");
    let out = fx.out();
    assert!(!out.contains("isn't yours to loot"), "{out}");
    assert_eq!(fx.holder(gem), fx.player);
}

#[test]
fn an_owner_less_player_corpse_falls_back_to_the_title_name() {
    let mut fx = Fx::new();
    let (corpse, gem) = player_corpse(&mut fx, "x", "the corpse of Tester");
    fx.world
        .entity_mut(corpse)
        .remove::<crate::corpses::PlayerCorpseOwner>();
    cmd_get(&mut fx.world, fx.player, "ruby from corpse");
    assert_eq!(fx.holder(gem), fx.player);
}

#[test]
fn the_state_component_overrides_the_proto() {
    let mut fx = Fx::new();
    let (chest, _) = fx.chest(60, "an iron chest", "chest");
    fx.world.entity_mut(chest).insert(ContainerState {
        closeable: true,
        closed: false,
        locked: false,
        pickproof: false,
        key: Some(KEY),
    });
    assert!(!container_state(&fx.world, chest).unwrap().closed);
}

// -- saved per-instance state ------------------------------------------------

#[test]
fn a_saved_state_overrides_the_proto_and_an_equal_one_does_not() {
    use super::containers::restore_saved;
    let mut fx = Fx::new();
    // Legacy saved the locked chest open and unlocked (60 is closed+locked).
    let (chest, _) = fx.chest(60, "an iron chest", "chest");
    restore_saved(
        &mut fx.world,
        chest,
        &serde_json::json!({"Key": 3051, "Flags": ["Closeable"]}),
    );
    let s = container_state(&fx.world, chest).unwrap();
    assert!(s.closeable && !s.closed && !s.locked, "{s:?}");
    assert_eq!(s.key, Some(KEY));
    // A row that saved exactly the authored state leaves the proto in charge.
    let (vault, _) = fx.chest(61, "a vault", "vault");
    restore_saved(
        &mut fx.world,
        vault,
        &serde_json::json!({"Key": 3051, "Flags": ["Closeable", "PickProof", "Closed", "Locked"]}),
    );
    assert!(fx.world.get::<ContainerState>(vault).is_none());
    // No Flags / Key on the row (new instance): proto default.
    let (sack, _) = fx.chest(62, "a canvas sack", "sack");
    restore_saved(&mut fx.world, sack, &serde_json::json!({}));
    assert!(fx.world.get::<ContainerState>(sack).is_none());
    // A non-container is ignored.
    let key = fx.give_key();
    restore_saved(
        &mut fx.world,
        key,
        &serde_json::json!({"Flags": ["Locked"]}),
    );
    assert!(fx.world.get::<ContainerState>(key).is_none());
}

#[test]
fn flag_names_round_trip_through_overlay() {
    let s = ContainerState::from_values(
        &serde_json::json!({"Key": 3051, "Flags": ["PickProof", "Locked", "Closed", "Closeable"]}),
    );
    assert_eq!(
        s.flag_names(),
        vec!["Closeable", "Closed", "Locked", "PickProof"]
    );
    let blank = ContainerState::from_values(&serde_json::json!({}));
    let back = blank.overlay(&serde_json::json!({"Flags": s.flag_names()}));
    assert_eq!(
        (back.closeable, back.closed, back.locked, back.pickproof),
        (true, true, true, true)
    );
    assert_eq!(back.key, None, "flags alone do not touch the key");
}

// -- directions beat items --------------------------------------------------

#[test]
fn a_direction_word_is_never_taken_for_an_item() {
    use super::containers::{Verb, try_container_verb};
    let mut fx = Fx::new();
    // A sack whose keyword is "d" (and one named "s") sits in the room.
    let (_sack, _) = fx.chest(62, "a canvas sack", "d");
    let d = try_container_verb(&mut fx.world, fx.player, Verb::Open, "d");
    assert!(!d, "`open d` is left to the door code");
    assert_eq!(fx.out(), "", "nothing was said about the sack");
    // Its full keyword is still a container.
    let (_sack2, _) = fx.chest(62, "a canvas sack", "satchel");
    assert!(try_container_verb(
        &mut fx.world,
        fx.player,
        Verb::Open,
        "satchel"
    ));
}

#[test]
fn a_non_container_item_falls_through_to_the_door_code() {
    use super::containers::{Verb, try_container_verb};
    let mut fx = Fx::new();
    let _key = item_at(&mut fx.world, fx.room, KEY.1, "an iron gate charm", "gate");
    assert!(!try_container_verb(
        &mut fx.world,
        fx.player,
        Verb::Open,
        "gate"
    ));
    assert_eq!(fx.out(), "", "no `You can't open that.`");
}
