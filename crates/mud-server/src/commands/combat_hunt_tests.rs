//! Fixes from the combat / death / progression bug hunt: every offensive
//! command honours the shared `offensive_target_allowed` gate, `retreat`
//! obeys the flee rules, charm ends the fight, group `AoE` spares group pets,
//! camp kits, clamped modify deltas and `release` ordering. Test-only.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Direction, ExitState, MobBehavior};
use mud_world::{
    AbilityCatalog, AppliedTo, CombatStats, EffectCatalog, EffectDef, ExitData, Exits, Fighting,
    Follower, Ghost, Health, Keywords, KnownAbilities, Located, Mob, MobBehaviors, Named,
    PeacefulRoom, Player, Posture, PostureKind, Room, SpellSlotData, Stamina,
};

use super::combat_commands::*;
use crate::commands::test_support::{Rx, ability_def, drain, player_in};
use crate::commands::{apply_modify_delta_actual, invoke_ability_with, reverse_modify_delta};

/// A room, a standing player with stamina and `CombatStats`, and its receiver.
fn arena() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    let room = world.spawn((Room, Exits::default())).id();
    let (player, rx) = player_in(&mut world, room);
    world.entity_mut(player).insert((
        Health { hp: 100, max: 100 },
        Stamina {
            current: 100,
            max: 100,
        },
        CombatStats::default(),
        Posture(PostureKind::Standing),
    ));
    (world, room, player, rx)
}

fn mob_in(world: &mut World, room: Entity, name: &str, keyword: &str) -> Entity {
    world
        .spawn((
            Mob,
            Named { name: name.into() },
            Keywords(vec![keyword.into()]),
            Located(room),
            Health { hp: 50, max: 50 },
            Posture(PostureKind::Standing),
        ))
        .id()
}

fn peaceful(world: &mut World, mob: Entity) {
    world
        .entity_mut(mob)
        .insert(MobBehaviors(vec![MobBehavior::Peaceful]));
}

fn hp(world: &World, e: Entity) -> i32 {
    world.get::<Health>(e).unwrap().hp
}

fn stamina(world: &World, e: Entity) -> i32 {
    world.get::<Stamina>(e).unwrap().current
}

/// A second, non-PK player standing in `room`.
fn other_player(world: &mut World, room: Entity) -> Entity {
    let (v, _rx) = player_in(world, room);
    world.entity_mut(v).insert((
        Named {
            name: "Victim".into(),
        },
        Keywords(vec!["victim".into()]),
        Health { hp: 100, max: 100 },
        Posture(PostureKind::Standing),
    ));
    v
}

// -- stomp -----------------------------------------------------------------

#[test]
fn stomp_is_refused_in_a_peaceful_room() {
    let (mut world, room, p, mut rx) = arena();
    world.entity_mut(room).insert(PeacefulRoom);
    let v = other_player(&mut world, room);
    cmd_stomp(&mut world, p, "victim");
    assert_eq!(hp(&world, v), 100, "no damage");
    assert_eq!(world.get::<Posture>(v).unwrap().0, PostureKind::Standing);
    assert_eq!(stamina(&world, p), 100, "no stamina spent");
    assert!(drain(&mut rx).contains("disturb the peace"));
}

#[test]
fn stomp_is_refused_against_a_peaceful_mob() {
    let (mut world, room, p, mut rx) = arena();
    let keeper = mob_in(&mut world, room, "the keeper", "keeper");
    peaceful(&mut world, keeper);
    cmd_stomp(&mut world, p, "keeper");
    assert_eq!(hp(&world, keeper), 50);
    assert_eq!(stamina(&world, p), 100);
    assert!(drain(&mut rx).contains("peaceful feeling"));
}

#[test]
fn stomp_cannot_hurt_a_player_who_has_pk_off() {
    let (mut world, room, p, mut rx) = arena();
    let v = other_player(&mut world, room);
    cmd_stomp(&mut world, p, "victim");
    assert_eq!(hp(&world, v), 100);
    assert!(drain(&mut rx).contains("PK"));
}

