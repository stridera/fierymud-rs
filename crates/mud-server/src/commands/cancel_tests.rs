//! `cancel` (issue #136): drop one of your own timed, beneficial spells.
//! Out of combat only; permanent and harmful effects refuse; the effect
//! ends through the expiry path so stat mods reverse exactly.

use super::dispatch;
use super::test_support::{Rx, ability_def, drain, player_in};
use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::UserRole;
use mud_world::{
    AbilityCatalog, AbilityMessageSet, Account, AppliedTo, CoreStats, EffectCatalog, EffectDef,
    EffectInstance, EffectSource, Fighting, Health, Located, ModifyDelta, Named, Room,
};

const BLESS: i32 = 1;
const CURSE: i32 = 2;
const STONE_SKIN: i32 = 3;
const FX_MODIFY: i32 = 10;
const FX_STATUS: i32 = 11;
const WATERFORM: i32 = 20;
const POISON: i32 = 21;
const BLINDNESS: i32 = 22;
const SLEEP: i32 = 23;
const PARALYSIS: i32 = 24;
const WALL_OF_STONE: i32 = 25;

fn effect_def(id: i32, name: &str, tags: &[&str]) -> EffectDef {
    EffectDef {
        id,
        name: name.into(),
        description: None,
        effect_type: "modify".into(),
        tags: tags.iter().map(|t| (*t).to_string()).collect(),
        presence_override: None,
        default_params: serde_json::json!({}),
        prevents_speaking: false,
        prevents_casting: false,
        prevents_movement: false,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    }
}

fn setup() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    let mut abilities = AbilityCatalog::default();
    for (id, name, violent) in [
        (BLESS, "Bless", false),
        (CURSE, "Curse", true),
        (STONE_SKIN, "Stone Skin", false),
        (WATERFORM, "Waterform", false),
        (POISON, "Poison", true),
        (BLINDNESS, "Blindness", true),
        (SLEEP, "Sleep", true),
        // Not flagged violent: hostile by its ENEMY target list alone.
        (PARALYSIS, "Paralysis", false),
        (WALL_OF_STONE, "Wall of Stone", false),
    ] {
        let mut def = ability_def(id, name, AbilityKind::Spell);
        def.violent = violent;
        abilities
            .by_name
            .insert(def.plain_name.to_ascii_lowercase(), def);
    }
    abilities.messages.insert(
        BLESS,
        AbilityMessageSet {
            wearoff_to_target: Some("You feel less blessed.".into()),
            wearoff_to_room: Some("{target.name} looks less blessed.".into()),
            ..Default::default()
        },
    );
    abilities.targeting.insert(
        PARALYSIS,
        mud_world::TargetingRule {
            valid_targets: vec!["ENEMY_PC".into(), "ENEMY_NPC".into()],
            scope: "SINGLE".into(),
            max_targets: 1,
            require_los: false,
        },
    );
    world.insert_resource(abilities);
    let mut effects = EffectCatalog::default();
    // The tag shapes the shared catalog rows really carry: both `buff` and
    // `debuff`, so the tags cannot tell a blessing from a curse.
    effects.by_id.insert(
        FX_MODIFY,
        effect_def(FX_MODIFY, "modify", &["buff", "debuff", "attribute"]),
    );
    effects.by_id.insert(
        FX_STATUS,
        effect_def(FX_STATUS, "status", &["status", "buff", "debuff"]),
    );
    world.insert_resource(effects);
    let room = world.spawn(Room).id();
    let (player, rx) = player_in(&mut world, room);
    world.entity_mut(player).insert((
        Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        },
        CoreStats {
            strength: 50,
            ..Default::default()
        },
        Health { hp: 100, max: 100 },
    ));
    (world, room, player, rx)
}

fn spawn_effect(
    world: &mut World,
    on: Entity,
    kind: i32,
    name: &str,
    remaining: i32,
    ability_id: Option<i32>,
    source: EffectSource,
) -> Entity {
    world
        .spawn((
            EffectInstance {
                kind,
                name: name.into(),
                strength: 1,
                remaining_secs: remaining,
                source,
                ability_id,
            },
            AppliedTo(on),
        ))
        .id()
}

