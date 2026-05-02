use bevy_ecs::prelude::*;
use mud_world::{
    AppliedTo, CombatStats, Corpse, CorpseDecay, Description, EffectInstance, Exits, Fighting,
    Ghost, Guarding, Health, Item, Keywords, Located, Mob, MobPrototypes, Named, Player,
    PlayerFlags, Posture, PostureKind, Slot, Stunned, Wealth, WearableIn, WorldKey, WorldKeyIndex,
};
use tracing::info;

use crate::TickCount;
use crate::commands::{
    apply_damage, broadcast_room_except_rendered, cmd_flee, color_mode_for,
    disengage_attackers_of, drain_stamina, name_of, render_color_tags, send_to, try_insert,
    try_remove,
};

const COMBAT_PERIOD_TICKS: u64 = 10;

/// Spawn a single hardcoded test mob in The Void so combat tests have a
/// stable target without depending on reset content. The Void has no
/// MobResets/ObjectResets in the imported world, so this dummy is the
/// only thing there. The dummy intentionally has no `CombatStats` —
/// it's a punching bag that doesn't fight back, useful for testing
/// hit-resolution without coping with retaliation.
///
/// (The "weak goblin in Town Center" we used to seed lived alongside
/// the real `MobResets` content for that room — now that resets spawn
/// real stray dogs there, the seeded goblin would just be a confusing
/// duplicate.)
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
/// One real-time second per tick fire (10 ticks at 10Hz). Decrements
/// every `CorpseDecay.remaining_secs`; on hitting 0 re-Locates any
/// items inside the corpse to the corpse's room, broadcasts a decay
/// line, and despawns the corpse entity. Ephemeral — corpses don't
/// survive server restart.
pub fn corpse_decay_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(10) {
        return;
    }
    // Snapshot so we can mutate freely.
    let corpses: Vec<(Entity, Entity, i32, String)> = {
        let mut q = world.query_filtered::<(Entity, &Located, &CorpseDecay, &Named), With<Corpse>>();
        q.iter(world)
            .map(|(e, l, d, n)| (e, l.0, d.remaining_secs, n.name.clone()))
            .collect()
    };
    for (corpse, room, prev_remaining, name) in corpses {
        // Decrement first (or expire and despawn).
        let new_remaining = {
            if let Some(mut d) = world.get_mut::<CorpseDecay>(corpse) {
                d.remaining_secs -= 1;
                d.remaining_secs
            } else {
                continue;
            }
        };
        // Atmospheric decay markers — fire on the tick that crosses
        // each threshold so a snapshot-restored corpse with a non-
        // canonical timer (e.g. 380s) still hits them on the way
        // down. Silent if the room has no observers.
        if let Some(line) = decay_milestone(prev_remaining, new_remaining, &name) {
            broadcast_room_except_rendered(world, room, &[], &line);
        }
        if new_remaining > 0 {
            continue;
        }
        // Expired — drop contents to the room, then despawn.
        let contents: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
            q.iter(world)
                .filter(|(_, l)| l.0 == corpse)
                .map(|(e, _)| e)
                .collect()
        };
        for it in contents {
            if let Some(mut l) = world.get_mut::<Located>(it) {
                l.0 = room;
            }
        }
        broadcast_room_except_rendered(
            world,
            room,
            &[],
            &format!("{name} crumbles to dust, leaving its contents behind.\r\n"),
        );
        if let Ok(em) = world.get_entity_mut(corpse) {
            em.despawn();
        }
    }
}

/// Atmospheric line for the tick that crossed a decay threshold.
/// `prev` is the value before this second's decrement, `now` after,
/// so a line fires on the exact tick where `prev > T >= now` for
/// each threshold. Returns None for ticks that didn't cross one.
fn decay_milestone(prev: i32, now: i32, name: &str) -> Option<String> {
    if prev > 300 && now <= 300 {
        Some(format!("Flies gather around {name}.\r\n"))
    } else if prev > 120 && now <= 120 {
        Some(format!("{name} begins to stink.\r\n"))
    } else if prev > 30 && now <= 30 {
        Some(format!("{name} sags as decay sets in.\r\n"))
    } else {
        None
    }
}

