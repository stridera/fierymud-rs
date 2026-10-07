//! Legacy `attack_ok` (`fierymud_legacy/src/fight.cpp:299`): may `ch` start
//! violence against `victim`? Shared by the violent-ability cast path and by
//! banish, so a mortal cannot hit another player, a peaceful mob or someone
//! else's pet with magic that the melee commands would refuse.
//!
//! Order, as in legacy:
//! 1. a pet never attacks its master;
//! 2. a peaceful room (either side) forbids it;
//! 3. a `Peaceful` mob can't be attacked;
//! 4. the dead (ghosts) can't be attacked;
//! 5. PK is fine when the server-wide `pk_allowed` toggle is on, or both
//!    parties stand in arena rooms, or (the modern per-player consent) both
//!    players have `PkEnabled`;
//! 6. otherwise a charmed pet counts as its master, attacking yourself or
//!    your own pet is fine, and player against player is refused.
//!
//! Legacy's paralysis/mesmerize check on the attacker is not ported here: the
//! cast path already refuses casters who can't act.

use bevy_ecs::prelude::*;
use mud_db::enums::{MobBehavior, PlayerFlag};
use mud_world::{
    AppliedTo, ArenaRoom, EffectInstance, Follower, Ghost, Located, Mob, MobBehaviors,
    MobPrototypes, PeacefulRoom, Player, PlayerFlags, RuntimeConfig, WorldKey,
};

use super::{cap_sentence_start, name_of, send_to};

/// `EFF_CHARM`: a `charmed` status effect sits on the entity.
pub(super) fn is_charmed(world: &mut World, entity: Entity) -> bool {
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    q.iter(world)
        .any(|(i, a)| a.0 == entity && i.name.eq_ignore_ascii_case("charmed"))
}

/// The player a pet belongs to: any mob following a player, whether it is
/// charmed or just a shop pet / summoned follower. A pet's attack is judged as
/// its owner's attack, and an attack on it as an attack on the owner.
pub(super) fn pet_owner(world: &World, entity: Entity) -> Option<Entity> {
    world.get::<Mob>(entity)?;
    let master = world.get::<Follower>(entity).map(|f| f.0)?;
    world.get::<Player>(master).is_some().then_some(master)
}

/// The master of a pet, if `entity` is one (any player-followed mob, or a
/// charmed mob following anyone).
fn charm_master(world: &mut World, entity: Entity) -> Option<Entity> {
    if let Some(owner) = pet_owner(world, entity) {
        return Some(owner);
    }
    let master = world.get::<Follower>(entity).map(|f| f.0)?;
    (world.get::<Mob>(entity).is_some() && is_charmed(world, entity)).then_some(master)
}

fn reflexive(world: &World, mob: Entity) -> &'static str {
    let gender = world
        .get::<WorldKey>(mob)
        .and_then(|k| {
            world
                .get_resource::<MobPrototypes>()?
                .by_key
                .get(&(k.zone, k.id))
                .map(|p| p.gender.to_ascii_lowercase())
        })
        .unwrap_or_default();
    match gender.as_str() {
        "male" => "himself",
        "female" => "herself",
        _ => "itself",
    }
}

fn is_pk_consenting(world: &World, e: Entity) -> bool {
    world
        .get::<PlayerFlags>(e)
        .is_some_and(|f| f.has(PlayerFlag::PkEnabled))
}