/// A stat buff applied the way a cast records it.
fn buff(world: &mut World, p: Entity, ability: i32, remaining: i32, amount: i32) -> Entity {
    assert!(super::apply_modify_delta(world, p, "strength", amount));
    let e = spawn_effect(
        world,
        p,
        FX_MODIFY,
        "strength",
        remaining,
        Some(ability),
        EffectSource::Spell,
    );
    world.entity_mut(e).insert(ModifyDelta {
        target: "strength".into(),
        amount,
    });
    e
}

fn strength(world: &World, p: Entity) -> i32 {
    world.get::<CoreStats>(p).unwrap().strength
}

fn alive(world: &World, e: Entity) -> bool {
    world.get::<EffectInstance>(e).is_some()
}

#[test]
fn cancel_removes_a_beneficial_timed_effect_and_reverses_its_stat_exactly() {
    let (mut world, room, p, mut rx) = setup();
    let watcher_room = room;
    let (watcher, mut wrx) = player_in(&mut world, watcher_room);
    world.entity_mut(watcher).insert(Named {
        name: "Watcher".into(),
    });
    let e = buff(&mut world, p, BLESS, 300, 7);
    assert_eq!(strength(&world, p), 57);
    dispatch(&mut world, p, "cancel bless");
    assert!(!alive(&world, e));
    assert_eq!(strength(&world, p), 50, "stat back to exactly the base");
    let out = drain(&mut rx);
    assert!(out.contains("You cancel Bless."), "{out}");
    assert!(
        out.contains("You feel less blessed."),
        "wear-off text: {out}"
    );
    let seen = drain(&mut wrx);
    assert!(seen.contains("looks less blessed"), "room line: {seen}");
}

#[test]
fn cancel_reverses_stacked_buffs_independently() {
    let (mut world, _room, p, mut rx) = setup();
    let bless = buff(&mut world, p, BLESS, 300, 7);
    let skin = buff(&mut world, p, STONE_SKIN, 300, 3);
    assert_eq!(strength(&world, p), 60);
    dispatch(&mut world, p, "cancel bless");
    let _ = drain(&mut rx);
    assert!(!alive(&world, bless) && alive(&world, skin));
    assert_eq!(strength(&world, p), 53);
    dispatch(&mut world, p, "cancel stone");
    assert_eq!(strength(&world, p), 50);
}

#[test]
fn cancel_matches_natural_expiry_state() {
    // Cancelling leaves the same stat and effect list as letting it expire.
    let (mut a, _ra, pa, _rxa) = setup();
    let (mut b, _rb, pb, _rxb) = setup();
    buff(&mut a, pa, BLESS, 1, 9);
    buff(&mut b, pb, BLESS, 1, 9);
    dispatch(&mut a, pa, "cancel bless");
    b.insert_resource(crate::TickCount(10));
    crate::effects::effects_tick(&mut b);
    assert_eq!(strength(&a, pa), strength(&b, pb));
    let count = |w: &mut World| w.query::<&EffectInstance>().iter(w).count();
    assert_eq!(count(&mut a), count(&mut b));
}

#[test]
fn cancel_refuses_in_combat() {
    let (mut world, room, p, mut rx) = setup();
    let e = buff(&mut world, p, BLESS, 300, 7);
    let foe = world
        .spawn((Located(room), Health { hp: 10, max: 10 }))
        .id();
    world.entity_mut(p).insert(Fighting(foe));
    dispatch(&mut world, p, "cancel bless");
    assert!(alive(&world, e));
    assert_eq!(strength(&world, p), 57);
    assert!(drain(&mut rx).contains("fight"));
}

#[test]
fn cancel_refuses_when_someone_is_fighting_you() {
    let (mut world, room, p, mut rx) = setup();
    let e = buff(&mut world, p, BLESS, 300, 7);
    world.spawn((Located(room), Fighting(p)));
    dispatch(&mut world, p, "cancel bless");
    assert!(alive(&world, e));
    assert!(drain(&mut rx).contains("fight"));
}