pub fn combat_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(COMBAT_PERIOD_TICKS) {
        return;
    }

    // Pre-pass: collect all entities currently affected by `berserk`
    // so the swing snapshot can apply a +50% damage bonus without a
    // separate per-attacker effect lookup.
    let berserk_attackers: std::collections::HashSet<Entity> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(eff, _)| eff.name.eq_ignore_ascii_case("berserk"))
            .map(|(_, a)| a.0)
            .collect()
    };

    // Phase 1: snapshot the swing list. Each tuple is fully owned data so
    // the borrow on the query is released before we start mutating.
    // Non-alert postures (Sleeping / Resting / Sitting) skip the swing —
    // mirrors the player-command posture gate (require_alert_posture)
    // and gives `stomp` (which sets target Posture to Sitting) a real
    // combat consequence. Mobs without a Posture component fall through
    // (they're alert by default). `Stunned` attackers also skip — the
    // marker is added by the stun effect-type and removed by
    // `effects_tick` once every backing stun EffectInstance expires.
    // Pre-pass: collect (guarder, defended) pairs whose guarder is in
    // the same room as the defended target. The swing-snapshot uses
    // this to redirect attacker hits onto the bodyguard.
    let guards: Vec<(Entity, Entity)> = {
        let mut q = world.query::<(Entity, &Guarding, &Located)>();
        q.iter(world)
            .filter_map(|(g, guarded, loc)| {
                let target_loc = world.get::<Located>(guarded.0).map(|l| l.0)?;
                if target_loc != loc.0 {
                    return None;
                }
                Some((g, guarded.0))
            })
            .collect()
    };
    let swings: Vec<Swing> = {
        let mut q = world
            .query::<(
                Entity,
                &Fighting,
                &CombatStats,
                &Named,
                Option<&Posture>,
                Option<&Stunned>,
            )>();
        q.iter(world)
            .filter(|(_, _, _, _, posture, stunned)| {
                stunned.is_none()
                    && matches!(posture.map(|p| p.0), None | Some(PostureKind::Standing))
            })
            .map(|(attacker, fighting, cs, name, _, _)| {
                let base = cs.dmg_roll.max(1);
                let damage = if berserk_attackers.contains(&attacker) {
                    (base * 3) / 2
                } else {
                    base
                };
                // Redirect swing onto a bodyguard if any guarder is
                // protecting the original target. First-match wins;
                // self-guard (guarder == target) is filtered out.
                let target = guards
                    .iter()
                    .find(|(g, defended)| *defended == fighting.0 && *g != fighting.0 && *g != attacker)
                    .map_or(fighting.0, |(g, _)| *g);
                Swing {
                    attacker,
                    target,
                    damage,
                    attacker_name: name.name.clone(),
                }
            })
            .collect()
    };

    for s in &swings {
        apply_swing(world, s);
    }
    // Fire FIGHT triggers on every still-living target after the
    // swing pass. Each fire binds `self` to the target and `actor`
    // to the attacker. Bodies typically self-throttle via
    // `time.stamp` deltas; the dispatcher just checks the flag.
    for s in &swings {
        if world.get_entity(s.target).is_err() {
            continue;
        }
        crate::triggers::fire_event_with_actor(
            world,
            s.target,
            s.attacker,
            mud_world::TriggerEvent::Fight,
        );
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
        try_remove::<Fighting>(world, s.attacker);
        return;
    }

    // Auto-disengage if the combatants are no longer in the same room.
    let attacker_room = world.get::<Located>(s.attacker).map(|l| l.0);
    let target_room = world.get::<Located>(s.target).map(|l| l.0);
    if attacker_room != target_room || attacker_room.is_none() {
        try_remove::<Fighting>(world, s.attacker);
        try_remove::<Fighting>(world, s.target);
        send_to(world, s.attacker, "Your target has slipped away.\r\n");
        return;
    }
    let room = attacker_room.unwrap();

    let target_name = name_of(world, s.target);

    if world.get::<Health>(s.target).is_none() {
        // No Health component: nothing to damage. End combat from this side.
        try_remove::<Fighting>(world, s.attacker);
        return;
    }
    let was_sleeping =
        world.get::<Posture>(s.target).map(|p| p.0) == Some(PostureKind::Sleeping);
    let (dead, threshold_msg) = apply_damage(world, s.target, s.damage);

    // Names may carry XML-Lite tags; render per-recipient so each player
    // gets ANSI or stripped output according to their own COLOR_BLIND flag.
    let attacker_mode = color_mode_for(world, s.attacker);
    let target_mode = color_mode_for(world, s.target);
    send_to(
        world,
        s.attacker,
        render_color_tags(
            &format!("You hit {target_name} for {} damage.\r\n", s.damage),
            attacker_mode,
        ),
    );
    send_to(
        world,
        s.target,
        render_color_tags(
            &format!("{} hits you for {} damage.\r\n", s.attacker_name, s.damage),
            target_mode,
        ),
    );
    if was_sleeping && !dead {
        try_insert(world, s.target, Posture(PostureKind::Standing));
        send_to(world, s.target, "You jolt awake!\r\n");
        broadcast_room_except_rendered(
            world,
            room,
            &[s.attacker, s.target],
            &format!("{target_name} jolts awake!\r\n"),
        );
    }
    if let Some(m) = threshold_msg {
        send_to(world, s.target, m);
    }
    broadcast_room_except_rendered(
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
    // their HP just dropped below the configured percentage of max,
    // attempt to flee. Default 25% if no `WimpyThreshold` component is
    // set; the `wimpy <pct>` command writes one.
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
    let wimpy_pct = world
        .get::<mud_world::WimpyThreshold>(s.target)
        .map_or(25, |w| w.0)
        .clamp(1, 99);
    if target_is_player && wimpy_set
        && let Some(hp) = world.get::<Health>(s.target).copied()
        && hp.hp > 0
        && hp.hp * 100 < hp.max * wimpy_pct
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

#[allow(clippy::too_many_lines)]
pub(crate) fn handle_death(
    world: &mut World,
    victim: Entity,
    victim_name: &str,
    room: Entity,
) {
    let is_player = world.get::<Player>(victim).is_some();

    if is_player {
        // If they're already a Ghost, don't double-corpse them — just
        // pin HP to 1 and stop combat. Ghosts in combat is rare (no
        // gate today) but defensible as a no-op safety branch.
        if world.get::<Ghost>(victim).is_some() {
            if let Some(mut hp) = world.get_mut::<Health>(victim) {
                hp.hp = hp.hp.max(1);
            }
            try_remove::<Fighting>(world, victim);
            disengage_attackers_of(world, victim);
            return;
        }

        // Player death: stop combat, spawn a corpse with their stuff
        // in it, ghost the player. They stay where they are (in their
        // body's last room) until they `release`. Default decay is
        // 10 minutes; legacy MUDs typically decayed in similar time.
        let attackers: Vec<Entity> = {
            let mut q = world.query::<(Entity, &Fighting)>();
            q.iter(world)
                .filter(|(_, f)| f.0 == victim)
                .map(|(e, _)| e)
                .collect()
        };
        try_remove::<Fighting>(world, victim);
        for a in attackers {
            try_remove::<Fighting>(world, a);
        }
        // Move every Item Located on the player (carried + worn) to
        // the corpse. Equipped slots are dropped (item becomes a
        // floor item inside the corpse). Spawn the corpse first, then
        // re-Located each item to it.
        let owned_items: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
            q.iter(world)
                .filter(|(_, l)| l.0 == victim)
                .map(|(e, _)| e)
                .collect()
        };
        let corpse_name = format!("the corpse of {victim_name}");
        let corpse = world
            .spawn((
                Item,
                Corpse,
                Named { name: corpse_name },
                Keywords(vec![
                    "corpse".to_string(),
                    victim_name.to_ascii_lowercase(),
                ]),
                Located(room),
                CorpseDecay { remaining_secs: 600 },
            ))
            .id();
        for it in owned_items {
            if let Some(mut l) = world.get_mut::<Located>(it) {
                l.0 = corpse;
            }
            // Strip EquippedSlot — items inside a corpse aren't
            // worn anymore. crate uses `mud_world::EquippedSlot`.
            try_remove::<mud_world::EquippedSlot>(world, it);
        }

        // Ghost the player. HP pinned to 1 so any further damage is
        // a no-op (data path and apply_damage both clamp at 0/1).
        try_insert(world, victim, Ghost);
        if let Some(mut hp) = world.get_mut::<Health>(victim) {
            hp.hp = 1;
        }

        send_to(
            world,
            victim,
            "You collapse, your spirit drifting free of your dying body.\r\nType `release` to return to your recall point.\r\n",
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[victim],
            &format!("{victim_name} collapses, dead.\r\n"),
        );
        info!(?victim, name = %victim_name, ?corpse, "player corpsed");
    } else {
        // Mob death: notify, drop loot into a corpse, despawn the
        // mob, stop attackers. Without the corpse path the items
        // the loader put on the mob (equipment) would be orphaned
        // ECS entities pointing at a despawned parent.
        broadcast_room_except_rendered(
            world,
            room,
            &[],
            &format!("{victim_name} dies.\r\n"),
        );
        award_kill_coin(world, victim, victim_name);
        award_kill_xp(world, victim, victim_name);
        // Fire DEATH triggers BEFORE despawn so the body can read
        // self.room, broadcast last words, etc. The trigger
        // dispatcher takes a snapshot of bodies up front, so even if
        // the body somehow despawns mid-fire it still completes
        // safely.
        crate::triggers::fire_event(world, victim, mud_world::TriggerEvent::Death);
        let owned_items: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
            q.iter(world)
                .filter(|(_, l)| l.0 == victim)
                .map(|(e, _)| e)
                .collect()
        };
        let killer: Option<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Fighting), With<Player>>();
            q.iter(world).find(|(_, f)| f.0 == victim).map(|(e, _)| e)
        };
        if !owned_items.is_empty() {
            let corpse = world
                .spawn((
                    Item,
                    Corpse,
                    Named { name: format!("the corpse of {victim_name}") },
                    Keywords(vec![
                        "corpse".to_string(),
                        victim_name.to_ascii_lowercase(),
                    ]),
                    Located(room),
                    CorpseDecay { remaining_secs: 600 },
                ))
                .id();
            for it in &owned_items {
                if let Some(mut l) = world.get_mut::<Located>(*it) {
                    l.0 = corpse;
                }
                try_remove::<mud_world::EquippedSlot>(world, *it);
            }
            // Auto-loot: if the killer has the flag, immediately
            // pull every item out of the corpse onto them and let
                // the corpse decay empty. Quiet — players opted in.
            let auto_loot = killer
                .and_then(|k| world.get::<mud_world::PlayerFlags>(k).cloned())
                .is_some_and(|pf| pf.has(mud_db::enums::PlayerFlag::AutoLoot));
            if let (Some(killer), true) = (killer, auto_loot) {
                let mut moved = 0;
                for it in &owned_items {
                    if let Some(mut l) = world.get_mut::<Located>(*it) {
                        l.0 = killer;
                        moved += 1;
                    }
                }
                if moved > 0 {
                    send_to(
                        world,
                        killer,
                        format!(
                            "You loot {moved} item(s) from the corpse of {victim_name}.\r\n"
                        ),
                    );
                }
            }
        }
        disengage_attackers_of(world, victim);
        if let Ok(e) = world.get_entity_mut(victim) {
            e.despawn();
        }
        info!(?victim, name = %victim_name, "mob despawned");
    }
}

