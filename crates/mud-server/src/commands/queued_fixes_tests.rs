//! Four queued fixes: no speech while asleep (legacy stance gate), the
//! score `Rest:` row, huge prototype weights, and internal zones in the
//! achievements list. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, RestSource, UserRole};
use mud_world::{
    Account, Item, Keywords, Located, Named, ObjectPrototypes, Posture, PostureKind, Room,
    SocialDef, SocialRegistry, WorldKey,
};

use crate::commands::dispatch;
use crate::commands::test_support::{Rx, drain, object_proto, player_in};

const DREAMS: &str = "In your dreams, or what?";

fn account() -> Account {
    Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    }
}

fn social(name: &str) -> SocialDef {
    SocialDef {
        name: name.into(),
        hide: false,
        char_no_arg: Some(format!("You {name}.")),
        others_no_arg: None,
        char_found: None,
        others_found: None,
        vict_found: None,
        not_found: None,
        char_auto: None,
        others_auto: None,
    }
}

/// "Tester" and "Bob" in one room; Tester is in `posture`.
fn setup(posture: PostureKind) -> (World, Entity, Rx, Rx) {
    let mut world = World::new();
    world.insert_resource(ObjectPrototypes::default());
    let mut socials = SocialRegistry::default();
    for name in ["smile", "dream"] {
        socials.by_name.insert(name.into(), social(name));
    }
    world.insert_resource(socials);
    let room = world.spawn(Room).id();
    let (p, prx) = player_in(&mut world, room);
    world.entity_mut(p).insert((account(), Posture(posture)));
    let (bob, brx) = player_in(&mut world, room);
    world
        .entity_mut(bob)
        .insert((Named { name: "Bob".into() }, mud_world::Online, account()));
    (world, p, prx, brx)
}

// ---- 1. speech while asleep ----

#[test]
fn sleepers_cannot_use_room_speech_or_shout() {
    let (mut world, p, mut rx, mut brx) = setup(PostureKind::Sleeping);
    for line in [
        "say hello",
        "' hello",
        "emote smiles",
        ": smiles",
        "emote's eyes close",
        "ask bob about it",
        "whisper bob hi",
        "insult bob",
        "shout hey",
        "sh hey",
        "qsay hi",
    ] {
        dispatch(&mut world, p, line);
        let out = drain(&mut rx);
        assert!(out.contains(DREAMS), "{line}: {out}");
    }
    // Nothing reached the listener.
    assert!(drain(&mut brx).is_empty());
}

#[test]
fn sleepers_cannot_use_socials_except_dream() {
    let (mut world, p, mut rx, _brx) = setup(PostureKind::Sleeping);
    dispatch(&mut world, p, "smile");
    let out = drain(&mut rx);
    assert!(out.contains(DREAMS), "{out}");
    dispatch(&mut world, p, "dream");
    let out = drain(&mut rx);
    assert!(out.contains("You dream."), "{out}");
    assert!(!out.contains(DREAMS), "{out}");
}

#[test]
fn awake_and_resting_characters_still_speak() {
    for posture in [PostureKind::Standing, PostureKind::Resting] {
        let (mut world, p, mut rx, mut brx) = setup(posture);
        dispatch(&mut world, p, "say hello");
        assert!(drain(&mut rx).contains("You say,"));
        assert!(drain(&mut brx).contains("Tester"));
        dispatch(&mut world, p, "smile");
        assert!(drain(&mut rx).contains("You smile."));
    }
}

#[test]
fn legacy_sleeping_stance_channels_stay_open_to_sleepers() {
    // `gossip`, `tell`, `reply`, `gsay` and `clan` are STANCE_SLEEPING in
    // legacy `interpreter.cpp`, so they are not gated.
    let (mut world, p, mut rx, _brx) = setup(PostureKind::Sleeping);
    for line in ["gossip hi", "tell bob hi", "reply hi", "gsay hi"] {
        dispatch(&mut world, p, line);
        let out = drain(&mut rx);
        assert!(!out.contains(DREAMS), "{line}: {out}");
    }
}

// ---- 2. score Rest row ----

#[test]
fn score_rest_row_hides_empty_sources() {
    use crate::commands::info::score_rest_display;
    assert!(score_rest_display(RestSource::None, 0, 0).is_none());
    // A logged-out rest that banked nothing says nothing.
    assert!(score_rest_display(RestSource::Quit, 0, 0).is_none());
    let quit = score_rest_display(RestSource::Quit, 0, 500).expect("banked pool shows");
    assert_eq!(
        crate::commands::format_rest_line(quit),
        "Rested while logged out; 500 XP banked"
    );
    let inn = score_rest_display(RestSource::Inn, 2, 0).expect("inn shows");
    assert_eq!(
        crate::commands::format_rest_line(inn),
        "Rented Inn tier 2 (resting)"
    );
}

// ---- 3. huge weights ----

fn weighty(world: &mut World, holder: Entity, id: i32, weight: f64) -> Entity {
    let mut proto = object_proto(30, id, ObjectType::Other);
    proto.weight = weight;
    world
        .resource_mut::<ObjectPrototypes>()
        .by_key
        .insert((30, id), proto);
    world
        .spawn((
            Item,
            Named {
                name: "a huge boulder".into(),
            },
            Keywords(vec!["boulder".into()]),
            Located(holder),
            WorldKey { zone: 30, id },
        ))
        .id()
}