#[test]
fn cancel_refuses_permanent_effects() {
    let (mut world, _room, p, mut rx) = setup();
    // A spell effect with no end, and a race innate.
    let endless = buff(&mut world, p, BLESS, -1, 4);
    let innate = spawn_effect(
        &mut world,
        p,
        FX_MODIFY,
        "infravision",
        -1,
        None,
        EffectSource::Other(mud_world::mob_effects::RACE_EFFECT_SOURCE.into()),
    );
    dispatch(&mut world, p, "cancel bless");
    let out = drain(&mut rx);
    assert!(out.contains("permanent"), "{out}");
    dispatch(&mut world, p, "cancel infravision");
    assert!(drain(&mut rx).contains("permanent"));
    assert!(alive(&world, endless) && alive(&world, innate));
    assert_eq!(strength(&world, p), 54);
}

#[test]
fn cancel_refuses_harmful_effects() {
    let (mut world, _room, p, mut rx) = setup();
    // Violent source abilities and an enemy-target one, each on a shared
    // `status` / `modify` row tagged buff and debuff like the real data.
    let mut doomed = Vec::new();
    for (ability, word) in [
        (CURSE, "curse"),
        (POISON, "poison"),
        (BLINDNESS, "blind"),
        (SLEEP, "sleep"),
        (PARALYSIS, "paralysis"),
    ] {
        let e = spawn_effect(
            &mut world,
            p,
            FX_STATUS,
            word,
            300,
            Some(ability),
            EffectSource::Spell,
        );
        dispatch(&mut world, p, &format!("cancel {word}"));
        let out = drain(&mut rx);
        assert!(out.contains("harmful"), "{word}: {out}");
        doomed.push(e);
    }
    let curse = buff(&mut world, p, CURSE, 300, -5);
    dispatch(&mut world, p, "cancel curse");
    assert!(drain(&mut rx).contains("harmful"));
    assert!(doomed.iter().all(|e| alive(&world, *e)) && alive(&world, curse));
    assert_eq!(strength(&world, p), 45);
}

#[test]
fn a_mortal_can_cancel_waterform_out_of_combat() {
    let (mut world, _room, p, mut rx) = setup();
    let e = spawn_effect(
        &mut world,
        p,
        FX_STATUS,
        "waterform",
        300,
        Some(WATERFORM),
        EffectSource::Spell,
    );
    dispatch(&mut world, p, "cancel");
    let out = drain(&mut rx);
    assert!(out.contains("Waterform"), "listed: {out}");
    dispatch(&mut world, p, "cancel waterform");
    let out = drain(&mut rx);
    assert!(out.contains("You cancel"), "{out}");
    assert!(!alive(&world, e));
}

#[test]
fn cancel_refuses_effects_with_no_source_spell() {
    let (mut world, _room, p, mut rx) = setup();
    let bleed = spawn_effect(
        &mut world,
        p,
        FX_MODIFY,
        "bleed",
        30,
        None,
        EffectSource::Spell,
    );
    dispatch(&mut world, p, "cancel bleed");
    assert!(alive(&world, bleed));
    assert!(drain(&mut rx).contains("can't cancel"));
}

#[test]
fn cancel_with_no_arg_lists_only_cancellable_effects() {
    let (mut world, _room, p, mut rx) = setup();
    dispatch(&mut world, p, "cancel");
    assert!(drain(&mut rx).contains("no effects you can cancel"));
    buff(&mut world, p, BLESS, 300, 7);
    buff(&mut world, p, STONE_SKIN, 300, 3);
    buff(&mut world, p, CURSE, 300, -5);
    buff(&mut world, p, BLESS, -1, 1);
    dispatch(&mut world, p, "cancel");
    let out = drain(&mut rx);
    assert!(out.contains("Stone Skin"), "{out}");
    assert!(!out.contains("Curse"), "harmful hidden: {out}");
    assert!(out.contains("Bless"), "{out}");
    assert!(out.contains("2 cancellable"), "{out}");
}