/// On mob death: look up the proto's `wealth`, find the first player
/// engaged with the victim, and (when `AUTO_GOLD` is set) add the coin
/// to their `Wealth`. No-op when the mob has no wealth, no proto, or
/// no player attacker.
///
/// When `AUTO_GOLD` is *not* set, the coin is forfeited and the player
/// is told as much. A real corpse model that survives the despawn
/// would let players `get coin from corpse` instead of forfeiting;
/// until then, the toggle is the only knob.
fn award_kill_coin(world: &mut World, victim: Entity, victim_name: &str) {
    let coin = world
        .get::<WorldKey>(victim)
        .and_then(|k| {
            world
                .get_resource::<MobPrototypes>()
                .and_then(|p| p.by_key.get(&(k.zone, k.id)).map(|proto| proto.wealth))
        })
        .unwrap_or(0);
    if coin <= 0 {
        return;
    }
    let killer: Option<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Fighting), With<Player>>();
        q.iter(world).find(|(_, f)| f.0 == victim).map(|(e, _)| e)
    };
    let Some(killer) = killer else { return };
    let auto_gold = world
        .get::<PlayerFlags>(killer)
        .is_some_and(|pf| pf.has(mud_db::enums::PlayerFlag::AutoGold));
    if !auto_gold {
        let msg = crate::commands::format_wealth(coin).unwrap_or_else(|| "no coin".to_string());
        send_to(
            world,
            killer,
            format!(
                "You leave {msg} scattered around the corpse of {victim_name}. \
                 (Set `autogold` to collect automatically.)\r\n"
            ),
        );
        return;
    }

    // AutoSplit: divide the coin among in-room group members.
    // Killer's flag drives the policy — if they don't have AutoSplit,
    // they keep the full take.
    let auto_split = world
        .get::<PlayerFlags>(killer)
        .is_some_and(|pf| pf.has(mud_db::enums::PlayerFlag::AutoSplit));
    let killer_room = world.get::<mud_world::Located>(killer).map(|l| l.0);
    let recipients: Vec<Entity> = if auto_split && killer_room.is_some() {
        let root = crate::commands::group_root(world, killer);
        let members = crate::commands::group_members(world, root);
        members
            .into_iter()
            .filter(|m| world.get::<mud_world::Located>(*m).map(|l| l.0) == killer_room)
            .collect()
    } else {
        vec![killer]
    };
    let recipients = if recipients.is_empty() {
        vec![killer]
    } else {
        recipients
    };
    let n = i64::try_from(recipients.len()).unwrap_or(1).max(1);
    let share = (coin / n).max(1);
    for r in &recipients {
        if let Some(mut w) = world.get_mut::<Wealth>(*r) {
            w.0 = w.0.saturating_add(share);
        } else {
            try_insert(world, *r, Wealth(share));
        }
        let line = if recipients.len() == 1 {
            let msg =
                crate::commands::format_wealth(coin).unwrap_or_else(|| "no coin".to_string());
            format!("You collect {msg} from the corpse of {victim_name}.\r\n")
        } else {
            let msg =
                crate::commands::format_wealth(share).unwrap_or_else(|| "no coin".to_string());
            format!(
                "You collect {msg} (group share) from the corpse of {victim_name}.\r\n"
            )
        };
        send_to(world, *r, line);
    }
}

