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
        say(
            world,
            if pet {
                "Sorry, you can't attack someone else's pet!\r\n".into()
            } else {
                "Sorry, player killing isn't allowed.\r\n".into()
            },
        );
        return false;
    }
    true
}