#[test]
fn cancel_takes_a_prefix_and_quoted_multi_word_names() {
    let (mut world, _room, p, mut rx) = setup();
    let skin = buff(&mut world, p, STONE_SKIN, 300, 3);
    let bless = buff(&mut world, p, BLESS, 300, 7);
    dispatch(&mut world, p, "cancel bl");
    let _ = drain(&mut rx);
    assert!(!alive(&world, bless) && alive(&world, skin));
    dispatch(&mut world, p, "cancel 'stone sk'");
    assert!(!alive(&world, skin));
    assert_eq!(strength(&world, p), 50);
    dispatch(&mut world, p, "cancel stone skin");
    assert!(drain(&mut rx).contains("No cancellable effect"));
}

#[test]
fn an_exact_name_beats_an_earlier_prefix_match() {
    let (mut world, _room, p, mut rx) = setup();
    let mut cat = world.resource_mut::<AbilityCatalog>();
    let long = ability_def(4, "Bless Weapon", AbilityKind::Spell);
    cat.by_name.insert("bless_weapon".into(), long);
    let long_fx = buff(&mut world, p, 4, 300, 2);
    let short_fx = buff(&mut world, p, BLESS, 300, 7);
    dispatch(&mut world, p, "cancel bless");
    let _ = drain(&mut rx);
    assert!(alive(&world, long_fx) && !alive(&world, short_fx));
}

#[test]
fn a_spell_with_several_effects_is_cancelled_whole() {
    let (mut world, _room, p, mut rx) = setup();
    let a = buff(&mut world, p, STONE_SKIN, 300, 3);
    let b = buff(&mut world, p, STONE_SKIN, 300, 2);
    dispatch(&mut world, p, "cancel");
    assert!(drain(&mut rx).contains("1 cancellable"));
    dispatch(&mut world, p, "cancel stone");
    assert!(!alive(&world, a) && !alive(&world, b));
    assert_eq!(strength(&world, p), 50);
}

#[test]
fn a_god_may_cancel_a_harmful_permanent_effect_in_combat_but_not_an_innate() {
    let (mut world, room, p, mut rx) = setup();
    world.entity_mut(p).insert(Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Immortal,
        account_role: UserRole::Immortal,
        perms: vec![],
    });
    let curse = buff(&mut world, p, CURSE, -1, -5);
    let innate = spawn_effect(
        &mut world,
        p,
        FX_MODIFY,
        "infravision",
        -1,
        None,
        EffectSource::Other(mud_world::mob_effects::RACE_EFFECT_SOURCE.into()),
    );
    let foe = world
        .spawn((Located(room), Health { hp: 10, max: 10 }))
        .id();
    world.entity_mut(p).insert(Fighting(foe));
    dispatch(&mut world, p, "cancel curse");
    let _ = drain(&mut rx);
    assert!(!alive(&world, curse));
    assert_eq!(strength(&world, p), 50);
    dispatch(&mut world, p, "cancel infravision");
    assert!(alive(&world, innate));
    assert!(drain(&mut rx).contains("permanent"));
}

fn wall(world: &mut World, room: Entity, dir: mud_db::enums::Direction) -> Entity {
    wall_with(world, room, dir, None, 100, mud_world::WallTraversal::Block)
}

/// A wall raised by `caster` (recorded the way the cast site does).
fn wall_by(
    world: &mut World,
    room: Entity,
    dir: mud_db::enums::Direction,
    caster: Entity,
) -> Entity {
    wall_with(
        world,
        room,
        dir,
        Some(caster),
        100,
        mud_world::WallTraversal::Block,
    )
}

