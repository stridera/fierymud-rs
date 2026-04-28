use bevy_ecs::prelude::*;
use mud_world::{
    CombatStats, Description, Exits, Fighting, Health, Item, Keywords, Located, Mob, Named,
    Player, PlayerFlags, Posture, PostureKind, Slot, WearableIn, WorldKeyIndex,
};
use tracing::info;

use crate::TickCount;
use crate::commands::{apply_damage, cmd_flee, drain_stamina, send_to};

const COMBAT_PERIOD_TICKS: u64 = 10;

/// Spawn a couple of hardcoded mobs so combat has someone to hit. Real
/// mob-reset spawning is a future step.
pub fn seed_test_mobs(world: &mut World) {
    let void = world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(0, 0))
        .copied();
    if let Some(room) = void {
        world.spawn((
            Mob,
            Named {
                name: "a training dummy".to_string(),
            },
            Keywords(vec!["dummy".into(), "training".into()]),
            Description("A scarecrow-like training dummy stands here, patiently waiting to be punched.".into()),
            Located(room),
            Health { hp: 30, max: 30 },
            Posture(PostureKind::Standing),
            // No CombatStats: dummy doesn't retaliate.
        ));
        info!("seeded training dummy in The Void");
    }

    let town = world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(30, 5))
        .copied();
    if let Some(room) = town {
        world.spawn((
            Mob,
            Named {
                name: "a weak goblin".to_string(),
            },
            Keywords(vec!["goblin".into(), "weak".into()]),
            Description("A weak goblin sneers and clutches a rusty knife.".into()),
            Located(room),
            Health { hp: 25, max: 25 },
            CombatStats {
                hit_roll: 0,
                dmg_roll: 2,
                ac: 10,
                alignment: -100,
            },
            Posture(PostureKind::Standing),
        ));
        info!("seeded weak goblin in Town Center");
    }
}

/// Spawn a couple of starter items in The Void so we can test inventory.
/// Real spawning via `ObjectResets` is a future step.
pub fn seed_test_items(world: &mut World) {
    let void = world
        .resource::<WorldKeyIndex>()
        .rooms
        .get(&(0, 0))
        .copied();
    let Some(room) = void else { return };

    world.spawn((
        Item,
        Named {
            name: "a rusty sword".to_string(),
        },
        Keywords(vec!["sword".into(), "rusty".into()]),
        Description(
            "An iron blade pitted with rust, edge dulled by years of disuse. Still serviceable."
                .into(),
        ),
        Located(room),
        WearableIn(Slot::Wield),
    ));
    world.spawn((
        Item,
        Named {
            name: "a healing potion".to_string(),
        },
        Keywords(vec!["potion".into(), "healing".into()]),
        Description(
            "A small glass vial filled with a swirling crimson liquid. \
             It smells faintly of mint and copper."
                .into(),
        ),
        Located(room),
    ));
    info!("seeded test items in The Void");
}

/// Exclusive system: every `COMBAT_PERIOD_TICKS` world ticks, every entity with
/// Fighting takes a swing at its target.
pub fn combat_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(COMBAT_PERIOD_TICKS) {
        return;
    }

    // Phase 1: snapshot the swing list. Each tuple is fully owned data so
    // the borrow on the query is released before we start mutating.
    // Sleeping attackers are skipped — they can't swing until awoken.
    let swings: Vec<Swing> = {
        let mut q =
            world.query::<(Entity, &Fighting, &CombatStats, &Named, Option<&Posture>)>();
        q.iter(world)
            .filter(|(_, _, _, _, posture)| {
                !matches!(posture.map(|p| p.0), Some(PostureKind::Sleeping))
            })
            .map(|(attacker, fighting, cs, name, _)| Swing {
                attacker,
                target: fighting.0,
                damage: cs.dmg_roll.max(1),
                attacker_name: name.name.clone(),
            })
            .collect()
    };

    for s in &swings {
        apply_swing(world, s);
    }
    // Prompts for combatants and bystanders are handled centrally by
    // commands::flush_prompts after schedule.run — every send_to here
    // already registers the recipient.
}

struct Swing {
    attacker: Entity,
    target: Entity,
    damage: i32,
    attacker_name: String,
}