/// On mob death: compute kill XP from the proto's `level` × role
/// multiplier and add it to the killer's `Profile.experience`.
/// Formula: `base_xp = level * 50`, scaled by role:
///   Trash 0.5x / Normal 1.0x / Elite 2.0x / Miniboss 5.0x /
///   Boss 10.0x / `RaidBoss` 20.0x.
/// No-op when the killer has no Profile (admin testing path) or
/// the mob has no proto.
fn award_kill_xp(world: &mut World, victim: Entity, victim_name: &str) {
    use mud_db::enums::MobRole;
    let proto = world
        .get::<WorldKey>(victim)
        .and_then(|k| {
            world
                .get_resource::<MobPrototypes>()
                .and_then(|p| p.by_key.get(&(k.zone, k.id)).cloned())
        });
    let Some(proto) = proto else { return };
    let multiplier_pct = match proto.role {
        MobRole::Trash => 50,
        MobRole::Normal => 100,
        MobRole::Elite => 200,
        MobRole::Miniboss => 500,
        MobRole::Boss => 1000,
        MobRole::RaidBoss => 2000,
    };
    let base = proto.level.max(1) * 50;
    let xp = (base * multiplier_pct) / 100;
    if xp <= 0 {
        return;
    }
    let killer: Option<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Fighting), With<Player>>();
        q.iter(world).find(|(_, f)| f.0 == victim).map(|(e, _)| e)
    };
    let Some(killer) = killer else { return };

    // Group XP share: walk the killer's group (rooted at the
    // top-of-chain follower target), keep only members in the
    // same room as the killer, and split the kill XP evenly. Solo
    // kills (no follow chain) get the full amount.
    let killer_room = world.get::<mud_world::Located>(killer).map(|l| l.0);
    let recipients: Vec<Entity> = {
        let root = crate::commands::group_root(world, killer);
        let members = crate::commands::group_members(world, root);
        members
            .into_iter()
            .filter(|m| {
                killer_room.is_some()
                    && world.get::<mud_world::Located>(*m).map(|l| l.0) == killer_room
            })
            .collect()
    };
    let recipients = if recipients.is_empty() {
        vec![killer]
    } else {
        recipients
    };
    let n = i32::try_from(recipients.len()).unwrap_or(1).max(1);
    let share = (xp / n).max(1);

    for entity in &recipients {
        if let Some(mut p) = world.get_mut::<mud_world::Profile>(*entity) {
            p.experience = p.experience.saturating_add(share);
        } else {
            continue;
        }
        let line = if *entity == killer && recipients.len() == 1 {
            format!("You gain {share} experience for the kill of {victim_name}.\r\n")
        } else {
            format!(
                "You gain {share} experience (group share) for the kill of {victim_name}.\r\n"
            )
        };
        send_to(world, *entity, line);
        check_level_up(world, *entity);
    }
}

