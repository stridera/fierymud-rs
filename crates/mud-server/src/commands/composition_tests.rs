//! Composition-based interaction restrictions (issue #137): a body that is
//! not solid cannot pick things up, work doors, trade, grab or ride, and
//! the spells that need a solid body cannot take hold in it. Staff are
//! exempt. Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Composition, Direction, ExitState, MobTrait, ObjectType, Phase, Sector};
use mud_world::{
    AppliedTo, CoinPile, CompositionOverride, EffectInstance, ExitData, Exits, Keywords, Located,
    Mob, MobTraits, Mountable, Named, ObjectPrototypes, RoomSector, Wealth,
};

use super::composition::{refuse_steal, strip_unsuitable_effects};
use super::dispatch;
use super::god_zone_tests::Fx;
use super::item_guard_tests::item_at;
use super::test_support::{Rx, ability_def, drain, object_proto};

fn plain(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn said(rx: &mut Rx) -> String {
    plain(&drain(rx))
}

struct Scene {
    fx: Fx,
    room: Entity,
    other_room: Entity,
    mortal: Entity,
    mortal_rx: Rx,
}

fn scene() -> Scene {
    let mut fx = Fx::new();
    let mut objects = ObjectPrototypes::default();
    let mut chest = object_proto(30, 60, ObjectType::Container);
    chest.name = "an iron chest".to_string();
    chest.keywords = vec!["chest".to_string()];
    chest.values = serde_json::json!({"Flags": ["Closeable", "Closed"], "Capacity": 50});
    objects.by_key.insert((30, 60), chest);
    let mut sack = object_proto(30, 62, ObjectType::Container);
    sack.name = "a canvas sack".to_string();
    sack.keywords = vec!["sack".to_string()];
    sack.values = serde_json::json!({"Flags": ["Closeable"], "Capacity": 50});
    objects.by_key.insert((30, 62), sack);
    fx.world.insert_resource(objects);
    let zone = fx.zone(30, false);
    let room = fx.room(zone, 30, 1);
    let other_room = fx.room(zone, 30, 2);
    let (mortal, mortal_rx) = fx.person("Tester", 30, room);
    fx.world
        .entity_mut(mortal)
        .insert(Keywords(vec!["tester".into()]));
    Scene {
        fx,
        room,
        other_room,
        mortal,
        mortal_rx,
    }
}

impl Scene {
    fn fluid(&mut self, who: Entity) {
        self.fx
            .world
            .entity_mut(who)
            .insert(CompositionOverride(Composition::Water));
    }

    fn holder(&self, item: Entity) -> Entity {
        self.fx.world.get::<Located>(item).unwrap().0
    }

    fn immortal(&mut self) -> (Entity, Rx) {
        let (e, rx) = self.fx.person("Laoris", 100, self.room);
        self.fluid(e);
        (e, rx)
    }

    fn mob(&mut self, name: &str, kw: &str) -> Entity {
        self.fx
            .world
            .spawn((
                Mob,
                Named { name: name.into() },
                Keywords(vec![kw.into()]),
                Located(self.room),
            ))
            .id()
    }

    fn cmd(&mut self, line: &str) -> String {
        dispatch(&mut self.fx.world, self.mortal, line);
        said(&mut self.mortal_rx)
    }
}

// ---- the phase table ---------------------------------------------------------

#[test]
fn phases_follow_the_legacy_composition_table() {
    use Composition as C;
    for c in [
        C::Flesh,
        C::Earth,
        C::Ice,
        C::Metal,
        C::Stone,
        C::Bone,
        C::Lava,
        C::Plant,
    ] {
        assert_eq!(c.phase(), Phase::Solid, "{c:?}");
        assert!(c.is_rigid(), "{c:?}");
    }
    assert_eq!(C::Air.phase(), Phase::Gas);
    assert_eq!(C::Mist.phase(), Phase::Gas);
    assert_eq!(C::Fire.phase(), Phase::Plasma);
    assert_eq!(C::Water.phase(), Phase::Liquid);
    assert_eq!(C::Ether.phase(), Phase::Ether);
    for c in [C::Air, C::Mist, C::Fire, C::Water, C::Ether] {
        assert!(!c.is_rigid(), "{c:?}");
    }
}

#[test]
fn an_illusion_is_rigid_but_not_a_solid_character() {
    let mut s = scene();
    let ghost = s.mob("an illusory knight", "knight");
    assert!(mud_world::actor_is_rigid(&s.fx.world, ghost));
    assert!(mud_world::actor_is_solid_char(&s.fx.world, ghost));
    s.fx.world
        .entity_mut(ghost)
        .insert(MobTraits(vec![MobTrait::Illusion]));
    assert!(mud_world::actor_is_rigid(&s.fx.world, ghost));
    assert!(!mud_world::actor_is_solid_char(&s.fx.world, ghost));
}

// ---- get ---------------------------------------------------------------------

#[test]
fn a_fluid_body_cannot_pick_things_up_off_the_floor() {
    let mut s = scene();
    let sword = item_at(&mut s.fx.world, s.room, 7, "a steel sword", "sword");
    s.fluid(s.mortal);
    let out = s.cmd("get sword");
    assert!(
        out.contains("In your fluid state, you can't pick things up."),
        "{out}"
    );
    assert_eq!(s.holder(sword), s.room);
    let out = s.cmd("get all");
    assert!(out.contains("In your fluid state"), "{out}");
    assert_eq!(s.holder(sword), s.room);
}

#[test]
fn a_solid_body_picks_things_up() {
    let mut s = scene();
    let sword = item_at(&mut s.fx.world, s.room, 7, "a steel sword", "sword");
    let out = s.cmd("get sword");
    assert!(out.contains("You pick up a steel sword."), "{out}");
    assert_eq!(s.holder(sword), s.mortal);
}

#[test]
fn staff_pick_things_up_whatever_their_body() {
    let mut s = scene();
    let sword = item_at(&mut s.fx.world, s.room, 7, "a steel sword", "sword");
    let (god, mut rx) = s.immortal();
    dispatch(&mut s.fx.world, god, "get sword");
    let out = said(&mut rx);
    assert!(out.contains("You pick up a steel sword."), "{out}");
    assert_eq!(s.holder(sword), god);
}

#[test]
fn a_fluid_body_reaches_into_a_container_it_carries() {
    let mut s = scene();
    let carried = item_at(&mut s.fx.world, s.mortal, 62, "a canvas sack", "sack");
    let gem = item_at(&mut s.fx.world, carried, 70, "a ruby", "ruby");
    s.fluid(s.mortal);
    let out = s.cmd("get ruby from sack");
    assert!(out.contains("You take a ruby from a canvas sack."), "{out}");
    assert_eq!(s.holder(gem), s.mortal);
}

#[test]
fn a_fluid_body_cannot_loot_a_container_lying_on_the_floor() {
    let mut s = scene();
    let sack = item_at(&mut s.fx.world, s.room, 62, "a canvas sack", "sack");
    let gem = item_at(&mut s.fx.world, sack, 70, "a ruby", "ruby");
    s.fluid(s.mortal);
    let out = s.cmd("get ruby from sack");
    assert!(
        out.contains("You can't handle solid objects in your condition."),
        "{out}"
    );
    assert_eq!(s.holder(gem), sack);

    // Solid again: it works.
    s.fx.world
        .entity_mut(s.mortal)
        .remove::<CompositionOverride>();
    let out = s.cmd("get ruby from sack");
    assert!(!out.contains("can't handle"), "{out}");
    assert_eq!(s.holder(gem), s.mortal);
}

// ---- doors and containers ----------------------------------------------------

fn closed_door(s: &mut Scene) {
    let to = s.other_room;
    s.fx.world.get_mut::<Exits>(s.room).unwrap().0.insert(
        Direction::North,
        ExitData {
            to: Some(to),
            state: ExitState::Closed,
            key: None,
            description: None,
            keywords: vec!["door".to_string()],
            is_hidden: false,
            is_pickproof: false,
            is_bashable: false,
            hit_points: None,
        },
    );
}

fn door_state(s: &Scene) -> ExitState {
    s.fx.world.get::<Exits>(s.room).unwrap().0[&Direction::North].state
}

#[test]
fn a_fluid_body_cannot_work_a_door() {
    let mut s = scene();
    closed_door(&mut s);
    s.fluid(s.mortal);
    for verb in ["open", "close", "lock", "unlock", "pick"] {
        let out = s.cmd(&format!("{verb} north"));
        assert!(
            out.contains("You can't operate doors in your watery state."),
            "{verb}: {out}"
        );
    }
    assert_eq!(door_state(&s), ExitState::Closed);
}

#[test]
fn a_solid_body_opens_a_door_and_so_does_staff_in_any_shape() {
    let mut s = scene();
    closed_door(&mut s);
    let out = s.cmd("open north");
    assert!(out.contains("You open"), "{out}");
    assert_eq!(door_state(&s), ExitState::Open);
    s.cmd("close north");
    assert_eq!(door_state(&s), ExitState::Closed);

    let (god, mut rx) = s.immortal();
    dispatch(&mut s.fx.world, god, "open north");
    let out = said(&mut rx);
    assert!(out.contains("You open"), "{out}");
    assert_eq!(door_state(&s), ExitState::Open);
}

#[test]
fn an_illusion_cannot_work_a_door_either() {
    let mut s = scene();
    closed_door(&mut s);
    s.fx.world
        .entity_mut(s.mortal)
        .insert(MobTraits(vec![MobTrait::Illusion]));
    let out = s.cmd("open north");
    assert!(out.contains("You can't operate doors in your"), "{out}");
    assert_eq!(door_state(&s), ExitState::Closed);
}

#[test]
fn a_fluid_body_works_a_container_it_carries() {
    let mut s = scene();
    let held = item_at(&mut s.fx.world, s.mortal, 60, "an iron chest", "chest");
    s.fluid(s.mortal);
    let out = s.cmd("open chest");
    assert!(out.contains("You open"), "{out}");
    assert_eq!(
        super::containers::container_state(&s.fx.world, held).map(|c| c.closed),
        Some(false)
    );
}

#[test]
fn a_fluid_body_cannot_work_a_container_lying_about() {
    let mut s = scene();
    let floor = item_at(&mut s.fx.world, s.room, 60, "an iron chest", "chest");
    s.fluid(s.mortal);
    let out = s.cmd("open chest");
    assert!(
        out.contains("You can't manipulate solid objects in your watery state."),
        "{out}"
    );
    assert_eq!(
        super::containers::container_state(&s.fx.world, floor).map(|c| c.closed),
        Some(true)
    );
    // Staff are not held back.
    let (god, mut rx) = s.immortal();
    dispatch(&mut s.fx.world, god, "open chest");
    assert!(said(&mut rx).contains("You open"));
}

// ---- drop / put flavour ------------------------------------------------------

#[test]
fn what_leaves_a_fluid_hand_turns_solid_again() {
    let mut s = scene();
    item_at(&mut s.fx.world, s.mortal, 7, "a steel sword", "sword");
    s.fluid(s.mortal);
    let out = s.cmd("drop sword");
    assert!(out.contains("You drop a steel sword."), "{out}");
    assert!(
        out.contains("A steel sword becomes solid again as it leaves your grasp."),
        "{out}"
    );
}

#[test]
fn drop_all_prints_the_grouped_line_before_the_solid_flavour() {
    let mut s = scene();
    for _ in 0..2 {
        item_at(&mut s.fx.world, s.mortal, 7, "a steel sword", "sword");
    }
    s.fluid(s.mortal);
    let out = s.cmd("drop all");
    let line = out.find("You drop a steel sword. (x2)").expect(&out);
    let flavour = out.find("becomes solid again").expect(&out);
    assert!(line < flavour, "{out}");
}

#[test]
fn a_solid_hand_drops_without_the_flavour() {
    let mut s = scene();
    item_at(&mut s.fx.world, s.mortal, 7, "a steel sword", "sword");
    let out = s.cmd("drop sword");
    assert!(!out.contains("becomes solid"), "{out}");
}

#[test]
fn putting_into_a_container_not_carried_turns_solid_but_a_carried_one_does_not() {
    let mut s = scene();
    let floor_sack = item_at(&mut s.fx.world, s.room, 62, "a canvas sack", "sack");
    item_at(&mut s.fx.world, s.mortal, 7, "a steel sword", "sword");
    s.fluid(s.mortal);
    let out = s.cmd("put sword sack");
    assert!(out.contains("becomes solid again"), "{out}");

    s.fx.world.entity_mut(floor_sack).insert(Located(s.mortal));
    item_at(&mut s.fx.world, s.mortal, 8, "a steel dagger", "dagger");
    let out = s.cmd("put dagger sack");
    assert!(out.contains("You put a steel dagger in"), "{out}");
    assert!(!out.contains("becomes solid"), "{out}");
}

// ---- give ---------------------------------------------------------------------

#[test]
fn an_item_given_to_a_fluid_body_falls_to_the_ground() {
    let mut s = scene();
    let (bob, mut bob_rx) = s.fx.person("Bob", 30, s.room);
    s.fx.world
        .entity_mut(bob)
        .insert(Keywords(vec!["bob".into()]));
    s.fluid(bob);
    let sword = item_at(&mut s.fx.world, s.mortal, 7, "a steel sword", "sword");
    let out = s.cmd("give sword bob");
    assert!(
        out.contains("You hand a steel sword to Bob, but it simply falls to the ground."),
        "{out}"
    );
    assert!(
        said(&mut bob_rx).contains("Tester gives you a steel sword, but you can't hold onto it."),
    );
    assert_eq!(s.holder(sword), s.room);
}

#[test]
fn a_gift_into_water_says_so() {
    let mut s = scene();
    s.fx.world
        .entity_mut(s.room)
        .insert(RoomSector(Sector::Water));
    let (bob, _rx) = s.fx.person("Bob", 30, s.room);
    s.fx.world
        .entity_mut(bob)
        .insert(Keywords(vec!["bob".into()]));
    s.fluid(bob);
    item_at(&mut s.fx.world, s.mortal, 7, "a steel sword", "sword");
    let out = s.cmd("give sword bob");
    assert!(out.contains("it simply falls into the water."), "{out}");
}

#[test]
fn a_fluid_giver_hands_over_a_solid_item_and_says_it_turned_solid() {
    let mut s = scene();
    let (bob, mut bob_rx) = s.fx.person("Bob", 30, s.room);
    s.fx.world
        .entity_mut(bob)
        .insert(Keywords(vec!["bob".into()]));
    s.fluid(s.mortal);
    let sword = item_at(&mut s.fx.world, s.mortal, 7, "a steel sword", "sword");
    let out = s.cmd("give sword bob");
    assert!(
        out.contains("A steel sword becomes solid again as it leaves your grasp."),
        "{out}"
    );
    assert_eq!(s.holder(sword), bob);
    assert!(said(&mut bob_rx).contains("Tester gives you a steel sword."));
}

#[test]
fn two_fluid_bodies_trade_as_normal() {
    let mut s = scene();
    let (bob, mut bob_rx) = s.fx.person("Bob", 30, s.room);
    s.fx.world
        .entity_mut(bob)
        .insert(Keywords(vec!["bob".into()]));
    s.fluid(s.mortal);
    s.fluid(bob);
    let sword = item_at(&mut s.fx.world, s.mortal, 7, "a steel sword", "sword");
    let out = s.cmd("give sword bob");
    assert!(!out.contains("solid again"), "{out}");
    assert!(out.contains("You give a steel sword to Bob."), "{out}");
    assert_eq!(s.holder(sword), bob);
    drain(&mut bob_rx);
}

#[test]
fn staff_can_receive_a_gift_in_any_shape() {
    let mut s = scene();
    let (god, _rx) = s.immortal();
    s.fx.world
        .entity_mut(god)
        .insert(Keywords(vec!["laoris".into()]));
    let sword = item_at(&mut s.fx.world, s.mortal, 7, "a steel sword", "sword");
    let out = s.cmd("give sword laoris");
    assert!(out.contains("You give a steel sword to Laoris."), "{out}");
    assert_eq!(s.holder(sword), god);
}

#[test]
fn a_staff_giver_keeps_their_purse_and_conjures_no_pile() {
    let mut s = scene();
    let (bob, _bob_rx) = s.fx.person("Bob", 30, s.room);
    s.fx.world
        .entity_mut(bob)
        .insert(Keywords(vec!["bob".into()]));
    s.fluid(bob);
    // Staff (Builder and up) keep their purse when they give.
    let (god, mut god_rx) = s.fx.person("Laoris", 101, s.room);
    s.fx.world.entity_mut(god).insert(Wealth(500));
    dispatch(&mut s.fx.world, god, "give 100 copper bob");
    assert!(
        said(&mut god_rx).contains("fall right through Bob's hands"),
        "described"
    );
    assert_eq!(s.fx.world.get::<Wealth>(god).unwrap().0, 500);
    let piles = s.fx.world.query::<&CoinPile>().iter(&s.fx.world).count();
    assert_eq!(piles, 0, "no coins from nowhere");
}

#[test]
fn coins_given_to_a_fluid_body_fall_through_its_hands() {
    let mut s = scene();
    let (bob, mut bob_rx) = s.fx.person("Bob", 30, s.room);
    s.fx.world
        .entity_mut(bob)
        .insert(Keywords(vec!["bob".into()]));
    s.fluid(bob);
    s.fx.world.entity_mut(s.mortal).insert(Wealth(500));
    let out = s.cmd("give 100 copper bob");
    assert!(
        out.contains("The coins seem to fall right through Bob's hands!"),
        "{out}"
    );
    assert_eq!(s.fx.world.get::<Wealth>(s.mortal).unwrap().0, 400);
    assert!(s.fx.world.get::<Wealth>(bob).is_none_or(|w| w.0 == 0));
    let piles: Vec<i64> = {
        let mut q = s.fx.world.query::<(&CoinPile, &Located)>();
        q.iter(&s.fx.world)
            .filter(|(_, l)| l.0 == s.room)
            .map(|(p, _)| p.0)
            .collect()
    };
    assert_eq!(piles, vec![100]);
    drain(&mut bob_rx);
}

#[test]
fn coins_to_a_solid_body_still_arrive() {
    let mut s = scene();
    let (bob, _rx) = s.fx.person("Bob", 30, s.room);
    s.fx.world
        .entity_mut(bob)
        .insert(Keywords(vec!["bob".into()]));
    s.fx.world.entity_mut(s.mortal).insert(Wealth(500));
    s.cmd("give 100 copper bob");
    assert_eq!(s.fx.world.get::<Wealth>(bob).unwrap().0, 100);
}

// ---- steal -------------------------------------------------------------------

#[test]
fn a_fluid_thief_cannot_handle_objects() {
    let mut s = scene();
    assert!(!refuse_steal(&s.fx.world, s.mortal));
    s.fluid(s.mortal);
    assert!(refuse_steal(&s.fx.world, s.mortal));
    let out = said(&mut s.mortal_rx);
    assert!(
        out.contains("You can't handle objects in your condition."),
        "{out}"
    );
    let (god, mut rx) = s.immortal();
    assert!(!refuse_steal(&s.fx.world, god));
    assert_eq!(said(&mut rx), "");
}

// ---- mount ---------------------------------------------------------------------

#[test]
fn a_solid_rider_cannot_sit_a_fluid_mount() {
    let mut s = scene();
    let mare = s.mob("a stout mare", "mare");
    s.fx.world.entity_mut(mare).insert(Mountable);
    s.fluid(mare);
    let out = s.cmd("mount mare");
    assert!(
        out.contains("You lay a hand on a stout mare, but it slips right through."),
        "{out}"
    );
    assert!(s.fx.world.get::<mud_world::Mounted>(s.mortal).is_none());
}

#[test]
fn an_ethereal_mount_cannot_even_be_touched() {
    let mut s = scene();
    let mare = s.mob("a stout mare", "mare");
    s.fx.world
        .entity_mut(mare)
        .insert((Mountable, CompositionOverride(Composition::Ether)));
    let out = s.cmd("mount mare");
    assert!(out.contains("you pass right through"), "{out}");
}

#[test]
fn a_fluid_rider_just_flows_off_a_solid_mount() {
    let mut s = scene();
    let mare = s.mob("a stout mare", "mare");
    s.fx.world.entity_mut(mare).insert(Mountable);
    s.fluid(s.mortal);
    let out = s.cmd("mount mare");
    assert!(
        out.contains("You try to climb it, but you just flow right off."),
        "{out}"
    );
    assert!(s.fx.world.get::<mud_world::Mounted>(s.mortal).is_none());
}

#[test]
fn like_bodies_mount_and_staff_mount_anything() {
    let mut s = scene();
    let mare = s.mob("a stout mare", "mare");
    s.fx.world.entity_mut(mare).insert(Mountable);
    let out = s.cmd("mount mare");
    assert!(out.contains("You mount a stout mare."), "{out}");
    s.cmd("dismount");

    // Both fluid: fine.
    s.fluid(s.mortal);
    s.fluid(mare);
    let out = s.cmd("mount mare");
    assert!(out.contains("You mount"), "{out}");
    s.cmd("dismount");

    // Staff (fluid) on a solid mount.
    s.fx.world.entity_mut(mare).remove::<CompositionOverride>();
    let (god, mut rx) = s.immortal();
    dispatch(&mut s.fx.world, god, "mount mare");
    assert!(said(&mut rx).contains("You mount"));
}

// ---- drag ------------------------------------------------------------------------

#[test]
fn a_fluid_body_cannot_drag_a_solid_object() {
    let mut s = scene();
    item_at(&mut s.fx.world, s.room, 7, "a steel sword", "sword");
    s.fluid(s.mortal);
    let out = s.cmd("drag sword north");
    assert!(
        out.contains("You can't handle solid objects in your watery condition."),
        "{out}"
    );
}

#[test]
fn a_solid_hand_passes_through_a_fluid_body_and_the_other_way_round() {
    let mut s = scene();
    let (bob, mut bob_rx) = s.fx.person("Bob", 30, s.room);
    s.fx.world
        .entity_mut(bob)
        .insert(Keywords(vec!["bob".into()]));
    s.fluid(bob);
    let out = s.cmd("drag bob north");
    assert!(
        out.contains("Bob's water seems to pass right through your fingers."),
        "{out}"
    );
    assert!(
        said(&mut bob_rx)
            .contains("tries to grab you, but they can't get a grip on your watery flesh."),
    );

    // Reverse: the fluid one cannot grip the solid one.
    s.fx.world.entity_mut(bob).remove::<CompositionOverride>();
    s.fluid(s.mortal);
    let out = s.cmd("drag bob north");
    assert!(
        out.contains("You can't get hold of Bob with your watery grip."),
        "{out}"
    );
}

// ---- spells ------------------------------------------------------------------------

const STONE_SKIN: i32 = 401;
const WATERFORM: i32 = 402;
const FX_STATUS: i32 = 9;

fn spell_fixture(s: &mut Scene) {
    let mut abilities = mud_world::AbilityCatalog::default();
    for (id, name, rigid, flag) in [
        (STONE_SKIN, "Stone Skin", true, "bless"),
        (WATERFORM, "Waterform", false, "waterform"),
    ] {
        let mut def = ability_def(id, name, AbilityKind::Spell);
        def.cast_time_rounds = 0;
        def.requires_rigid = rigid;
        abilities.by_name.insert(name.to_ascii_lowercase(), def);
        abilities.effects_for.insert(
            id,
            vec![(
                FX_STATUS,
                Some(serde_json::json!({"flag": flag, "duration": "60"})),
            )],
        );
    }
    s.fx.world.insert_resource(abilities);
    let mut effects = mud_world::EffectCatalog::default();
    effects.by_id.insert(
        FX_STATUS,
        mud_world::EffectDef {
            id: FX_STATUS,
            name: "status".into(),
            description: None,
            effect_type: "status".into(),
            tags: vec![],
            presence_override: None,
            default_params: serde_json::json!({}),
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        },
    );
    s.fx.world.insert_resource(effects);
}

fn cast(world: &mut World, caster: Entity, args: &str) {
    crate::commands::invoke_ability_with(
        world,
        caster,
        args,
        AbilityKind::Spell,
        "cast",
        false,
        true,
        true,
        None,
    );
}

fn instances_of(world: &mut World, who: Entity, ability: i32) -> usize {
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    q.iter(world)
        .filter(|(i, a)| a.0 == who && i.ability_id == Some(ability))
        .count()
}

#[test]
fn stone_skin_is_refused_on_a_waterform_target() {
    let mut s = scene();
    spell_fixture(&mut s);
    s.fluid(s.mortal);
    cast(&mut s.fx.world, s.mortal, "stone skin");
    let out = said(&mut s.mortal_rx);
    assert!(
        out.contains("The spell is unable to take hold in your substance."),
        "{out}"
    );
    assert_eq!(instances_of(&mut s.fx.world, s.mortal, STONE_SKIN), 0);
}

#[test]
fn the_caster_hears_that_the_spell_cannot_alter_a_fluid_friend() {
    let mut s = scene();
    spell_fixture(&mut s);
    let (bob, mut bob_rx) = s.fx.person("Bob", 30, s.room);
    s.fx.world
        .entity_mut(bob)
        .insert(Keywords(vec!["bob".into()]));
    s.fluid(bob);
    cast(&mut s.fx.world, s.mortal, "stone skin bob");
    let out = said(&mut s.mortal_rx);
    assert!(
        out.contains("The spell is unable to alter Bob's substance."),
        "{out}"
    );
    assert!(said(&mut bob_rx).contains("The spell is unable to take hold in your substance."));
    assert_eq!(instances_of(&mut s.fx.world, bob, STONE_SKIN), 0);
}

#[test]
fn stone_skin_lands_on_a_solid_body() {
    let mut s = scene();
    spell_fixture(&mut s);
    cast(&mut s.fx.world, s.mortal, "stone skin");
    assert_eq!(instances_of(&mut s.fx.world, s.mortal, STONE_SKIN), 1);
}

#[test]
fn turning_to_water_strips_stone_skin_with_its_wearoff_line() {
    let mut s = scene();
    spell_fixture(&mut s);
    cast(&mut s.fx.world, s.mortal, "stone skin");
    assert_eq!(instances_of(&mut s.fx.world, s.mortal, STONE_SKIN), 1);
    drain(&mut s.mortal_rx);
    cast(&mut s.fx.world, s.mortal, "waterform");
    assert_eq!(
        mud_world::actor_composition(&s.fx.world, s.mortal),
        Composition::Water
    );
    assert_eq!(instances_of(&mut s.fx.world, s.mortal, STONE_SKIN), 0);
    assert_eq!(instances_of(&mut s.fx.world, s.mortal, WATERFORM), 1);
    let out = said(&mut s.mortal_rx);
    // Legacy `remove_unsuitable_spells` goes through `active_effect_remove`,
    // which says the spell's wear-off text.
    assert_eq!(out.matches("fades").count(), 1, "one wear-off line: {out}");
}

#[test]
fn the_tick_sweep_catches_a_body_that_went_fluid_by_another_route() {
    let mut s = scene();
    spell_fixture(&mut s);
    cast(&mut s.fx.world, s.mortal, "stone skin");
    s.fluid(s.mortal);
    assert_eq!(instances_of(&mut s.fx.world, s.mortal, STONE_SKIN), 1);
    super::composition::sweep_fluid_bearers(&mut s.fx.world);
    assert_eq!(instances_of(&mut s.fx.world, s.mortal, STONE_SKIN), 0);
    // Nothing to strip from a solid body.
    s.fx.world
        .entity_mut(s.mortal)
        .remove::<CompositionOverride>();
    assert_eq!(strip_unsuitable_effects(&mut s.fx.world, s.mortal), 0);
}

#[test]
fn staff_take_the_spell_whatever_their_body() {
    let mut s = scene();
    spell_fixture(&mut s);
    let (god, _rx) = s.immortal();
    cast(&mut s.fx.world, god, "stone skin");
    assert_eq!(instances_of(&mut s.fx.world, god, STONE_SKIN), 1);
}