fn wall_with(
    world: &mut World,
    room: Entity,
    dir: mud_db::enums::Direction,
    caster: Option<Entity>,
    hp: i32,
    traversal: mud_world::WallTraversal,
) -> Entity {
    use mud_world::{RoomBlockedExit, RoomBlockedExits};
    let e = spawn_effect(
        world,
        room,
        FX_MODIFY,
        "wall-stone",
        300,
        Some(WALL_OF_STONE),
        EffectSource::Spell,
    );
    let entry = RoomBlockedExit {
        kind_label: "wall of stone".into(),
        backed_by: e,
        hp,
        traversal,
    };
    if let Some(c) = caster {
        let character_id = world.get::<Account>(c).map(|a| a.character_id.clone());
        world.entity_mut(e).insert(mud_world::WallCaster {
            entity: c,
            character_id,
        });
    }
    if let Some(mut b) = world.get_mut::<RoomBlockedExits>(room) {
        b.by_direction.insert(dir, entry);
    } else {
        let mut b = RoomBlockedExits::default();
        b.by_direction.insert(dir, entry);
        world.entity_mut(room).insert(b);
    }
    e
}

fn blocked(world: &World, room: Entity, dir: mud_db::enums::Direction) -> bool {
    world
        .get::<mud_world::RoomBlockedExits>(room)
        .is_some_and(|b| b.by_direction.contains_key(&dir))
}

#[test]
fn cancelling_one_of_two_walls_frees_only_that_exit() {
    use mud_db::enums::Direction::{East, North};
    let (mut world, room, p, mut rx) = setup();
    let n = wall_by(&mut world, room, North, p);
    let e = wall_by(&mut world, room, East, p);
    dispatch(&mut world, p, "cancel wall");
    let _ = drain(&mut rx);
    let gone = [n, e].into_iter().filter(|w| !alive(&world, *w)).count();
    assert_eq!(gone, 1, "exactly one wall dropped");
    // The surviving wall still blocks its own exit and no entry dangles.
    let by = &world
        .get::<mud_world::RoomBlockedExits>(room)
        .expect("one wall left")
        .by_direction;
    assert_eq!(by.len(), 1);
    assert!(by.values().all(|b| alive(&world, b.backed_by)));
    // Cancelling the other frees the room entirely.
    dispatch(&mut world, p, "cancel wall");
    let _ = drain(&mut rx);
    assert!(!alive(&world, n) && !alive(&world, e));
    assert!(!blocked(&world, room, North) && !blocked(&world, room, East));
    assert!(world.get::<mud_world::RoomBlockedExits>(room).is_none());
}

#[test]
fn any_removal_path_frees_a_walls_exit() {
    use mud_db::enums::Direction::North;
    let (mut world, room, _p, _rx) = setup();
    let n = wall(&mut world, room, North);
    crate::effects::remove_effect_instance(&mut world, room, n);
    assert!(!alive(&world, n));
    assert!(!blocked(&world, room, North));
}

/// A second mortal in the same room as `p`, with their own character id.
fn bystander(world: &mut World, room: Entity) -> (Entity, Rx) {
    let (other, rx) = player_in(world, room);
    world.entity_mut(other).insert((
        Account {
            user_id: "u2".into(),
            character_id: "c2".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        },
        Health { hp: 100, max: 100 },
    ));
    (other, rx)
}

#[test]
fn the_caster_can_cancel_their_own_wall() {
    use mud_db::enums::Direction::North;
    let (mut world, room, p, mut rx) = setup();
    let n = wall_by(&mut world, room, North, p);
    dispatch(&mut world, p, "cancel wall");
    assert!(drain(&mut rx).contains("You cancel"));
    assert!(!alive(&world, n));
    assert!(!blocked(&world, room, North));
}

#[test]
fn the_caster_is_matched_by_character_id_across_a_relog() {
    use mud_db::enums::Direction::North;
    let (mut world, room, p, _rx) = setup();
    let n = wall_by(&mut world, room, North, p);
    // Same character, fresh entity (relog); the old entity is gone.
    world.despawn(p);
    let (again, mut rx2) = bystander(&mut world, room);
    world.get_mut::<Account>(again).unwrap().character_id = "c".into();
    dispatch(&mut world, again, "cancel wall");
    assert!(drain(&mut rx2).contains("You cancel"));
    assert!(!alive(&world, n));
}