/// Check whether `entity`'s `Profile.experience` has crossed the
/// next-level threshold, and if so promote (possibly multiple
/// levels in one call) — incrementing `Profile.level`, expanding
/// `Health.max` and `Stamina.max` by the row's gain values, and
/// emitting a "you advanced to level N" line per step.
fn check_level_up(world: &mut World, entity: Entity) {
    use mud_world::{LevelTable, Profile};
    let table = world.resource::<LevelTable>().clone_rows();
    loop {
        let (level, xp) = match world.get::<Profile>(entity) {
            Some(p) => (p.level, p.experience),
            None => return,
        };
        let next = level + 1;
        let Some(next_row) = table.iter().find(|r| r.level == next) else {
            return; // max level
        };
        if xp < next_row.exp_required {
            return;
        }
        // Level up.
        if let Some(mut p) = world.get_mut::<Profile>(entity) {
            p.level = next;
        }
        if let Some(mut h) = world.get_mut::<mud_world::Health>(entity) {
            h.max = h.max.saturating_add(next_row.hp_gain);
            h.hp = h.max; // full heal on level-up
        }
        if let Some(mut s) = world.get_mut::<mud_world::Stamina>(entity) {
            s.max = s.max.saturating_add(next_row.stamina_gain);
            s.current = s.max;
        }
        // Grant 1 practice point per level gained. The runtime
        // formula isn't carried by `LevelDefinition` today; bumping
        // the schema is logged in SUGGESTIONS for the user.
        if let Some(mut sp) = world.get_mut::<mud_world::SkillPoints>(entity) {
            sp.0 = sp.0.saturating_add(1);
        }
        send_to(
            world,
            entity,
            format!(
                "*** You have advanced to level {next}{}! ***\r\n\
                 You gained 1 practice point.\r\n",
                next_row
                    .name
                    .as_deref()
                    .map_or_else(String::new, |n| format!(" ({n})"))
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn a minimal "room" (just an Entity with no components — combat
    /// only needs an Entity handle for Located references; nothing reads
    /// room contents during a swing) and return its handle.
    fn make_room(world: &mut World) -> Entity {
        world.spawn_empty().id()
    }

    /// Spawn an attacker with Fighting+CombatStats+Located+Named pointed
    /// at `target`. `dmg_roll` is configurable so callers can predict the
    /// numeric outcome.
    fn make_attacker(
        world: &mut World,
        room: Entity,
        target: Entity,
        dmg_roll: i32,
    ) -> Entity {
        world
            .spawn((
                Named { name: "Attacker".to_string() },
                Located(room),
                Fighting(target),
                CombatStats {
                    hit_roll: 0,
                    dmg_roll,
                    ac: 10,
                    alignment: 0,
                },
                Posture(PostureKind::Standing),
            ))
            .id()
    }

    fn make_target(world: &mut World, room: Entity, hp: i32) -> Entity {
        world
            .spawn((
                Named { name: "Target".to_string() },
                Located(room),
                Health { hp, max: hp },
            ))
            .id()
    }

    fn run_combat_tick(world: &mut World) {
        // combat_tick fires only on multiples of COMBAT_PERIOD_TICKS (10).
        world.insert_resource(TickCount(COMBAT_PERIOD_TICKS));
        combat_tick(world);
    }

    #[test]
    fn applies_damage_when_attacker_swings() {
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = make_target(&mut world, room, 50);
        let _attacker = make_attacker(&mut world, room, target, 7);

        run_combat_tick(&mut world);

        let hp = world.get::<Health>(target).expect("target still has Health");
        assert_eq!(hp.hp, 43, "target HP dropped by attacker's dmg_roll");
        assert_eq!(hp.max, 50, "max HP unchanged");
    }

    #[test]
    fn skips_off_period_ticks() {
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = make_target(&mut world, room, 50);
        let _attacker = make_attacker(&mut world, room, target, 7);
        // Off-period: nothing should happen.
        world.insert_resource(TickCount(COMBAT_PERIOD_TICKS - 1));
        combat_tick(&mut world);
        let hp = world.get::<Health>(target).expect("target still has Health");
        assert_eq!(hp.hp, 50, "no swing fires off-period");
    }

    #[test]
    fn auto_disengages_on_room_mismatch() {
        let mut world = World::new();
        let room_a = make_room(&mut world);
        let room_b = make_room(&mut world);
        let target = make_target(&mut world, room_b, 50);
        let attacker = make_attacker(&mut world, room_a, target, 7);

        run_combat_tick(&mut world);

        // Different rooms — attacker drops Fighting, no damage applied.
        assert!(
            world.get::<Fighting>(attacker).is_none(),
            "attacker disengaged after room mismatch"
        );
        assert_eq!(world.get::<Health>(target).unwrap().hp, 50);
    }

    #[test]
    fn sleeping_attacker_does_not_swing() {
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = make_target(&mut world, room, 50);
        let attacker = make_attacker(&mut world, room, target, 7);
        // Override posture to Sleeping.
        world
            .get_entity_mut(attacker)
            .unwrap()
            .insert(Posture(PostureKind::Sleeping));

        run_combat_tick(&mut world);

        assert_eq!(
            world.get::<Health>(target).unwrap().hp,
            50,
            "no damage from sleeping attacker"
        );
        // Fighting stays — the player is still committed; they just couldn't act.
        assert!(world.get::<Fighting>(attacker).is_some());
    }

    #[test]
    fn lethal_blow_despawns_mob() {
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = world
            .spawn((
                Mob,
                Named { name: "Target".to_string() },
                Located(room),
                Health { hp: 5, max: 5 },
            ))
            .id();
        let attacker = make_attacker(&mut world, room, target, 100);

        run_combat_tick(&mut world);

        assert!(
            world.get_entity(target).is_err(),
            "lethal blow despawned the mob"
        );
        // Attacker's Fighting should be cleared too — handle_death sweeps
        // every Fighting against the dead victim.
        assert!(
            world.get::<Fighting>(attacker).is_none(),
            "attacker's Fighting cleared after target died"
        );
    }

    #[test]
    fn handle_death_player_spawns_corpse_and_ghosts_player() {
        let mut world = World::new();
        let room = make_room(&mut world);
        // Insert a tick resource so any system queried during the test
        // doesn't panic on missing TickCount.
        world.insert_resource(TickCount(0));
        let player = world
            .spawn((
                Player,
                Named { name: "Tester".to_string() },
                Located(room),
                Health { hp: 0, max: 100 },
                Posture(PostureKind::Standing),
            ))
            .id();
        // Two carried items, one of them equipped on the body slot.
        let _carried = world
            .spawn((
                Item,
                Named { name: "a stick".to_string() },
                Keywords(vec!["stick".to_string()]),
                Located(player),
            ))
            .id();
        let _worn = world
            .spawn((
                Item,
                Named { name: "a robe".to_string() },
                Keywords(vec!["robe".to_string()]),
                Located(player),
                mud_world::EquippedSlot(Slot::Body),
            ))
            .id();

        super::handle_death(&mut world, player, "Tester", room);

        // Player should now be a Ghost with HP pinned to 1.
        assert!(
            world.get::<Ghost>(player).is_some(),
            "player gains Ghost marker on death"
        );
        let hp = world.get::<Health>(player).expect("player keeps Health");
        assert_eq!(hp.hp, 1, "ghost HP pinned to 1");
        // A Corpse Item should exist in the death room.
        let corpse = world
            .query_filtered::<(Entity, &Located, &Named, &CorpseDecay), With<Corpse>>()
            .iter(&world)
            .find(|(_, l, _, _)| l.0 == room)
            .map(|(e, _, n, d)| (e, n.name.clone(), d.remaining_secs));
        let (corpse_entity, corpse_name, decay) = corpse.expect("corpse spawned in room");
        assert!(corpse_name.contains("Tester"), "corpse names the dead player");
        assert!(decay > 0, "corpse has positive decay timer");
        // Both items should now be Located on the corpse, not the player.
        let on_corpse: Vec<String> = world
            .query_filtered::<(&Located, &Named), With<Item>>()
            .iter(&world)
            .filter(|(l, _)| l.0 == corpse_entity)
            .map(|(_, n)| n.name.clone())
            .collect();
        assert_eq!(on_corpse.len(), 2, "both items moved to corpse");
        // Worn item should have shed its EquippedSlot.
        let still_equipped = world
            .query_filtered::<&mud_world::EquippedSlot, With<Item>>()
            .iter(&world)
            .count();
        assert_eq!(still_equipped, 0, "EquippedSlot stripped on death");
    }

    #[test]
    fn decay_milestone_fires_on_crossing_tick() {
        let n = "the corpse of a wolf";
        // Crosses 300 from above.
        assert!(decay_milestone(301, 300, n).unwrap().contains("Flies"));
        // No crossing (still above the threshold).
        assert!(decay_milestone(450, 449, n).is_none());
        // Crosses 120.
        assert!(decay_milestone(121, 120, n).unwrap().contains("stink"));
        // Crosses 30.
        assert!(decay_milestone(31, 30, n).unwrap().contains("decay"));
        // No second fire when already past threshold.
        assert!(decay_milestone(120, 119, n).is_none());
        // Snapshot-restored corpse with weird boundary still trips
        // the right threshold on the way down.
        assert!(decay_milestone(305, 295, n).unwrap().contains("Flies"));
    }
}
