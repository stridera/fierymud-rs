//! Legacy composition susceptibility on spell and skill damage
//! (`mag_damage`: `dam = dam * sus / 100`) and its pain reactions. Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::Composition;
use mud_world::{CombatStats, Health, Keywords, Located, Mob, Named, Posture, PostureKind};

use super::skill_target_tests::arena;
use super::test_support::drain;
use super::{dispatch, pain_reaction_line};

const DAMAGE: i32 = 9;
const KICK: i32 = 71;

fn body(world: &mut World, room: Entity, comp: Composition) -> Entity {
    world
        .spawn((
            Mob,
            Named {
                name: "a thing".into(),
            },
            Keywords(vec!["thing".into()]),
            Located(room),
            Health {
                hp: 1000,
                max: 1000,
            },
            CombatStats::default(),
            Posture(PostureKind::Standing),
            mud_world::CompositionOverride(comp),
        ))
        .id()
}

fn hit(comp: Composition, element: &str) -> i32 {
    let (mut fx, p, mut rx) = arena();
    fx.world
        .resource_mut::<mud_world::AbilityCatalog>()
        .effects_for
        .insert(
            KICK,
            vec![(
                DAMAGE,
                Some(serde_json::json!({ "amount": 100, "type": element })),
            )],
        );
    let room = fx.a;
    let t = body(&mut fx.world, room, comp);
    dispatch(&mut fx.world, p, "kick thing");
    let out = drain(&mut rx);
    let left = fx.world.get::<Health>(t).map_or(0, |h| h.hp);
    assert!(left > 0, "{out}");
    1000 - left
}

#[test]
fn fire_does_more_to_a_plant_and_nothing_to_a_fire_body() {
    assert_eq!(hit(Composition::Flesh, "fire"), 100);
    assert_eq!(hit(Composition::Plant, "fire"), 120);
    assert_eq!(hit(Composition::Fire, "fire"), 0);
    assert_eq!(hit(Composition::Water, "fire"), 50);
}

#[test]
fn physical_damage_types_read_their_own_column() {
    assert_eq!(hit(Composition::Plant, "slash"), 120);
    assert_eq!(hit(Composition::Plant, "crush"), 60);
    assert_eq!(hit(Composition::Ether, "pierce"), 0);
    // An element the legacy table has no column for is neutral.
    assert_eq!(hit(Composition::Plant, "holy"), 100);
}

#[test]
fn pain_reaction_matches_legacy_thresholds_and_one_in_four() {
    // Highly vulnerable (> 119): cries out. Very vulnerable (> 104): cringes.
    let cry = pain_reaction_line(120, 1, "the vine", "its").unwrap();
    assert!(cry.contains("the vine cries out in pain!"), "{cry}");
    let wince = pain_reaction_line(105, 1, "the vine", "its").unwrap();
    assert!(
        wince.contains("the vine cringes with a pained look on its face."),
        "{wince}"
    );
    assert!(pain_reaction_line(104, 1, "the vine", "its").is_none());
    assert!(pain_reaction_line(100, 1, "the vine", "its").is_none());
    for roll in 2..=4 {
        assert!(pain_reaction_line(150, roll, "the vine", "its").is_none());
    }
}

#[test]
fn the_room_sees_the_pain_but_the_victim_does_not() {
    let (mut fx, p, mut rx) = arena();
    let room = fx.a;
    let t = body(&mut fx.world, room, Composition::Plant);
    super::spell_pain_reaction(&mut fx.world, t, "a thing", super::Pronouns::IT, 120, 1);
    let out = drain(&mut rx);
    assert!(out.contains("A thing cries out in pain!"), "{p:?} {out}");
}