#[test]
fn stomp_still_works_on_an_ordinary_mob() {
    let (mut world, room, p, _rx) = arena();
    let goblin = mob_in(&mut world, room, "a goblin", "goblin");
    cmd_stomp(&mut world, p, "goblin");
    assert!(hp(&world, goblin) < 50, "control: the stomp lands");
    assert_eq!(
        world.get::<Posture>(goblin).unwrap().0,
        PostureKind::Sitting
    );
}

// -- the rest of the offensive commands ---------------------------------------

#[test]
fn single_target_skills_refuse_peaceful_targets_before_spending_stamina() {
    type Cmd = fn(&mut World, Entity, &str);
    let cmds: [(&str, Cmd); 5] = [
        ("kick", |w, p, a| cmd_kick(w, p, a)),
        ("tripup", cmd_tripup),
        ("roundhouse", |w, p, a| cmd_roundhouse(w, p, a)),
        ("rend", cmd_rend),
        ("gouge", cmd_gouge),
    ];
    for (name, cmd) in cmds {
        let (mut world, room, p, mut rx) = arena();
        let keeper = mob_in(&mut world, room, "the keeper", "keeper");
        peaceful(&mut world, keeper);
        // kick / roundhouse act on the current opponent.
        world.entity_mut(p).insert(Fighting(keeper));
        cmd(&mut world, p, "keeper");
        assert_eq!(stamina(&world, p), 100, "{name}: stamina untouched");
        assert_eq!(hp(&world, keeper), 50, "{name}: keeper untouched");
        let out = drain(&mut rx);
        assert!(out.contains("peaceful feeling"), "{name}: {out:?}");
    }
}

#[test]
fn single_target_skills_refuse_a_peaceful_room() {
    type Cmd = fn(&mut World, Entity, &str);
    let cmds: [(&str, Cmd); 3] = [
        ("kick", |w, p, a| cmd_kick(w, p, a)),
        ("rend", cmd_rend),
        ("gouge", cmd_gouge),
    ];
    for (name, cmd) in cmds {
        let (mut world, room, p, mut rx) = arena();
        world.entity_mut(room).insert(PeacefulRoom);
        let goblin = mob_in(&mut world, room, "a goblin", "goblin");
        world.entity_mut(p).insert(Fighting(goblin));
        cmd(&mut world, p, "goblin");
        assert_eq!(stamina(&world, p), 100, "{name}");
        assert!(drain(&mut rx).contains("disturb the peace"), "{name}");
    }
}

#[test]
fn steal_obeys_attack_ok_for_mobs_too() {
    let (mut world, room, p, mut rx) = arena();
    world.insert_resource(mud_world::CoreAbilities {
        steal: Some(77),
        ..Default::default()
    });
    world.entity_mut(p).insert(KnownAbilities {
        entries: vec![(77, 500, true)],
    });
    let keeper = mob_in(&mut world, room, "the quest giver", "giver");
    peaceful(&mut world, keeper);
    cmd_steal(&mut world, p, "coins giver");
    let out = drain(&mut rx);
    assert!(out.contains("peaceful feeling"), "{out:?}");
}

#[test]
fn sweep_and_hitall_refuse_peaceful_rooms() {
    for hitall in [false, true] {
        let (mut world, room, p, mut rx) = arena();
        world.entity_mut(room).insert(PeacefulRoom);
        let goblin = mob_in(&mut world, room, "a goblin", "goblin");
        if hitall {
            cmd_hitall(&mut world, p, "");
        } else {
            cmd_sweep(&mut world, p, "");
        }
        assert_eq!(hp(&world, goblin), 50, "hitall={hitall}");
        assert_eq!(stamina(&world, p), 100, "hitall={hitall}");
        assert!(drain(&mut rx).contains("peaceful aura"), "hitall={hitall}");
    }
}

