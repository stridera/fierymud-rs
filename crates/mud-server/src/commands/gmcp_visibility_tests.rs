//! GMCP room panels follow the same visibility rules as the text
//! `look`: dark rooms (with the infravision "red shape" exception),
//! magical invisibility vs `detect_invisible`, and gods / `HOLY_LIGHT`.

use bevy_ecs::prelude::*;
use mud_db::enums::PlayerFlag;
use mud_world::{DetectInvis, Infravision, Invisible, Located, Mob, Named, PlayerFlags};
use serde_json::Value;

use super::dispatch;
use super::gmcp_tests::{Fx, drain_bytes, fixture, frames, of, player};
use super::test_support::Rx;

fn darken(fx: &mut Fx) {
    let a = fx.a;
    fx.world
        .entity_mut(a)
        .insert(mud_world::RoomMagicalDarkness);
}

fn mob(world: &mut World, room: Entity, name: &str) -> Entity {
    world
        .spawn((Mob, Named { name: name.into() }, Located(room)))
        .id()
}

/// Latest `package` payload from a fresh `look`, parsed.
fn after_look(fx: &mut Fx, p: Entity, rx: &mut Rx, package: &str) -> Value {
    drain_bytes(rx);
    dispatch(&mut fx.world, p, "look");
    let fr = frames(&drain_bytes(rx));
    let all = of(&fr, package);
    serde_json::from_str(all.last().unwrap_or_else(|| panic!("no {package}: {fr:?}"))).unwrap()
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn dark_room_hides_mobs_and_players_without_infravision() {
    let mut fx = fixture();
    darken(&mut fx);
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    mob(&mut fx.world, a, "a lurking wolf");
    let (_other, _orx) = player(&mut fx.world, a, "Bystander");

    assert_eq!(
        after_look(&mut fx, p, &mut rx, "Room.Mobs"),
        serde_json::json!([])
    );
    assert_eq!(
        after_look(&mut fx, p, &mut rx, "Room.Players"),
        serde_json::json!([])
    );
    assert_eq!(
        after_look(&mut fx, p, &mut rx, "Room.Services"),
        serde_json::json!({"services": []})
    );
    assert_eq!(
        after_look(&mut fx, p, &mut rx, "Room.Info"),
        serde_json::json!({})
    );
}

#[test]
fn dark_room_with_infravision_gets_generic_shapes_only() {
    let mut fx = fixture();
    darken(&mut fx);
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    fx.world.entity_mut(p).insert(Infravision);
    let wolf = mob(&mut fx.world, a, "a lurking wolf");
    let (_other, _orx) = player(&mut fx.world, a, "Bystander");

    let mobs = after_look(&mut fx, p, &mut rx, "Room.Mobs");
    let n = names(&mobs);
    assert_eq!(n.len(), 1, "{mobs}");
    assert!(n[0].contains("red shape"), "{mobs}");
    assert!(!mobs.to_string().contains("wolf"), "{mobs}");
    assert_eq!(mobs[0]["hostile"], false);
    let players = after_look(&mut fx, p, &mut rx, "Room.Players");
    assert!(names(&players)[0].contains("red shape"), "{players}");
    assert!(!players.to_string().contains("Bystander"), "{players}");

    // The shape has no detail view to click through to.
    drain_bytes(&mut rx);
    let payload = format!(r#"{{"id":"{}"}}"#, wolf.to_bits());
    super::handle_room_mob_get(&mut fx.world, p, &payload);
    assert!(of(&frames(&drain_bytes(&mut rx)), "Room.Mob.Info").is_empty());
}

#[test]
fn dark_room_diffs_name_nobody_to_a_blind_observer() {
    let mut fx = fixture();
    darken(&mut fx);
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    let (newcomer, _nrx) = player(&mut fx.world, a, "Sneaky");
    drain_bytes(&mut rx);
    super::broadcast_room_player_diff(&mut fx.world, a, newcomer, "AddPlayer");
    assert!(frames(&drain_bytes(&mut rx)).is_empty());

    fx.world.entity_mut(p).insert(Infravision);
    super::broadcast_room_player_diff(&mut fx.world, a, newcomer, "AddPlayer");
    let fr = frames(&drain_bytes(&mut rx));
    let add = of(&fr, "Room.AddPlayer");
    assert_eq!(add.len(), 1, "{fr:?}");
    assert!(
        add[0].contains("red shape") && !add[0].contains("Sneaky"),
        "{}",
        add[0]
    );
}

#[test]
fn holylight_god_sees_everything_in_the_dark() {
    let mut fx = fixture();
    darken(&mut fx);
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Godly");
    fx.world
        .entity_mut(p)
        .insert(PlayerFlags(vec![PlayerFlag::HolyLight]));
    let wolf = mob(&mut fx.world, a, "a lurking wolf");
    fx.world.entity_mut(wolf).insert(Invisible);
    let (other, _orx) = player(&mut fx.world, a, "Bystander");
    fx.world.entity_mut(other).insert(Invisible);

    assert_eq!(
        names(&after_look(&mut fx, p, &mut rx, "Room.Mobs")),
        ["a lurking wolf"]
    );
    assert_eq!(
        names(&after_look(&mut fx, p, &mut rx, "Room.Players")),
        ["Bystander"]
    );
    assert_eq!(
        after_look(&mut fx, p, &mut rx, "Room.Info")["name"],
        "Room 18"
    );
}

#[test]
fn invisible_mob_is_hidden_unless_the_viewer_detects_it() {
    let mut fx = fixture();
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    let ghost = mob(&mut fx.world, a, "a ghostly wolf");
    fx.world.entity_mut(ghost).insert(Invisible);
    mob(&mut fx.world, a, "a plain rat");

    assert_eq!(
        names(&after_look(&mut fx, p, &mut rx, "Room.Mobs")),
        ["a plain rat"]
    );
    // The text look agrees.
    drain_bytes(&mut rx);
    dispatch(&mut fx.world, p, "look");
    let text = String::from_utf8_lossy(&drain_bytes(&mut rx)).into_owned();
    assert!(
        text.contains("a plain rat") && !text.contains("ghostly"),
        "{text}"
    );

    fx.world.entity_mut(p).insert(DetectInvis);
    let seen = names(&after_look(&mut fx, p, &mut rx, "Room.Mobs"));
    assert!(seen.contains(&"a ghostly wolf".to_string()), "{seen:?}");
}

#[test]
fn invisible_mob_name_does_not_leak_through_aggro_or_combat() {
    let mut fx = fixture();
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    let ghost = mob(&mut fx.world, a, "a ghostly wolf");
    fx.world.entity_mut(ghost).insert((
        Invisible,
        crate::combat::HateList(vec![p]),
        mud_world::Health { hp: 10, max: 10 },
    ));
    fx.world.entity_mut(p).insert((
        mud_world::Health { hp: 10, max: 10 },
        mud_world::Fighting(ghost),
    ));
    super::send_prompt(&mut fx.world, p);
    let out = String::from_utf8_lossy(&drain_bytes(&mut rx)).into_owned();
    assert!(!out.contains("ghostly"), "{out}");
    assert!(out.contains("someone"), "{out}");
}

#[test]
fn prompt_refreshes_the_panels_when_the_light_goes_out() {
    let mut fx = fixture();
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    mob(&mut fx.world, a, "a lurking wolf");
    let (_other, _orx) = player(&mut fx.world, a, "Bystander");
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    assert!(of(&fr, "Room.Mobs")[0].contains("lurking wolf"), "{fr:?}");
    assert!(of(&fr, "Room.Players")[0].contains("Bystander"), "{fr:?}");

    darken(&mut fx);
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    assert_eq!(of(&fr, "Room.Mobs"), ["[]"], "{fr:?}");
    assert_eq!(of(&fr, "Room.Players"), ["[]"], "{fr:?}");
    assert_eq!(of(&fr, "Room.Info"), ["{}"], "{fr:?}");
}

#[test]
fn unrelated_component_changes_do_not_resend_the_room_lists() {
    #[derive(Component)]
    struct Marker;

    let mut fx = fixture();
    let a = fx.a;
    let (p, mut rx) = player(&mut fx.world, a, "Seeker");
    let (first, _frx) = player(&mut fx.world, a, "Alpha");
    let (_second, _srx) = player(&mut fx.world, a, "Zed");
    let wolf = mob(&mut fx.world, a, "a lurking wolf");
    let _rat = mob(&mut fx.world, a, "a rat");
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    assert_eq!(of(&fr, "Room.Players").len(), 1, "{fr:?}");
    assert_eq!(of(&fr, "Room.Mobs").len(), 1, "{fr:?}");

    // Moving entities to a new archetype reorders ECS query results, but
    // not what the viewer perceives.
    fx.world.entity_mut(first).insert(Marker);
    fx.world.entity_mut(wolf).insert(Marker);
    super::send_prompt(&mut fx.world, p);
    let fr = frames(&drain_bytes(&mut rx));
    assert!(of(&fr, "Room.Players").is_empty(), "resent players: {fr:?}");
    assert!(of(&fr, "Room.Mobs").is_empty(), "resent mobs: {fr:?}");
}