/// Legacy `attack_ok(ch, victim, verbose)`. With `verbose` the refusal is
/// sent to `ch`.
pub(crate) fn attack_ok(world: &mut World, ch: Entity, victim: Entity, verbose: bool) -> bool {
    let say = |world: &World, text: String| {
        if verbose {
            send_to(world, ch, text);
        }
    };

    // Prevent pets from attacking their masters.
    if world.get::<Mob>(ch).is_some() && world.get::<Follower>(ch).is_some_and(|f| f.0 == victim) {
        return false;
    }

    let ch_room = world.get::<Located>(ch).map(|l| l.0);
    let victim_room = world.get::<Located>(victim).map(|l| l.0);
    let in_room_flag = |world: &World, room: Option<Entity>, peaceful: bool| {
        room.is_some_and(|r| {
            if peaceful {
                world.get::<PeacefulRoom>(r).is_some()
            } else {
                world.get::<ArenaRoom>(r).is_some()
            }
        })
    };

    if ch != victim
        && (in_room_flag(world, victim_room, true) || in_room_flag(world, ch_room, true))
    {
        say(
            world,
            "You feel ashamed trying to disturb the peace of this room.\r\n".into(),
        );
        return false;
    }

    if world
        .get::<MobBehaviors>(victim)
        .is_some_and(|b| b.has(MobBehavior::Peaceful))
    {
        let n = cap_sentence_start(&name_of(world, victim));
        let refl = reflexive(world, victim);
        say(
            world,
            format!("But {n} just has such a calm, peaceful feeling about {refl}!\r\n"),
        );
        return false;
    }

    if world.get::<Ghost>(victim).is_some() {
        let n = cap_sentence_start(&name_of(world, victim));
        say(world, format!("{n} is already dead.\r\n"));
        return false;
    }

    // From here on, we consider PK.
    let pk_allowed = world
        .get_resource::<RuntimeConfig>()
        .is_some_and(|rc| rc.get_bool("social", "pk_allowed", false));
    if pk_allowed {
        return true;
    }
    if in_room_flag(world, victim_room, false) && in_room_flag(world, ch_room, false) {
        return true;
    }

    // A pet counts as its master.
    let mut attacker = ch;
    let mut target = victim;
    if let Some(m) = charm_master(world, attacker) {
        attacker = m;
    }
    let mut pet = false;
    if let Some(m) = charm_master(world, target) {
        target = m;
        pet = true;
    }

    // Hit yourself (or your own pet) as much as you please.
    if attacker == target {
        return true;
    }

    if world.get::<Player>(attacker).is_some() && world.get::<Player>(target).is_some() {
        if is_pk_consenting(world, attacker) && is_pk_consenting(world, target) {
            return true;
        }
        let msg = if pet {
            "Sorry, you can't attack someone else's pet!\r\n".to_string()
        } else if !is_pk_consenting(world, attacker) {
            "You must turn on PK first (type pk).\r\n".to_string()
        } else {
            let n = cap_sentence_start(&name_of(world, target));
            format!("{n} is not a player killer.\r\n")
        };
        // A pet has no connection: tell its owner.
        if verbose {
            send_to(world, attacker, msg);
        }
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::info::cmd_pk;
    use crate::commands::test_support::{drain, player_in};
    use mud_world::{Fighting, Named};

    fn pk_on(world: &mut World, e: Entity) {
        world
            .entity_mut(e)
            .insert(PlayerFlags(vec![PlayerFlag::PkEnabled]));
    }

    fn has_pk(world: &World, e: Entity) -> bool {
        world
            .get::<PlayerFlags>(e)
            .is_some_and(|f| f.has(PlayerFlag::PkEnabled))
    }

    /// Two players in one room; neither has any flags yet.
    fn duo() -> (World, Entity, Entity, crate::commands::test_support::Rx) {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (a, rx) = player_in(&mut world, room);
        let (b, _brx) = player_in(&mut world, room);
        world.entity_mut(b).insert(Named {
            name: "Victim".into(),
        });
        world
            .entity_mut(a)
            .insert((PlayerFlags(vec![]), mud_world::RecallPoint(room)));
        world
            .entity_mut(b)
            .insert((PlayerFlags(vec![]), mud_world::RecallPoint(room)));
        (world, a, b, rx)
    }

    #[test]
    fn pk_toggles_and_sets_the_persisted_flag() {
        let (mut world, a, _b, mut rx) = duo();
        cmd_pk(&mut world, a, "");
        assert!(has_pk(&world, a));
        assert!(drain(&mut rx).contains("Player killing is now ON"));
        cmd_pk(&mut world, a, "");
        assert!(!has_pk(&world, a));
        assert!(drain(&mut rx).contains("Player killing is now OFF"));
        cmd_pk(&mut world, a, "on");
        assert!(has_pk(&world, a));
        cmd_pk(&mut world, a, "on");
        assert!(has_pk(&world, a), "explicit on is idempotent");
        assert!(drain(&mut rx).contains("already ON"));
        cmd_pk(&mut world, a, "off");
        assert!(!has_pk(&world, a));
        cmd_pk(&mut world, a, "bogus");
        assert!(drain(&mut rx).contains("Usage: pk"));
        assert!(!has_pk(&world, a));
    }

    #[test]
    fn pk_off_is_refused_while_fighting_a_player() {
        let (mut world, a, b, mut rx) = duo();
        pk_on(&mut world, a);
        pk_on(&mut world, b);
        world.entity_mut(a).insert(Fighting(b));
        cmd_pk(&mut world, a, "off");
        assert!(has_pk(&world, a));
        assert!(drain(&mut rx).contains("Not while you're fighting!"));
        // The one being attacked can't slip away either.
        world.entity_mut(a).remove::<Fighting>();
        world.entity_mut(b).insert(Fighting(a));
        cmd_pk(&mut world, a, "off");
        assert!(has_pk(&world, a));
        // Fight over: allowed again.
        world.entity_mut(b).remove::<Fighting>();
        cmd_pk(&mut world, a, "off");
        assert!(!has_pk(&world, a));
    }

    #[test]
    fn pk_off_is_allowed_while_fighting_a_mob() {
        let (mut world, a, _b, _rx) = duo();
        pk_on(&mut world, a);
        let room = world.get::<Located>(a).unwrap().0;
        let m = world
            .spawn((Mob, Named { name: "rat".into() }, Located(room)))
            .id();
        world.entity_mut(a).insert(Fighting(m));
        cmd_pk(&mut world, a, "off");
        assert!(!has_pk(&world, a));
    }

    #[test]
    fn players_fight_only_when_both_have_pk() {
        let (mut world, a, b, mut rx) = duo();
        // Neither: refused, attacker told to turn PK on.
        assert!(!attack_ok(&mut world, a, b, true));
        assert!(drain(&mut rx).contains("You must turn on PK first (type pk)."));
        // Victim only: the attacker is still the one who is out.
        pk_on(&mut world, b);
        assert!(!attack_ok(&mut world, a, b, true));
        assert!(drain(&mut rx).contains("You must turn on PK first"));
        // Attacker only: the victim is not a player killer.
        world.entity_mut(b).insert(PlayerFlags(vec![]));
        pk_on(&mut world, a);
        assert!(!attack_ok(&mut world, a, b, true));
        assert!(drain(&mut rx).contains("Victim is not a player killer."));
        // Both: allowed, in both directions.
        pk_on(&mut world, b);
        assert!(attack_ok(&mut world, a, b, true));
        assert!(attack_ok(&mut world, b, a, true));
    }

    #[test]
    fn melee_commands_obey_the_pk_rule() {
        let (mut world, a, b, mut rx) = duo();
        world
            .entity_mut(b)
            .insert(mud_world::CombatStats::default());
        crate::commands::combat_commands::cmd_attack(&mut world, a, "victim");
        assert!(world.get::<Fighting>(a).is_none());
        assert!(world.get::<Fighting>(b).is_none());
        assert!(drain(&mut rx).contains("You must turn on PK first"));
    }

    #[test]
    fn mobs_are_unaffected_by_the_pk_rule() {
        let (mut world, a, _b, _rx) = duo();
        let room = world.get::<Located>(a).unwrap().0;
        let m = world
            .spawn((Mob, Named { name: "rat".into() }, Located(room)))
            .id();
        // A player without PK can attack a mob, and a mob can attack them.
        assert!(attack_ok(&mut world, a, m, false));
        assert!(attack_ok(&mut world, m, a, false));
    }

    #[test]
    fn aoe_skips_players_who_fail_the_mutual_rule() {
        let (mut world, a, b, mut rx) = duo();
        let room = world.get::<Located>(a).unwrap().0;
        // A non-PK player is never an AOE target.
        let targets = crate::commands::aoe_targets_in_room(
            &mut world,
            a,
            room,
            crate::commands::AoeScope::RoomEnemies,
        );
        assert!(targets.is_empty());
        // A PK-on victim is listed but filtered out when the caster has PK
        // off; the caster is told once, not per target.
        pk_on(&mut world, b);
        let cast = crate::commands::invoke_ability_aoe(
            &mut world,
            a,
            mud_db::abilities::AbilityKind::Spell,
            "cast",
            "fireball",
            crate::commands::AoeScope::RoomEnemies,
            "Nothing here.\r\n",
        );
        assert!(!cast);
        let text = drain(&mut rx);
        assert_eq!(
            text.matches("You must turn on PK first").count(),
            1,
            "{text}"
        );
    }

    #[test]
    fn auto_assist_follower_without_pk_does_not_join_a_pk_fight() {
        let (mut world, a, b, _rx) = duo();
        let room = world.get::<Located>(a).unwrap().0;
        let (h, _hrx) = player_in(&mut world, room);
        world
            .entity_mut(h)
            .insert((Follower(b), PlayerFlags(vec![PlayerFlag::AutoAssist])));
        pk_on(&mut world, a);
        pk_on(&mut world, b);
        crate::commands::auto_assist_followers_of(&mut world, b, a, room);
        assert!(world.get::<Fighting>(h).is_none());
        pk_on(&mut world, h);
        world.entity_mut(h).insert(PlayerFlags(vec![
            PlayerFlag::AutoAssist,
            PlayerFlag::PkEnabled,
        ]));
        crate::commands::auto_assist_followers_of(&mut world, b, a, room);
        assert!(world.get::<Fighting>(h).is_some());
    }

    /// `owner`'s pet: a mob that only has `Follower(owner)` (a shop pet or a
    /// summoned follower, no charm effect).
    fn pet_of(world: &mut World, owner: Entity) -> Entity {
        let room = world.get::<Located>(owner).unwrap().0;
        world
            .spawn((
                Mob,
                Named { name: "pet".into() },
                mud_world::Keywords(vec!["pet".into()]),
                Located(room),
                Follower(owner),
                mud_world::CombatStats::default(),
            ))
            .id()
    }

    fn staff_profile(level: i32) -> mud_world::Profile {
        mud_world::Profile {
            level,
            class_id: None,
            race: "Human".into(),
            experience: 0,
            gender: "neutral".into(),
        }
    }

    #[test]
    fn pk_off_is_refused_away_from_the_recall_point() {
        let (mut world, a, _b, mut rx) = duo();
        pk_on(&mut world, a);
        let elsewhere = world.spawn_empty().id();
        let home = world.get::<Located>(a).unwrap().0;
        world.entity_mut(home).insert(Named {
            name: "The Temple of Midgaard".into(),
        });
        world.entity_mut(a).insert(Located(elsewhere));
        cmd_pk(&mut world, a, "off");
        assert!(has_pk(&world, a));
        assert!(drain(&mut rx).contains(
            "You can only turn off player killing at your recall point (The Temple of Midgaard)."
        ));
        // The toggle command takes the same road.
        crate::commands::info::cmd_toggle(&mut world, a, "pk");
        assert!(has_pk(&world, a));
    }

    #[test]
    fn pk_off_is_allowed_at_the_recall_point_and_on_works_anywhere() {
        let (mut world, a, _b, _rx) = duo();
        pk_on(&mut world, a);
        cmd_pk(&mut world, a, "off");
        assert!(!has_pk(&world, a));
        let elsewhere = world.spawn_empty().id();
        world.entity_mut(a).insert(Located(elsewhere));
        cmd_pk(&mut world, a, "on");
        assert!(has_pk(&world, a));
    }

    #[test]
    fn staff_may_turn_pk_off_anywhere() {
        let (mut world, a, _b, _rx) = duo();
        pk_on(&mut world, a);
        let elsewhere = world.spawn_empty().id();
        world
            .entity_mut(a)
            .insert((Located(elsewhere), staff_profile(100)));
        cmd_pk(&mut world, a, "off");
        assert!(!has_pk(&world, a));
    }

    #[test]
    fn toggle_pk_obeys_the_fighting_rule() {
        let (mut world, a, b, mut rx) = duo();
        pk_on(&mut world, a);
        pk_on(&mut world, b);
        world.entity_mut(a).insert(Fighting(b));
        crate::commands::info::cmd_toggle(&mut world, a, "pk");
        assert!(has_pk(&world, a));
        assert!(drain(&mut rx).contains("Not while you're fighting!"));
    }

    #[test]
    fn pk_off_is_refused_while_a_pet_fights_another_players_pet() {
        let (mut world, a, b, mut rx) = duo();
        pk_on(&mut world, a);
        let mine = pet_of(&mut world, a);
        let theirs = pet_of(&mut world, b);
        world.entity_mut(mine).insert(Fighting(theirs));
        cmd_pk(&mut world, a, "off");
        assert!(has_pk(&world, a));
        assert!(drain(&mut rx).contains("Not while you're fighting!"));
        // Fighting a plain mob with a pet does not count.
        let room = world.get::<Located>(a).unwrap().0;
        let rat = world
            .spawn((Mob, Named { name: "rat".into() }, Located(room)))
            .id();
        world.entity_mut(mine).insert(Fighting(rat));
        cmd_pk(&mut world, a, "off");
        assert!(!has_pk(&world, a));
    }

    #[test]
    fn a_followed_mob_counts_as_its_owners_pet() {
        let (mut world, a, b, mut rx) = duo();
        let pet = pet_of(&mut world, a);
        // The owner has PK off: the pet may not attack a player.
        pk_on(&mut world, b);
        assert!(!attack_ok(&mut world, pet, b, true));
        assert!(drain(&mut rx).contains("You must turn on PK first"));
        // And a non-PK stranger can't attack it.
        assert!(!attack_ok(&mut world, b, pet, false));
        // With both owners PK-on, the pet is fair game.
        pk_on(&mut world, a);
        assert!(attack_ok(&mut world, pet, b, false));
        assert!(attack_ok(&mut world, b, pet, false));
    }

    #[test]
    fn ordering_a_pet_to_kill_a_player_obeys_the_pk_rule() {
        let (mut world, a, b, mut rx) = duo();
        world
            .entity_mut(b)
            .insert(mud_world::CombatStats::default());
        let pet = pet_of(&mut world, a);
        crate::commands::info::cmd_order(&mut world, a, "pet kill victim");
        assert!(world.get::<Fighting>(pet).is_none());
        assert!(world.get::<Fighting>(b).is_none());
        let text = drain(&mut rx);
        assert!(text.contains("You must turn on PK first"), "{text}");
        // Both players PK-on: the pet may engage.
        pk_on(&mut world, a);
        pk_on(&mut world, b);
        crate::commands::info::cmd_order(&mut world, a, "pet kill victim");
        let text = drain(&mut rx);
        assert!(text.contains("pet attacks Victim"), "{text}");
        assert!(!text.contains("You must turn on PK"), "{text}");
    }

    #[test]
    fn bash_backstab_and_taunt_obey_the_pk_rule() {
        use crate::commands::combat_commands::{cmd_backstab, cmd_bash, cmd_taunt};
        let (mut world, a, b, mut rx) = duo();
        world
            .entity_mut(b)
            .insert(mud_world::CombatStats::default());
        world
            .entity_mut(b)
            .insert(mud_world::Keywords(vec!["victim".into()]));
        cmd_bash(&mut world, a, "victim");
        let text = drain(&mut rx);
        assert!(text.contains("You must turn on PK first"), "bash: {text}");
        cmd_backstab(&mut world, a, "victim");
        let text = drain(&mut rx);
        assert!(
            text.contains("You must turn on PK first"),
            "backstab: {text}"
        );
        cmd_taunt(&mut world, a, "victim");
        let text = drain(&mut rx);
        assert!(text.contains("You must turn on PK first"), "taunt: {text}");
        assert!(world.get::<Fighting>(a).is_none());
        assert!(world.get::<Fighting>(b).is_none());
    }

    #[test]
    fn disarm_obeys_the_pk_rule() {
        let (mut world, a, b, mut rx) = duo();
        world
            .entity_mut(b)
            .insert(mud_world::CombatStats::default());
        crate::commands::combat_commands::cmd_disarm(&mut world, a, "victim");
        let text = drain(&mut rx);
        assert!(text.contains("You must turn on PK first"), "{text}");
    }

    #[test]
    fn hitall_and_sweep_skip_other_players_pets() {
        use crate::commands::combat_commands::{cmd_hitall, cmd_sweep};
        let (mut world, a, b, _rx) = duo();
        let theirs = pet_of(&mut world, b);
        let mine = pet_of(&mut world, a);
        let room = world.get::<Located>(a).unwrap().0;
        let rat = world
            .spawn((
                Mob,
                Named { name: "rat".into() },
                Located(room),
                mud_world::Health { hp: 50, max: 50 },
            ))
            .id();
        for e in [theirs, mine] {
            world
                .entity_mut(e)
                .insert(mud_world::Health { hp: 50, max: 50 });
        }
        cmd_hitall(&mut world, a, "");
        cmd_sweep(&mut world, a, "");
        let hp = |w: &World, e: Entity| w.get::<mud_world::Health>(e).unwrap().hp;
        assert_eq!(hp(&world, theirs), 50, "another player's pet is skipped");
        assert_eq!(hp(&world, mine), 50, "the attacker's own pet is skipped");
        assert!(hp(&world, rat) < 50, "plain mobs are still hit");
    }

    #[test]
    fn rescue_from_a_player_obeys_the_pk_rule() {
        use mud_db::abilities::AbilityKind;
        let (mut world, rescuer, attacker, mut rx) = duo();
        let room = world.get::<Located>(rescuer).unwrap().0;
        let (ally, _arx) = player_in(&mut world, room);
        world.entity_mut(ally).insert(Named {
            name: "Ally".into(),
        });
        world
            .entity_mut(ally)
            .insert(mud_world::Keywords(vec!["ally".into()]));
        world.entity_mut(attacker).insert(Fighting(ally));
        world.entity_mut(ally).insert(Fighting(attacker));
        let mut abilities = mud_world::AbilityCatalog::default();
        let def = crate::commands::test_support::ability_def(900, "Rescue", AbilityKind::Skill);
        abilities.by_name.insert("rescue".to_string(), def);
        abilities
            .effects_for
            .insert(900, vec![(901, Some(serde_json::json!({"aggro": true})))]);
        world.insert_resource(abilities);
        let mut effects = mud_world::EffectCatalog::default();
        effects.by_id.insert(
            901,
            mud_world::EffectDef {
                id: 901,
                name: "redirect".into(),
                description: None,
                effect_type: "redirect".into(),
                tags: vec![],
                presence_override: None,
                default_params: serde_json::json!({"aggro": true}),
                prevents_speaking: false,
                prevents_casting: false,
                prevents_movement: false,
                on_apply: None,
                on_tick: None,
                on_remove: None,
            },
        );
        world.insert_resource(effects);
        world.insert_resource(mud_world::WeatherCatalog::default());
        world.insert_resource(mud_world::RaceCatalog::default());
        world.entity_mut(rescuer).insert(mud_world::KnownAbilities {
            entries: vec![(900, 1000, true)],
        });
        crate::commands::combat_commands::cmd_rescue(&mut world, rescuer, "ally");
        let text = drain(&mut rx);
        assert!(text.contains("You must turn on PK first"), "{text}");
        assert!(world.get::<Fighting>(rescuer).is_none());
        assert!(world.get::<Fighting>(attacker).is_some_and(|f| f.0 == ally));
    }
}