#[test]
fn clamp_weight_bounds_sentinels_and_junk() {
    use crate::commands::{MAX_ITEM_WEIGHT, clamp_weight, is_immovable_weight};
    assert!((clamp_weight(2_147_483_648.0) - MAX_ITEM_WEIGHT).abs() < f64::EPSILON);
    assert!((clamp_weight(f64::INFINITY) - MAX_ITEM_WEIGHT).abs() < f64::EPSILON);
    assert!(clamp_weight(f64::NAN).abs() < f64::EPSILON);
    assert!(clamp_weight(-5.0).abs() < f64::EPSILON);
    assert!((clamp_weight(12.5) - 12.5).abs() < f64::EPSILON);
    assert!(is_immovable_weight(MAX_ITEM_WEIGHT));
    assert!(!is_immovable_weight(600.0));
}

#[test]
fn carried_weight_stays_finite_with_sentinel_weights() {
    use crate::commands::{MAX_ITEM_WEIGHT, carried_weight, item_weight};
    let (mut world, p, _rx, _brx) = setup(PostureKind::Standing);
    let a = weighty(&mut world, p, 1, 2_147_483_648.0);
    weighty(&mut world, p, 2, 4_294_967_296.0);
    assert!((item_weight(&world, a) - MAX_ITEM_WEIGHT).abs() < f64::EPSILON);
    let total = carried_weight(&mut world, p);
    assert!(total.is_finite());
    assert!((total - 2.0 * MAX_ITEM_WEIGHT).abs() < 1e-6, "{total}");
}

#[test]
fn look_omits_the_weight_line_for_an_immovable_sentinel() {
    let (mut world, p, mut rx, _brx) = setup(PostureKind::Standing);
    let room = world.get::<Located>(p).unwrap().0;
    weighty(&mut world, room, 1, 2_147_483_648.0);
    weighty(&mut world, room, 2, 5.0);
    dispatch(&mut world, p, "look boulder");
    let out = drain(&mut rx);
    assert!(!out.contains("2147483648"), "{out}");
    assert!(!out.contains("1000000.0"), "{out}");
}

// ---- 4. internal zones in achievements ----

fn exploration_def(id: i32, code: &str, title: &str) -> mud_world::AchievementDef {
    mud_world::AchievementDef {
        id,
        code: code.into(),
        title: title.into(),
        description: "Visit it.".into(),
        category: mud_db::enums::AchievementCategory::Exploration,
        hidden: false,
        sort_order: id,
    }
}

#[test]
fn achievements_list_skips_internal_and_god_zones() {
    let (mut world, p, mut rx, _brx) = setup(PostureKind::Standing);
    let mut index = mud_world::WorldKeyIndex::default();
    let heavens = world.spawn((mud_world::Zone, mud_world::GodZone)).id();
    index.zones.insert(12, heavens);
    world.insert_resource(index);
    let mut catalog = mud_world::AchievementCatalog::default();
    for (id, code, title) in [
        (1, "zone_0_cleared", "Walked: Void"),
        (2, "zone_1_cleared", "Walked: Reserved - create food items"),
        (3, "zone_2_cleared", "Walked: non-loading items zone"),
        (4, "zone_12_cleared", "Walked: The Heavens"),
        (5, "zone_30_cleared", "Walked: Mielikki"),
        (6, "kills_100", "Centurion"),
    ] {
        let def = exploration_def(id, code, title);
        catalog.by_code.insert(code.into(), def.clone());
        catalog.by_id.insert(id, def);
    }
    world.insert_resource(catalog);
    dispatch(&mut world, p, "achievements");
    let out = drain(&mut rx);
    assert!(out.contains("Walked: Mielikki"), "{out}");
    assert!(out.contains("Centurion"), "{out}");
    for hidden in ["Void", "Reserved", "non-loading", "Heavens"] {
        assert!(!out.contains(hidden), "{hidden} listed: {out}");
    }
    assert!(out.contains("0 unlocked of 2 total."), "{out}");
}

#[test]
fn internal_zone_ids_are_zero_one_two() {
    assert!(mud_world::zone_is_internal(0));
    assert!(mud_world::zone_is_internal(1));
    assert!(mud_world::zone_is_internal(2));
    assert!(!mud_world::zone_is_internal(3));
    assert!(!mud_world::zone_is_internal(30));
}

#[test]
fn walking_an_internal_zone_grants_no_exploration_credit() {
    let (mut world, p, _rx, _brx) = setup(PostureKind::Standing);
    let mut catalog = mud_world::AchievementCatalog::default();
    let def = exploration_def(1, "zone_0_cleared", "Walked: Void");
    catalog.by_code.insert("zone_0_cleared".into(), def.clone());
    catalog.by_id.insert(1, def);
    world.insert_resource(catalog);
    world.insert_resource(mud_world::WorldKeyIndex::default());
    let void = world.spawn((Room, WorldKey { zone: 0, id: 0 })).id();
    world
        .resource_mut::<mud_world::WorldKeyIndex>()
        .rooms
        .insert((0, 0), void);
    crate::commands::mark_room_visited(&mut world, p, void);
    assert!(
        world
            .get::<mud_world::CharacterAchievements>(p)
            .is_none_or(|c| c.unlocked.is_empty())
    );
}