fn apply_swing(world: &mut World, s: &Swing) {
    // Target may have been despawned earlier in this same tick.
    if world.get_entity(s.target).is_err() {
        if let Ok(mut e) = world.get_entity_mut(s.attacker) {
            e.remove::<Fighting>();
        }
        return;
    }

    // Auto-disengage if the combatants are no longer in the same room.
    let attacker_room = world.get::<Located>(s.attacker).map(|l| l.0);
    let target_room = world.get::<Located>(s.target).map(|l| l.0);
    if attacker_room != target_room || attacker_room.is_none() {
        if let Ok(mut e) = world.get_entity_mut(s.attacker) {
            e.remove::<Fighting>();
        }
        if let Ok(mut e) = world.get_entity_mut(s.target) {
            e.remove::<Fighting>();
        }
        send_to(world, s.attacker, "Your target has slipped away.\r\n");
        return;
    }
    let room = attacker_room.unwrap();

    let target_name = world
        .get::<Named>(s.target)
        .map_or_else(String::new, |n| n.name.clone());

    if world.get::<Health>(s.target).is_none() {
        // No Health component: nothing to damage. End combat from this side.
        if let Ok(mut e) = world.get_entity_mut(s.attacker) {
            e.remove::<Fighting>();
        }
        return;
    }
    let was_sleeping =
        world.get::<Posture>(s.target).map(|p| p.0) == Some(PostureKind::Sleeping);
    let (dead, threshold_msg) = apply_damage(world, s.target, s.damage);

    send_to(
        world,
        s.attacker,
        format!("You hit {target_name} for {} damage.\r\n", s.damage),
    );
    send_to(
        world,
        s.target,
        format!(
            "{} hits you for {} damage.\r\n",
            s.attacker_name, s.damage
        ),
    );
    if was_sleeping && !dead {
        if let Ok(mut e) = world.get_entity_mut(s.target) {
            e.insert(Posture(PostureKind::Standing));
        }
        send_to(world, s.target, "You jolt awake!\r\n");
        broadcast_room_except(
            world,
            room,
            &[s.attacker, s.target],
            &format!("{target_name} jolts awake!\r\n"),
        );
    }
    if let Some(m) = threshold_msg {
        send_to(world, s.target, m);
    }
    broadcast_room_except(
        world,
        room,
        &[s.attacker, s.target],
        &format!("{} hits {target_name}.\r\n", s.attacker_name),
    );

    // Sustained-combat stamina drain: 1 per swing on the attacker. No-op
    // for actors without a Stamina component (most mobs). Threshold
    // messages ("getting tired" / "collapse") fire automatically the first
    // time the attacker crosses each band — adds pressure to disengage
    // rather than fight forever.
    if !dead {
        drain_stamina(world, s.attacker, 1);
    }

    if dead {
        handle_death(world, s.target, &target_name, room);
        return;
    }

    // Wimpy auto-flee: if the defender is a player with the WIMPY flag and
    // their HP just dropped below 25% of max, attempt to flee. Done after
    // the threshold message so the dramatic order reads:
    //
    //   X hits you for 12 damage.
    //   You are badly hurt!
    //   You panic and flee east!
    //
    // cmd_flee handles "no exits" and the room-broadcast itself.
    let target_is_player = world.get::<Player>(s.target).is_some();
    let wimpy_set = world
        .get::<PlayerFlags>(s.target)
        .is_some_and(|pf| pf.has(mud_db::enums::PlayerFlag::Wimpy));
    if target_is_player && wimpy_set
        && let Some(hp) = world.get::<Health>(s.target).copied()
        && hp.hp > 0
        && hp.hp * 4 < hp.max
    {
        // Look for any open exit before announcing the panic — otherwise
        // we'd print "You panic!" and then immediately "There's nowhere
        // to run!" from cmd_flee, which reads as a contradiction.
        let has_exit = world
            .get::<Exits>(room)
            .is_some_and(|e| {
                e.0.values()
                    .any(|ed| ed.state == mud_db::enums::ExitState::Open && ed.to.is_some())
            });
        if has_exit {
            send_to(world, s.target, "You panic!\r\n");
            cmd_flee(world, s.target, "");
        } else {
            send_to(
                world,
                s.target,
                "You panic, but there's nowhere to run!\r\n",
            );
        }
    }
}

fn handle_death(world: &mut World, victim: Entity, victim_name: &str, room: Entity) {
    let is_player = world.get::<Player>(victim).is_some();

    // Find every entity that was fighting the victim.
    let attackers: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Fighting)>();
        q.iter(world)
            .filter(|(_, f)| f.0 == victim)
            .map(|(e, _)| e)
            .collect()
    };

    if is_player {
        // Player "death": revive in place, end all combat involving them.
        if let Some(mut hp) = world.get_mut::<Health>(victim) {
            hp.hp = hp.max;
        }
        if let Ok(mut e) = world.get_entity_mut(victim) {
            e.remove::<Fighting>();
        }
        for a in attackers {
            if let Ok(mut e) = world.get_entity_mut(a) {
                e.remove::<Fighting>();
            }
        }
        send_to(
            world,
            victim,
            "You collapse, then gasp back to life with full health.\r\n",
        );
        broadcast_room_except(
            world,
            room,
            &[victim],
            &format!("{victim_name} collapses, then revives.\r\n"),
        );
        info!(?victim, name = %victim_name, "player auto-revived");
    } else {
        // Mob death: notify, despawn, stop attackers.
        broadcast_room_except(world, room, &[], &format!("{victim_name} dies.\r\n"));
        for a in attackers {
            if let Ok(mut e) = world.get_entity_mut(a) {
                e.remove::<Fighting>();
            }
            send_to(world, a, "Your target falls.\r\n");
        }
        if let Ok(e) = world.get_entity_mut(victim) {
            e.despawn();
        }
        info!(?victim, name = %victim_name, "mob despawned");
    }
}

fn broadcast_room_except(world: &mut World, room: Entity, except: &[Entity], msg: &str) {
    let targets: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located)>();
        q.iter(world)
            .filter(|(e, l)| l.0 == room && !except.contains(e))
            .map(|(e, _)| e)
            .collect()
    };
    for t in targets {
        send_to(world, t, msg);
    }
}