#[test]
fn another_mortal_cannot_cancel_the_wall() {
    use mud_db::enums::Direction::North;
    let (mut world, room, p, _rx) = setup();
    let n = wall_by(&mut world, room, North, p);
    let (other, mut orx) = bystander(&mut world, room);
    dispatch(&mut world, other, "cancel wall");
    assert!(drain(&mut orx).contains("That wall isn't yours to dispel."));
    assert!(alive(&world, n));
    assert!(blocked(&world, room, North), "exit stays blocked");
    // It does not even list for them.
    dispatch(&mut world, other, "cancel");
    assert!(drain(&mut orx).contains("no effects you can cancel"));
}

#[test]
fn a_wall_with_no_recorded_caster_is_not_a_mortals_to_cancel() {
    use mud_db::enums::Direction::North;
    let (mut world, room, p, mut rx) = setup();
    let n = wall(&mut world, room, North);
    dispatch(&mut world, p, "cancel wall");
    assert!(drain(&mut rx).contains("isn't yours"));
    assert!(alive(&world, n));
}

#[test]
fn a_god_can_cancel_any_wall() {
    use mud_db::enums::Direction::North;
    let (mut world, room, p, _rx) = setup();
    let n = wall_by(&mut world, room, North, p);
    let (god, mut grx) = bystander(&mut world, room);
    world.get_mut::<Account>(god).unwrap().role = UserRole::Immortal;
    world.get_mut::<Account>(god).unwrap().account_role = UserRole::Immortal;
    dispatch(&mut world, god, "cancel wall");
    let out = drain(&mut grx);
    assert!(out.contains("You cancel"), "{out}");
    assert!(!alive(&world, n));
    assert!(world.get::<mud_world::RoomBlockedExits>(room).is_none());
}

/// Two rooms joined by a north exit, `p` in the first with a stamina pool.
fn walled_pair() -> (World, Entity, Entity, Entity, Rx) {
    use mud_db::enums::ExitState;
    use mud_world::{ExitData, Exits, Stamina};
    let (mut world, a, p, rx) = setup();
    let b = world.spawn((Room, Exits::default())).id();
    let open = |to| ExitData {
        to: Some(to),
        state: ExitState::Open,
        key: None,
        description: None,
        keywords: Vec::new(),
        is_hidden: false,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    };
    world
        .entity_mut(a)
        .insert(Exits([(mud_db::enums::Direction::North, open(b))].into()));
    world.entity_mut(p).insert(Stamina {
        current: 500,
        max: 500,
    });
    (world, a, b, p, rx)
}

#[test]
fn bashing_a_wall_down_frees_its_exit_and_leaves_no_dangling_entity() {
    use mud_db::enums::Direction::North;
    let (mut world, a, _b, p, mut rx) = walled_pair();
    let w = wall_with(
        &mut world,
        a,
        North,
        Some(p),
        1,
        mud_world::WallTraversal::Block,
    );
    dispatch(&mut world, p, "doorbash north");
    let out = drain(&mut rx);
    assert_eq!(out.matches("final shattering blow").count(), 1, "{out}");
    assert!(!alive(&world, w));
    assert!(!blocked(&world, a, North));
    assert!(world.get::<mud_world::RoomBlockedExits>(a).is_none());
}

#[test]
fn walking_through_an_illusory_wall_frees_its_exit_and_leaves_no_dangling_entity() {
    use mud_db::enums::Direction::North;
    let (mut world, a, b, p, mut rx) = walled_pair();
    world.init_resource::<mud_world::RaceCatalog>();
    let w = wall_with(
        &mut world,
        a,
        North,
        Some(p),
        0,
        mud_world::WallTraversal::Passable,
    );
    dispatch(&mut world, p, "north");
    let out = drain(&mut rx);
    assert_eq!(out.matches("ripples and dissolves").count(), 1, "{out}");
    assert!(!alive(&world, w));
    assert!(world.get::<mud_world::RoomBlockedExits>(a).is_none());
    assert_eq!(world.get::<Located>(p).unwrap().0, b, "{out}");
}