#[test]
fn sweep_and_hitall_spare_peaceful_mobs_but_hit_the_rest() {
    for hitall in [false, true] {
        let (mut world, room, p, _rx) = arena();
        let keeper = mob_in(&mut world, room, "the shopkeeper", "shopkeeper");
        peaceful(&mut world, keeper);
        let goblin = mob_in(&mut world, room, "a goblin", "goblin");
        if hitall {
            cmd_hitall(&mut world, p, "");
        } else {
            cmd_sweep(&mut world, p, "");
        }
        assert_eq!(hp(&world, keeper), 50, "hitall={hitall}: keeper spared");
        assert!(hp(&world, goblin) < 50, "hitall={hitall}: goblin hit");
    }
}

#[test]
fn sweep_with_only_peaceful_mobs_finds_nothing_to_hit() {
    let (mut world, room, p, mut rx) = arena();
    let keeper = mob_in(&mut world, room, "the shopkeeper", "shopkeeper");
    peaceful(&mut world, keeper);
    cmd_sweep(&mut world, p, "");
    assert_eq!(stamina(&world, p), 100);
    assert!(drain(&mut rx).contains("Nothing here to sweep"));
}

// -- retreat ---------------------------------------------------------------

fn exit(to: Entity) -> ExitData {
    ExitData {
        to: Some(to),
        state: ExitState::Open,
        key: None,
        description: None,
        keywords: Vec::new(),
        is_hidden: false,
        is_pickproof: false,
        is_bashable: false,
        hit_points: None,
    }
}

/// Fighting player in room A with an open north exit to B.
fn retreat_world() -> (World, Entity, Entity, Entity, Rx) {
    let (mut world, a, p, rx) = arena();
    let b = world.spawn((Room, Exits::default())).id();
    world
        .entity_mut(a)
        .insert(Exits(HashMap::from([(Direction::North, exit(b))])));
    let foe = mob_in(&mut world, a, "a goblin", "goblin");
    world.entity_mut(p).insert(Fighting(foe));
    world.entity_mut(foe).insert(Fighting(p));
    (world, a, b, p, rx)
}

fn room_of(world: &World, e: Entity) -> Entity {
    world.get::<Located>(e).unwrap().0
}

#[test]
fn retreat_works_for_a_healthy_fighter() {
    let (mut world, _a, b, p, _rx) = retreat_world();
    cmd_retreat(&mut world, p, "north");
    assert_eq!(room_of(&world, p), b, "control: the retreat goes through");
}

#[test]
fn retreat_refuses_a_sleeper_a_stunned_and_a_held_player() {
    // Sleeping.
    let (mut world, a, _b, p, mut rx) = retreat_world();
    world.entity_mut(p).insert(Posture(PostureKind::Sleeping));
    cmd_retreat(&mut world, p, "north");
    assert_eq!(room_of(&world, p), a);
    assert!(drain(&mut rx).contains("dream of fleeing"));
    // Stunned.
    let (mut world, a, _b, p, _rx) = retreat_world();
    world.entity_mut(p).insert(mud_world::Stunned);
    cmd_retreat(&mut world, p, "north");
    assert_eq!(room_of(&world, p), a);
    // Sitting: the scramble to its feet is the whole turn.
    let (mut world, a, _b, p, mut rx) = retreat_world();
    world.entity_mut(p).insert(Posture(PostureKind::Sitting));
    cmd_retreat(&mut world, p, "north");
    assert_eq!(room_of(&world, p), a);
    assert!(drain(&mut rx).contains("scramble madly"));
}

