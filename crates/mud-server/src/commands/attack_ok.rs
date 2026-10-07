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

/// The master of a charmed pet, if `entity` is one.
fn charm_master(world: &mut World, entity: Entity) -> Option<Entity> {
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
        say(world, msg);
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
        world.entity_mut(a).insert(PlayerFlags(vec![]));
        world.entity_mut(b).insert(PlayerFlags(vec![]));
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
}