#[test]
fn retreat_refuses_a_player_held_by_a_movement_preventing_effect() {
    const ROOT: i32 = 41;
    let (mut world, a, _b, p, mut rx) = retreat_world();
    let mut catalog = EffectCatalog::default();
    catalog.by_id.insert(
        ROOT,
        EffectDef {
            id: ROOT,
            name: "rooted".into(),
            description: None,
            effect_type: "status".into(),
            tags: Vec::new(),
            presence_override: None,
            default_params: serde_json::json!({}),
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: true,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        },
    );
    world.insert_resource(catalog);
    world.spawn((
        mud_world::EffectInstance {
            kind: ROOT,
            name: "rooted".into(),
            strength: 1,
            remaining_secs: 30,
            source: mud_world::EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(p),
    ));
    cmd_retreat(&mut world, p, "north");
    assert_eq!(room_of(&world, p), a);
    assert!(drain(&mut rx).contains("can't move"));
}

#[test]
fn retreat_cannot_walk_through_a_stone_wall() {
    let (mut world, a, _b, p, mut rx) = retreat_world();
    let backing = world.spawn_empty().id();
    world.entity_mut(a).insert(mud_world::RoomBlockedExits {
        by_direction: HashMap::from([(
            Direction::North,
            mud_world::RoomBlockedExit {
                kind_label: "wall of stone".into(),
                backed_by: backing,
                hp: 100,
                traversal: mud_world::WallTraversal::Block,
            },
        )]),
    });
    cmd_retreat(&mut world, p, "north");
    assert_eq!(room_of(&world, p), a);
    assert!(drain(&mut rx).contains("wall of stone blocks your path"));
    assert!(world.get::<Fighting>(p).is_some(), "still fighting");
}

#[test]
fn flee_does_not_pick_a_walled_exit() {
    let (mut world, a, _b, p, mut rx) = retreat_world();
    let backing = world.spawn_empty().id();
    world.entity_mut(a).insert(mud_world::RoomBlockedExits {
        by_direction: HashMap::from([(
            Direction::North,
            mud_world::RoomBlockedExit {
                kind_label: "wall of stone".into(),
                backed_by: backing,
                hp: 100,
                traversal: mud_world::WallTraversal::Block,
            },
        )]),
    });
    cmd_flee(&mut world, p, "");
    assert_eq!(room_of(&world, p), a);
    assert!(drain(&mut rx).contains("nowhere to run"));
}

#[test]
fn retreat_breaks_a_cast_in_progress() {
    let (mut world, _a, b, p, _rx) = retreat_world();
    world.entity_mut(p).insert(mud_world::Casting {
        ability_id: 1,
        ability_name: "Mend".into(),
        args: String::new(),
        kind_label: "spell".into(),
        verb: "cast".into(),
        ticks_remaining: 8,
        ticks_total: 8,
        target: mud_world::CastTarget::Caster,
        recognized_by: Vec::new(),
        slot_reservation: None,
        via_innate: false,
    });
    cmd_retreat(&mut world, p, "north");
    assert_eq!(room_of(&world, p), b);
    assert!(world.get::<mud_world::Casting>(p).is_none());
}

// -- charm -------------------------------------------------------------------

/// An arena whose caster knows a working `charm` spell, plus a goblin
/// fighting the caster (it carries the hate and memory a melee leaves).
fn charm_arena() -> (World, Entity, Entity, Entity, Rx) {
    const CHARM: i32 = 1;
    const FX: i32 = 2;
    let (mut world, room, caster, rx) = arena();
    let mut catalog = AbilityCatalog::default();
    let mut def = ability_def(CHARM, "charm", AbilityKind::Spell);
    def.violent = true;
    def.cast_time_rounds = 0;
    catalog.by_name.insert("charm".into(), def);
    catalog
        .effects_for
        .insert(CHARM, vec![(FX, Some(serde_json::json!({"duration": 30})))]);
    world.insert_resource(catalog);
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(
        FX,
        EffectDef {
            id: FX,
            name: "charmed".into(),
            description: None,
            effect_type: "charmed".into(),
            tags: Vec::new(),
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
    world.insert_resource(effects);
    world.insert_resource(SpellSlotData::default());
    world.insert_resource(mud_world::ClassSkillsData::default());
    world.entity_mut(caster).insert(KnownAbilities {
        entries: vec![(CHARM, 500, true)],
    });
    let goblin = mob_in(&mut world, room, "a goblin", "goblin");
    world.entity_mut(goblin).insert((
        Fighting(caster),
        crate::combat::HateList(vec![caster]),
        crate::combat::MobMemory([caster].into_iter().collect()),
    ));
    world.entity_mut(caster).insert(Fighting(goblin));
    (world, room, caster, goblin, rx)
}

fn cast_charm(world: &mut World, caster: Entity) {
    invoke_ability_with(
        world,
        caster,
        "'charm' goblin",
        AbilityKind::Spell,
        "cast",
        false,
        false,
        false,
        None,
    );
}

fn combat_round(world: &mut World) {
    world.insert_resource(crate::TickCount(crate::combat::COMBAT_PERIOD_TICKS));
    crate::combat::combat_tick(world);
}

#[test]
fn charming_a_mob_that_is_fighting_you_ends_the_fight() {
    let (mut world, _room, caster, goblin, _rx) = charm_arena();
    cast_charm(&mut world, caster);

    assert_eq!(world.get::<Follower>(goblin).map(|f| f.0), Some(caster));
    assert!(
        world.get::<Fighting>(goblin).is_none(),
        "the pet stops swinging at its new master"
    );
    assert!(world.get::<Fighting>(caster).is_none());
}

#[test]
fn a_charmed_pet_does_not_turn_on_its_new_master_next_round() {
    // Control: the same goblin, not charmed, goes back to the fight.
    let (mut world, _room, caster, goblin, _rx) = charm_arena();
    stop_fight(&mut world, caster, goblin);
    combat_round(&mut world);
    assert!(
        world.get::<Fighting>(goblin).is_some(),
        "control: an uncharmed goblin re-engages from its hate list"
    );

    let (mut world, _room, caster, goblin, _rx) = charm_arena();
    cast_charm(&mut world, caster);
    assert!(world.get::<crate::combat::HateList>(goblin).is_none());
    assert!(world.get::<crate::combat::MobMemory>(goblin).is_none());
    combat_round(&mut world);
    assert!(
        world.get::<Fighting>(goblin).is_none(),
        "the pet does not fight its master"
    );
    assert!(world.get::<Fighting>(caster).is_none());
}

#[test]
fn a_servant_or_a_mob_in_a_peaceful_room_ignores_its_hate_list() {
    for case in ["servant", "peaceful room"] {
        let (mut world, room, caster, goblin, _rx) = charm_arena();
        stop_fight(&mut world, caster, goblin);
        if case == "servant" {
            world.entity_mut(goblin).insert(Follower(caster));
        } else {
            world.entity_mut(room).insert(PeacefulRoom);
        }
        combat_round(&mut world);
        assert!(world.get::<Fighting>(goblin).is_none(), "{case}");
    }
}

fn stop_fight(world: &mut World, a: Entity, b: Entity) {
    world.entity_mut(a).remove::<Fighting>();
    world.entity_mut(b).remove::<Fighting>();
}

// -- AoE target selection ------------------------------------------------------

#[test]
fn room_enemy_aoe_spares_group_pets_and_mounts() {
    let (mut world, room, leader, _rx) = arena();
    let (member, _mrx) = player_in(&mut world, room);
    world.entity_mut(member).insert((
        Named {
            name: "Mate".into(),
        },
        mud_world::GroupMember(leader),
    ));
    let pet = mob_in(&mut world, room, "a mate's wolf", "wolf");
    world.entity_mut(pet).insert(Follower(member));
    let steed = mob_in(&mut world, room, "a mate's horse", "horse");
    world.entity_mut(steed).insert(mud_world::RiddenBy(member));
    let stranger_pet = mob_in(&mut world, room, "a stranger's cat", "cat");
    let (stranger, _srx) = player_in(&mut world, room);
    world.entity_mut(stranger_pet).insert(Follower(stranger));
    let goblin = mob_in(&mut world, room, "a goblin", "goblin");

    let hit: Vec<Entity> = crate::commands::aoe_targets_in_room(
        &mut world,
        leader,
        room,
        crate::commands::AoeScope::RoomEnemies,
    )
    .into_iter()
    .map(|(e, _)| e)
    .collect();
    assert!(hit.contains(&goblin), "plain mob is a target");
    assert!(hit.contains(&stranger_pet), "an outsider's pet still is");
    assert!(!hit.contains(&pet), "a groupmate's pet is spared");
    assert!(!hit.contains(&steed), "a groupmate's mount is spared");
}

// -- camp ------------------------------------------------------------------------

#[test]
fn camp_kit_given_away_gives_no_bonus_and_is_not_despawned() {
    use mud_world::{Camping, Item, RestState};
    for kept in [true, false] {
        let mut world = World::new();
        world.insert_resource(crate::TickCount(0));
        let room = world.spawn_empty().id();
        let camper = world
            .spawn((
                Player,
                Named {
                    name: "Camper".into(),
                },
                Located(room),
            ))
            .id();
        let friend = world.spawn((Player, Located(room))).id();
        let kit = world
            .spawn((
                Item,
                Named {
                    name: "a camp kit".into(),
                },
                Located(if kept { camper } else { friend }),
            ))
            .id();
        world.entity_mut(camper).insert(Camping {
            since_tick: 0,
            started_in: room,
            kit_entity: Some(kit),
            kit_world_key: Some((1, 9)),
            kit_tier_bonus: 2,
        });
        world.insert_resource(crate::TickCount(crate::camp::CAMP_DURATION_TICKS));
        crate::camp::camp_tick(&mut world);
        assert!(world.get::<RestState>(camper).is_some(), "camp completed");
        let tier = world.get::<RestState>(camper).unwrap().tier;
        let wake = world.get::<mud_world::PendingWakeAttachments>(camper);
        if kept {
            assert_eq!(tier, 3, "kit bonus applies");
            assert!(wake.is_some());
            assert!(world.get_entity(kit).is_err(), "kit consumed");
        } else {
            assert_eq!(tier, 1, "no kit bonus for a kit that is gone");
            assert!(wake.is_none(), "no kit wake effects either");
            assert!(world.get_entity(kit).is_ok(), "friend keeps their item");
            assert_eq!(world.get::<Located>(kit).unwrap().0, friend);
        }
    }
}

// -- clamped modify deltas -----------------------------------------------------------

#[test]
fn a_clamped_max_hp_debuff_reverses_exactly() {
    let (mut world, _room, p, _rx) = arena();
    world.entity_mut(p).insert(Health { hp: 5, max: 5 });
    let landed = apply_modify_delta_actual(&mut world, p, "max_hp", -20);
    assert_eq!(landed, Some(-4), "max HP bottoms out at 1");
    assert_eq!(world.get::<Health>(p).unwrap().max, 1);
    reverse_modify_delta(&mut world, p, "max_hp", landed.unwrap());
    assert_eq!(
        world.get::<Health>(p).unwrap().max,
        5,
        "removing the debuff restores the original max, no free gain"
    );
}

#[test]
fn other_clamped_stats_report_what_landed() {
    let (mut world, _room, p, _rx) = arena();
    world.entity_mut(p).insert(Stamina { current: 2, max: 3 });
    assert_eq!(
        apply_modify_delta_actual(&mut world, p, "max_stamina", -10),
        Some(-3)
    );
    world.get_mut::<CombatStats>(p).unwrap().armor_pct = 95;
    assert_eq!(
        apply_modify_delta_actual(&mut world, p, "armor_pct", 20),
        Some(5),
        "capped at 100"
    );
    assert_eq!(
        apply_modify_delta_actual(&mut world, p, "strength", 3),
        Some(3),
        "unclamped stats land in full"
    );
    assert_eq!(apply_modify_delta_actual(&mut world, p, "bogus", 3), None);
}

// -- release -------------------------------------------------------------------------

#[test]
fn release_with_no_valid_recall_target_stays_a_ghost() {
    let (mut world, room, p, mut rx) = arena();
    world.insert_resource(mud_world::RaceDefaults::default());
    world.insert_resource(mud_world::WorldKeyIndex::default());
    world
        .entity_mut(p)
        .insert((Ghost, Health { hp: 0, max: 100 }));
    // No RecallPoint, no race home, no Void room: nowhere to go.
    super::release::cmd_release(&mut world, p, "");
    assert!(world.get::<Ghost>(p).is_some(), "still a ghost");
    assert_eq!(hp(&world, p), 0, "no free heal from a refused release");
    assert_eq!(room_of(&world, p), room);
    assert!(drain(&mut rx).contains("nowhere to return to"));
}

#[test]
fn release_with_a_vanished_recall_point_stays_a_ghost() {
    let (mut world, _room, p, mut rx) = arena();
    world.insert_resource(mud_world::RaceDefaults::default());
    world.insert_resource(mud_world::WorldKeyIndex::default());
    let gone = world.spawn_empty().id();
    world.despawn(gone);
    world.entity_mut(p).insert((
        Ghost,
        Health { hp: 0, max: 100 },
        mud_world::RecallPoint(gone),
    ));
    super::release::cmd_release(&mut world, p, "");
    assert!(world.get::<Ghost>(p).is_some());
    assert_eq!(hp(&world, p), 0);
    assert!(drain(&mut rx).contains("recall point has vanished"));
}

#[test]
fn release_leaves_the_mount_behind() {
    let (mut world, a, p, _rx) = arena();
    let b = world.spawn((Room, Exits::default())).id();
    let steed = mob_in(&mut world, a, "a horse", "horse");
    world
        .entity_mut(p)
        .insert((Ghost, mud_world::RecallPoint(b), mud_world::Mounted(steed)));
    world.entity_mut(steed).insert(mud_world::RiddenBy(p));
    super::release::cmd_release(&mut world, p, "");
    assert_eq!(room_of(&world, p), b);
    assert_eq!(room_of(&world, steed), a, "the mount stays");
    assert!(world.get::<mud_world::Mounted>(p).is_none());
    assert!(world.get::<mud_world::RiddenBy>(steed).is_none());
}

#[test]
fn sweep_and_hitall_spare_mounts_ridden_by_the_caster_or_a_groupmate() {
    for hitall in [false, true] {
        let (mut world, room, p, _rx) = arena();
        let (mate, _mrx) = player_in(&mut world, room);
        world.entity_mut(mate).insert(mud_world::GroupMember(p));
        let mine = mob_in(&mut world, room, "my horse", "horse");
        world.entity_mut(mine).insert(mud_world::RiddenBy(p));
        let theirs = mob_in(&mut world, room, "a mate's mare", "mare");
        world.entity_mut(theirs).insert(mud_world::RiddenBy(mate));
        let goblin = mob_in(&mut world, room, "a goblin", "goblin");
        if hitall {
            cmd_hitall(&mut world, p, "");
        } else {
            cmd_sweep(&mut world, p, "");
        }
        assert_eq!(hp(&world, mine), 50, "hitall={hitall}: own mount spared");
        assert_eq!(
            hp(&world, theirs),
            50,
            "hitall={hitall}: mate's mount spared"
        );
        assert!(
            hp(&world, goblin) < 50,
            "hitall={hitall}: control, goblin hit"
        );
    }
}

// -- restored buffs ------------------------------------------------------------------

#[test]
fn a_clamped_buff_restored_at_login_reverses_exactly_on_expiry() {
    let (mut world, _room, p, _rx) = arena();
    world.entity_mut(p).insert(Health { hp: 5, max: 5 });
    world.insert_resource(crate::TickCount(0));
    let persisted: crate::login::PersistedEffects = serde_json::from_value(serde_json::json!({
        "saved_at_unix": i64::MAX / 2,
        "effects": [{
            "kind": 1,
            "name": "wither",
            "strength": 1,
            "remaining_secs": 30,
            "source": "Spell",
            "ability_id": null,
            "modify_delta": ["max_hp", -20],
        }],
    }))
    .expect("persisted effects shape");
    crate::login::restore_persisted_effects(&mut world, p, persisted);
    assert_eq!(world.get::<Health>(p).unwrap().max, 1, "debuff clamps at 1");
    let recorded: Vec<i32> = world
        .query::<&mud_world::ModifyDelta>()
        .iter(&world)
        .map(|d| d.amount)
        .collect();
    assert_eq!(
        recorded,
        vec![-4],
        "the delta that landed, not the saved one"
    );
    {
        let mut q = world.query::<&mut mud_world::EffectInstance>();
        for mut i in q.iter_mut(&mut world) {
            i.remaining_secs = 1;
        }
    }
    world.insert_resource(crate::TickCount(10));
    crate::effects::effects_tick(&mut world);
    assert_eq!(
        world.get::<Health>(p).unwrap().max,
        5,
        "expiry gives back exactly what was taken"
    );
}

// -- stale pet markers ---------------------------------------------------------------

#[test]
fn persistent_pet_marker_is_dropped_when_the_follow_link_breaks() {
    let (mut world, room, owner, _rx) = arena();
    let pet = mob_in(&mut world, room, "a wolf", "wolf");
    world
        .entity_mut(pet)
        .insert((Follower(owner), mud_world::PersistentPet));
    crate::combat::prune_stale_pet_markers(&mut world);
    assert!(
        world.get::<mud_world::PersistentPet>(pet).is_some(),
        "control: a followed pet keeps the marker"
    );
    // Unfollowed / ordered away.
    world.entity_mut(pet).remove::<Follower>();
    crate::combat::prune_stale_pet_markers(&mut world);
    assert!(world.get::<mud_world::PersistentPet>(pet).is_none());

    // The master is gone (quit / removed).
    let other = world.spawn_empty().id();
    world
        .entity_mut(pet)
        .insert((Follower(other), mud_world::PersistentPet));
    world.despawn(other);
    crate::combat::prune_stale_pet_markers(&mut world);
    assert!(world.get::<mud_world::PersistentPet>(pet).is_none());

    // Following a mob (a scripted escort) is not a player's pet.
    let boss = mob_in(&mut world, room, "a boss", "boss");
    world
        .entity_mut(pet)
        .insert((Follower(boss), mud_world::PersistentPet));
    crate::combat::prune_stale_pet_markers(&mut world);
    assert!(world.get::<mud_world::PersistentPet>(pet).is_none());
}

#[test]
fn dismissing_a_pet_drops_the_persistent_marker_at_once() {
    let (mut world, room, owner, _rx) = arena();
    let pet = mob_in(&mut world, room, "a wolf", "wolf");
    world
        .entity_mut(pet)
        .insert((Follower(owner), mud_world::PersistentPet));
    crate::commands::release_from(&mut world, pet, owner);
    assert!(world.get::<Follower>(pet).is_none());
    assert!(world.get::<mud_world::PersistentPet>(pet).is_none());
}

#[test]
fn a_dismissed_pet_pays_kill_credit_again_only_as_an_ordinary_mob() {
    // The marker is what made its death pay nothing; with the link broken
    // it is just a mob again.
    let (mut world, room, owner, _rx) = arena();
    let pet = mob_in(&mut world, room, "a wolf", "wolf");
    world
        .entity_mut(pet)
        .insert((Follower(owner), mud_world::PersistentPet));
    assert!(crate::combat::is_player_pet(&mut world, pet));
    world.entity_mut(pet).remove::<Follower>();
    crate::combat::prune_stale_pet_markers(&mut world);
    assert!(!crate::combat::is_player_pet(&mut world, pet));
}

// -- retreat refusals ----------------------------------------------------------------

#[test]
fn retreat_tells_a_ghost_a_frozen_or_a_stunned_player_why_not() {
    for (what, expect) in [
        ("ghost", "disembodied"),
        ("frozen", "frozen"),
        ("stunned", "stunned"),
    ] {
        let (mut world, a, _b, p, mut rx) = retreat_world();
        match what {
            "ghost" => world.entity_mut(p).insert(Ghost),
            "frozen" => world.entity_mut(p).insert(mud_world::Frozen),
            _ => world.entity_mut(p).insert(mud_world::Stunned),
        };
        cmd_retreat(&mut world, p, "north");
        assert_eq!(room_of(&world, p), a, "{what}");
        assert!(drain(&mut rx).contains(expect), "{what}");
    }
}
